use axum::{
    Json,
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

use crate::{
    CatalogProvider, admin, model_catalog, open_database_for_admin, require_dashboard_auth,
};

const SAFE_FIELDS: &[&str] = &[
    "id",
    "provider",
    "authType",
    "name",
    "email",
    "displayName",
    "priority",
    "globalPriority",
    "isActive",
    "defaultModel",
    "testStatus",
    "lastError",
    "lastErrorAt",
    "errorCode",
    "expiresAt",
    "lastUsedAt",
    "consecutiveUseCount",
    "createdAt",
    "updatedAt",
];
const SAFE_PSD_FIELDS: &[&str] = &[
    "baseUrl",
    "azureEndpoint",
    "deployment",
    "apiVersion",
    "accountId",
    "region",
    "projectId",
    "resourceUrl",
    "proxyPoolId",
    "connectionProxyEnabled",
    "connectionProxyUrl",
    "connectionNoProxy",
    "githubLogin",
    "githubName",
    "githubEmail",
    "githubUserId",
    "username",
    "firstName",
    "lastName",
    "authMethod",
    "authKind",
    "profileArn",
];
const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_PAGE_SIZE: usize = 500;

#[derive(Clone)]
struct ConnectionRow {
    id: String,
    provider: String,
    auth_type: String,
    name: Option<String>,
    email: Option<String>,
    priority: Option<i64>,
    is_active: bool,
    created_at: String,
    updated_at: String,
    data: Value,
}

fn decode_jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn backfill_codex_emails(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare(
            "SELECT id, email, data FROM providerConnections WHERE provider = 'codex' AND authType = 'oauth'",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut updates = Vec::new();
    for row in rows {
        let (id, email, raw_data) = row.map_err(|error| error.to_string())?;
        let mut data = serde_json::from_str::<Value>(&raw_data)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let Some(id_token) = data.get("idToken").and_then(Value::as_str) else {
            continue;
        };
        let Some(payload) = decode_jwt_payload(id_token) else {
            continue;
        };
        let auth = payload
            .get("https://api.openai.com/auth")
            .and_then(Value::as_object);
        let account_id = auth
            .and_then(|claims| claims.get("chatgpt_account_id"))
            .or_else(|| payload.get("account_id"))
            .filter(|value| admin::json_truthy(value));
        let plan_type = auth
            .and_then(|claims| claims.get("chatgpt_plan_type"))
            .or_else(|| payload.get("plan_type"))
            .filter(|value| admin::json_truthy(value));
        let missing_email = email.as_deref().is_none_or(str::is_empty);
        let has_account = data
            .get("providerSpecificData")
            .and_then(|value| value.get("chatgptAccountId"))
            .is_some_and(admin::json_truthy);
        if !missing_email && has_account {
            continue;
        }
        let new_email = if missing_email {
            payload
                .get("email")
                .and_then(Value::as_str)
                .filter(|email| !email.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        let mut psd = data
            .get("providerSpecificData")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if account_id.is_some() || plan_type.is_some() {
            if let Some(value) = account_id {
                psd.insert("chatgptAccountId".to_owned(), value.clone());
            }
            if let Some(value) = plan_type {
                psd.insert("chatgptPlanType".to_owned(), value.clone());
            }
            data.insert("providerSpecificData".to_owned(), Value::Object(psd));
        }
        if new_email.is_some() || account_id.is_some() || plan_type.is_some() {
            updates.push((id, new_email, Value::Object(data).to_string()));
        }
    }
    drop(statement);
    if updates.is_empty() {
        return Ok(());
    }
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let now = admin::utc_timestamp();
    for (id, email, data) in updates {
        transaction
            .execute(
                "UPDATE providerConnections SET email = CASE WHEN ?1 IS NULL THEN email ELSE COALESCE(NULLIF(email, ''), ?1) END, data = ?2, updatedAt = ?3 WHERE id = ?4",
                params![email, data, now, id],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn read_connections(connection: &Connection) -> Result<Vec<ConnectionRow>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, provider, authType, name, email, priority, isActive, createdAt, updatedAt, data FROM providerConnections",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let raw_data = row.get::<_, String>(9)?;
            Ok(ConnectionRow {
                id: row.get(0)?,
                provider: row.get(1)?,
                auth_type: row.get(2)?,
                name: row.get(3)?,
                email: row.get(4)?,
                priority: row.get(5)?,
                is_active: row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                data: serde_json::from_str(&raw_data).unwrap_or(Value::Null),
            })
        })
        .map_err(|error| error.to_string())?;
    rows.map(|row| row.map_err(|error| error.to_string()))
        .collect()
}

fn usage_eligible(connection: &ConnectionRow, provider: Option<&CatalogProvider>) -> bool {
    let Some(provider) = provider else {
        return false;
    };
    provider.usage_supported && (connection.auth_type == "oauth" || provider.usage_apikey_supported)
}

fn mask_name(name: &str) -> String {
    if name.len() <= 16 {
        return name.to_owned();
    }
    let bytes = name.as_bytes();
    if bytes.windows(32).any(|window| {
        window
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }) {
        let prefix = name.chars().take(8).collect::<String>();
        format!("{prefix}***")
    } else {
        name.to_owned()
    }
}

fn sanitized_connection(connection: &ConnectionRow) -> Value {
    let mut safe = Map::new();
    let mut fields = Map::new();
    for (name, value) in [
        ("id", Some(Value::String(connection.id.clone()))),
        ("provider", Some(Value::String(connection.provider.clone()))),
        (
            "authType",
            Some(Value::String(connection.auth_type.clone())),
        ),
        (
            "name",
            Some(connection.name.clone().map_or(Value::Null, Value::String)),
        ),
        (
            "email",
            Some(connection.email.clone().map_or(Value::Null, Value::String)),
        ),
        (
            "priority",
            Some(connection.priority.map_or(Value::Null, Value::from)),
        ),
        ("isActive", Some(Value::Bool(connection.is_active))),
        (
            "createdAt",
            Some(Value::String(connection.created_at.clone())),
        ),
        (
            "updatedAt",
            Some(Value::String(connection.updated_at.clone())),
        ),
    ] {
        if let Some(value) = value {
            fields.insert(name.to_owned(), value);
        }
    }
    if let Some(data) = connection.data.as_object() {
        for name in SAFE_FIELDS {
            if fields.contains_key(*name) {
                continue;
            }
            if let Some(value) = data.get(*name) {
                fields.insert((*name).to_owned(), value.clone());
            }
        }
    }
    if let Some(Value::String(name)) = fields.get("name").cloned()
        && !name.is_empty()
    {
        fields.insert("name".to_owned(), Value::String(mask_name(&name)));
    }
    if let Some(psd) = connection
        .data
        .get("providerSpecificData")
        .and_then(Value::as_object)
    {
        let mut safe_psd = Map::new();
        for name in SAFE_PSD_FIELDS {
            if let Some(value) = psd.get(*name) {
                safe_psd.insert((*name).to_owned(), value.clone());
            }
        }
        safe.insert("providerSpecificData".to_owned(), Value::Object(safe_psd));
    }
    safe.extend(fields);
    Value::Object(safe)
}

fn parse_positive_int(raw: Option<String>, fallback: usize) -> usize {
    let Some(raw) = raw else {
        return fallback;
    };
    let raw = raw.trim_start();
    let digits = raw
        .strip_prefix('+')
        .unwrap_or(raw)
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn provider_client_response(request: &Request) -> Result<Value, String> {
    let connection = open_database_for_admin().map_err(|error| {
        eprintln!("[api/providers/client] database unavailable: {error}");
        error
    })?;
    if let Err(error) = backfill_codex_emails(&connection) {
        eprintln!("[api/providers/client] Codex account backfill failed: {error}");
    }
    let all = read_connections(&connection)?;
    let provider_order = model_catalog()
        .providers
        .iter()
        .map(|provider| (provider.id.as_str(), provider.usage_order))
        .collect::<std::collections::HashMap<_, _>>();
    let eligible = all
        .iter()
        .filter(|entry| {
            usage_eligible(
                entry,
                model_catalog()
                    .providers
                    .iter()
                    .find(|provider| provider.id == entry.provider),
            )
        })
        .collect::<Vec<_>>();
    let mut provider_options = eligible
        .iter()
        .map(|entry| entry.provider.clone())
        .collect::<Vec<_>>();
    provider_options.sort();
    provider_options.dedup();

    let query = request.uri().query().unwrap_or_default();
    let mut query_params = std::collections::HashMap::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        query_params
            .entry(key.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    let provider_filter = query_params
        .get("provider")
        .map(String::as_str)
        .unwrap_or("all");
    let account_status = query_params
        .get("accountStatus")
        .map(String::as_str)
        .unwrap_or("all");
    let sort = query_params
        .get("sort")
        .map(String::as_str)
        .unwrap_or("priority");
    let page = parse_positive_int(query_params.get("page").cloned(), 1);
    let page_size = parse_positive_int(query_params.get("pageSize").cloned(), DEFAULT_PAGE_SIZE)
        .min(MAX_PAGE_SIZE);
    let provider_filtered = eligible
        .iter()
        .copied()
        .filter(|entry| provider_filter == "all" || entry.provider == provider_filter)
        .collect::<Vec<_>>();
    let account_filtered = provider_filtered
        .iter()
        .copied()
        .filter(|entry| match account_status {
            "active" => entry.is_active,
            "inactive" => !entry.is_active,
            _ => true,
        })
        .collect::<Vec<_>>();
    let mut sorted = account_filtered;
    if sort == "provider" {
        sorted.sort_by(|left, right| {
            let left_order = provider_order
                .get(left.provider.as_str())
                .copied()
                .unwrap_or(usize::MAX);
            let right_order = provider_order
                .get(right.provider.as_str())
                .copied()
                .unwrap_or(usize::MAX);
            left_order
                .cmp(&right_order)
                .then_with(|| left.provider.cmp(&right.provider))
                .then_with(|| {
                    left.priority
                        .unwrap_or(i64::MAX)
                        .cmp(&right.priority.unwrap_or(i64::MAX))
                })
        });
    } else {
        sorted.sort_by(|left, right| {
            left.priority
                .unwrap_or(i64::MAX)
                .cmp(&right.priority.unwrap_or(i64::MAX))
                .then_with(|| left.provider.cmp(&right.provider))
        });
    }
    let total = sorted.len();
    let total_pages = total.div_ceil(page_size).max(1);
    let current_page = page.min(total_pages);
    let offset = (current_page - 1) * page_size;
    let page_connections = sorted
        .iter()
        .skip(offset)
        .take(page_size)
        .map(|entry| sanitized_connection(entry))
        .collect::<Vec<_>>();
    Ok(json!({
        "connections": page_connections,
        "providerOptions": provider_options,
        "pagination": {
            "page": current_page,
            "pageSize": page_size,
            "total": total,
            "totalPages": total_pages,
        },
        "totals": {
            "eligibleConnections": eligible.len(),
            "providerFilteredConnections": provider_filtered.len(),
        },
    }))
}

pub(super) async fn route(request: Request) -> Response {
    if request.method() != Method::GET {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({ "error": "Method not allowed" })),
        )
            .into_response();
    }
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return (status, Json(json!({ "error": message }))).into_response();
    }
    match provider_client_response(&request) {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            eprintln!("[api/providers/client] query failed: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to fetch providers" })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionRow, backfill_codex_emails, mask_name, parse_positive_int, sanitized_connection,
        usage_eligible,
    };
    use crate::model_catalog;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rusqlite::Connection;
    use serde_json::{Value, json};

    #[test]
    fn connection_sanitizer_only_returns_dashboard_safe_fields() {
        let connection = ConnectionRow {
            id: "id".to_owned(),
            provider: "codex".to_owned(),
            auth_type: "oauth".to_owned(),
            name: Some("account".to_owned()),
            email: Some("user@example.com".to_owned()),
            priority: Some(2),
            is_active: true,
            created_at: "created".to_owned(),
            updated_at: "updated".to_owned(),
            data: json!({
                "displayName": "OpenAI",
                "accessToken": "secret",
                "apiKey": "secret",
                "testStatus": "active",
                "providerSpecificData": {
                    "baseUrl": "https://api.example",
                    "connectionProxyUrl": "http://proxy.example",
                    "apiKey": "secret",
                    "otherSecret": "secret"
                }
            }),
        };
        let result = sanitized_connection(&connection);
        assert_eq!(result["name"], "account");
        assert_eq!(result["testStatus"], "active");
        assert_eq!(
            result["providerSpecificData"],
            json!({
                "baseUrl": "https://api.example",
                "connectionProxyUrl": "http://proxy.example"
            })
        );
        assert!(result.get("accessToken").is_none());
        assert!(result.get("apiKey").is_none());
    }

    #[test]
    fn provider_client_pagination_and_name_masking_match_node_rules() {
        assert_eq!(parse_positive_int(Some("  +23more".to_owned()), 1), 23);
        assert_eq!(parse_positive_int(Some("0".to_owned()), 20), 20);
        assert_eq!(parse_positive_int(Some("bad".to_owned()), 20), 20);
        assert_eq!(mask_name("12345678abcdefghijklmnopqrstuvwx"), "12345678***");
        assert_eq!(mask_name("short-name"), "short-name");
    }

    #[test]
    fn provider_usage_eligibility_matches_registry_feature_flags() {
        let codex = model_catalog()
            .providers
            .iter()
            .find(|provider| provider.id == "codex")
            .expect("Codex provider is in registry catalog");
        let oauth = ConnectionRow {
            id: "oauth".to_owned(),
            provider: "codex".to_owned(),
            auth_type: "oauth".to_owned(),
            name: None,
            email: None,
            priority: None,
            is_active: true,
            created_at: String::new(),
            updated_at: String::new(),
            data: Value::Null,
        };
        let apikey = ConnectionRow {
            auth_type: "apikey".to_owned(),
            ..oauth.clone()
        };
        assert!(usage_eligible(&oauth, Some(codex)));
        assert!(!usage_eligible(&apikey, Some(codex)));

        let apikey_usage_provider = model_catalog()
            .providers
            .iter()
            .find(|provider| provider.usage_supported && provider.usage_apikey_supported)
            .expect("catalog has API-key usage providers");
        let other_auth = ConnectionRow {
            provider: apikey_usage_provider.id.clone(),
            auth_type: "cookie".to_owned(),
            ..oauth
        };
        assert!(usage_eligible(&other_auth, Some(apikey_usage_provider)));
    }

    #[test]
    fn codex_backfill_persists_missing_email_and_account_claims() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE providerConnections (
                    id TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    authType TEXT NOT NULL,
                    email TEXT,
                    data TEXT NOT NULL,
                    updatedAt TEXT NOT NULL
                );",
            )
            .unwrap();
        let payload = URL_SAFE_NO_PAD.encode(
            json!({
                "email": "codex@example.com",
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": "account-1",
                    "chatgpt_plan_type": "plus"
                }
            })
            .to_string(),
        );
        let data = json!({
            "idToken": format!("header.{payload}.signature"),
            "providerSpecificData": {}
        });
        connection
            .execute(
                "INSERT INTO providerConnections (id, provider, authType, email, data, updatedAt) VALUES ('codex-1', 'codex', 'oauth', NULL, ?1, '')",
                [data.to_string()],
            )
            .unwrap();

        backfill_codex_emails(&connection).unwrap();

        let (email, raw_data): (Option<String>, String) = connection
            .query_row(
                "SELECT email, data FROM providerConnections WHERE id = 'codex-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let data: Value = serde_json::from_str(&raw_data).unwrap();
        assert_eq!(email.as_deref(), Some("codex@example.com"));
        assert_eq!(
            data["providerSpecificData"]["chatgptAccountId"],
            "account-1"
        );
        assert_eq!(data["providerSpecificData"]["chatgptPlanType"], "plus");
    }
}
