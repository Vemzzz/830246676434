use crate::auth;
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Json, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
    jwk::{JwkSet, PublicKeyUse},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    env,
    time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

const COOKIE_NAMES: [&str; 3] = ["oidc_state", "oidc_nonce", "oidc_code_verifier"];
const DEFAULT_SCOPES: &str = "openid profile email";

#[derive(Clone)]
struct Config {
    issuer_url: String,
    client_id: String,
    client_secret: String,
    scopes: String,
}

#[derive(serde::Deserialize)]
struct Discovery {
    issuer: Option<String>,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    jwks_uri: Option<String>,
}

fn trimmed_setting(settings: &Value, key: &str) -> String {
    settings
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn runtime_config() -> Result<Option<Config>, String> {
    let settings = auth::settings_from_db()?;
    let mode = trimmed_setting(&settings, "authMode");
    let issuer_url = trimmed_setting(&settings, "oidcIssuerUrl")
        .trim_end_matches('/')
        .to_owned();
    let client_id = trimmed_setting(&settings, "oidcClientId");
    let client_secret = trimmed_setting(&settings, "oidcClientSecret");
    if !["oidc", "both"].contains(&mode.as_str())
        || issuer_url.is_empty()
        || client_id.is_empty()
        || client_secret.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(Config {
        issuer_url,
        client_id,
        client_secret,
        scopes: {
            let scopes = trimmed_setting(&settings, "oidcScopes");
            if scopes.is_empty() {
                DEFAULT_SCOPES.to_owned()
            } else {
                scopes
            }
        },
    }))
}

fn public_origin(request: &Request<Body>) -> Result<String, String> {
    let configured = env::var("BASE_URL")
        .or_else(|_| env::var("NEXT_PUBLIC_BASE_URL"))
        .unwrap_or_default();
    if !configured.trim().is_empty() {
        return normalize_origin(configured.trim());
    }
    let headers = request.headers();
    let forwarded_host = headers
        .get("x-forwarded-host")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty());
    let host = forwarded_host
        .or_else(|| headers.get("host").and_then(|value| value.to_str().ok()))
        .ok_or_else(|| "Request Host header is missing".to_owned())?;
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .filter(|value| *value == "http" || *value == "https")
        .or_else(|| request.uri().scheme_str())
        .unwrap_or("http");
    normalize_origin(&format!("{proto}://{host}"))
}

fn normalize_origin(value: &str) -> Result<String, String> {
    let parsed = Url::parse(value).map_err(|error| error.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Dashboard origin must use HTTP or HTTPS".to_owned());
    }
    Ok(parsed.origin().ascii_serialization())
}

fn callback_uri(request: &Request<Body>) -> Result<String, String> {
    Ok(format!(
        "{}/api/auth/oidc/callback",
        public_origin(request)?
    ))
}

fn query_param(request: &Request<Body>, name: &str) -> Option<String> {
    request
        .uri()
        .query()
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

fn random_url_token(length: usize) -> Result<String, String> {
    let mut bytes = vec![0; length];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn set_cookie(name: &str, value: &str, secure: bool, max_age: u64) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
}

fn expire_oidc_cookies(secure: bool) -> Vec<String> {
    COOKIE_NAMES
        .iter()
        .map(|name| auth::expire_cookie(name, secure))
        .collect()
}

fn redirect(location: &str, cookies: &[String]) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert("location", value);
    } else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Invalid redirect URL").into_response();
    }
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

fn login_redirect(request: &Request<Body>, error: &str, cookies: &[String]) -> Response {
    let Ok(origin) = public_origin(request) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Could not determine dashboard origin"})),
        )
            .into_response();
    };
    let mut url = match Url::parse(&format!("{origin}/login")) {
        Ok(url) => url,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    url.query_pairs_mut().append_pair("error", error);
    redirect(url.as_str(), cookies)
}

async fn fetch_json(url: &str) -> Result<Value, String> {
    let parsed = Url::parse(url).map_err(|error| error.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("OIDC endpoint URL is invalid".to_owned());
    }
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?
        .get(parsed)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to load OIDC endpoint document (HTTP {})",
            response.status()
        ));
    }
    response.json().await.map_err(|error| error.to_string())
}

async fn discovery(issuer_url: &str) -> Result<Discovery, String> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer_url.trim_end_matches('/')
    );
    let value = fetch_json(&url).await?;
    serde_json::from_value(value)
        .map_err(|error| format!("Invalid OIDC discovery document: {error}"))
}

