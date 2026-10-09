use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Path, Request},
    http::{Method, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{any, get},
};
use futures_util::StreamExt;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    env,
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::PathBuf,
    sync::{Mutex as StdMutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};

mod admin;
mod auth;
mod availability;
mod locale;
mod models;
mod oidc;
mod pricing;
mod provider_discovery;
mod provider_overrides;
mod providers_client;
mod request_details;
mod usage;

const VERSION_CACHE_TTL: Duration = Duration::from_secs(300);
const VERSION_FAILURE_BACKOFF: Duration = Duration::from_secs(30);
const NPM_VERSION_URL: &str = "https://registry.npmjs.org/vansrouter/latest";
const APP_PACKAGE_JSON: &str = include_str!("../../package.json");
const MODEL_CATALOG_JSON: &str = include_str!("../models.json");
const PRICING_CATALOG_JSON: &str = include_str!("../pricing.json");
const MAX_CHAT_BODY_BYTES: usize = 128 * 1024 * 1024;
const MAX_ADMIN_BODY_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
struct ModelCatalog {
    providers: Vec<CatalogProvider>,
}

#[derive(Deserialize)]
struct PricingPattern {
    pattern: String,
    pricing: Value,
}

#[derive(Deserialize)]
struct PricingCatalog {
    canonical: HashMap<String, Value>,
    providers: HashMap<String, HashMap<String, Value>>,
    patterns: Vec<PricingPattern>,
}

fn pricing_catalog() -> &'static PricingCatalog {
    static CATALOG: OnceLock<PricingCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(PRICING_CATALOG_JSON)
            .expect("bundled pricing catalog must be valid JSON")
    })
}

fn merge_pricing(base: Option<&Value>, custom: Option<&Value>) -> Option<Value> {
    match (base, custom) {
        (Some(base), Some(custom)) => {
            let mut merged = base.as_object()?.clone();
            if let Some(overrides) = custom.as_object() {
                merged.extend(overrides.clone());
            }
            Some(Value::Object(merged))
        }
        (Some(base), None) => Some(base.clone()),
        (None, Some(custom)) => Some(custom.clone()),
        (None, None) => None,
    }
}

fn model_pricing(
    connection: &Connection,
    provider: &str,
    model: &str,
) -> Result<Option<Value>, String> {
    let catalog = pricing_catalog();
    let base_model = model.rsplit('/').next().unwrap_or(model);
    let user_pricing: Option<String> = connection
        .query_row(
            "SELECT value FROM kv WHERE scope = 'pricing' AND key = ?1",
            params![provider],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let user_pricing = user_pricing
        .map(|raw| serde_json::from_str::<Value>(&raw).map_err(|error| error.to_string()))
        .transpose()?;
    let custom = user_pricing.as_ref().and_then(|pricing| pricing.get(model));
    let default = catalog
        .providers
        .get(provider)
        .and_then(|pricing| pricing.get(model))
        .or_else(|| {
            catalog
                .canonical
                .get(base_model)
                .or_else(|| catalog.canonical.get(model))
        })
        .or_else(|| {
            catalog
                .patterns
                .iter()
                .find(|entry| {
                    wildcard_match(&entry.pattern, base_model)
                        || wildcard_match(&entry.pattern, model)
                })
                .map(|entry| &entry.pricing)
        });
    if default.is_some() || custom.is_some() {
        return Ok(merge_pricing(default, custom));
    }
    Ok(None)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase().into_bytes();
    let value = value.to_ascii_lowercase().into_bytes();
    let mut matches = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    matches[0][0] = true;
    for pattern_index in 0..pattern.len() {
        for value_index in 0..=value.len() {
            if !matches[pattern_index][value_index] {
                continue;
            }
            if pattern[pattern_index] == b'*' {
                matches[pattern_index + 1][value_index] = true;
                if value_index < value.len() {
                    matches[pattern_index][value_index + 1] = true;
                }
            } else if value.get(value_index) == Some(&pattern[pattern_index]) {
                matches[pattern_index + 1][value_index + 1] = true;
            }
        }
    }
    matches[pattern.len()][value.len()]
}

fn calculate_chat_cost(usage: &Value, pricing: &Value) -> f64 {
    let token_count = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| {
                name.split('.')
                    .try_fold(usage, |value, part| value.get(part))
                    .and_then(Value::as_f64)
            })
            .unwrap_or_default()
    };
    let rate = |name: &str, fallback: &str| {
        pricing
            .get(name)
            .and_then(Value::as_f64)
            .or_else(|| pricing.get(fallback).and_then(Value::as_f64))
            .unwrap_or_default()
    };
    let input_tokens = token_count(&["prompt_tokens", "input_tokens"]);
    let cached_tokens = token_count(&[
        "cached_tokens",
        "cache_read_input_tokens",
        "prompt_tokens_details.cached_tokens",
    ]);
    let cache_creation_tokens = token_count(&["cache_creation_input_tokens"]);
    let output_tokens = token_count(&["completion_tokens", "output_tokens"]);
    let reasoning_tokens = token_count(&["reasoning_tokens"]);
    let non_cached_input = (input_tokens - cached_tokens - cache_creation_tokens).max(0.0);
    (non_cached_input * rate("input", "input")
        + cached_tokens * rate("cached", "input")
        + cache_creation_tokens * rate("cache_creation", "input")
        + output_tokens * rate("output", "output")
        + reasoning_tokens * rate("reasoning", "output"))
        / 1_000_000.0
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogProvider {
    id: String,
    alias: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    headers: Value,
    no_auth: bool,
    #[serde(default)]
    auth_modes: Vec<String>,
    #[serde(default)]
    service_kinds: Vec<String>,
    #[serde(default)]
    search_config: Option<Value>,
    #[serde(default)]
    has_search_via_chat: bool,
    #[serde(default)]
    has_fetch_config: bool,
    #[serde(default)]
    usage_supported: bool,
    #[serde(default)]
    usage_apikey_supported: bool,
    #[serde(default)]
    usage_order: usize,
    models: Vec<CatalogModel>,
    #[serde(default)]
    all_models: Vec<CatalogModel>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogModel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    capabilities: Value,
    #[serde(default)]
    options: Option<Value>,
    #[serde(default)]
    dimensions: Option<Value>,
    #[serde(default)]
    context_window: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
struct ListedModel {
    id: String,
    object: &'static str,
    owned_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ListedModel>,
}

#[derive(Default)]
struct ApiKeyAcl {
    allowed_providers: Option<Vec<String>>,
    allowed_combos: Option<Vec<String>>,
    allowed_kinds: Option<Vec<String>>,
}

struct ApiKeyLimits {
    id: String,
    requests_per_minute: Option<u64>,
    max_input_tokens: Option<u64>,
    max_output_tokens: Option<u64>,
    has_ip_whitelist: bool,
}

#[derive(Clone)]
struct ActiveConnection {
    provider: String,
    output_alias: String,
    enabled_models: Option<Vec<String>>,
}

fn model_catalog() -> &'static ModelCatalog {
    static CATALOG: OnceLock<ModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(MODEL_CATALOG_JSON)
            .expect("bundled provider model catalog must be valid JSON")
    })
}

fn database_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("db").join("data.sqlite"))
}

fn data_dir() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| "HOME is not set".to_owned())?;
    let default_dir = || {
        #[cfg(windows)]
        {
            let app_data = env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&home).join("AppData").join("Roaming"));
            app_data.join("9router")
        }
        #[cfg(not(windows))]
        {
            PathBuf::from(&home).join(".9router")
        }
    };
    let configured = env::var("DATA_DIR").ok();
    let production =
        env::var("NODE_ENV").as_deref() == Ok("production") || env::var_os("pm_id").is_some();
    let allow_temp = env::var("DATA_DIR_ALLOW_TEMP").as_deref() == Ok("1");
    Ok(resolve_data_dir(
        configured.as_deref(),
        default_dir(),
        production,
        allow_temp,
        cfg!(windows),
        cfg!(target_os = "macos"),
    ))
}

fn resolve_data_dir(
    configured: Option<&str>,
    default_dir: PathBuf,
    production: bool,
    allow_temp: bool,
    windows: bool,
    macos: bool,
) -> PathBuf {
    let Some(configured) = configured else {
        return default_dir;
    };
    let normalized = configured.to_ascii_lowercase().replace('\\', "/");
    let looks_temporary = normalized.contains("smoke")
        || normalized.starts_with("/tmp/9router-data-")
        || (macos && normalized.contains("/var/folders/"));
    if looks_temporary && !allow_temp && production {
        eprintln!(
            "[DATA_DIR] '{}' looks temporary; using persistent default '{}'",
            configured,
            default_dir.display()
        );
        return default_dir;
    }
    if windows && configured.starts_with('/') {
        eprintln!(
            "[DATA_DIR] '{}' is a Unix path on Windows; using persistent default '{}'",
            configured,
            default_dir.display()
        );
        return default_dir;
    }
    PathBuf::from(configured)
}

fn open_database() -> Result<Connection, String> {
    let path = database_path()?;
    if !path.is_file() {
        return Err(format!("database file does not exist: {}", path.display()));
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    connection
        .pragma_update(None, "query_only", "ON")
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn open_database_for_writes() -> Result<Connection, String> {
    let path = database_path()?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn parse_acl(raw: Option<String>) -> Option<Vec<String>> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
}

fn parse_string_list(value: Option<&Value>) -> Option<Vec<String>> {
    value.and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

fn extract_api_key(request: &Request) -> Option<String> {
    if let Some(value) = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        return Some(value.to_owned());
    }
    request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn extract_cli_token(request: &Request) -> Option<&str> {
    request
        .headers()
        .get("x-9r-cli-token")
        .and_then(|value| value.to_str().ok())
}

fn is_valid_cli_token(token: &str) -> bool {
    let Ok(directory) = data_dir() else {
        return false;
    };
    let Ok(machine_id) = std::fs::read_to_string(directory.join("machine-id")) else {
        return false;
    };
    let Ok(secret) = std::fs::read_to_string(directory.join("auth").join("cli-secret")) else {
        return false;
    };
    let mut hasher = Sha256::new();
    hasher.update(machine_id.trim());
    hasher.update("9r-cli-auth");
    hasher.update(secret.trim());
    let digest = format!("{:x}", hasher.finalize());
    token == &digest[..16]
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn is_local_request(request: &Request, peer: SocketAddr) -> bool {
    let peer_is_loopback = match peer.ip() {
        std::net::IpAddr::V4(address) => address.is_loopback(),
        std::net::IpAddr::V6(address) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
    };
    let has_proxy_headers = request.headers().contains_key("x-9r-via-proxy")
        || request.headers().contains_key("x-forwarded-for")
        || request.headers().contains_key("x-forwarded-host")
        || request.headers().contains_key("forwarded");
    if !peer_is_loopback || has_proxy_headers {
        return false;
    }
    let Some(origin) = request.headers().get("origin") else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    uri.host().is_some_and(is_loopback_host)
}

fn public_api_access_allowed(
    is_local: bool,
    valid_cli_token: bool,
    valid_api_key: bool,
    require_api_key: bool,
    allow_remote_no_api_key: bool,
) -> bool {
    is_local || valid_cli_token || valid_api_key || (!require_api_key && allow_remote_no_api_key)
}

fn setting_bool(connection: &Connection, name: &str) -> Result<Option<bool>, String> {
    let raw = connection
        .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|error| error.to_string())?;
    Ok(raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|settings| settings.get(name).and_then(Value::as_bool)))
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(
            json!({ "error": { "message": message, "type": match status {
            StatusCode::UNAUTHORIZED => "authentication_error",
            StatusCode::FORBIDDEN => "permission_error",
            StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE => "invalid_request_error",
            StatusCode::NOT_FOUND => "not_found_error",
            StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
            _ => "server_error",
        } } }),
        ),
    )
        .into_response()
}

fn admin_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn is_sqlite_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::ConstraintViolation
    )
}

