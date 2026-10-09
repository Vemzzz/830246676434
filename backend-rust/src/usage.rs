use super::*;

fn query_param(request: &Request, name: &str) -> Option<String> {
    request
        .uri()
        .query()
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

fn usage_database(request: &Request) -> Result<Connection, (StatusCode, &'static str)> {
    require_dashboard_auth(request)?;
    open_database().map_err(|error| {
        eprintln!("[api/usage] database unavailable: {error}");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend database unavailable",
        )
    })
}

fn usage_provider_name(provider_id: &str, node_name: Option<&str>) -> String {
    node_name
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            super::admin::provider_definition(provider_id)
                .map(|provider| provider.display_name.clone())
        })
        .unwrap_or_else(|| provider_id.to_owned())
}

pub(super) async fn usage_providers(request: Request) -> Response {
    let connection = match usage_database(&request) {
        Ok(connection) => connection,
        Err((status, message)) => return admin_error(status, message),
    };
    let provider_ids = match connection.prepare(
        "SELECT DISTINCT provider FROM requestDetails WHERE provider IS NOT NULL ORDER BY provider ASC",
    ).and_then(|mut statement| {
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()
    }) {
        Ok(provider_ids) => provider_ids,
        Err(error) => {
            eprintln!("[api/usage/providers] query failed: {error}");
            return admin_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch providers",
            );
        }
    };
    let mut providers = Vec::with_capacity(provider_ids.len());
    for id in provider_ids {
        let node_name = match connection
            .query_row(
                "SELECT name FROM providerNodes WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
        {
            Ok(name) => name.flatten(),
            Err(error) => {
                eprintln!("[api/usage/providers] provider node lookup failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch providers",
                );
            }
        };
        providers.push(json!({
            "id": id,
            "name": usage_provider_name(&id, node_name.as_deref())
        }));
    }
    Json(json!({ "providers": providers })).into_response()
}

pub(super) async fn usage_history(request: Request) -> Response {
    let connection = match usage_database(&request) {
        Ok(connection) => connection,
        Err((status, message)) => return admin_error(status, message),
    };
    let mut sql = String::from(
        "SELECT timestamp, provider, model, connectionId, apiKey, endpoint, cost, status, tokens FROM usageHistory",
    );
    let mut conditions = Vec::new();
    let mut values = Vec::<String>::new();
    for (query_name, column) in [
        ("provider", "provider"),
        ("model", "model"),
        ("startDate", "timestamp"),
        ("endDate", "timestamp"),
    ] {
        if let Some(value) = query_param(&request, query_name).filter(|value| !value.is_empty()) {
            values.push(value);
            let comparison = if query_name == "endDate" {
                "<="
            } else if query_name == "startDate" {
                ">="
            } else {
                "="
            };
            conditions.push(format!("{column} {comparison} ?{}", values.len()));
        }
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY id ASC");
    let mut statement = match connection.prepare(&sql) {
        Ok(statement) => statement,
        Err(error) => {
            eprintln!("[api/usage/history] query prepare failed: {error}");
            return admin_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch usage history",
            );
        }
    };
    let rows = match statement.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        let api_key: Option<String> = row.get(4)?;
        let raw_tokens: Option<String> = row.get(8)?;
        Ok(json!({
            "timestamp": row.get::<_, String>(0)?,
            "provider": row.get::<_, Option<String>>(1)?,
            "model": row.get::<_, Option<String>>(2)?,
            "connectionId": row.get::<_, Option<String>>(3)?,
            "apiKeyMasked": api_key.as_deref().map(mask_usage_api_key),
            "endpoint": row.get::<_, Option<String>>(5)?,
            "cost": row.get::<_, Option<f64>>(6)?.unwrap_or_default(),
            "status": row.get::<_, Option<String>>(7)?,
            "tokens": raw_tokens.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()).unwrap_or(json!({}))
        }))
    }) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("[api/usage/history] query failed: {error}");
            return admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch usage history");
        }
    };
    let mut history = Vec::new();
    for row in rows {
        match row {
            Ok(row) => history.push(row),
            Err(error) => {
                eprintln!("[api/usage/history] row read failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to fetch usage history",
                );
            }
        }
    }
    Json(history).into_response()
}

fn valid_usage_period(period: &str) -> bool {
    matches!(period, "today" | "24h" | "7d" | "30d" | "60d" | "all")
}

