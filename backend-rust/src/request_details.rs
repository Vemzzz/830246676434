use axum::{
    Json,
    extract::{Path, Request},
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use serde_json::{Value, json};

fn query_param(request: &Request, name: &str) -> Option<String> {
    request
        .uri()
        .query()
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

fn parse_js_integer(value: Option<String>, default: i64) -> i64 {
    let Some(value) = value else {
        return default;
    };
    let value = value.trim_start();
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let digit_count = digits.bytes().take_while(u8::is_ascii_digit).count();
    if digit_count == 0 {
        return default;
    }
    let Ok(number) = digits[..digit_count].parse::<i64>() else {
        return default;
    };
    if negative { -number } else { number }
}

fn db_error(endpoint: &str, error: rusqlite::Error) -> Response {
    eprintln!("[{endpoint}] database query failed: {error}");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "Failed to fetch request details" })),
    )
        .into_response()
}

fn open_database(endpoint: &str) -> Result<Connection, (axum::http::StatusCode, &'static str)> {
    crate::open_database_for_admin().map_err(|error| {
        eprintln!("[{endpoint}] database unavailable: {error}");
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable",
        )
    })
}

fn format_log_date(timestamp: &str) -> String {
    let Some((date, time)) = timestamp.split_once('T') else {
        return "Invalid Date".to_owned();
    };
    let mut date = date.split('-');
    let (Some(year), Some(month), Some(day)) = (date.next(), date.next(), date.next()) else {
        return "Invalid Date".to_owned();
    };
    let mut time = time
        .split(['.', 'Z', '+', '-'])
        .next()
        .unwrap_or_default()
        .split(':');
    let (Some(hour), Some(minute), Some(second)) = (time.next(), time.next(), time.next()) else {
        return "Invalid Date".to_owned();
    };
    format!("{day}-{month}-{year} {hour}:{minute}:{second}")
}

fn usage_log_value(value: Option<i64>, tokens: &Value, token_field: &str) -> Value {
    value
        .map(Value::from)
        .or_else(|| tokens.get(token_field).cloned())
        .unwrap_or_else(|| Value::String("-".to_owned()))
}

pub(super) async fn logs(request: Request) -> Response {
    if let Err((status, message)) = crate::require_dashboard_auth(&request) {
        return (status, Json(json!({ "error": message }))).into_response();
    }
    let connection = match open_database("api/usage/logs") {
        Ok(connection) => connection,
        Err((status, message)) => {
            return (status, Json(json!({ "error": message }))).into_response();
        }
    };
    let rows = match connection
        .prepare(
            "SELECT timestamp, provider, model, connectionId, promptTokens, completionTokens, status, tokens
             FROM usageHistory ORDER BY id DESC LIMIT 200",
        )
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        }) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("[api/usage/logs] query failed: {error}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to fetch logs" })),
            )
                .into_response();
        }
    };
    let names = match connection
        .prepare("SELECT id, name, email FROM providerConnections")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        }) {
        Ok(rows) => rows
            .into_iter()
            .map(|(id, name, email)| {
                let name = name
                    .filter(|name| !name.is_empty())
                    .or_else(|| email.filter(|email| !email.is_empty()))
                    .unwrap_or_default();
                (id, name)
            })
            .collect::<std::collections::HashMap<_, _>>(),
        Err(error) => {
            eprintln!("[api/usage/logs] account query failed: {error}");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to fetch logs" })),
            )
                .into_response();
        }
    };
    let logs = rows
        .into_iter()
        .map(
            |(
                timestamp,
                provider,
                model,
                connection_id,
                prompt,
                completion,
                status,
                raw_tokens,
            )| {
                let tokens = raw_tokens
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .unwrap_or_else(|| json!({}));
                let provider = provider
                    .filter(|provider| !provider.is_empty())
                    .map(|provider| provider.to_uppercase())
                    .unwrap_or_else(|| "-".to_owned());
                let model = model
                    .filter(|model| !model.is_empty())
                    .unwrap_or_else(|| "-".to_owned());
                let account = connection_id
                    .as_ref()
                    .map(|id| {
                        names
                            .get(id)
                            .filter(|name| !name.is_empty())
                            .cloned()
                            .unwrap_or_else(|| id.chars().take(8).collect())
                    })
                    .unwrap_or_else(|| "-".to_owned());
                let sent = usage_log_value(prompt, &tokens, "prompt_tokens");
                let received = usage_log_value(completion, &tokens, "completion_tokens");
                format!(
                    "{} | {} | {} | {} | {} | {} | {}",
                    format_log_date(&timestamp),
                    model,
                    provider,
                    account,
                    display_value(&sent),
                    display_value(&received),
                    status.unwrap_or_else(|| "-".to_owned())
                )
            },
        )
        .collect::<Vec<_>>();
    Json(logs).into_response()
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "-".to_owned(),
        _ => value.to_string(),
    }
}