fn dashboard_token_valid(token: &str, secret: &str) -> bool {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    validation.validate_nbf = true;
    validation.required_spec_claims.insert("exp".to_owned());
    decode::<Value>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .is_ok()
}

fn dashboard_session_token(request: &Request) -> Option<&str> {
    request
        .headers()
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| (name == "auth_token").then_some(value))
}

pub(crate) fn require_dashboard_auth(request: &Request) -> Result<(), (StatusCode, &'static str)> {
    if extract_cli_token(request).is_some_and(is_valid_cli_token) {
        return Ok(());
    }
    let secret = auth::dashboard_secret().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Dashboard JWT_SECRET is not configured on the Rust backend",
        )
    })?;
    if dashboard_session_token(request).is_some_and(|token| dashboard_token_valid(token, &secret)) {
        return Ok(());
    }
    Err((
        StatusCode::UNAUTHORIZED,
        "Dashboard authentication required",
    ))
}

pub(crate) async fn parse_admin_json(body: Body) -> Result<Value, (StatusCode, &'static str)> {
    let bytes = to_bytes(body, MAX_ADMIN_BODY_BYTES)
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"))?;
    serde_json::from_slice(&bytes).map_err(|_| (StatusCode::BAD_REQUEST, "Invalid JSON body"))
}

fn load_key_acl(connection: &Connection, api_key: &str) -> Result<Option<ApiKeyAcl>, String> {
    let row = connection
        .query_row(
            "SELECT isActive, allowedProviders, allowedCombos, allowedKinds FROM apiKeys WHERE key = ?1",
            params![api_key],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?.unwrap_or_default() != 0,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((active, providers, combos, kinds)) = row else {
        return Ok(None);
    };
    if !active {
        return Ok(None);
    }
    Ok(Some(ApiKeyAcl {
        allowed_providers: parse_acl(providers),
        allowed_combos: parse_acl(combos),
        allowed_kinds: parse_acl(kinds),
    }))
}

fn load_api_key_limits(
    connection: &Connection,
    api_key: &str,
) -> Result<Option<ApiKeyLimits>, String> {
    connection
        .query_row(
            "SELECT id, requestsPerMinute, maxInputTokens, maxOutputTokens, ipWhitelist, isActive FROM apiKeys WHERE key = ?1",
            params![api_key],
            |row| {
                let active = row.get::<_, Option<i64>>(5)?.unwrap_or_default() != 0;
                let whitelist = row.get::<_, Option<String>>(4)?;
                Ok((
                    active,
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    whitelist,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())
        .map(|row| {
            row.and_then(|(active, id, requests, input, output, whitelist)| {
                active.then(|| ApiKeyLimits {
                    id,
                    requests_per_minute: requests.and_then(|value| u64::try_from(value).ok()),
                    max_input_tokens: input.and_then(|value| u64::try_from(value).ok()),
                    max_output_tokens: output.and_then(|value| u64::try_from(value).ok()),
                    has_ip_whitelist: whitelist
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                        .is_some(),
                })
            })
        })
}

fn consume_api_key_request(
    connection: &Connection,
    api_key_id: &str,
    limit: u64,
    now_ms: u64,
) -> Result<(bool, u64), String> {
    let window_start = now_ms / 60_000 * 60_000;
    let limit = i64::try_from(limit).map_err(|error| error.to_string())?;
    let window_start_sql = i64::try_from(window_start).map_err(|error| error.to_string())?;
    let result = connection
        .execute(
            "INSERT INTO apiKeyRateLimits(apiKeyId, windowStart, requestCount) VALUES(?1, ?2, 1)
             ON CONFLICT(apiKeyId) DO UPDATE SET
               requestCount = CASE
                 WHEN apiKeyRateLimits.windowStart = excluded.windowStart THEN apiKeyRateLimits.requestCount + 1
                 ELSE 1
               END,
               windowStart = excluded.windowStart
             WHERE apiKeyRateLimits.windowStart != excluded.windowStart
               OR apiKeyRateLimits.requestCount < ?3",
            params![api_key_id, window_start_sql, limit],
        )
        .map_err(|error| error.to_string())?;
    if result > 0 {
        Ok((true, 0))
    } else {
        Ok((
            false,
            ((window_start + 60_000 - now_ms).div_ceil(1000)).max(1),
        ))
    }
}

fn read_active_connections(connection: &Connection) -> Result<Vec<ActiveConnection>, String> {
    let mut statement = connection
        .prepare("SELECT provider, data FROM providerConnections WHERE isActive = 1")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let provider: String = row.get(0)?;
            let data: String = row.get(1)?;
            Ok((provider, data))
        })
        .map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let (provider, raw_data) = row.map_err(|error| error.to_string())?;
        if !seen.insert(provider.clone()) {
            continue;
        }
        let data: Value = serde_json::from_str(&raw_data).unwrap_or_else(|_| json!({}));
        let provider_data = data.get("providerSpecificData").and_then(Value::as_object);
        let provider_catalog = model_catalog()
            .providers
            .iter()
            .find(|entry| entry.id == provider || entry.alias == provider);
        let static_alias = provider_catalog
            .map(|entry| entry.alias.as_str())
            .unwrap_or(provider.as_str());
        let output_alias = provider_data
            .and_then(|data| data.get("prefix"))
            .and_then(Value::as_str)
            .filter(|prefix| !prefix.is_empty())
            .unwrap_or(static_alias)
            .to_owned();
        let enabled_models = provider_data
            .and_then(|data| data.get("enabledModels"))
            .and_then(|value| parse_string_list(Some(value)))
            .filter(|models| !models.is_empty());
        result.push(ActiveConnection {
            provider,
            output_alias,
            enabled_models,
        });
    }
    Ok(result)
}

fn read_kv_scope(connection: &Connection, scope: &str) -> Result<HashMap<String, Value>, String> {
    let mut statement = connection
        .prepare("SELECT key, value FROM kv WHERE scope = ?1")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut values = HashMap::new();
    for row in rows {
        let (key, raw_value) = row.map_err(|error| error.to_string())?;
        let value = serde_json::from_str(&raw_value).unwrap_or(Value::Null);
        values.insert(key, value);
    }
    Ok(values)
}

fn provider_allowed(
    connection: &Connection,
    acl: &ApiKeyAcl,
    provider: &str,
    provider_id: &str,
) -> Result<bool, String> {
    let Some(allowed) = &acl.allowed_providers else {
        return Ok(true);
    };
    if allowed
        .iter()
        .any(|entry| entry == provider || entry == provider_id)
    {
        return Ok(true);
    }
    if let Some(catalog_provider) = model_catalog()
        .providers
        .iter()
        .find(|entry| entry.id == provider_id)
        && allowed.iter().any(|entry| entry == &catalog_provider.alias)
    {
        return Ok(true);
    }
    let node_data = connection
        .query_row(
            "SELECT data FROM providerNodes WHERE id = ?1",
            params![provider_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let prefix = node_data
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|data| {
            data.get("prefix")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    Ok(prefix.is_some_and(|prefix| allowed.iter().any(|entry| entry == &prefix)))
}

fn kind_allowed(acl: &ApiKeyAcl, kind: &str) -> bool {
    acl.allowed_kinds
        .as_ref()
        .is_none_or(|allowed| allowed.iter().any(|entry| entry == kind))
}

fn combo_allowed(acl: &ApiKeyAcl, name: &str) -> bool {
    acl.allowed_combos
        .as_ref()
        .is_none_or(|allowed| allowed.iter().any(|entry| entry == name))
}

fn model_entry(id: String, owner: String, capabilities: Option<Value>) -> ListedModel {
    let context_length = capabilities
        .as_ref()
        .and_then(|caps| caps.get("contextWindow"))
        .and_then(Value::as_u64);
    let max_completion_tokens = capabilities
        .as_ref()
        .and_then(|caps| caps.get("maxOutput"))
        .and_then(Value::as_u64);
    ListedModel {
        id,
        object: "model",
        owned_by: owner,
        capabilities,
        context_length,
        max_completion_tokens,
    }
}

fn normalize_model_id(id: &str, aliases: &[&str]) -> Option<String> {
    let mut normalized = id.trim();
    for alias in aliases {
        if let Some(suffix) = normalized.strip_prefix(&format!("{alias}/")) {
            normalized = suffix;
            break;
        }
    }
    (!normalized.is_empty()).then(|| normalized.to_owned())
}

fn infer_llm_kind(model_id: &str) -> bool {
    let id = model_id.to_ascii_lowercase();
    ![
        "embed",
        "tts",
        "speech",
        "audio",
        "voice",
        "image",
        "imagen",
        "dall-e",
        "dalle",
        "flux",
        "sdxl",
        "sd-",
        "stable-diffusion",
        "video",
    ]
    .iter()
    .any(|marker| id.contains(marker))
}

fn read_v1_models(
    api_key: Option<String>,
    cli_token: Option<String>,
    is_local: bool,
) -> Result<ModelList, (StatusCode, String)> {
    let connection = open_database().map_err(|error| {
        eprintln!("[v1/models] database unavailable: {error}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable".to_owned(),
        )
    })?;
    read_v1_models_from_connection(&connection, api_key, cli_token, is_local)
}

fn read_v1_models_from_connection(
    connection: &Connection,
    api_key: Option<String>,
    cli_token: Option<String>,
    is_local: bool,
) -> Result<ModelList, (StatusCode, String)> {
    let settings_raw = connection
        .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|error| {
            eprintln!("[v1/models] settings query failed: {error}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable".to_owned(),
            )
        })?;
    let require_api_key = settings_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|settings| settings.get("requireApiKey").and_then(Value::as_bool))
        .unwrap_or_else(|| env::var("REQUIRE_API_KEY").as_deref() == Ok("true"));
    let allow_remote_no_api_key = settings_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|settings| settings.get("allowRemoteNoApiKey").and_then(Value::as_bool))
        .unwrap_or(false);
    let cli_token_valid = cli_token.as_deref().is_some_and(is_valid_cli_token);
    let key_acl = if let Some(api_key) = api_key.as_deref() {
        load_key_acl(connection, api_key).map_err(|error| {
            eprintln!("[v1/models] API key lookup failed: {error}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable".to_owned(),
            )
        })?
    } else {
        None
    };
    if !public_api_access_allowed(
        is_local,
        cli_token_valid,
        key_acl.is_some(),
        require_api_key,
        allow_remote_no_api_key,
    ) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "API key required for remote API access".to_owned(),
        ));
    }

    let acl = if require_api_key {
        if api_key.is_none() {
            return Err((StatusCode::UNAUTHORIZED, "Missing API key".to_owned()));
        }
        Some(key_acl.ok_or_else(|| (StatusCode::UNAUTHORIZED, "Invalid API key".to_owned()))?)
    } else {
        None
    };
    let acl = acl.unwrap_or_default();

    let connections = read_active_connections(connection).map_err(|error| {
        eprintln!("[v1/models] provider connection query failed: {error}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable".to_owned(),
        )
    })?;
    let custom_models = read_kv_scope(connection, "customModels").map_err(|error| {
        eprintln!("[v1/models] custom model query failed: {error}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable".to_owned(),
        )
    })?;
    let aliases = read_kv_scope(connection, "modelAliases").map_err(|error| {
        eprintln!("[v1/models] model alias query failed: {error}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable".to_owned(),
        )
    })?;
    let disabled = read_kv_scope(connection, "disabledModels").map_err(|error| {
        eprintln!("[v1/models] disabled model query failed: {error}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable".to_owned(),
        )
    })?;

    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut add_entry = |entry: ListedModel| {
        if seen.insert(entry.id.clone()) {
            entries.push(entry);
        }
    };

    {
        let mut statement = connection
            .prepare("SELECT name, kind FROM combos ORDER BY createdAt ASC")
            .map_err(|error| {
                eprintln!("[v1/models] combo query failed: {error}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable".to_owned(),
                )
            })?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(|error| {
                eprintln!("[v1/models] combo query failed: {error}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable".to_owned(),
                )
            })?;
        for row in rows {
            let (name, kind) = row.map_err(|error| {
                eprintln!("[v1/models] combo row invalid: {error}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable".to_owned(),
                )
            })?;
            if kind.as_deref().unwrap_or("llm") != "llm"
                || !kind_allowed(&acl, "llm")
                || !combo_allowed(&acl, &name)
            {
                continue;
            }
            add_entry(model_entry(
                format!("combo/{name}"),
                "combo".to_owned(),
                None,
            ));
        }
    }

    let mut active_by_id = HashMap::new();
    for active in &connections {
        active_by_id.insert(active.provider.as_str(), active);
    }
    for provider in &model_catalog().providers {
        let active = active_by_id
            .get(provider.id.as_str())
            .copied()
            .or_else(|| active_by_id.get(provider.alias.as_str()).copied());
        if active.is_none() && !provider.no_auth {
            continue;
        }
        let output_alias = active
            .map(|connection| connection.output_alias.as_str())
            .unwrap_or(provider.alias.as_str());
        if !kind_allowed(&acl, "llm")
            || !provider_allowed(connection, &acl, output_alias, &provider.id).map_err(|error| {
                eprintln!("[v1/models] provider ACL lookup failed: {error}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable".to_owned(),
                )
            })?
        {
            continue;
        }
        let mut ids: Vec<String> = active
            .and_then(|connection| connection.enabled_models.clone())
            .unwrap_or_else(|| {
                provider
                    .models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect()
            });
        let mut custom_llm_ids = HashSet::new();
        let mut model_aliases = vec![output_alias, provider.alias.as_str(), provider.id.as_str()];
        model_aliases.dedup();

        for value in aliases.values() {
            let Some(target) = value.as_str() else {
                continue;
            };
            if model_aliases
                .iter()
                .any(|alias| target.starts_with(&format!("{alias}/")))
                && let Some(model_id) = normalize_model_id(target, &model_aliases)
            {
                ids.push(model_id);
            }
        }
        for value in custom_models.values() {
            let Some(custom) = value.as_object() else {
                continue;
            };
            let custom_kind = custom.get("type").and_then(Value::as_str).unwrap_or("llm");
            if custom_kind != "llm" && custom_kind != "imageToText" {
                continue;
            }
            if !custom
                .get("providerAlias")
                .and_then(Value::as_str)
                .is_some_and(|alias| model_aliases.contains(&alias))
            {
                continue;
            }
            if let Some(id) = custom.get("id").and_then(Value::as_str) {
                ids.push(id.to_owned());
                custom_llm_ids.insert(id.to_owned());
            }
        }
        ids.sort();
        ids.dedup();

        let disabled_for_alias = disabled.get(output_alias).and_then(Value::as_array);
        let disabled_for_static_alias = disabled.get(&provider.alias).and_then(Value::as_array);
        for model_id in ids {
            let Some(model_id) = normalize_model_id(&model_id, &model_aliases) else {
                continue;
            };
            let is_static_llm = provider.models.iter().any(|model| model.id == model_id);
            if !is_static_llm && !custom_llm_ids.contains(&model_id) && !infer_llm_kind(&model_id) {
                continue;
            }
            if disabled_for_alias
                .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(&model_id)))
                || disabled_for_static_alias
                    .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(&model_id)))
            {
                continue;
            }
            let capabilities = if provider.no_auth {
                None
            } else {
                provider
                    .models
                    .iter()
                    .find(|model| model.id == model_id)
                    .map(|model| model.capabilities.clone())
            };
            add_entry(model_entry(
                format!("{output_alias}/{model_id}"),
                output_alias.to_owned(),
                capabilities,
            ));
        }
    }
    for active in &connections {
        let has_static_catalog = model_catalog()
            .providers
            .iter()
            .any(|provider| provider.id == active.provider || provider.alias == active.provider);
        if has_static_catalog || !kind_allowed(&acl, "llm") {
            continue;
        }
        if !provider_allowed(connection, &acl, &active.output_alias, &active.provider).map_err(
            |error| {
                eprintln!("[v1/models] provider ACL lookup failed: {error}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable".to_owned(),
                )
            },
        )? {
            continue;
        }
        let mut prefix_aliases = vec![active.output_alias.as_str(), active.provider.as_str()];
        prefix_aliases.dedup();
        let mut ids = active.enabled_models.clone().unwrap_or_default();
        let mut custom_llm_ids = HashSet::new();
        for value in aliases.values() {
            let Some(target) = value.as_str() else {
                continue;
            };
            if prefix_aliases
                .iter()
                .any(|alias| target.starts_with(&format!("{alias}/")))
                && let Some(model_id) = normalize_model_id(target, &prefix_aliases)
            {
                ids.push(model_id);
            }
        }
        for value in custom_models.values() {
            let Some(custom) = value.as_object() else {
                continue;
            };
            let custom_kind = custom.get("type").and_then(Value::as_str).unwrap_or("llm");
            if custom_kind != "llm" && custom_kind != "imageToText"
                || !custom
                    .get("providerAlias")
                    .and_then(Value::as_str)
                    .is_some_and(|alias| prefix_aliases.contains(&alias))
            {
                continue;
            }
            if let Some(id) = custom.get("id").and_then(Value::as_str) {
                ids.push(id.to_owned());
                custom_llm_ids.insert(id.to_owned());
            }
        }
        ids.sort();
        ids.dedup();
        let disabled_models = disabled.get(&active.output_alias).and_then(Value::as_array);
        for id in ids {
            let Some(id) = normalize_model_id(&id, &prefix_aliases) else {
                continue;
            };
            if !custom_llm_ids.contains(&id) && !infer_llm_kind(&id) {
                continue;
            }
            if disabled_models
                .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(&id)))
            {
                continue;
            }
            add_entry(model_entry(
                format!("{}/{id}", active.output_alias),
                active.output_alias.clone(),
                None,
            ));
        }
    }

    Ok(ModelList {
        object: "list",
        data: entries,
    })
}

