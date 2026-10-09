use super::{data_dir, open_database, open_database_for_writes};
use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use bcrypt::{hash, verify};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    env,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const RESET_HINT: &str =
    "Forgot password? Use the VansRouter CLI on the host to set a new dashboard password.";
const PLACEHOLDER_PASSWORDS: &[&str] = &[
    "123456",
    "change-me",
    "change-me-in-production",
    "change-me-to-a-long-random-secret",
    "password",
    "changeme",
];

struct LoginAttempt {
    failures: u32,
    lock_until: Option<Instant>,
    lock_level: usize,
    last_failure: Instant,
}

fn login_attempts() -> &'static Mutex<HashMap<String, LoginAttempt>> {
    static ATTEMPTS: OnceLock<Mutex<HashMap<String, LoginAttempt>>> = OnceLock::new();
    ATTEMPTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn unix_time() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| error.to_string())
}

pub(super) fn dashboard_secret() -> Result<String, String> {
    if let Ok(secret) = env::var("JWT_SECRET")
        && !secret.is_empty()
    {
        return Ok(secret);
    }
    let directory = data_dir()?;
    std::fs::read_to_string(directory.join("jwt-secret"))
        .map(|secret| secret.trim().to_owned())
        .map_err(|_| "Dashboard JWT_SECRET is not configured on the Rust backend".to_owned())
        .and_then(|secret| {
            if secret.is_empty() {
                Err("Dashboard JWT_SECRET is empty".to_owned())
            } else {
                Ok(secret)
            }
        })
}

fn read_settings(connection: &Connection) -> Result<Value, String> {
    let settings = connection
        .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|error| error.to_string())?;
    match settings {
        Some(raw) => serde_json::from_str(&raw).map_err(|error| error.to_string()),
        None => Ok(json!({})),
    }
}

fn initial_password() -> Option<String> {
    let configured = env::var("INITIAL_PASSWORD")
        .unwrap_or_default()
        .trim()
        .to_owned();
    if configured.is_empty() {
        return Some("123456".to_owned());
    }
    if env::var("NODE_ENV").as_deref() != Ok("production") || configured == "123456" {
        return Some(configured);
    }
    if configured.len() >= 12
        && !PLACEHOLDER_PASSWORDS
            .iter()
            .any(|placeholder| configured.eq_ignore_ascii_case(placeholder))
    {
        Some(configured)
    } else {
        None
    }
}

fn is_placeholder_password(password: &str) -> bool {
    PLACEHOLDER_PASSWORDS
        .iter()
        .any(|placeholder| password.trim().eq_ignore_ascii_case(placeholder))
}

fn strong_password(password: &str) -> bool {
    password.trim().len() >= 12 && !is_placeholder_password(password)
}

pub(super) fn verify_dashboard_token(token: &str, secret: &str) -> Option<Value> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    validation.validate_nbf = true;
    validation.required_spec_claims.insert("exp".to_owned());
    decode::<Value>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()
    .map(|data| data.claims)
}

fn cookie_value<'a>(headers: &'a HeaderMap, cookie_name: &str) -> Option<&'a str> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| (name == cookie_name).then_some(value))
}

