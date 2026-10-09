use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_HEADERS: usize = 20;
const MAX_HEADER_VALUE_LENGTH: usize = 8192;
const BLOCKED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "connection",
    "transfer-encoding",
    "authorization",
    "cookie",
];

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn normalize_headers(value: Option<&Value>) -> Result<Option<Map<String, Value>>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let Some(headers) = value.as_object() else {
        return Err("headers must be an object".to_owned());
    };
    let headers = headers
        .iter()
        .filter(|(_, value)| value.as_str() != Some("") && !value.is_null())
        .collect::<Vec<_>>();
    if headers.len() > MAX_HEADERS {
        return Err(format!("Too many headers (max {MAX_HEADERS})"));
    }
    let mut normalized = Map::new();
    for (name, value) in headers {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!("Invalid header name: {name}"));
        }
        let Some(value) = value.as_str() else {
            return Err(format!("Invalid value for header {name}"));
        };
        if value.contains(['\r', '\n']) {
            return Err(format!("Invalid value for header {name}"));
        }
        if value.encode_utf16().count() > MAX_HEADER_VALUE_LENGTH {
            return Err(format!(
                "Header {name} value too long (max {MAX_HEADER_VALUE_LENGTH})"
            ));
        }
        if BLOCKED_HEADERS
            .iter()
            .any(|blocked| name.eq_ignore_ascii_case(blocked))
        {
            return Err(format!("Header {name} cannot be overridden"));
        }
        normalized.insert(name.clone(), Value::String(value.to_owned()));
    }
    Ok((!normalized.is_empty()).then_some(normalized))
}

fn settings(connection: &rusqlite::Connection) -> Result<Map<String, Value>, String> {
    let raw = connection
        .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|error| error.to_string())?;
    Ok(raw
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default())
}

fn provider_key(id: &str) -> (String, Value) {
    if let Some(provider) = crate::admin::provider_definition(id) {
        (provider.id.clone(), provider.headers.clone())
    } else {
        (id.to_owned(), json!({}))
    }
}

pub(super) async fn route(Path(id): Path<String>, request: Request) -> Response {
    if let Err((status, message)) = crate::require_dashboard_auth(&request) {
        return api_error(status, message);
    }
    let (canonical, builtin_headers) = provider_key(&id);
    match request.method().clone() {
        axum::http::Method::GET => {
            let connection = match crate::open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/providers/{id}/overrides] database unavailable: {error}");
                    return api_error(StatusCode::SERVICE_UNAVAILABLE, "Failed to get overrides");
                }
            };
            let settings = match settings(&connection) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("[api/providers/{id}/overrides] settings read failed: {error}");
                    return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to get overrides");
                }
            };
            let override_value = settings
                .get("providerOverrides")
                .and_then(|value| value.get(&canonical))
                .unwrap_or(&Value::Null);
            let headers = override_value
                .get("headers")
                .filter(|value| match value {
                    Value::Null | Value::Bool(false) => false,
                    Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
                    Value::String(value) => !value.is_empty(),
                    _ => true,
                })
                .cloned()
                .unwrap_or_else(|| json!({}));
            Json(json!({ "headers": headers, "builtinHeaders": builtin_headers })).into_response()
        }
        axum::http::Method::PUT => {
            let bytes = match to_bytes(request.into_body(), MAX_BODY_BYTES).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    return api_error(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
                }
            };
            let body = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|_| json!({}));
            if body.is_null() {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to save overrides",
                );
            }
            let override_headers = match normalize_headers(body.get("headers")) {
                Ok(headers) => headers,
                Err(error) => return api_error(StatusCode::BAD_REQUEST, &error),
            };
            let mut connection = match crate::open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/providers/{id}/overrides] database unavailable: {error}");
                    return api_error(StatusCode::SERVICE_UNAVAILABLE, "Failed to save overrides");
                }
            };
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/providers/{id}/overrides] transaction failed: {error}");
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to save overrides",
                    );
                }
            };
            let raw_settings = match transaction
                .query_row("SELECT data FROM settings WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
            {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("[api/providers/{id}/overrides] settings query failed: {error}");
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to save overrides",
                    );
                }
            };
            let mut settings = raw_settings
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            let mut all_overrides = settings
                .get("providerOverrides")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(headers) = override_headers.as_ref() {
                all_overrides.insert(canonical.clone(), json!({ "headers": headers }));
            } else {
                all_overrides.remove(&canonical);
            }
            settings.insert("providerOverrides".to_owned(), Value::Object(all_overrides));
            let revision = match transaction
                .query_row(
                    "SELECT value FROM _meta WHERE key = 'settings_revision'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            {
                Ok(revision) => revision
                    .and_then(|revision| revision.parse::<u64>().ok())
                    .unwrap_or_default()
                    .saturating_add(1),
                Err(error) => {
                    eprintln!(
                        "[api/providers/{id}/overrides] settings revision read failed: {error}"
                    );
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to save overrides",
                    );
                }
            };
            if let Err(error) = transaction.execute(
                "INSERT INTO _meta(key, value) VALUES('settings_revision', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![revision.to_string()],
            ) {
                eprintln!(
                    "[api/providers/{id}/overrides] settings revision update failed: {error}"
                );
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to save overrides",
                );
            }
            if let Err(error) = transaction.execute(
                "INSERT INTO settings(id, data) VALUES(1, ?1)
                 ON CONFLICT(id) DO UPDATE SET data = excluded.data",
                params![Value::Object(settings).to_string()],
            ) {
                eprintln!("[api/providers/{id}/overrides] settings update failed: {error}");
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to save overrides",
                );
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/providers/{id}/overrides] commit failed: {error}");
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to save overrides",
                );
            }
            Json(json!({
                "headers": override_headers.map(Value::Object).unwrap_or_else(|| json!({}))
            }))
            .into_response()
        }
        _ => api_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_headers;
    use serde_json::{Value, json};

    #[test]
    fn header_overrides_match_node_validation_and_normalization() {
        assert_eq!(normalize_headers(None).unwrap(), None);
        assert_eq!(normalize_headers(Some(&Value::Null)).unwrap(), None);
        assert_eq!(
            normalize_headers(Some(&json!({"X-Test": "", "X-Null": null}))).unwrap(),
            None
        );
        assert_eq!(
            normalize_headers(Some(&json!({"X-Test": "ok"}))).unwrap(),
            Some(json!({"X-Test": "ok"}).as_object().unwrap().clone())
        );
        assert_eq!(
            normalize_headers(Some(&json!({"content-length": "1"}))).unwrap_err(),
            "Header content-length cannot be overridden"
        );
        assert_eq!(
            normalize_headers(Some(&json!({"Bad Header": "value"}))).unwrap_err(),
            "Invalid header name: Bad Header"
        );
        assert!(normalize_headers(Some(&json!({"X-Test": "line\nbreak"}))).is_err());
    }
}