async fn v1_models(ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request) -> Response {
    let api_key = extract_api_key(&request);
    let cli_token = extract_cli_token(&request).map(str::to_owned);
    let is_local = is_local_request(&request, peer);
    match tokio::task::spawn_blocking(move || read_v1_models(api_key, cli_token, is_local)).await {
        Ok(Ok(models)) => {
            Json(json!({ "object": models.object, "data": models.data })).into_response()
        }
        Ok(Err((status, message))) => api_error(status, &message),
        Err(error) => {
            eprintln!("[v1/models] database task failed: {error}");
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable",
            )
        }
    }
}

pub(crate) fn open_database_for_admin() -> Result<Connection, String> {
    let path = database_path()?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn resolve_model_alias(connection: &Connection, requested_model: &str) -> Result<String, String> {
    let aliases = read_kv_scope(connection, "modelAliases")?;
    let mut current = requested_model.to_owned();
    let mut seen = HashSet::new();
    for _ in 0..16 {
        if !seen.insert(current.clone()) {
            return Err("Model alias cycle detected".to_owned());
        }
        let Some(target) = aliases.get(&current).and_then(Value::as_str) else {
            return Ok(current);
        };
        current = target.to_owned();
    }
    Err("Model alias chain is too long".to_owned())
}

fn combo_models_for_chat(
    connection: &Connection,
    model: &str,
) -> Result<Option<(String, Vec<String>, String)>, String> {
    let name = if let Some(name) = model.strip_prefix("combo/") {
        name
    } else if !model.contains('/') {
        model
    } else {
        return Ok(None);
    };
    let Some((canonical_name, raw_models)) = connection
        .query_row(
            "SELECT name, models FROM combos WHERE name = ?1",
            params![name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let models = serde_json::from_str::<Vec<String>>(&raw_models)
        .map_err(|error| format!("Combo model list is invalid: {error}"))?
        .into_iter()
        .filter(|model| !model.trim().is_empty())
        .collect::<Vec<_>>();
    let settings = connection
        .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|error| error.to_string())?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    let strategy = settings
        .get("comboStrategies")
        .and_then(|strategies| strategies.get(&canonical_name))
        .and_then(|settings| settings.get("fallbackStrategy"))
        .and_then(Value::as_str)
        .or_else(|| settings.get("comboStrategy").and_then(Value::as_str))
        .unwrap_or("fallback")
        .to_owned();
    Ok(Some((canonical_name, models, strategy)))
}

fn combo_rotation_index(name: &str, length: usize) -> usize {
    static ROTATION: OnceLock<StdMutex<HashMap<String, usize>>> = OnceLock::new();
    let Ok(mut rotation) = ROTATION
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
    else {
        return 0;
    };
    let index = rotation.get(name).copied().unwrap_or_default() % length;
    rotation.insert(name.to_owned(), (index + 1) % length);
    index
}

fn chat_provider_id(
    connection: &Connection,
    output_alias: &str,
) -> Result<Option<(String, bool)>, String> {
    if let Some(provider) = model_catalog()
        .providers
        .iter()
        .find(|provider| provider.alias == output_alias || provider.id == output_alias)
    {
        return Ok(Some((provider.id.clone(), provider.id == "openai")));
    }

    let mut statement = connection
        .prepare("SELECT id, type, data FROM providerNodes")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (id, kind, raw_data) = row.map_err(|error| error.to_string())?;
        let data = serde_json::from_str::<Value>(&raw_data).unwrap_or(Value::Null);
        let prefix = data.get("prefix").and_then(Value::as_str).unwrap_or(&id);
        if prefix == output_alias || id == output_alias {
            let is_openai_chat = kind == "openai-compatible"
                && data.get("apiType").and_then(Value::as_str) == Some("chat");
            return Ok(Some((id, is_openai_chat)));
        }
    }
    Ok(None)
}

fn load_openai_connection(
    connection: &Connection,
    provider_id: &str,
) -> Result<(String, String, String), String> {
    let (connection_id, raw_data) = connection
        .query_row(
            "SELECT id, data FROM providerConnections WHERE provider = ?1 AND isActive = 1 ORDER BY priority ASC LIMIT 1",
            params![provider_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "No active OpenAI-compatible provider connection".to_owned())?;
    let data: Value = serde_json::from_str(&raw_data).map_err(|error| error.to_string())?;
    let api_key = data
        .get("accessToken")
        .or_else(|| data.get("apiKey"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Provider connection has no API credential".to_owned())?
        .to_owned();
    let configured_base = data
        .get("providerSpecificData")
        .and_then(|value| value.get("baseUrl"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(if provider_id == "openai" {
            "https://api.openai.com/v1"
        } else {
            ""
        });
    if configured_base.is_empty() {
        return Err("Custom OpenAI-compatible provider has no base URL".to_owned());
    }
    let url = if configured_base.ends_with("/chat/completions") {
        configured_base.to_owned()
    } else {
        format!("{}/chat/completions", configured_base.trim_end_matches('/'))
    };
    let parsed: reqwest::Url = url
        .parse()
        .map_err(|error| format!("Invalid OpenAI base URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("OpenAI base URL must use HTTP or HTTPS".to_owned());
    }
    Ok((connection_id, api_key, url))
}

fn add_usage_counter(
    counters: &mut Value,
    key: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cost: f64,
    meta: Value,
) {
    if !counters.is_object() {
        *counters = json!({});
    }
    let entry = counters
        .as_object_mut()
        .expect("usage counters are initialized as objects")
        .entry(key.to_owned())
        .or_insert_with(|| {
            json!({
                "requests": 0,
                "promptTokens": 0,
                "completionTokens": 0,
                "cachedTokens": 0,
                "cost": 0.0
            })
        });
    for (field, amount) in [
        ("requests", 1),
        ("promptTokens", prompt_tokens),
        ("completionTokens", completion_tokens),
        ("cachedTokens", cached_tokens),
    ] {
        let current = entry.get(field).and_then(Value::as_u64).unwrap_or_default();
        entry[field] = Value::from(current.saturating_add(amount));
    }
    let current_cost = entry
        .get("cost")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    entry["cost"] = json!(current_cost + cost);
    if let Some(fields) = meta.as_object() {
        for (key, value) in fields {
            entry[key] = value.clone();
        }
    }
}

fn extract_sse_usage(pending: &mut Vec<u8>, flush: bool) -> Option<Value> {
    let mut latest = None;
    loop {
        let delimiter = pending
            .windows(2)
            .position(|window| window == b"\n\n")
            .map(|index| (index, 2))
            .or_else(|| {
                pending
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| (index, 4))
            });
        let Some((index, delimiter_len)) = delimiter else {
            break;
        };
        let event = pending.drain(..index + delimiter_len).collect::<Vec<_>>();
        if let Some(usage) = usage_from_sse_event(&event) {
            latest = Some(usage);
        }
    }
    if flush && !pending.is_empty() {
        if let Some(usage) = usage_from_sse_event(pending) {
            latest = Some(usage);
        }
        pending.clear();
    }
    if pending.len() > 1024 * 1024 {
        pending.clear();
    }
    latest
}

fn mask_usage_api_key(key: &str) -> String {
    let chars = key.chars().collect::<Vec<_>>();
    if chars.len() <= 12 {
        return format!("{}***", chars.first().copied().unwrap_or('*'));
    }
    format!(
        "{}***{}",
        chars.iter().take(8).collect::<String>(),
        chars.iter().skip(chars.len() - 4).collect::<String>()
    )
}

fn usage_from_sse_event(event: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(event);
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            continue;
        }
        if let Ok(payload) = serde_json::from_str::<Value>(data)
            && let Some(usage) = payload.get("usage")
            && (usage.get("prompt_tokens").is_some()
                || usage.get("input_tokens").is_some()
                || usage.get("completion_tokens").is_some()
                || usage.get("output_tokens").is_some())
        {
            return Some(usage.clone());
        }
    }
    None
}

fn record_chat_usage(
    provider: &str,
    model: &str,
    connection_id: &str,
    api_key: Option<&str>,
    endpoint: &str,
    usage: &Value,
) -> Result<(), String> {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let cached_tokens = usage
        .get("cached_tokens")
        .or_else(|| usage.get("cache_read_input_tokens"))
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if prompt_tokens == 0 && completion_tokens == 0 {
        return Ok(());
    }
    let mut connection = open_database_for_writes()?;
    let cost = model_pricing(&connection, provider, model)?
        .as_ref()
        .map(|pricing| calculate_chat_cost(usage, pricing))
        .unwrap_or_default();
    persist_chat_usage(
        &mut connection,
        provider,
        model,
        connection_id,
        api_key,
        endpoint,
        usage,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_chat_usage(
    connection: &mut Connection,
    provider: &str,
    model: &str,
    connection_id: &str,
    api_key: Option<&str>,
    endpoint: &str,
    usage: &Value,
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    cost: f64,
) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let timestamp = admin::utc_timestamp();
    let api_key_value = api_key.filter(|value| !value.is_empty());
    transaction
        .execute(
            "INSERT INTO usageHistory(timestamp, provider, model, connectionId, apiKey, endpoint, promptTokens, completionTokens, cost, status, tokens, meta) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'ok', ?10, '{}')",
            params![
                timestamp,
                provider,
                model,
                connection_id,
                api_key_value,
                endpoint,
                prompt_tokens,
                completion_tokens,
                cost,
                usage.to_string()
            ],
        )
        .map_err(|error| error.to_string())?;

    let date_key: String = transaction
        .query_row("SELECT date(?1, 'localtime')", params![timestamp], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())?;
    let raw_day: Option<String> = transaction
        .query_row(
            "SELECT data FROM usageDaily WHERE dateKey = ?1",
            params![date_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let mut day = raw_day
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| {
            json!({
                "requests": 0,
                "promptTokens": 0,
                "completionTokens": 0,
                "cachedTokens": 0,
                "cost": 0.0,
                "byProvider": {},
                "byModel": {},
                "byAccount": {},
                "byApiKey": {},
                "byEndpoint": {}
            })
        });
    for (field, amount) in [
        ("requests", 1),
        ("promptTokens", prompt_tokens),
        ("completionTokens", completion_tokens),
        ("cachedTokens", cached_tokens),
    ] {
        let current = day.get(field).and_then(Value::as_u64).unwrap_or_default();
        day[field] = Value::from(current.saturating_add(amount));
    }
    let total_cost = day.get("cost").and_then(Value::as_f64).unwrap_or_default();
    day["cost"] = json!(total_cost + cost);
    let model_key = format!("{model}|{provider}");
    let api_key_string = api_key_value.unwrap_or("local-no-key");
    let api_key_model_key = format!("{api_key_string}|{model}|{provider}");
    let endpoint_model_key = format!("{endpoint}|{model}|{provider}");
    let meta = json!({"rawModel": model, "provider": provider});
    add_usage_counter(
        &mut day["byProvider"],
        provider,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost,
        json!({}),
    );
    add_usage_counter(
        &mut day["byModel"],
        &model_key,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost,
        meta.clone(),
    );
    add_usage_counter(
        &mut day["byAccount"],
        connection_id,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost,
        meta,
    );
    add_usage_counter(
        &mut day["byApiKey"],
        &api_key_model_key,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost,
        json!({"rawModel": model, "provider": provider, "apiKey": api_key_value}),
    );
    add_usage_counter(
        &mut day["byEndpoint"],
        &endpoint_model_key,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost,
        json!({"endpoint": endpoint, "rawModel": model, "provider": provider}),
    );
    transaction
        .execute(
            "INSERT INTO usageDaily(dateKey, data) VALUES(?1, ?2) ON CONFLICT(dateKey) DO UPDATE SET data = excluded.data",
            params![date_key, day.to_string()],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO _meta(key, value) VALUES('totalRequestsLifetime', '1') ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

fn chat_error(status: StatusCode, message: &str) -> Response {
    api_error(status, message)
}

fn chat_rate_limit_error(retry_after: u64) -> Response {
    let mut response = chat_error(
        StatusCode::TOO_MANY_REQUESTS,
        "API key requests-per-minute limit exceeded",
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&retry_after.to_string()) {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

async fn finish_chat_upstream(
    upstream: reqwest::Response,
    provider: &str,
    model: &str,
    connection_id: &str,
    api_key: Option<&str>,
) -> Response {
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let is_event_stream = upstream
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
    let upstream_headers: Vec<_> = [
        "content-type",
        "cache-control",
        "retry-after",
        "x-request-id",
    ]
    .into_iter()
    .filter_map(|name| {
        upstream
            .headers()
            .get(name)
            .map(|value| (name, value.clone()))
    })
    .collect();
    let mut response = if is_event_stream {
        let stream = upstream.bytes_stream();
        let provider = provider.to_owned();
        let model = model.to_owned();
        let connection_id = connection_id.to_owned();
        let api_key = api_key.map(str::to_owned);
        let body_stream = futures_util::stream::unfold(
            (stream, Vec::new(), None::<Value>),
            move |(mut stream, mut pending, mut usage)| {
                let provider = provider.clone();
                let model = model.clone();
                let connection_id = connection_id.clone();
                let api_key = api_key.clone();
                async move {
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            pending.extend_from_slice(&chunk);
                            if let Some(found) = extract_sse_usage(&mut pending, false) {
                                usage = Some(found);
                            }
                            Some((Ok::<_, std::io::Error>(chunk), (stream, pending, usage)))
                        }
                        Some(Err(error)) => {
                            Some((Err(std::io::Error::other(error)), (stream, pending, usage)))
                        }
                        None => {
                            if let Some(found) = extract_sse_usage(&mut pending, true) {
                                usage = Some(found);
                            }
                            if let Some(usage) = usage {
                                let _ = tokio::task::spawn_blocking(move || {
                                    if let Err(error) = record_chat_usage(
                                        &provider,
                                        &model,
                                        &connection_id,
                                        api_key.as_deref(),
                                        "/v1/chat/completions",
                                        &usage,
                                    ) {
                                        eprintln!(
                                            "[v1/chat/completions] streaming usage write failed: {error}"
                                        );
                                    }
                                })
                                .await;
                            }
                            None
                        }
                    }
                }
            },
        );
        Response::new(Body::from_stream(body_stream))
    } else {
        match upstream.bytes().await {
            Ok(bytes) => {
                if status.is_success()
                    && let Ok(body) = serde_json::from_slice::<Value>(&bytes)
                    && let Some(usage) = body.get("usage")
                    && let Err(error) = record_chat_usage(
                        provider,
                        model,
                        connection_id,
                        api_key,
                        "/v1/chat/completions",
                        usage,
                    )
                {
                    eprintln!("[v1/chat/completions] usage write failed: {error}");
                }
                Response::new(Body::from(bytes))
            }
            Err(error) => {
                eprintln!("[v1/chat/completions] upstream response read failed: {error}");
                return chat_error(StatusCode::BAD_GATEWAY, "OpenAI upstream response failed");
            }
        }
    };
    *response.status_mut() = status;
    for (header_name, value) in upstream_headers {
        response.headers_mut().insert(header_name, value);
    }
    response.headers_mut().insert(
        "access-control-allow-origin",
        axum::http::HeaderValue::from_static("*"),
    );
    response
}

async fn chat_completions(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let api_key = extract_api_key(&request);
    let cli_token = extract_cli_token(&request).map(str::to_owned);
    let is_local = is_local_request(&request, peer);
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_CHAT_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            let message = if error
                .to_string()
                .to_ascii_lowercase()
                .contains("length limit")
            {
                "Request body too large"
            } else {
                "Unable to read request body"
            };
            let status = if message == "Request body too large" {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            return chat_error(status, message);
        }
    };
    let mut payload: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(payload) if payload.is_object() => payload,
        _ => return chat_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    let requested_model = match payload.get("model").and_then(Value::as_str) {
        Some(model) if !model.trim().is_empty() => model.to_owned(),
        _ => return chat_error(StatusCode::BAD_REQUEST, "Missing model"),
    };

    let connection = match open_database() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("[v1/chat/completions] database unavailable: {error}");
            return chat_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable",
            );
        }
    };
    let requested_model = match resolve_model_alias(&connection, &requested_model) {
        Ok(model) => model,
        Err(error) => {
            eprintln!("[v1/chat/completions] model alias resolution failed: {error}");
            return chat_error(StatusCode::BAD_REQUEST, "Invalid model alias");
        }
    };

    let models =
        match read_v1_models_from_connection(&connection, api_key.clone(), cli_token, is_local) {
            Ok(models) => models,
            Err((status, message)) => return chat_error(status, &message),
        };
    let (combo_name, mut candidate_models, combo_strategy) =
        match combo_models_for_chat(&connection, &requested_model) {
            Ok(Some((name, candidates, strategy))) => {
                if !models
                    .data
                    .iter()
                    .any(|entry| entry.id == format!("combo/{name}"))
                {
                    return chat_error(
                        StatusCode::FORBIDDEN,
                        "Combo is not allowed for this API key",
                    );
                }
                (Some(name), candidates, strategy)
            }
            Ok(None) => (None, vec![requested_model.clone()], "fallback".to_owned()),
            Err(error) => {
                eprintln!("[v1/chat/completions] combo lookup failed: {error}");
                return chat_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                );
            }
        };
    if candidate_models.is_empty() {
        return chat_error(StatusCode::NOT_FOUND, "Combo has no configured models");
    }
    if combo_strategy == "round-robin" && candidate_models.len() > 1 {
        let start =
            combo_rotation_index(combo_name.as_deref().unwrap_or(""), candidate_models.len());
        candidate_models.rotate_left(start);
    }
    for candidate in &mut candidate_models {
        match resolve_model_alias(&connection, candidate) {
            Ok(model) => *candidate = model,
            Err(error) => {
                eprintln!("[v1/chat/completions] combo model alias resolution failed: {error}");
                return chat_error(StatusCode::BAD_REQUEST, "Invalid model alias");
            }
        }
    }
    let is_combo = combo_name.is_some();
    if !is_combo
        && !models
            .data
            .iter()
            .any(|entry| entry.id == candidate_models[0])
    {
        return chat_error(StatusCode::NOT_FOUND, "Model is not available");
    }
    if is_combo
        && !models
            .data
            .iter()
            .any(|entry| entry.id == candidate_models[0])
    {
        candidate_models.retain(|candidate| models.data.iter().any(|entry| entry.id == *candidate));
    }
    if is_combo && candidate_models.is_empty() {
        return chat_error(
            StatusCode::FORBIDDEN,
            "Combo has no models available to this API key",
        );
    }
    if payload.get("messages").and_then(Value::as_array).is_none() {
        return chat_error(StatusCode::BAD_REQUEST, "messages must be an array");
    }
    let require_api_key = match setting_bool(&connection, "requireApiKey") {
        Ok(Some(value)) => value,
        Ok(None) => env::var("REQUIRE_API_KEY").as_deref() == Ok("true"),
        Err(error) => {
            eprintln!("[v1/chat/completions] settings lookup failed: {error}");
            return chat_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable",
            );
        }
    };
    if require_api_key && let Some(api_key) = api_key.as_deref() {
        let limits = match load_api_key_limits(&connection, api_key) {
            Ok(Some(limits)) => limits,
            Ok(None) => return chat_error(StatusCode::UNAUTHORIZED, "Invalid API key"),
            Err(error) => {
                eprintln!("[v1/chat/completions] API key limits lookup failed: {error}");
                return chat_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                );
            }
        };
        if limits.has_ip_whitelist {
            return chat_error(
                StatusCode::FORBIDDEN,
                "Rust chat IP-whitelisted keys require a trusted client-IP proxy configuration",
            );
        }
        if let Some(max_input_tokens) = limits.max_input_tokens {
            let estimated_tokens = payload.to_string().len().div_ceil(4) as u64;
            if estimated_tokens > max_input_tokens {
                return chat_error(
                    StatusCode::BAD_REQUEST,
                    "Estimated input size exceeds this API key's limit",
                );
            }
        }
        if let Some(max_output_tokens) = limits.max_output_tokens {
            let fields = ["max_tokens", "max_completion_tokens", "max_output_tokens"];
            let mut applied = false;
            for field in fields {
                if let Some(requested) = payload.get(field).and_then(Value::as_u64) {
                    if let Some(object) = payload.as_object_mut() {
                        object.insert(
                            field.to_owned(),
                            Value::from(requested.min(max_output_tokens)),
                        );
                    }
                    applied = true;
                }
            }
            if !applied && let Some(object) = payload.as_object_mut() {
                object.insert("max_tokens".to_owned(), Value::from(max_output_tokens));
            }
        }
        if let Some(limit) = limits.requests_per_minute {
            let writable = match open_database_for_writes() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[v1/chat/completions] cannot open rate-limit database: {error}");
                    return chat_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            match consume_api_key_request(&writable, &limits.id, limit, now_ms) {
                Ok((true, _)) => {}
                Ok((false, retry_after)) => return chat_rate_limit_error(retry_after),
                Err(error) => {
                    eprintln!("[v1/chat/completions] rate-limit update failed: {error}");
                    return chat_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            }
        }
    }
    let client = reqwest::Client::new();
    let mut last_upstream = None;
    let mut last_error = None;
    let mut supported_candidates = 0;
    for (candidate_index, candidate) in candidate_models.iter().enumerate() {
        let Some((model_provider, upstream_model)) = candidate.split_once('/') else {
            if !is_combo {
                return chat_error(
                    StatusCode::BAD_REQUEST,
                    "Expected model format provider/<model>",
                );
            }
            last_error = Some(chat_error(
                StatusCode::BAD_REQUEST,
                "Invalid combo model format",
            ));
            continue;
        };
        let available = models.data.iter().find(|entry| entry.id == *candidate);
        let Some(available) = available else {
            if !is_combo {
                return chat_error(StatusCode::NOT_FOUND, "Model is not available");
            }
            continue;
        };
        if model_provider != available.owned_by {
            if !is_combo {
                return chat_error(StatusCode::BAD_REQUEST, "Model provider does not match");
            }
            continue;
        }
        let provider_id = match chat_provider_id(&connection, model_provider) {
            Ok(Some((provider_id, true))) => provider_id,
            Ok(Some((_, false))) | Ok(None) => {
                if !is_combo {
                    return chat_error(
                        StatusCode::NOT_IMPLEMENTED,
                        "Rust chat supports OpenAI-compatible providers only; this provider still uses Node",
                    );
                }
                continue;
            }
            Err(error) => {
                eprintln!("[v1/chat/completions] provider lookup failed: {error}");
                return chat_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                );
            }
        };
        supported_candidates += 1;
        let (connection_id, upstream_key, upstream_url) =
            match load_openai_connection(&connection, &provider_id) {
                Ok(credentials) => credentials,
                Err(error) => {
                    eprintln!("[v1/chat/completions] provider connection unavailable: {error}");
                    last_error = Some(chat_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "OpenAI-compatible provider is not configured",
                    ));
                    continue;
                }
            };
        let mut candidate_payload = payload.clone();
        if let Some(object) = candidate_payload.as_object_mut() {
            object.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
        }
        let mut upstream_request = client
            .post(upstream_url)
            .bearer_auth(upstream_key)
            .header("content-type", "application/json")
            .body(candidate_payload.to_string());
        if let Some(accept) = parts.headers.get("accept") {
            upstream_request = upstream_request.header("accept", accept);
        }
        let upstream = match upstream_request.send().await {
            Ok(response) => response,
            Err(error) => {
                eprintln!("[v1/chat/completions] upstream request failed: {error}");
                last_error = Some(chat_error(
                    StatusCode::BAD_GATEWAY,
                    "OpenAI upstream request failed",
                ));
                continue;
            }
        };
        let status = upstream.status();
        let fallback_status =
            matches!(status.as_u16(), 401 | 402 | 403 | 404 | 429) || status.is_server_error();
        if is_combo && fallback_status && candidate_index + 1 < candidate_models.len() {
            last_upstream = Some((
                upstream,
                model_provider.to_owned(),
                upstream_model.to_owned(),
                connection_id,
            ));
            continue;
        }
        return finish_chat_upstream(
            upstream,
            model_provider,
            upstream_model,
            &connection_id,
            api_key.as_deref(),
        )
        .await;
    }
    if let Some((upstream, provider, model, connection_id)) = last_upstream {
        return finish_chat_upstream(
            upstream,
            &provider,
            &model,
            &connection_id,
            api_key.as_deref(),
        )
        .await;
    }
    if let Some(error) = last_error {
        return error;
    }
    if supported_candidates == 0 {
        return chat_error(
            StatusCode::NOT_IMPLEMENTED,
            "Rust chat supports OpenAI-compatible providers only; this provider still uses Node",
        );
    }
    chat_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "All combo models are unavailable",
    )
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn initialized() -> &'static str {
    "Initialized"
}