fn password_change_grant(headers: &HeaderMap, secret: &str) -> bool {
    cookie_value(headers, "password_change")
        .and_then(|token| verify_dashboard_token(token, secret))
        .and_then(|claims| {
            claims
                .get("scope")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|scope| scope == "password-change")
}

fn request_ip(headers: &HeaderMap) -> String {
    let expected_peer_token = env::var("VANSROUTER_PEER_TOKEN")
        .or_else(|_| env::var("NINEROUTER_PEER_TOKEN"))
        .ok();
    if expected_peer_token.as_deref().is_some_and(|expected| {
        headers
            .get("x-9r-peer-token")
            .and_then(|value| value.to_str().ok())
            == Some(expected)
    }) && let Some(ip) = headers
        .get("x-9r-real-ip")
        .and_then(|value| value.to_str().ok())
    {
        return ip.to_owned();
    }
    if env::var("TRUST_PROXY").as_deref() == Ok("true")
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
    {
        return ip.trim().to_owned();
    }
    "unknown".to_owned()
}

fn check_login_lock(ip: &str) -> Result<Option<u64>, String> {
    let mut attempts = login_attempts()
        .lock()
        .map_err(|_| "Login limiter lock is unavailable".to_owned())?;
    let Some(entry) = attempts.get_mut(ip) else {
        return Ok(None);
    };
    let now = Instant::now();
    if now.duration_since(entry.last_failure) > Duration::from_secs(3600)
        && entry.lock_until.is_none_or(|until| now >= until)
    {
        attempts.remove(ip);
        return Ok(None);
    }
    Ok(entry
        .lock_until
        .and_then(|until| until.checked_duration_since(now))
        .filter(|remaining| !remaining.is_zero())
        .map(|remaining| remaining.as_secs().saturating_add(1)))
}

fn record_login_failure(ip: &str) -> Result<(u32, Option<u64>), String> {
    const LOCK_STEPS: [u64; 4] = [30, 120, 600, 1800];
    let mut attempts = login_attempts()
        .lock()
        .map_err(|_| "Login limiter lock is unavailable".to_owned())?;
    let now = Instant::now();
    let entry = attempts.entry(ip.to_owned()).or_insert(LoginAttempt {
        failures: 0,
        lock_until: None,
        lock_level: 0,
        last_failure: now,
    });
    entry.failures += 1;
    entry.last_failure = now;
    if entry.failures >= 5 {
        let seconds = LOCK_STEPS[entry.lock_level.min(LOCK_STEPS.len() - 1)];
        entry.lock_until = Some(now + Duration::from_secs(seconds));
        entry.lock_level += 1;
        entry.failures = 0;
    }
    let remaining = 5u32.saturating_sub(entry.failures);
    let retry_after = entry
        .lock_until
        .and_then(|until| until.checked_duration_since(now))
        .filter(|duration| !duration.is_zero())
        .map(|duration| duration.as_secs().saturating_add(1));
    Ok((remaining, retry_after))
}

fn record_login_success(ip: &str) -> Result<(), String> {
    login_attempts()
        .lock()
        .map_err(|_| "Login limiter lock is unavailable".to_owned())?
        .remove(ip);
    Ok(())
}

pub(super) fn cookie_secure(headers: &HeaderMap) -> bool {
    env::var("AUTH_COOKIE_SECURE").as_deref() == Ok("true")
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            == Some("https")
}

pub(super) fn make_cookie(name: &str, value: &str, age_seconds: u64, secure: bool) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={age_seconds}{}",
        if secure { "; Secure" } else { "" }
    )
}

pub(super) fn expire_cookie(name: &str, secure: bool) -> String {
    format!(
        "{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

pub(super) fn token_response(status: StatusCode, body: Value, cookies: &[String]) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            response.headers_mut().append("set-cookie", value);
        }
    }
    response
}

pub(super) fn auth_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

pub(super) fn server_error(message: &str) -> Response {
    auth_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

fn set_retry_after(response: &mut Response, seconds: u64) {
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert("retry-after", value);
    }
}

struct LoginTimer(Instant);

impl Drop for LoginTimer {
    fn drop(&mut self) {
        let elapsed = self.0.elapsed();
        if elapsed >= Duration::from_millis(500) {
            eprintln!(
                "[api/auth/login] total request time: {}ms",
                elapsed.as_millis()
            );
        }
    }
}

fn warn_if_slow_login_stage(stage: &str, elapsed: Duration) {
    if elapsed >= Duration::from_millis(500) {
        eprintln!("[api/auth/login] {stage} took {}ms", elapsed.as_millis());
    }
}

async fn verify_password(password: &str, encoded_hash: &str) -> Result<bool, String> {
    let password = password.to_owned();
    let encoded_hash = encoded_hash.to_owned();
    tokio::task::spawn_blocking(move || verify(&password, &encoded_hash))
        .await
        .map_err(|error| format!("Password verification task failed: {error}"))?
        .map_err(|error| format!("Stored password hash is invalid: {error}"))
}

async fn hash_password(password: &str) -> Result<String, String> {
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || hash(password, 10))
        .await
        .map_err(|error| format!("Password hashing task failed: {error}"))?
        .map_err(|error| format!("Password hashing failed: {error}"))
}

fn validate_tunnel_access(headers: &HeaderMap, settings: &Value) -> bool {
    if settings
        .get("tunnelDashboardAccess")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return true;
    }
    let incoming_host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    for setting in ["tunnelUrl", "tailscaleUrl"] {
        let Some(url) = settings.get(setting).and_then(Value::as_str) else {
            continue;
        };
        if url.trim().is_empty() {
            continue;
        }
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };
        let Some(tunnel_host) = parsed.host_str() else {
            return false;
        };
        if tunnel_host.eq_ignore_ascii_case(&incoming_host) {
            return false;
        }
    }
    true
}

pub(super) fn settings_from_db() -> Result<Value, String> {
    let connection = open_database()?;
    read_settings(&connection)
}