pub(super) async fn usage_chart(request: Request) -> Response {
    let connection = match usage_database(&request) {
        Ok(connection) => connection,
        Err((status, message)) => return admin_error(status, message),
    };
    let period = query_param(&request, "period").unwrap_or_else(|| "7d".to_owned());
    if !valid_usage_period(&period) {
        return admin_error(StatusCode::BAD_REQUEST, "Invalid period");
    }
    let mut sql = if period == "today" || period == "24h" {
        "SELECT strftime('%Y-%m-%d %H', timestamp, 'localtime') AS bucket, SUM(promptTokens + completionTokens), SUM(cost), COUNT(*) FROM usageHistory WHERE ".to_owned()
    } else {
        "SELECT dateKey, json_extract(data, '$.promptTokens') + json_extract(data, '$.completionTokens'), json_extract(data, '$.cost'), json_extract(data, '$.requests') FROM usageDaily WHERE ".to_owned()
    };
    match period.as_str() {
        "today" => sql.push_str("date(timestamp, 'localtime') = date('now', 'localtime') "),
        "24h" => sql.push_str("timestamp >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-24 hours') "),
        "7d" => sql.push_str("dateKey >= date('now', 'localtime', '-6 day') "),
        "30d" => sql.push_str("dateKey >= date('now', 'localtime', '-29 day') "),
        "60d" => sql.push_str("dateKey >= date('now', 'localtime', '-59 day') "),
        "all" => sql.push_str("1 = 1 "),
        _ => return admin_error(StatusCode::BAD_REQUEST, "Invalid period"),
    }
    sql.push_str("GROUP BY 1 ORDER BY 1");
    let rows = match connection.prepare(&sql).and_then(|mut statement| {
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<u64>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<f64>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<u64>>(3)?.unwrap_or_default(),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
    }) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("[api/usage/chart] query failed: {error}");
            return admin_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch chart data",
            );
        }
    };
    let data = rows
        .into_iter()
        .map(|(key, tokens, cost, requests)| {
            let label = if period == "today" || period == "24h" {
                key.rsplit(' ')
                    .next()
                    .map(|hour| format!("{hour}:00"))
                    .unwrap_or(key)
            } else {
                let parts = key.split('-').collect::<Vec<_>>();
                if parts.len() == 3 {
                    let month = parts[1].parse::<usize>().unwrap_or(0);
                    let day = parts[2].parse::<u8>().unwrap_or(0);
                    let month_name = [
                        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct",
                        "Nov", "Dec",
                    ]
                    .get(month)
                    .copied()
                    .unwrap_or("");
                    format!("{month_name} {day}")
                } else {
                    key
                }
            };
            json!({"label": label, "tokens": tokens, "cost": cost, "requests": requests})
        })
        .collect::<Vec<_>>();
    Json(data).into_response()
}

