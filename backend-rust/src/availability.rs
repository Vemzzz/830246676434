use axum::{
    Json,
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn entries(
    id: &str,
    provider: &str,
    name: Option<&str>,
    email: Option<&str>,
    data: &Value,
    now: &str,
) -> Vec<Value> {
    let connection_name = name
        .filter(|value| !value.is_empty())
        .or_else(|| email.filter(|value| !value.is_empty()))
        .unwrap_or(id);
    let last_error = data
        .get("lastError")
        .filter(|value| json_truthy(value))
        .cloned()
        .unwrap_or(Value::Null);
    let mut models = Vec::new();
    let mut has_active_lock = false;

    if let Some(data) = data.as_object() {
        for (key, until) in data {
            if !key.starts_with("modelLock_")
                || !json_truthy(until)
                || !until.as_str().is_some_and(|value| value > now)
            {
                continue;
            }
            has_active_lock = true;
            let model = key.strip_prefix("modelLock_").unwrap_or_default();
            models.push(json!({
                "provider": provider,
                "model": if model.is_empty() { "__all" } else { model },
                "status": "cooldown",
                "until": until,
                "connectionId": id,
                "connectionName": connection_name,
                "lastError": last_error,
            }));
        }
    }
    if !has_active_lock && data.get("testStatus").and_then(Value::as_str) == Some("unavailable") {
        models.push(json!({
            "provider": provider,
            "model": "__all",
            "status": "unavailable",
            "connectionId": id,
            "connectionName": connection_name,
            "lastError": last_error,
        }));
    }
    models
}

fn clear_cooldown(data: &mut serde_json::Map<String, Value>, lock_key: &str) -> bool {
    if !data.get(lock_key).is_some_and(json_truthy) {
        return false;
    }
    data.insert(lock_key.to_owned(), Value::Null);
    if data.get("testStatus").and_then(Value::as_str) == Some("unavailable") {
        data.insert("testStatus".to_owned(), Value::String("active".to_owned()));
        data.insert("lastError".to_owned(), Value::Null);
        data.insert("lastErrorAt".to_owned(), Value::Null);
        data.insert("backoffLevel".to_owned(), json!(0));
    }
    true
}

fn admin_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn open_database() -> Result<Connection, (StatusCode, &'static str)> {
    crate::open_database_for_admin().map_err(|error| {
        eprintln!("[api/models/availability] database unavailable: {error}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable",
        )
    })
}

pub(super) async fn route(request: Request) -> Response {
    if let Err((status, message)) = crate::require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    match request.method().clone() {
        Method::GET => {
            let connection = match open_database() {
                Ok(connection) => connection,
                Err((status, message)) => return admin_error(status, message),
            };
            let now = crate::admin::utc_timestamp();
            let rows = match connection
                .prepare("SELECT id, provider, name, email, data FROM providerConnections")
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, String>(4)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()
                }) {
                Ok(rows) => rows,
                Err(error) => {
                    eprintln!("[api/models/availability] query failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to fetch model availability",
                    );
                }
            };
            let models = rows
                .into_iter()
                .flat_map(|(id, provider, name, email, raw_data)| {
                    let data =
                        serde_json::from_str::<Value>(&raw_data).unwrap_or_else(|_| json!({}));
                    entries(
                        &id,
                        &provider,
                        name.as_deref(),
                        email.as_deref(),
                        &data,
                        &now,
                    )
                })
                .collect::<Vec<_>>();
            Json(json!({ "models": models, "unavailableCount": models.len() })).into_response()
        }
        Method::POST => {
            let payload = match crate::parse_admin_json(request.into_body()).await {
                Ok(Value::Object(payload)) => Value::Object(payload),
                Ok(_) => return admin_error(StatusCode::BAD_REQUEST, "Invalid request"),
                Err((status, message)) => return admin_error(status, message),
            };
            let Some(provider) = payload.get("provider").and_then(Value::as_str) else {
                return admin_error(StatusCode::BAD_REQUEST, "Invalid request");
            };
            let Some(model) = payload.get("model").and_then(Value::as_str) else {
                return admin_error(StatusCode::BAD_REQUEST, "Invalid request");
            };
            if payload.get("action").and_then(Value::as_str) != Some("clearCooldown")
                || provider.is_empty()
                || model.is_empty()
            {
                return admin_error(StatusCode::BAD_REQUEST, "Invalid request");
            }
            let mut connection = match open_database() {
                Ok(connection) => connection,
                Err((status, message)) => return admin_error(status, message),
            };
            let lock_key = format!("modelLock_{model}");
            let rows = match connection
                .prepare("SELECT id, data FROM providerConnections WHERE provider = ?1")
                .and_then(|mut statement| {
                    statement
                        .query_map(params![provider], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()
                }) {
                Ok(rows) => rows,
                Err(error) => {
                    eprintln!("[api/models/availability] query failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to clear cooldown",
                    );
                }
            };
            let now = crate::admin::utc_timestamp();
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/models/availability] transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to clear cooldown",
                    );
                }
            };
            for (id, raw_data) in rows {
                let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&raw_data) else {
                    continue;
                };
                if !clear_cooldown(&mut data, &lock_key) {
                    continue;
                }
                if let Err(error) = transaction.execute(
                    "UPDATE providerConnections SET data = ?1, updatedAt = ?2 WHERE id = ?3",
                    params![Value::Object(data).to_string(), now, id],
                ) {
                    eprintln!("[api/models/availability] update failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to clear cooldown",
                    );
                }
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/models/availability] commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to clear cooldown",
                );
            }
            Json(json!({ "ok": true })).into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