pub async fn login(request: axum::http::Request<Body>) -> Response {
    let _timer = LoginTimer(Instant::now());
    let headers = request.headers().clone();
    let peer = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ConnectInfo(address)| *address)
        .unwrap_or_else(|| std::net::SocketAddr::from(([0, 0, 0, 0], 0)));
    let is_local = super::is_local_request(&request, peer);
    let ip = request_ip(request.headers());
    match check_login_lock(&ip) {
        Ok(Some(retry_after)) => {
            let mut response = token_response(
                StatusCode::TOO_MANY_REQUESTS,
                json!({
                    "error": format!("Too many failed attempts. Try again in {retry_after}s. {RESET_HINT}"),
                    "retryAfter": retry_after,
                    "resetHint": RESET_HINT
                }),
                &[],
            );
            set_retry_after(&mut response, retry_after);
            return response;
        }
        Ok(None) => {}
        Err(error) => return server_error(&error),
    }

    let bytes = match to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return auth_error(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => return auth_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    let Some(password) = body.get("password").and_then(Value::as_str) else {
        return auth_error(StatusCode::BAD_REQUEST, "Password is required");
    };

    let settings_started = Instant::now();
    let settings = match tokio::task::spawn_blocking(settings_from_db).await {
        Ok(Ok(settings)) => settings,
        Ok(Err(error)) => return server_error(&error),
        Err(error) => return server_error(&format!("Settings read task failed: {error}")),
    };
    warn_if_slow_login_stage("settings read", settings_started.elapsed());
    if !validate_tunnel_access(&headers, &settings) {
        return auth_error(
            StatusCode::FORBIDDEN,
            "Dashboard access via tunnel is disabled",
        );
    }

    let oidc_configured = ["oidcIssuerUrl", "oidcClientId", "oidcClientSecret"]
        .iter()
        .all(|field| {
            settings
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        });
    if settings.get("authMode").and_then(Value::as_str) == Some("oidc") && oidc_configured {
        return auth_error(
            StatusCode::FORBIDDEN,
            "Password login is disabled. Use OIDC sign in.",
        );
    }

    let stored_hash = settings
        .get("password")
        .and_then(Value::as_str)
        .filter(|password_hash| !password_hash.is_empty());
    if stored_hash.is_none() && initial_password().is_none() {
        return token_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": "INITIAL_PASSWORD must be 123456 or a strong value before login" }),
            &[],
        );
    }
    let password_valid = match stored_hash {
        Some(stored_hash) => {
            let verify_started = Instant::now();
            let result = verify_password(password, stored_hash).await;
            warn_if_slow_login_stage("bcrypt verification", verify_started.elapsed());
            match result {
                Ok(valid) => valid,
                Err(error) => return server_error(&error),
            }
        }
        None => initial_password().is_some_and(|initial| password == initial),
    };
    if !password_valid {
        match record_login_failure(&ip) {
            Ok((_remaining, Some(retry_after))) => {
                let mut response = token_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({
                        "error": format!("Too many failed attempts. Try again in {retry_after}s. {RESET_HINT}"),
                        "retryAfter": retry_after,
                        "resetHint": RESET_HINT
                    }),
                    &[],
                );
                set_retry_after(&mut response, retry_after);
                return response;
            }
            Ok((remaining, None)) => {
                return token_response(
                    StatusCode::UNAUTHORIZED,
                    json!({
                        "error": format!("Invalid password. {remaining} attempt(s) left before lockout."),
                        "remainingBeforeLock": remaining
                    }),
                    &[],
                );
            }
            Err(error) => return server_error(&error),
        }
    }
    if let Err(error) = record_login_success(&ip) {
        return server_error(&error);
    }

    let secure = cookie_secure(&headers);
    let initial = initial_password();
    let must_change = stored_hash.is_none()
        && initial.as_deref().is_some_and(is_placeholder_password)
        && !is_local;
    let secret = match dashboard_secret() {
        Ok(secret) => secret,
        Err(error) => return auth_error(StatusCode::SERVICE_UNAVAILABLE, &error),
    };
    let now = match unix_time() {
        Ok(now) => now,
        Err(error) => return server_error(&error),
    };
    let (cookie_name, claims, max_age, response_body) = if must_change {
        (
            "password_change",
            json!({ "scope": "password-change", "iat": now, "exp": now + 600 }),
            600,
            json!({ "success": true, "mustChangePassword": true }),
        )
    } else {
        (
            "auth_token",
            json!({ "authenticated": true, "iat": now, "exp": now + 86400 }),
            86400,
            json!({ "success": true, "mustChangePassword": false }),
        )
    };
    let token = match encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    ) {
        Ok(token) => token,
        Err(error) => return server_error(&error.to_string()),
    };
    let mut cookies = vec![make_cookie(cookie_name, &token, max_age, secure)];
    if must_change {
        cookies.push(expire_cookie("auth_token", secure));
    } else {
        cookies.push(expire_cookie("password_change", secure));
    }
    token_response(StatusCode::OK, response_body, &cookies)
}