pub(super) async fn list(request: Request) -> Response {
    if let Err((status, message)) = crate::require_dashboard_auth(&request) {
        return (status, Json(json!({ "error": message }))).into_response();
    }
    let page = parse_js_integer(query_param(&request, "page"), 1);
    let page_size = parse_js_integer(query_param(&request, "pageSize"), 20);
    if page < 1 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Page must be >= 1" })),
        )
            .into_response();
    }
    if !(1..=100).contains(&page_size) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "PageSize must be between 1 and 100" })),
        )
            .into_response();
    }

    let mut conditions = Vec::new();
    let mut values = Vec::<SqlValue>::new();
    for (key, column) in [
        ("provider", "provider"),
        ("model", "model"),
        ("connectionId", "connectionId"),
        ("status", "status"),
        ("startDate", "timestamp"),
        ("endDate", "timestamp"),
    ] {
        if let Some(value) = query_param(&request, key).filter(|value| !value.is_empty()) {
            conditions.push(format!(
                "{column} {} ?{}",
                if key == "startDate" {
                    ">="
                } else if key == "endDate" {
                    "<="
                } else {
                    "="
                },
                values.len() + 1
            ));
            values.push(SqlValue::Text(value));
        }
    }
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    let connection = match open_database("api/usage/request-details") {
        Ok(connection) => connection,
        Err((status, message)) => {
            return (status, Json(json!({ "error": message }))).into_response();
        }
    };
    let count_sql = format!("SELECT COUNT(*) FROM requestDetails{where_clause}");
    let total_items =
        match connection.query_row(&count_sql, params_from_iter(values.iter()), |row| {
            row.get::<_, u64>(0)
        }) {
            Ok(count) => count,
            Err(error) => return db_error("api/usage/request-details", error),
        };
    let page_count = total_items.div_ceil(page_size as u64);
    let offset = (page - 1) * page_size;
    values.push(SqlValue::Integer(page_size));
    values.push(SqlValue::Integer(offset));
    let rows = match connection
        .prepare(&format!(
            "SELECT data FROM requestDetails{where_clause} ORDER BY timestamp DESC LIMIT ? OFFSET ?"
        ))
        .and_then(|mut statement| {
            statement
                .query_map(params_from_iter(values.iter()), |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()
        }) {
        Ok(rows) => rows,
        Err(error) => return db_error("api/usage/request-details", error),
    };
    let details = rows
        .into_iter()
        .map(|raw| {
            let mut detail = serde_json::from_str::<Value>(&raw)
                .unwrap_or_else(|_| json!({}))
                .as_object()
                .cloned()
                .unwrap_or_default();
            for key in ["request", "providerRequest", "providerResponse", "response"] {
                if detail.contains_key(key) {
                    detail.insert(key.to_owned(), json!({ "redacted": true }));
                }
            }
            Value::Object(detail)
        })
        .collect::<Vec<_>>();
    Json(json!({
        "details": details,
        "pagination": {
            "page": page,
            "pageSize": page_size,
            "totalItems": total_items,
            "totalPages": page_count,
            "hasNext": page < page_count as i64,
            "hasPrev": page > 1
        }
    }))
    .into_response()
}

pub(super) async fn by_id(Path(id): Path<String>, request: Request) -> Response {
    if let Err((status, message)) = crate::require_dashboard_auth(&request) {
        return (status, Json(json!({ "error": message }))).into_response();
    }
    let connection = match open_database("api/usage/request-details/{id}") {
        Ok(connection) => connection,
        Err((status, message)) => {
            return (status, Json(json!({ "error": message }))).into_response();
        }
    };
    let raw = match connection.query_row(
        "SELECT data FROM requestDetails WHERE id = ?1",
        [id],
        |row| row.get::<_, String>(0),
    ) {
        Ok(raw) => raw,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({ "error": "Not found" })),
            )
                .into_response();
        }
        Err(error) => return db_error("api/usage/request-details/{id}", error),
    };
    let detail = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
    Json(json!({ "detail": detail })).into_response()
}

#[cfg(test)]
mod tests {
    use super::{format_log_date, parse_js_integer};

    #[test]
    fn integer_query_parsing_matches_parse_int_defaults() {
        assert_eq!(parse_js_integer(Some(" 12tail".to_owned()), 20), 12);
        assert_eq!(parse_js_integer(Some("2.9".to_owned()), 20), 2);
        assert_eq!(parse_js_integer(Some("invalid".to_owned()), 20), 20);
        assert_eq!(parse_js_integer(None, 20), 20);
    }

    #[test]
    fn log_date_format_matches_dashboard_log_contract_for_utc_timestamps() {
        assert_eq!(
            format_log_date("2026-10-08T12:34:56.789Z"),
            "08-10-2026 12:34:56"
        );
        assert_eq!(format_log_date("bad timestamp"), "Invalid Date");
    }
}