async fn ready() -> Response {
    match open_database().and_then(|connection| {
        connection
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .map_err(|error| error.to_string())
    }) {
        Ok(1) => (
            StatusCode::OK,
            [("cache-control", "no-store")],
            Json(json!({ "ok": true, "database": "ready" })),
        )
            .into_response(),
        Ok(_) => admin_error(StatusCode::SERVICE_UNAVAILABLE, "Database unavailable"),
        Err(error) => {
            eprintln!("[ready] database check failed: {error}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [("cache-control", "no-store")],
                Json(json!({ "ok": false, "database": "unavailable" })),
            )
                .into_response()
        }
    }
}

fn login_requirement_response(settings: &Value) -> Value {
    json!({
        "requireLogin": settings.get("requireLogin").and_then(Value::as_bool) != Some(false),
        "tunnelDashboardAccess": settings.get("tunnelDashboardAccess").and_then(Value::as_bool) != Some(false),
        "tunnelUrl": settings.get("tunnelUrl").and_then(Value::as_str).unwrap_or(""),
        "tailscaleUrl": settings.get("tailscaleUrl").and_then(Value::as_str).unwrap_or(""),
    })
}

async fn login_requirement() -> Json<Value> {
    let settings = open_database().and_then(|connection| {
        connection
            .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(|error| error.to_string())
    });
    match settings {
        Ok(Some(raw)) => {
            let settings = serde_json::from_str::<Value>(&raw).unwrap_or_else(|error| {
                eprintln!("[api/settings/require-login] settings JSON invalid: {error}");
                json!({})
            });
            Json(login_requirement_response(&settings))
        }
        Ok(None) => Json(login_requirement_response(&json!({}))),
        Err(error) => {
            eprintln!("[api/settings/require-login] settings unavailable: {error}");
            Json(json!({ "requireLogin": true }))
        }
    }
}