#[cfg(test)]
mod tests {
    use super::{clear_cooldown, entries};
    use serde_json::{Value, json};

    #[test]
    fn reports_active_locks_and_unavailable_accounts() {
        let now = "2026-10-08T12:00:00.000Z";
        let locked = entries(
            "conn-1",
            "openai",
            Some("Main"),
            Some("main@example.com"),
            &json!({
                "modelLock_gpt-4.1": "2026-10-08T13:00:00.000Z",
                "modelLock_": "2026-10-08T13:30:00.000Z",
                "modelLock_expired": "2026-10-08T11:00:00.000Z",
                "lastError": "Rate limited",
                "testStatus": "unavailable"
            }),
            now,
        );
        assert_eq!(locked.len(), 2);
        assert!(locked.iter().any(|entry| {
            entry["model"] == "gpt-4.1"
                && entry["status"] == "cooldown"
                && entry["connectionName"] == "Main"
                && entry["lastError"] == "Rate limited"
        }));
        assert!(
            locked
                .iter()
                .any(|entry| entry["model"] == "__all" && entry["status"] == "cooldown")
        );

        let unavailable = entries(
            "conn-2",
            "claude",
            None,
            Some("claude@example.com"),
            &json!({"testStatus": "unavailable"}),
            now,
        );
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0]["model"], "__all");
        assert_eq!(unavailable[0]["status"], "unavailable");
        assert_eq!(unavailable[0]["connectionName"], "claude@example.com");
    }

    #[test]
    fn clearing_a_cooldown_resets_unavailable_health_state_only_when_locked() {
        let mut data = json!({
            "modelLock_gpt-4.1": "2026-10-08T13:00:00.000Z",
            "testStatus": "unavailable",
            "lastError": "Rate limited",
            "lastErrorAt": "2026-10-08T11:00:00.000Z",
            "backoffLevel": 3
        })
        .as_object()
        .unwrap()
        .clone();

        assert!(clear_cooldown(&mut data, "modelLock_gpt-4.1"));
        assert_eq!(data["modelLock_gpt-4.1"], Value::Null);
        assert_eq!(data["testStatus"], "active");
        assert_eq!(data["lastError"], Value::Null);
        assert_eq!(data["lastErrorAt"], Value::Null);
        assert_eq!(data["backoffLevel"], 0);
        assert!(!clear_cooldown(&mut data, "modelLock_missing"));
    }
}