fn authorization_url(
    endpoint: &str,
    config: &Config,
    callback: &str,
    state: &str,
    nonce: &str,
    challenge: &str,
) -> Result<String, String> {
    let mut url = Url::parse(endpoint).map_err(|error| error.to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("OIDC authorization endpoint URL is invalid".to_owned());
    }
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", callback)
        .append_pair("scope", &config.scopes)
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

fn fetch_metadata_error(error: &str) -> Response {
    (StatusCode::BAD_GATEWAY, Json(json!({"error": error}))).into_response()
}

pub async fn start(request: Request<Body>) -> Response {
    let headers = request.headers().clone();
    let secure = auth::cookie_secure(&headers);
    let sec_purpose = headers
        .get("sec-purpose")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let purpose = headers
        .get("purpose")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if sec_purpose == "prefetch" || purpose == "prefetch" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Prefetch not allowed"})),
        )
            .into_response();
    }
    if headers
        .get("sec-fetch-mode")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != "navigate")
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Invalid request mode"})),
        )
            .into_response();
    }
    if headers
        .get("sec-fetch-dest")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != "document")
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Invalid request destination"})),
        )
            .into_response();
    }
    let config = match runtime_config() {
        Ok(Some(config)) => config,
        Ok(None) => return login_redirect(&request, "oidc_not_configured", &[]),
        Err(error) => return auth::server_error(&error),
    };
    let origin = match public_origin(&request) {
        Ok(origin) => origin,
        Err(error) => return auth::server_error(&error),
    };
    let discovery = match discovery(&config.issuer_url).await {
        Ok(discovery) => discovery,
        Err(error) => return login_redirect(&request, &error, &[]),
    };
    let Some(endpoint) = discovery.authorization_endpoint.as_deref() else {
        return login_redirect(&request, "OIDC authorization endpoint is missing", &[]);
    };
    let state = match random_url_token(16) {
        Ok(token) => token,
        Err(error) => return auth::server_error(&error),
    };
    let nonce = match random_url_token(16) {
        Ok(token) => token,
        Err(error) => return auth::server_error(&error),
    };
    let verifier = match random_url_token(32) {
        Ok(token) => token,
        Err(error) => return auth::server_error(&error),
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let callback = format!("{origin}/api/auth/oidc/callback");
    let auth_url = match authorization_url(endpoint, &config, &callback, &state, &nonce, &challenge)
    {
        Ok(url) => url,
        Err(error) => return login_redirect(&request, &error, &[]),
    };
    redirect(
        &auth_url,
        &[
            set_cookie("oidc_state", &state, secure, 600),
            set_cookie("oidc_nonce", &nonce, secure, 600),
            set_cookie("oidc_code_verifier", &verifier, secure, 600),
        ],
    )
}