pub(super) async fn usage_stats(request: Request) -> Response {
    let connection = match usage_database(&request) {
        Ok(connection) => connection,
        Err((status, message)) => return admin_error(status, message),
    };
    let period = query_param(&request, "period").unwrap_or_else(|| "7d".to_owned());
    if !valid_usage_period(&period) {
        return admin_error(StatusCode::BAD_REQUEST, "Invalid period");
    }
    let cutoff = match period.as_str() {
        "today" => Some("date(timestamp, 'localtime') = date('now', 'localtime')".to_owned()),
        "24h" => Some("timestamp >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-24 hours')".to_owned()),
        "7d" => Some(
            "timestamp >= strftime('%Y-%m-%dT00:00:00.000Z', 'now', '-6 day', 'localtime')"
                .to_owned(),
        ),
        "30d" => Some(
            "timestamp >= strftime('%Y-%m-%dT00:00:00.000Z', 'now', '-29 day', 'localtime')"
                .to_owned(),
        ),
        "60d" => Some(
            "timestamp >= strftime('%Y-%m-%dT00:00:00.000Z', 'now', '-59 day', 'localtime')"
                .to_owned(),
        ),
        "all" => None,
        _ => return admin_error(StatusCode::BAD_REQUEST, "Invalid period"),
    };
    let mut sql = String::from(
        "SELECT timestamp, provider, model, connectionId, apiKey, endpoint, promptTokens, completionTokens, cost, status, tokens FROM usageHistory",
    );
    if let Some(cutoff) = cutoff {
        sql.push_str(" WHERE ");
        sql.push_str(&cutoff);
    }
    sql.push_str(" ORDER BY id ASC");
    let rows = match connection.prepare(&sql).and_then(|mut statement| {
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<u64>>(6)?.unwrap_or_default(),
                    row.get::<_, Option<u64>>(7)?.unwrap_or_default(),
                    row.get::<_, Option<f64>>(8)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(9)?
                        .unwrap_or_else(|| "ok".to_owned()),
                    row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
    }) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("[api/usage/stats] query failed: {error}");
            return admin_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to fetch usage stats",
            );
        }
    };
    let mut stats = json!({
        "totalRequests": 0,
        "totalPromptTokens": 0,
        "totalCompletionTokens": 0,
        "totalCachedTokens": 0,
        "totalCost": 0.0,
        "byProvider": {},
        "byModel": {},
        "byAccount": {},
        "byApiKey": {},
        "byEndpoint": {},
        "last10Minutes": [],
        "pending": {"byModel": {}, "byAccount": {}},
        "activeRequests": [],
        "recentRequests": [],
        "errorProvider": ""
    });
    let mut recent = rows.iter().rev().take(100).collect::<Vec<_>>();
    let mut seen_recent = HashSet::new();
    let mut recent_requests = Vec::new();
    for row in recent.drain(..) {
        let tokens: Value = serde_json::from_str(&row.10).unwrap_or_else(|_| json!({}));
        let prompt = tokens
            .get("prompt_tokens")
            .or_else(|| tokens.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(row.6);
        let completion = tokens
            .get("completion_tokens")
            .or_else(|| tokens.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(row.7);
        if prompt == 0 && completion == 0 {
            continue;
        }
        let minute = row.0.get(..16).unwrap_or(&row.0);
        let key = format!("{}|{}|{prompt}|{completion}|{minute}", row.2, row.1);
        if !seen_recent.insert(key) {
            continue;
        }
        recent_requests.push(json!({
            "timestamp": row.0,
            "model": row.2,
            "provider": row.1,
            "promptTokens": prompt,
            "completionTokens": completion,
            "cachedTokens": tokens.get("cached_tokens").or_else(|| tokens.get("cache_read_input_tokens")).and_then(Value::as_u64).unwrap_or_default(),
            "status": row.9
        }));
        if recent_requests.len() == 20 {
            break;
        }
    }
    stats["recentRequests"] = json!(recent_requests);
    for row in &rows {
        let tokens: Value = serde_json::from_str(&row.10).unwrap_or_else(|_| json!({}));
        let cached = tokens
            .get("cached_tokens")
            .or_else(|| tokens.get("cache_read_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default();
        stats["totalRequests"] = json!(stats["totalRequests"].as_u64().unwrap_or_default() + 1);
        stats["totalPromptTokens"] =
            json!(stats["totalPromptTokens"].as_u64().unwrap_or_default() + row.6);
        stats["totalCompletionTokens"] =
            json!(stats["totalCompletionTokens"].as_u64().unwrap_or_default() + row.7);
        stats["totalCachedTokens"] =
            json!(stats["totalCachedTokens"].as_u64().unwrap_or_default() + cached);
        stats["totalCost"] = json!(stats["totalCost"].as_f64().unwrap_or_default() + row.8);
        let model_key = format!("{} ({})", row.2, row.1);
        let api_key = row.4.as_deref().map(mask_usage_api_key);
        let api_key_key = api_key
            .as_deref()
            .map(|value| format!("{value}|{}|{}", row.2, row.1))
            .unwrap_or_else(|| "local-no-key".to_owned());
        let endpoint = row.5.as_deref().unwrap_or("Unknown");
        let endpoint_key = format!("{endpoint}|{}|{}", row.2, row.1);
        add_usage_counter(
            &mut stats["byProvider"],
            &row.1,
            row.6,
            row.7,
            cached,
            row.8,
            json!({}),
        );
        add_usage_counter(
            &mut stats["byModel"],
            &model_key,
            row.6,
            row.7,
            cached,
            row.8,
            json!({
                "rawModel": row.2, "provider": row.1, "lastUsed": row.0
            }),
        );
        if let Some(connection_id) = row.3.as_deref() {
            add_usage_counter(
                &mut stats["byAccount"],
                connection_id,
                row.6,
                row.7,
                cached,
                row.8,
                json!({
                    "rawModel": row.2, "provider": row.1, "connectionId": connection_id,
                    "accountName": connection_id, "lastUsed": row.0
                }),
            );
        }
        add_usage_counter(
            &mut stats["byApiKey"],
            &api_key_key,
            row.6,
            row.7,
            cached,
            row.8,
            json!({
                "rawModel": row.2, "provider": row.1, "apiKeyMasked": api_key,
                "keyName": api_key.as_deref().unwrap_or("Local (No API Key)"),
                "apiKeyKey": api_key.as_deref().unwrap_or("local-no-key"), "lastUsed": row.0
            }),
        );
        add_usage_counter(
            &mut stats["byEndpoint"],
            &endpoint_key,
            row.6,
            row.7,
            cached,
            row.8,
            json!({
                "endpoint": endpoint, "rawModel": row.2, "provider": row.1, "lastUsed": row.0
            }),
        );
    }
    Json(stats).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_provider_names_prefer_nodes_then_catalog_then_id() {
        assert_eq!(
            usage_provider_name("custom-node-id", Some("My Node")),
            "My Node"
        );
        assert_eq!(
            usage_provider_name("openai", None),
            super::super::admin::provider_definition("openai")
                .unwrap()
                .display_name
        );
        assert_eq!(
            usage_provider_name("unknown-provider", None),
            "unknown-provider"
        );
    }
}