async fn ollama_tags() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "models": [
            {
                "name": "llama3.2",
                "modified_at": "2025-12-26T00:00:00Z",
                "size": 2000000000_u64,
                "digest": "abc123def456",
                "details": {
                    "format": "gguf",
                    "family": "llama",
                    "parameter_size": "3B",
                    "quantization_level": "Q4_K_M"
                }
            },
            {
                "name": "qwen2.5",
                "modified_at": "2025-12-26T00:00:00Z",
                "size": 4000000000_u64,
                "digest": "def456abc123",
                "details": {
                    "format": "gguf",
                    "family": "qwen",
                    "parameter_size": "7B",
                    "quantization_level": "Q4_K_M"
                }
            }
        ]
    }))
}

#[derive(Default)]
struct VersionCache {
    value: Option<String>,
    fetched_at: Option<Instant>,
    last_failure_at: Option<Instant>,
}

#[derive(serde::Deserialize)]
struct NpmVersion {
    version: Option<String>,
}

#[derive(serde::Serialize)]
struct VersionResponse {
    #[serde(rename = "currentVersion")]
    current_version: String,
    #[serde(rename = "latestVersion")]
    latest_version: Option<String>,
    #[serde(rename = "hasUpdate")]
    has_update: bool,
    #[serde(rename = "buildId")]
    build_id: Option<String>,
}