async fn exchange_code(
    endpoint: &str,
    config: &Config,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Value, String> {
    let endpoint_url = Url::parse(endpoint).map_err(|error| error.to_string())?;
    if !matches!(endpoint_url.scheme(), "http" | "https") || endpoint_url.host_str().is_none() {
        return Err("OIDC token endpoint URL is invalid".to_owned());
    }
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?
        .post(endpoint_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let data: Value = response
        .json()
        .await
        .map_err(|error| format!("OIDC token endpoint returned invalid JSON: {error}"))?;
    if !status.is_success() {
        return Err(data
            .get("error_description")
            .or_else(|| data.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("OIDC token exchange failed ({status})")));
    }
    Ok(data)
}

fn algorithm_allowed(algorithm: Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512
            | Algorithm::ES256
            | Algorithm::ES384
            | Algorithm::EdDSA
    )
}

fn verify_id_token(
    token: &str,
    issuer: &str,
    audience: &str,
    jwks: &JwkSet,
    nonce: &str,
) -> Result<Value, String> {
    let header = decode_header(token).map_err(|error| error.to_string())?;
    if !algorithm_allowed(header.alg) {
        return Err("OIDC ID token uses an unsupported signing algorithm".to_owned());
    }
    let kid = header
        .kid
        .as_deref()
        .ok_or_else(|| "OIDC ID token does not include a key ID".to_owned())?;
    let jwk = jwks
        .find(kid)
        .ok_or_else(|| "OIDC signing key was not found".to_owned())?;
    if jwk
        .common
        .public_key_use
        .as_ref()
        .is_some_and(|use_| *use_ != PublicKeyUse::Signature)
    {
        return Err("OIDC signing key is not intended for signatures".to_owned());
    }
    if jwk
        .common
        .key_algorithm
        .is_some_and(|algorithm| format!("{algorithm:?}") != format!("{:?}", header.alg))
    {
        return Err("OIDC signing key algorithm does not match the ID token".to_owned());
    }
    if jwk
        .common
        .key_operations
        .as_ref()
        .is_some_and(|operations| !operations.contains(&jsonwebtoken::jwk::KeyOperations::Verify))
    {
        return Err("OIDC signing key cannot be used for verification".to_owned());
    }
    match (&jwk.algorithm, header.alg) {
        (
            jsonwebtoken::jwk::AlgorithmParameters::RSA(_),
            Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512,
        ) => {}
        (jsonwebtoken::jwk::AlgorithmParameters::EllipticCurve(params), Algorithm::ES256)
            if params.curve == jsonwebtoken::jwk::EllipticCurve::P256 => {}
        (jsonwebtoken::jwk::AlgorithmParameters::EllipticCurve(params), Algorithm::ES384)
            if params.curve == jsonwebtoken::jwk::EllipticCurve::P384 => {}
        (jsonwebtoken::jwk::AlgorithmParameters::OctetKeyPair(params), Algorithm::EdDSA)
            if params.curve == jsonwebtoken::jwk::EllipticCurve::Ed25519 => {}
        _ => return Err("OIDC signing key type does not match the ID token".to_owned()),
    }
    let decoding_key = DecodingKey::from_jwk(jwk).map_err(|error| error.to_string())?;
    let mut validation = Validation::new(header.alg);
    validation.leeway = 0;
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    validation.required_spec_claims = ["exp", "iss", "sub", "aud"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let claims = decode::<Value>(token, &decoding_key, &validation)
        .map_err(|error| format!("OIDC ID token validation failed: {error}"))?
        .claims;
    validate_nonce(&claims, nonce)?;
    if claims
        .get("sub")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("OIDC ID token subject is missing".to_owned());
    }
    Ok(claims)
}

fn validate_nonce(claims: &Value, nonce: &str) -> Result<(), String> {
    if claims.get("nonce").and_then(Value::as_str) == Some(nonce) {
        Ok(())
    } else {
        Err("OIDC ID token nonce does not match".to_owned())
    }
}

fn oidc_email(claims: &Value) -> Option<String> {
    claims
        .get("email")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn oidc_name(claims: &Value) -> String {
    ["preferred_username", "email", "name", "given_name", "sub"]
        .iter()
        .find_map(|key| {
            claims
                .get(*key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("OIDC user")
        .to_owned()
}

fn create_session(headers: &HeaderMap, claims: &Value) -> Response {
    let secret = match auth::dashboard_secret() {
        Ok(secret) => secret,
        Err(error) => return auth::auth_error(StatusCode::SERVICE_UNAVAILABLE, &error),
    };
    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(now) => now.as_secs(),
        Err(error) => return auth::server_error(&error.to_string()),
    };
    let name = oidc_name(claims);
    let email = oidc_email(claims);
    let token_claims = json!({
        "authenticated": true,
        "oidc": true,
        "oidcSub": claims.get("sub").and_then(Value::as_str),
        "oidcEmail": email,
        "oidcName": name,
        "iat": now,
        "exp": now + 86400,
    });
    let token = match encode(
        &Header::new(Algorithm::HS256),
        &token_claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    ) {
        Ok(token) => token,
        Err(error) => return auth::server_error(&error.to_string()),
    };
    let secure = auth::cookie_secure(headers);
    auth::token_response(
        StatusCode::SEE_OTHER,
        json!({"success": true}),
        &[auth::make_cookie("auth_token", &token, 86400, secure)],
    )
}

pub async fn callback(request: Request<Body>) -> Response {
    let secure = auth::cookie_secure(request.headers());
    let origin = match public_origin(&request) {
        Ok(origin) => origin,
        Err(error) => return auth::server_error(&error),
    };
    if let Some(error) = query_param(&request, "error") {
        return login_redirect(&request, &error, &expire_oidc_cookies(secure));
    }
    let Some(code) = query_param(&request, "code") else {
        return login_redirect(&request, "oidc_missing_code", &expire_oidc_cookies(secure));
    };
    let Some(state) = query_param(&request, "state") else {
        return login_redirect(&request, "oidc_missing_code", &expire_oidc_cookies(secure));
    };
    let stored_state = cookie_value(request.headers(), "oidc_state");
    let stored_nonce = cookie_value(request.headers(), "oidc_nonce");
    let verifier = cookie_value(request.headers(), "oidc_code_verifier");
    if stored_state.as_deref() != Some(state.as_str())
        || stored_nonce.is_none()
        || verifier.is_none()
    {
        return login_redirect(&request, "oidc_invalid_state", &expire_oidc_cookies(secure));
    }
    let config = match runtime_config() {
        Ok(Some(config)) => config,
        Ok(None) => {
            return login_redirect(
                &request,
                "oidc_not_configured",
                &expire_oidc_cookies(secure),
            );
        }
        Err(error) => return login_redirect(&request, &error, &expire_oidc_cookies(secure)),
    };
    let discovery = match discovery(&config.issuer_url).await {
        Ok(discovery) => discovery,
        Err(error) => return login_redirect(&request, &error, &expire_oidc_cookies(secure)),
    };
    let Some(token_endpoint) = discovery.token_endpoint.as_deref() else {
        return login_redirect(
            &request,
            "OIDC token endpoint is missing",
            &expire_oidc_cookies(secure),
        );
    };
    let Some(jwks_uri) = discovery.jwks_uri.as_deref() else {
        return login_redirect(
            &request,
            "OIDC JWKS URI is missing",
            &expire_oidc_cookies(secure),
        );
    };
    let redirect_uri = match callback_uri(&request) {
        Ok(uri) => uri,
        Err(error) => return auth::server_error(&error),
    };
    let token_data = match exchange_code(
        token_endpoint,
        &config,
        &code,
        &redirect_uri,
        verifier.as_deref().unwrap_or_default(),
    )
    .await
    {
        Ok(data) => data,
        Err(error) => return login_redirect(&request, &error, &expire_oidc_cookies(secure)),
    };
    let Some(id_token) = token_data.get("id_token").and_then(Value::as_str) else {
        return login_redirect(
            &request,
            "OIDC provider did not return an id_token",
            &expire_oidc_cookies(secure),
        );
    };
    let jwks_data = match fetch_json(jwks_uri).await {
        Ok(data) => data,
        Err(error) => return login_redirect(&request, &error, &expire_oidc_cookies(secure)),
    };
    let jwks: JwkSet = match serde_json::from_value(jwks_data) {
        Ok(jwks) => jwks,
        Err(error) => {
            return login_redirect(
                &request,
                &format!("Invalid OIDC JWKS: {error}"),
                &expire_oidc_cookies(secure),
            );
        }
    };
    let issuer = discovery
        .issuer
        .as_deref()
        .filter(|issuer| !issuer.is_empty())
        .unwrap_or(&config.issuer_url);
    let claims = match verify_id_token(
        id_token,
        issuer,
        &config.client_id,
        &jwks,
        stored_nonce.as_deref().unwrap_or_default(),
    ) {
        Ok(claims) => claims,
        Err(error) => return login_redirect(&request, &error, &expire_oidc_cookies(secure)),
    };
    let mut response = create_session(request.headers(), &claims);
    for cookie in expire_oidc_cookies(secure) {
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append("set-cookie", value);
        }
    }
    if let Ok(location) = HeaderValue::from_str(&format!("{origin}/dashboard")) {
        response.headers_mut().insert("location", location);
    }
    response
}

pub async fn test(request: Request<Body>) -> Response {
    let origin = match public_origin(&request) {
        Ok(origin) => origin,
        Err(error) => return auth::server_error(&error),
    };
    let settings = match auth::settings_from_db() {
        Ok(settings) => settings,
        Err(error) => return auth::server_error(&error),
    };
    if settings.get("requireLogin").and_then(Value::as_bool) != Some(false) {
        let secret = match auth::dashboard_secret() {
            Ok(secret) => secret,
            Err(error) => return auth::auth_error(StatusCode::SERVICE_UNAVAILABLE, &error),
        };
        let token = cookie_value(request.headers(), "auth_token");
        if !token
            .as_deref()
            .and_then(|token| auth::verify_dashboard_token(token, &secret))
            .is_some()
        {
            return auth::auth_error(StatusCode::UNAUTHORIZED, "Unauthorized");
        }
    }
    let body = match to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(body) => body,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let issuer_url = body
        .get("issuerUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            settings
                .get("oidcIssuerUrl")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default();
    let client_id = body
        .get("clientId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            settings
                .get("oidcClientId")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default();
    let scopes = body
        .get("scopes")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            settings
                .get("oidcScopes")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(DEFAULT_SCOPES)
        .to_owned();
    let client_secret = if body
        .as_object()
        .is_some_and(|object| object.contains_key("clientSecret"))
    {
        body.get("clientSecret")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    } else {
        settings
            .get("oidcClientSecret")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    if issuer_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Issuer URL is required"})),
        )
            .into_response();
    }
    if client_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Client ID is required"})),
        )
            .into_response();
    }
    let discovery = match discovery(&issuer_url).await {
        Ok(discovery) => discovery,
        Err(error) => return fetch_metadata_error(&error),
    };
    let Some(token_endpoint) = discovery.token_endpoint.as_deref() else {
        return fetch_metadata_error("OIDC token endpoint is missing");
    };
    let redirect_uri = format!("{origin}/api/auth/oidc/callback");
    let (tested, valid, message) = if client_secret.is_empty() {
        (
            false,
            Value::Null,
            "No client secret was provided, so secret validation was skipped.".to_owned(),
        )
    } else {
        match probe_secret(token_endpoint, &client_id, &client_secret, &redirect_uri).await {
            Ok(result) => result,
            Err(error) => return fetch_metadata_error(&error),
        }
    };
    let authorization_endpoint = discovery.authorization_endpoint.unwrap_or_default();
    let jwks_uri = discovery.jwks_uri.unwrap_or_default();
    let mut response_body = json!({
        "ok": valid != Value::Bool(false),
        "discoveryOk": true,
        "clientSecretTested": tested,
        "clientSecretValid": valid,
        "issuerUrl": issuer_url,
        "clientId": client_id,
        "scopes": scopes,
        "redirectUri": redirect_uri,
        "authorizationEndpoint": authorization_endpoint,
        "tokenEndpoint": token_endpoint,
        "jwksUri": jwks_uri,
        "message": message,
    });
    if valid == Value::Bool(false) {
        response_body["error"] = json!(format!(
            "Discovery loaded, but the client secret is not valid: {message}"
        ));
    }
    (StatusCode::OK, Json(response_body)).into_response()
}

async fn probe_secret(
    token_endpoint: &str,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
) -> Result<(bool, Value, String), String> {
    let endpoint = Url::parse(token_endpoint).map_err(|error| error.to_string())?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err("OIDC token endpoint URL is invalid".to_owned());
    }
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?
        .post(endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", "__oidc_test_invalid_code__"),
            ("redirect_uri", redirect_uri),
            ("code_verifier", "__oidc_test_invalid_verifier__"),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let data: Value = response
        .json()
        .await
        .map_err(|error| format!("OIDC token endpoint returned invalid JSON: {error}"))?;
    let error = data
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let description = data
        .get("error_description")
        .or_else(|| data.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if status.is_success() {
        return Ok((
            true,
            Value::Bool(true),
            "Client secret was accepted by the token endpoint.".to_owned(),
        ));
    }
    if error == "invalid_client"
        || error == "unauthorized_client"
        || (description.to_ascii_lowercase().contains("client")
            && ["invalid", "failed", "mismatch"]
                .iter()
                .any(|word| description.to_ascii_lowercase().contains(word)))
    {
        return Ok((true, Value::Bool(false), description));
    }
    if error == "invalid_grant"
        || error == "invalid_code"
        || description.to_ascii_lowercase().contains("grant")
        || description.to_ascii_lowercase().contains("code")
    {
        return Ok((
            true,
            Value::Bool(true),
            "Client secret was accepted; the token exchange failed only because the test authorization code is invalid.".to_owned(),
        ));
    }
    Ok((
        true,
        Value::Null,
        if description.is_empty() {
            format!("Token endpoint responded with {status}")
        } else {
            description
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oidc_origin_normalization_rejects_non_http_schemes() {
        assert_eq!(
            normalize_origin("https://dashboard.example/path/").unwrap(),
            "https://dashboard.example"
        );
        assert!(normalize_origin("javascript:alert(1)").is_err());
    }

    #[test]
    fn oidc_rejects_hmac_signing_algorithms() {
        for algorithm in [Algorithm::HS256, Algorithm::HS384, Algorithm::HS512] {
            assert!(!algorithm_allowed(algorithm));
        }
        assert!(algorithm_allowed(Algorithm::RS256));
        assert!(algorithm_allowed(Algorithm::PS256));
        assert!(algorithm_allowed(Algorithm::ES256));
        assert!(algorithm_allowed(Algorithm::EdDSA));
    }

    #[test]
    fn oidc_nonce_is_required_and_exact() {
        let claims = json!({"nonce":"expected"});
        assert!(validate_nonce(&claims, "expected").is_ok());
        assert!(validate_nonce(&claims, "attacker-value").is_err());
        assert!(validate_nonce(&json!({}), "expected").is_err());
    }
}