pub async fn status(request: axum::http::Request<Body>) -> Response {
    let settings = match settings_from_db() {
        Ok(settings) => settings,
        Err(_) => {
            return Json(json!({
                "requireLogin": true,
                "authMode": "password",
                "oidcConfigured": false,
                "oidcLoginLabel": "Sign in with OIDC",
                "hasPassword": false,
                "displayName": "Password user",
                "loginMethod": "Password",
                "oidcName": null,
                "oidcEmail": null,
                "oidcLogin": false
            }))
            .into_response();
        }
    };
    let secret = dashboard_secret().ok();
    let session = match (
        secret.as_deref(),
        cookie_value(request.headers(), "auth_token"),
    ) {
        (Some(secret), Some(token)) => verify_dashboard_token(token, secret),
        _ => None,
    };
    let oidc_login = session
        .as_ref()
        .and_then(|claims| claims.get("oidc"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let oidc_name = session
        .as_ref()
        .and_then(|claims| claims.get("oidcName"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let oidc_email = session
        .as_ref()
        .and_then(|claims| claims.get("oidcEmail"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let oidc_configured = ["oidcIssuerUrl", "oidcClientId", "oidcClientSecret"]
        .iter()
        .all(|field| {
            settings
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        });
    let auth_mode = settings
        .get("authMode")
        .and_then(Value::as_str)
        .unwrap_or("password");
    let login_label = settings
        .get("oidcLoginLabel")
        .and_then(Value::as_str)
        .unwrap_or("Sign in with OIDC")
        .trim();
    let display_name = if !oidc_name.is_empty() {
        oidc_name
    } else if !oidc_email.is_empty() {
        oidc_email
    } else if oidc_login {
        "OIDC user"
    } else {
        "Password user"
    };
    (
        [("cache-control", "no-store")],
        Json(json!({
            "requireLogin": settings.get("requireLogin").and_then(Value::as_bool) != Some(false),
            "authMode": auth_mode,
            "oidcConfigured": oidc_configured,
            "oidcLoginLabel": if login_label.is_empty() { "Sign in with OIDC" } else { login_label },
            "hasPassword": settings.get("password").and_then(Value::as_str).is_some_and(|value| !value.is_empty()),
            "displayName": display_name,
            "loginMethod": if oidc_login { "OIDC" } else { "Password" },
            "oidcName": if oidc_name.is_empty() { None } else { Some(oidc_name) },
            "oidcEmail": if oidc_email.is_empty() { None } else { Some(oidc_email) },
            "oidcLogin": oidc_login
        })),
    )
        .into_response()
}

pub async fn check(request: axum::http::Request<Body>) -> Response {
    let settings = match settings_from_db() {
        Ok(settings) => settings,
        Err(_) => return auth_error(StatusCode::UNAUTHORIZED, "Unauthenticated"),
    };
    if settings.get("requireLogin").and_then(Value::as_bool) == Some(false) {
        return (StatusCode::OK, Json(json!({ "authenticated": true }))).into_response();
    }
    let secret = match dashboard_secret() {
        Ok(secret) => secret,
        Err(_) => return auth_error(StatusCode::UNAUTHORIZED, "Unauthenticated"),
    };
    let valid = cookie_value(request.headers(), "auth_token")
        .and_then(|token| verify_dashboard_token(token, &secret))
        .is_some();
    if valid {
        (StatusCode::OK, Json(json!({ "authenticated": true }))).into_response()
    } else {
        auth_error(StatusCode::UNAUTHORIZED, "Unauthenticated")
    }
}

pub async fn logout(request: axum::http::Request<Body>) -> Response {
    let secure = cookie_secure(request.headers());
    token_response(
        StatusCode::OK,
        json!({ "success": true }),
        &[
            expire_cookie("auth_token", secure),
            expire_cookie("password_change", secure),
            expire_cookie("oidc_state", secure),
            expire_cookie("oidc_nonce", secure),
            expire_cookie("oidc_code_verifier", secure),
        ],
    )
}

fn update_password(connection: &mut Connection, password: &str) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let current = read_settings(&transaction)?;
    let mut settings = current.as_object().cloned().unwrap_or_default();
    settings.insert("password".to_owned(), Value::String(password.to_owned()));
    let serialized =
        serde_json::to_string(&Value::Object(settings)).map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO settings(id, data) VALUES(1, ?1)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![serialized],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO _meta(key, value) VALUES('settings_revision', '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

pub async fn reset_password(request: axum::http::Request<Body>) -> Response {
    let headers = request.headers().clone();
    let peer = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ConnectInfo(address)| *address)
        .unwrap_or_else(|| std::net::SocketAddr::from(([0, 0, 0, 0], 0)));
    let is_local = super::is_local_request(&request, peer);
    let secret = match dashboard_secret() {
        Ok(secret) => secret,
        Err(error) => return auth_error(StatusCode::SERVICE_UNAVAILABLE, &error),
    };
    let is_change_grant = password_change_grant(&headers, &secret);
    let is_dashboard_session = is_local
        && cookie_value(&headers, "auth_token")
            .and_then(|token| verify_dashboard_token(token, &secret))
            .is_some();
    let is_cli_session = super::extract_cli_token(&request).is_some_and(super::is_valid_cli_token);
    if !is_change_grant && !is_dashboard_session && !is_cli_session {
        return auth_error(
            StatusCode::UNAUTHORIZED,
            "Dashboard authentication required",
        );
    }
    let bytes = match to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return auth_error(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => return auth_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    let Some(new_password) = body.get("newPassword").and_then(Value::as_str) else {
        return auth_error(StatusCode::BAD_REQUEST, "New password is required");
    };
    if !strong_password(new_password) {
        return auth_error(
            StatusCode::BAD_REQUEST,
            "New password must be at least 12 characters and not a placeholder",
        );
    }

    let settings = match settings_from_db() {
        Ok(settings) => settings,
        Err(error) => return server_error(&error),
    };
    if is_change_grant {
        let Some(current_password) = body.get("currentPassword").and_then(Value::as_str) else {
            return auth_error(
                StatusCode::FORBIDDEN,
                "Password change required: currentPassword is required",
            );
        };
        let password_valid = match settings.get("password").and_then(Value::as_str) {
            Some(stored_hash) => match verify_password(current_password, stored_hash).await {
                Ok(valid) => valid,
                Err(error) => return server_error(&error),
            },
            None => initial_password().is_some_and(|initial| current_password == initial),
        };
        if !password_valid {
            return auth_error(StatusCode::UNAUTHORIZED, "Invalid current password");
        }
    }
    let password_hash = match hash_password(new_password).await {
        Ok(password_hash) => password_hash,
        Err(error) => return server_error(&error),
    };
    let mut connection = match open_database_for_writes() {
        Ok(connection) => connection,
        Err(error) => return server_error(&error),
    };
    if let Err(error) = update_password(&mut connection, &password_hash) {
        return server_error(&error);
    }
    let secure = cookie_secure(&headers);
    let cookies = if is_change_grant {
        let now = match unix_time() {
            Ok(now) => now,
            Err(error) => return server_error(&error),
        };
        let claims = json!({ "authenticated": true, "iat": now, "exp": now + 86400 });
        let token = match encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        ) {
            Ok(token) => token,
            Err(error) => return server_error(&error.to_string()),
        };
        vec![
            make_cookie("auth_token", &token, 86400, secure),
            expire_cookie("password_change", secure),
        ]
    } else {
        Vec::new()
    };
    token_response(StatusCode::OK, json!({ "success": true }), &cookies)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bcrypt_password_hashes_verify_with_the_configured_cost() {
        let encoded = hash_password("a-long-password-for-test").await.unwrap();
        assert!(
            verify_password("a-long-password-for-test", &encoded)
                .await
                .unwrap()
        );
        assert!(
            !verify_password("a-different-password", &encoded)
                .await
                .unwrap()
        );
    }

    #[test]
    fn password_change_cookie_requires_the_scoped_hs256_claim() {
        let secret = "shared-dashboard-test-secret";
        let claims = json!({
            "scope": "password-change",
            "iat": 1_800_000_000u64,
            "exp": 1_800_000_600u64
        });
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("password_change={token}")).unwrap(),
        );

        assert!(password_change_grant(&headers, secret));

        let wrong_scope = json!({
            "scope": "dashboard",
            "iat": 1_800_000_000u64,
            "exp": 1_800_000_600u64
        });
        let token = encode(
            &Header::new(Algorithm::HS256),
            &wrong_scope,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("password_change={token}")).unwrap(),
        );
        assert!(!password_change_grant(&headers, secret));
    }
}