async fn fetch_latest_version() -> Result<String, String> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|error| error.to_string())?
        .get(NPM_VERSION_URL)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let release = response
        .json::<NpmVersion>()
        .await
        .map_err(|error| error.to_string())?;

    release
        .version
        .filter(|version| !version.is_empty())
        .ok_or_else(|| "npm response did not contain a version".to_owned())
}

async fn latest_version_cached() -> Option<String> {
    static CACHE: OnceLock<Mutex<VersionCache>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(VersionCache::default()))
        .lock()
        .await;
    let now = Instant::now();

    if cache
        .fetched_at
        .is_some_and(|fetched_at| now.duration_since(fetched_at) < VERSION_CACHE_TTL)
    {
        return cache.value.clone();
    }
    if cache
        .last_failure_at
        .is_some_and(|failed_at| now.duration_since(failed_at) < VERSION_FAILURE_BACKOFF)
    {
        return cache.value.clone();
    }

    match fetch_latest_version().await {
        Ok(version) => {
            cache.value = Some(version);
            cache.fetched_at = Some(Instant::now());
            cache.last_failure_at = None;
        }
        Err(error) => {
            eprintln!("[version] npm lookup failed: {error}");
            cache.last_failure_at = Some(Instant::now());
        }
    }

    cache.value.clone()
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    for (left, right) in a.split('.').zip(b.split('.')).take(3) {
        if let (Ok(left), Ok(right)) = (left.parse::<u64>(), right.parse::<u64>()) {
            match left.cmp(&right) {
                Ordering::Equal => {}
                ordering => return ordering,
            }
        }
    }
    Ordering::Equal
}

fn version_response(
    current_version: String,
    latest_version: Option<String>,
    build_id: Option<String>,
) -> VersionResponse {
    let has_update = latest_version
        .as_deref()
        .is_some_and(|latest| compare_versions(latest, &current_version) == Ordering::Greater);
    VersionResponse {
        current_version,
        latest_version,
        has_update,
        build_id,
    }
}

fn application_version() -> String {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            serde_json::from_str::<Value>(APP_PACKAGE_JSON)
                .ok()
                .and_then(|package| {
                    package
                        .get("version")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
        })
        .clone()
}

async fn version() -> Json<VersionResponse> {
    let skip_update_check = env::var("VANSROUTER_SKIP_UPDATE_CHECK").as_deref() == Ok("1")
        || env::var("VANROUTER_SKIP_UPDATE_CHECK").as_deref() == Ok("1");
    let latest_version = if skip_update_check {
        None
    } else {
        latest_version_cached().await
    };
    let current_version = env::var("VANSROUTER_VERSION").unwrap_or_else(|_| application_version());
    let build_id = env::var("RELEASE_BUILD_ID").ok();

    Json(version_response(current_version, latest_version, build_id))
}

async fn provider_resource_route(Path(id): Path<String>, request: Request) -> Response {
    admin::provider_resource(request, id).await
}

async fn combo_resource_route(Path(id): Path<String>, request: Request) -> Response {
    admin::combo_resource(request, id).await
}

async fn api_key_resource_route(Path(id): Path<String>, request: Request) -> Response {
    admin::api_key_resource(request, id).await
}

async fn provider_node_resource_route(Path(id): Path<String>, request: Request) -> Response {
    admin::provider_node_resource(request, id).await
}

async fn model_aliases_route_handler(request: Request) -> Response {
    admin::model_aliases_route(request).await
}

async fn custom_models_route_handler(request: Request) -> Response {
    admin::custom_models_route(request).await
}

async fn disabled_models_route_handler(request: Request) -> Response {
    admin::disabled_models_route(request).await
}

fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api/init", get(initialized))
        .route("/api/ready", get(ready))
        .route("/api/auth/login", axum::routing::post(auth::login))
        .route("/api/auth/logout", axum::routing::post(auth::logout))
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/check", get(auth::check))
        .route("/api/auth/oidc/start", get(oidc::start))
        .route("/api/auth/oidc/callback", get(oidc::callback))
        .route("/api/auth/oidc/test", axum::routing::post(oidc::test))
        .route(
            "/api/auth/reset-password",
            axum::routing::post(auth::reset_password),
        )
        .route("/api/locale", axum::routing::post(locale::set_locale))
        .route("/api/pricing", any(pricing::route))
        .route("/api/tags", get(ollama_tags))
        .route("/api/settings/require-login", get(login_requirement))
        .route("/api/version", get(version))
        .route("/api/usage/providers", get(usage::usage_providers))
        .route("/api/usage/history", get(usage::usage_history))
        .route("/api/usage/stats", get(usage::usage_stats))
        .route("/api/usage/chart", get(usage::usage_chart))
        .route("/api/usage/request-details", get(request_details::list))
        .route(
            "/api/usage/request-details/{id}",
            get(request_details::by_id),
        )
        .route("/api/usage/logs", get(request_details::logs))
        .route("/api/usage/request-logs", get(request_details::logs))
        .route("/api/providers", any(admin::providers_collection))
        .route(
            "/api/providers/validate",
            any(admin::provider_validation_route),
        )
        .route("/api/providers/{id}", any(provider_resource_route))
        .route(
            "/api/providers/{id}/overrides",
            any(provider_overrides::route),
        )
        .route(
            "/api/providers/suggested-models",
            any(provider_discovery::suggested_models),
        )
        .route(
            "/api/providers/kilo/free-models",
            any(provider_discovery::kilo_free_models),
        )
        .route("/api/providers/client", any(providers_client::route))
        .route("/api/keys", any(admin::api_keys_collection))
        .route("/api/keys/{id}", any(api_key_resource_route))
        .route("/api/provider-nodes", any(admin::provider_nodes_collection))
        .route(
            "/api/provider-nodes/validate",
            any(admin::provider_node_validation_route),
        )
        .route(
            "/api/provider-nodes/{id}",
            any(provider_node_resource_route),
        )
        .route("/api/models/alias", any(model_aliases_route_handler))
        .route("/api/models", any(models::api_models))
        .route("/api/models/custom", any(custom_models_route_handler))
        .route("/api/models/disabled", any(disabled_models_route_handler))
        .route("/api/models/availability", any(availability::route))
        .route("/api/combos", any(admin::combos_collection))
        .route("/api/combos/{id}", any(combo_resource_route))
        .route("/v1/models", get(v1_models))
        .route("/v1/models/info", any(models::model_info))
        .route("/v1beta/models", any(models::gemini_models))
        .route(
            "/v1/chat/completions",
            axum::routing::post(chat_completions),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers(Any),
        )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = env::var("PORT")
        .unwrap_or_else(|_| "8080".to_owned())
        .parse::<u16>()?;
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    println!("Rust backend listening on {address}");
    axum::serve(
        listener,
        app().into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rusqlite::Connection;
    use tower::ServiceExt;

    fn models_test_database(allowed_providers: &str, allowed_combos: &str) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE settings (id INTEGER PRIMARY KEY, data TEXT NOT NULL);
                 CREATE TABLE apiKeys (
                   id TEXT NOT NULL,
                   key TEXT PRIMARY KEY,
                   isActive INTEGER NOT NULL,
                   allowedProviders TEXT,
                   allowedCombos TEXT,
                   allowedKinds TEXT,
                   requestsPerMinute INTEGER,
                   maxInputTokens INTEGER,
                   maxOutputTokens INTEGER,
                   ipWhitelist TEXT
                 );
                 CREATE TABLE apiKeyRateLimits (
                   apiKeyId TEXT PRIMARY KEY,
                   windowStart INTEGER NOT NULL,
                   requestCount INTEGER NOT NULL
                 );
                 CREATE TABLE providerConnections (
                   provider TEXT NOT NULL,
                   isActive INTEGER NOT NULL,
                   data TEXT NOT NULL
                 );
                 CREATE TABLE providerNodes (id TEXT PRIMARY KEY, data TEXT NOT NULL);
                 CREATE TABLE combos (
                   name TEXT NOT NULL,
                   kind TEXT,
                   createdAt TEXT NOT NULL
                 );
                 CREATE TABLE kv (scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL);
                 INSERT INTO settings (id, data) VALUES (1, '{\"requireApiKey\":true}');
                 INSERT INTO providerConnections (provider, isActive, data)
                   VALUES ('openai', 1, '{}');
                 INSERT INTO combos (name, kind, createdAt) VALUES
                   ('good', NULL, '2026-01-01'),
                   ('blocked', NULL, '2026-01-02');",
            )
            .unwrap_or_else(|error| panic!("test schema setup failed: {error}"));
        connection
            .execute(
                "INSERT INTO apiKeys
                 (id, key, isActive, allowedProviders, allowedCombos, allowedKinds)
                 VALUES ('test-key-id', 'test-key', 1, ?1, ?2, '[\"llm\"]')",
                params![allowed_providers, allowed_combos],
            )
            .unwrap();
        connection
    }

    #[tokio::test]
    async fn api_health_returns_ok_json() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .header("origin", "https://dashboard.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "*"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({ "ok": true })
        );
    }

    #[test]
    fn login_requirement_response_matches_dashboard_defaults_and_settings() {
        assert_eq!(
            login_requirement_response(&json!({})),
            json!({
                "requireLogin": true,
                "tunnelDashboardAccess": true,
                "tunnelUrl": "",
                "tailscaleUrl": ""
            })
        );
        assert_eq!(
            login_requirement_response(&json!({
                "requireLogin": false,
                "tunnelDashboardAccess": false,
                "tunnelUrl": "https://tunnel.example",
                "tailscaleUrl": "https://tailnet.example"
            })),
            json!({
                "requireLogin": false,
                "tunnelDashboardAccess": false,
                "tunnelUrl": "https://tunnel.example",
                "tailscaleUrl": "https://tailnet.example"
            })
        );
    }

    #[test]
    fn api_key_generation_matches_machine_key_format_and_hmac() {
        let secret = b"test-api-key-secret";
        let machine_id = "0123456789abcdef";
        let key = admin::generate_api_key(machine_id, secret).unwrap();
        let parts: Vec<_> = key.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "sk");
        assert_eq!(parts[1], machine_id);
        assert_eq!(parts[2].len(), 6);
        assert!(
            parts[2]
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        );
        let expected_crc =
            admin::hmac_sha256_hex(secret, format!("{machine_id}{}", parts[2]).as_bytes());
        assert_eq!(parts[3], &expected_crc[..8]);
        assert_eq!(
            admin::hmac_sha256_hex(b"key", b"The quick brown fox jumps over the lazy dog"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn api_key_limits_and_ip_whitelists_are_normalized() {
        assert_eq!(
            admin::api_key_limit(Some(&json!("1200")), "maxInputTokens").unwrap(),
            Some(1200)
        );
        assert_eq!(
            admin::api_key_limit(Some(&json!("")), "maxInputTokens").unwrap(),
            None
        );
        assert!(admin::api_key_limit(Some(&json!(0)), "requestsPerMinute").is_err());
        assert!(admin::api_key_limit(Some(&json!(1.5)), "maxOutputTokens").is_err());

        assert_eq!(
            admin::normalize_ip_whitelist(Some(&json!(
                "192.0.2.4\n2001:0db8::1, ::ffff:192.0.2.4"
            )))
            .unwrap(),
            Some(vec!["192.0.2.4".to_owned(), "2001:db8::1".to_owned()])
        );
        assert!(admin::normalize_ip_whitelist(Some(&json!("example.com"))).is_err());
        assert!(!admin::json_truthy(&Value::Null));
        assert!(!admin::json_truthy(&json!(0)));
        assert!(!admin::json_truthy(&json!("")));
        assert!(admin::json_truthy(&json!("false")));
        assert!(admin::json_truthy(&json!([])));
    }

    #[test]
    fn provider_node_base_urls_match_node_route_normalization() {
        assert_eq!(
            admin::sanitize_provider_node_base_url(
                "anthropic-compatible",
                "https://gateway.example/messages/"
            ),
            "https://gateway.example/v1"
        );
        assert_eq!(
            admin::sanitize_provider_node_base_url(
                "anthropic-compatible",
                "https://gateway.example/v1"
            ),
            "https://gateway.example/v1"
        );
        assert_eq!(
            admin::sanitize_provider_node_base_url(
                "custom-embedding",
                "https://gateway.example/embeddings/"
            ),
            "https://gateway.example"
        );
        assert_eq!(
            admin::sanitize_provider_node_base_url(
                "openai-compatible",
                "https://gateway.example/v1/"
            ),
            "https://gateway.example/v1/"
        );
    }

    #[tokio::test]
    async fn api_errors_use_openai_compatible_error_types() {
        for (status, expected_type) in [
            (StatusCode::UNAUTHORIZED, "authentication_error"),
            (StatusCode::FORBIDDEN, "permission_error"),
            (StatusCode::BAD_REQUEST, "invalid_request_error"),
            (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        ] {
            let response = api_error(status, "test error");
            assert_eq!(response.status(), status);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["error"]["type"], expected_type);
        }
    }

    #[test]
    fn version_response_matches_dashboard_contract() {
        let response = version_response(
            "1.2.3".to_owned(),
            Some("1.2.4".to_owned()),
            Some("build-123".to_owned()),
        );
        let value = serde_json::to_value(response).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "currentVersion": "1.2.3",
                "latestVersion": "1.2.4",
                "hasUpdate": true,
                "buildId": "build-123"
            })
        );
    }

    #[test]
    fn default_application_version_matches_dashboard_package() {
        let package: Value = serde_json::from_str(APP_PACKAGE_JSON).unwrap();
        assert_eq!(application_version(), package["version"].as_str().unwrap());
    }

    #[test]
    fn dashboard_auth_accepts_only_valid_unexpired_hs256_tokens() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = json!({
            "authenticated": true,
            "iat": now,
            "exp": now + 60,
        });
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap();
        assert!(dashboard_token_valid(&token, "test-secret"));
        assert!(!dashboard_token_valid(&token, "wrong-secret"));

        let expired = encode(
            &Header::new(Algorithm::HS256),
            &json!({"authenticated":true,"iat":now-120,"exp":now-60}),
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap();
        assert!(!dashboard_token_valid(&expired, "test-secret"));

        let not_yet_valid = encode(
            &Header::new(Algorithm::HS256),
            &json!({"authenticated":true,"iat":now,"nbf":now+60,"exp":now+120}),
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap();
        assert!(!dashboard_token_valid(&not_yet_valid, "test-secret"));
    }

    #[test]
    fn rust_data_directory_matches_node_production_fallback_rules() {
        let persistent = PathBuf::from("/home/user/.9router");
        assert_eq!(
            resolve_data_dir(
                Some("/tmp/9router-data-smoke"),
                persistent.clone(),
                true,
                false,
                false,
                false,
            ),
            persistent
        );
        assert_eq!(
            resolve_data_dir(
                Some("/tmp/9router-data-smoke"),
                PathBuf::from("/home/user/.9router"),
                true,
                true,
                false,
                false,
            ),
            PathBuf::from("/tmp/9router-data-smoke")
        );
        assert_eq!(
            resolve_data_dir(
                None,
                PathBuf::from("/home/user/.9router"),
                false,
                false,
                false,
                false,
            ),
            PathBuf::from("/home/user/.9router")
        );
        assert_eq!(
            resolve_data_dir(
                Some("/var/lib/9router"),
                PathBuf::from("/home/user/.9router"),
                true,
                false,
                true,
                false,
            ),
            PathBuf::from("/home/user/.9router")
        );
    }

    #[test]
    fn combo_validation_and_timestamps_match_database_contract() {
        assert!(admin::combo_name_valid("primary.v2_test-1"));
        assert!(!admin::combo_name_valid("bad/name"));
        assert_eq!(
            admin::combo_context_length(Some(&json!(8192))),
            Ok(Some(8192))
        );
        assert_eq!(
            admin::combo_context_length(Some(&json!("8192"))),
            Ok(Some(8192))
        );
        assert_eq!(admin::combo_context_length(Some(&Value::Null)), Ok(None));
        assert_eq!(admin::combo_context_length(Some(&json!(""))), Ok(None));
        assert!(admin::combo_context_length(Some(&json!(2_000_001))).is_err());
        assert!(
            regex_timestamp(&admin::utc_timestamp()),
            "timestamps must sort and parse like Node ISO timestamps"
        );
    }

    #[test]
    fn chat_usage_updates_history_daily_summary_and_lifetime_counter() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE usageHistory (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp TEXT NOT NULL,
                    provider TEXT,
                    model TEXT,
                    connectionId TEXT,
                    apiKey TEXT,
                    endpoint TEXT,
                    promptTokens INTEGER DEFAULT 0,
                    completionTokens INTEGER DEFAULT 0,
                    cost REAL DEFAULT 0,
                    status TEXT,
                    tokens TEXT,
                    meta TEXT
                 );
                 CREATE TABLE usageDaily (dateKey TEXT PRIMARY KEY, data TEXT NOT NULL);
                 CREATE TABLE _meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .unwrap();
        let usage = json!({
            "prompt_tokens": 120,
            "completion_tokens": 30,
            "prompt_tokens_details": {"cached_tokens": 40}
        });

        persist_chat_usage(
            &mut connection,
            "openai",
            "gpt-4.1",
            "connection-1",
            Some("sk-client"),
            "/v1/chat/completions",
            &usage,
            120,
            30,
            40,
            0.0006,
        )
        .unwrap();

        let history: (String, i64, i64, f64, String) = connection
            .query_row(
                "SELECT provider, promptTokens, completionTokens, cost, tokens FROM usageHistory",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(history.0, "openai");
        assert_eq!((history.1, history.2), (120, 30));
        assert!((history.3 - 0.0006).abs() < f64::EPSILON);
        assert_eq!(serde_json::from_str::<Value>(&history.4).unwrap(), usage);

        let (day_data, total): (String, String) = connection
            .query_row(
                "SELECT usageDaily.data, _meta.value FROM usageDaily, _meta WHERE _meta.key = 'totalRequestsLifetime'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let day: Value = serde_json::from_str(&day_data).unwrap();
        assert_eq!(day["requests"], 1);
        assert_eq!(day["promptTokens"], 120);
        assert_eq!(day["completionTokens"], 30);
        assert_eq!(day["cachedTokens"], 40);
        assert_eq!(day["cost"], 0.0006);
        assert_eq!(day["byProvider"]["openai"]["requests"], 1);
        assert_eq!(day["byAccount"]["connection-1"]["rawModel"], "gpt-4.1");
        assert_eq!(total, "1");
    }

    #[test]
    fn chat_cost_uses_canonical_and_custom_pricing() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE kv (scope TEXT, key TEXT, value TEXT, PRIMARY KEY(scope, key));
                 INSERT INTO kv(scope, key, value) VALUES('pricing', 'openai', '{\"gpt-4.1\":{\"input\":4.0}}');",
            )
            .unwrap();
        let usage = json!({
            "prompt_tokens": 120,
            "completion_tokens": 30,
            "prompt_tokens_details": {"cached_tokens": 40}
        });
        let canonical = model_pricing(&connection, "anthropic", "gpt-4.1")
            .unwrap()
            .unwrap();
        assert_eq!(calculate_chat_cost(&usage, &canonical), 0.00055);
        let custom = model_pricing(&connection, "openai", "gpt-4.1")
            .unwrap()
            .unwrap();
        assert_eq!(custom["input"], 4.0);
        assert_eq!(custom["output"], 10.0);
        assert_eq!(calculate_chat_cost(&usage, &custom), 0.00067);
    }

    #[test]
    fn streaming_usage_parser_handles_chunk_boundaries_and_crlf_events() {
        let mut pending = b"data: {\"choices\":[]}\r\n\r\n".to_vec();
        assert!(extract_sse_usage(&mut pending, false).is_none());
        pending.extend_from_slice(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\r\n\r\n");
        let usage = extract_sse_usage(&mut pending, false).unwrap();
        assert_eq!(usage["prompt_tokens"], 4);
        assert_eq!(usage["completion_tokens"], 2);

        pending
            .extend_from_slice(b"data: {\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":3}}");
        assert_eq!(
            extract_sse_usage(&mut pending, true).unwrap()["prompt_tokens"],
            8
        );
    }

    #[test]
    fn combo_chat_loads_members_and_uses_combo_specific_strategy() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                r#"CREATE TABLE combos (name TEXT, models TEXT);
                   CREATE TABLE settings (id INTEGER PRIMARY KEY, data TEXT);
                   INSERT INTO combos (name, models) VALUES ('chat-test', '["openai/a", "node/b"]');
                   INSERT INTO settings (id, data) VALUES (1, '{"comboStrategy":"fallback","comboStrategies":{"chat-test":{"fallbackStrategy":"round-robin"}}}');"#,
            )
            .unwrap();

        let (name, models, strategy) = combo_models_for_chat(&connection, "combo/chat-test")
            .unwrap()
            .unwrap();
        assert_eq!(name, "chat-test");
        assert_eq!(models, ["openai/a", "node/b"]);
        assert_eq!(strategy, "round-robin");
        assert!(
            combo_models_for_chat(&connection, "openai/gpt-4.1")
                .unwrap()
                .is_none()
        );
    }

    fn regex_timestamp(value: &str) -> bool {
        let bytes = value.as_bytes();
        bytes.len() == 24
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b'T'
            && bytes[13] == b':'
            && bytes[16] == b':'
            && bytes[19] == b'.'
            && bytes[23] == b'Z'
    }

    #[test]
    fn provider_connection_responses_never_include_credentials() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE providerConnections (
               id TEXT PRIMARY KEY, provider TEXT NOT NULL, authType TEXT NOT NULL,
               name TEXT, email TEXT, priority INTEGER, isActive INTEGER,
               data TEXT NOT NULL, createdAt TEXT NOT NULL, updatedAt TEXT NOT NULL
             );
             INSERT INTO providerConnections
               VALUES ('secret-row', 'openai', 'apikey', 'OpenAI', NULL, 1, 1,
                 '{\"apiKey\":\"gateway-provider-secret\",\"accessToken\":\"oauth-secret\",\"providerSpecificData\":{}}',
                 '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');",
        )
        .unwrap();

        let response = admin::provider_connection_value(&db, "secret-row")
            .unwrap()
            .unwrap();
        let serialized = response.to_string();
        assert!(!serialized.contains("gateway-provider-secret"));
        assert!(!serialized.contains("oauth-secret"));
        assert_eq!(response["provider"], "openai");
        assert_eq!(response["isActive"], true);
    }

    #[test]
    fn version_comparison_checks_numeric_segments() {
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.3", "1.2.4"), Ordering::Less);
    }

    #[test]
    fn null_acl_allows_and_empty_acl_denies() {
        let unrestricted = ApiKeyAcl::default();
        assert!(kind_allowed(&unrestricted, "llm"));
        assert!(combo_allowed(&unrestricted, "primary"));

        let restricted = ApiKeyAcl {
            allowed_providers: Some(vec![]),
            allowed_combos: Some(vec![]),
            allowed_kinds: Some(vec![]),
        };
        assert!(!kind_allowed(&restricted, "llm"));
        assert!(!combo_allowed(&restricted, "primary"));
    }

    #[test]
    fn v1_models_requires_valid_key_and_applies_acl_filters() {
        let db = models_test_database("[\"openai\"]", "[\"good\"]");
        assert_eq!(
            read_v1_models_from_connection(&db, None, None, true)
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            read_v1_models_from_connection(&db, Some("invalid".to_owned()), None, true)
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );

        let models =
            read_v1_models_from_connection(&db, Some("test-key".to_owned()), None, true).unwrap();
        assert!(models.data.iter().any(|model| model.owned_by == "openai"));
        assert!(models.data.iter().any(|model| model.id == "combo/good"));
        assert!(!models.data.iter().any(|model| model.id == "combo/blocked"));
    }

    #[test]
    fn remote_api_gate_requires_key_unless_explicitly_opened() {
        assert!(public_api_access_allowed(true, false, false, true, false));
        assert!(public_api_access_allowed(false, true, false, true, false));
        assert!(public_api_access_allowed(false, false, true, true, false));
        assert!(!public_api_access_allowed(
            false, false, false, false, false
        ));
        assert!(public_api_access_allowed(false, false, false, false, true));
        assert!(!public_api_access_allowed(false, false, false, true, true));
    }

    #[test]
    fn api_key_rate_limit_is_atomic_and_resets_each_minute() {
        let db = models_test_database("[\"openai\"]", "[\"good\"]");
        assert_eq!(
            consume_api_key_request(&db, "key-id", 1, 60_001).unwrap(),
            (true, 0)
        );
        let (allowed, retry_after) = consume_api_key_request(&db, "key-id", 1, 60_500).unwrap();
        assert!(!allowed);
        assert_eq!(retry_after, 60);
        assert_eq!(
            consume_api_key_request(&db, "key-id", 1, 120_000).unwrap(),
            (true, 0)
        );
    }

    #[test]
    fn local_access_is_not_inferred_through_forwarding_headers() {
        let local_peer = "127.0.0.1:8080".parse().unwrap();
        let remote_peer = "203.0.113.10:8080".parse().unwrap();
        let direct_local_request = Request::builder().body(Body::empty()).unwrap();
        assert!(is_local_request(&direct_local_request, local_peer));
        assert!(!is_local_request(&direct_local_request, remote_peer));

        let forwarded_request = Request::builder()
            .header("x-forwarded-for", "203.0.113.20")
            .body(Body::empty())
            .unwrap();
        assert!(!is_local_request(&forwarded_request, local_peer));

        let remote_origin_request = Request::builder()
            .header("origin", "https://dashboard.example")
            .body(Body::empty())
            .unwrap();
        assert!(!is_local_request(&remote_origin_request, local_peer));
    }

    #[test]
    fn provider_lookup_normalizes_names_aliases_and_custom_nodes() {
        assert_eq!(
            admin::provider_definition(" OpenAI ").map(|provider| provider.id.as_str()),
            Some("openai")
        );
        assert_eq!(
            admin::provider_definition("aai").map(|provider| provider.id.as_str()),
            Some("assemblyai")
        );

        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE providerNodes (
               id TEXT PRIMARY KEY, type TEXT, name TEXT, data TEXT NOT NULL
             );
             INSERT INTO providerNodes VALUES (
               'openai-compatible-test', 'openai-compatible', 'Test Node',
               '{\"prefix\":\"demo\",\"apiType\":\"chat\",\"baseUrl\":\"https://api.example/v1\"}'
             );",
        )
        .unwrap();
        assert_eq!(
            admin::provider_node_definition(&db, "openai-compatible-test").unwrap(),
            Some((
                "openai-compatible".to_owned(),
                "Test Node".to_owned(),
                json!({
                    "prefix": "demo",
                    "apiType": "chat",
                    "baseUrl": "https://api.example/v1"
                })
            ))
        );
        assert_eq!(
            chat_provider_id(&db, "demo").unwrap(),
            Some(("openai-compatible-test".to_owned(), true))
        );
    }

    #[test]
    fn bundled_catalog_contains_provider_models() {
        let catalog = model_catalog();
        assert!(!catalog.providers.is_empty());
        let openai = catalog
            .providers
            .iter()
            .find(|provider| provider.id == "openai")
            .expect("OpenAI provider must be in bundled catalog");
        assert_eq!(openai.alias, "openai");
        assert_eq!(openai.category.as_deref(), Some("apikey"));
        assert_eq!(openai.display_name, "OpenAI");
        assert!(openai.auth_modes.iter().any(|mode| mode == "apikey"));
        assert!(!openai.models.is_empty());
        assert_eq!(
            admin::provider_definition("aai").map(|provider| provider.id.as_str()),
            Some("assemblyai")
        );
    }

    #[tokio::test]
    async fn ollama_tags_matches_existing_response() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "models": [
                    {
                        "name": "llama3.2",
                        "modified_at": "2025-12-26T00:00:00Z",
                        "size": 2000000000_u64,
                        "digest": "abc123def456",
                        "details": {
                            "format": "gguf",
                            "family": "llama",
                            "parameter_size": "3B",
                            "quantization_level": "Q4_K_M"
                        }
                    },
                    {
                        "name": "qwen2.5",
                        "modified_at": "2025-12-26T00:00:00Z",
                        "size": 4000000000_u64,
                        "digest": "def456abc123",
                        "details": {
                            "format": "gguf",
                            "family": "qwen",
                            "parameter_size": "7B",
                            "quantization_level": "Q4_K_M"
                        }
                    }
                ]
            })
        );
    }
}
