use super::*;

fn default_pricing() -> &'static Value {
    static DEFAULTS: OnceLock<Value> = OnceLock::new();
    DEFAULTS.get_or_init(|| {
        let catalog: Value = serde_json::from_str(PRICING_CATALOG_JSON)
            .expect("bundled pricing catalog must be valid JSON");
        catalog
            .get("providers")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
    })
}

fn user_pricing(connection: &Connection) -> Result<Value, String> {
    let mut statement = connection
        .prepare("SELECT key, value FROM kv WHERE scope = 'pricing'")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut pricing = serde_json::Map::new();
    for row in rows {
        let (provider, raw) = row.map_err(|error| error.to_string())?;
        let provider_pricing =
            serde_json::from_str(&raw).map_err(|error| format!("Invalid pricing data: {error}"))?;
        pricing.insert(provider, provider_pricing);
    }
    Ok(Value::Object(pricing))
}

fn merged_pricing(defaults: &Value, custom: &Value) -> Value {
    let mut merged = defaults.as_object().cloned().unwrap_or_default();
    if let Some(custom) = custom.as_object() {
        for (provider, models) in custom {
            let Some(custom_models) = models.as_object() else {
                merged.insert(provider.clone(), models.clone());
                continue;
            };
            let models = merged
                .entry(provider.clone())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            let Some(models) = models.as_object_mut() else {
                *models = Value::Object(serde_json::Map::new());
                continue;
            };
            for (model, custom_price) in custom_models {
                if let Some(custom_price) = custom_price.as_object() {
                    let base_price = models
                        .entry(model.clone())
                        .or_insert_with(|| Value::Object(serde_json::Map::new()));
                    if let Some(base_price) = base_price.as_object_mut() {
                        base_price.extend(custom_price.clone());
                    } else {
                        *base_price = Value::Object(custom_price.clone());
                    }
                } else {
                    models.insert(model.clone(), custom_price.clone());
                }
            }
        }
    }
    Value::Object(merged)
}

fn validate_pricing(payload: &Value) -> Result<(), String> {
    let Some(providers) = payload.as_object() else {
        return Err("Invalid pricing data format".to_owned());
    };
    const VALID_FIELDS: &[&str] = &["input", "output", "cached", "reasoning", "cache_creation"];
    for (provider, models) in providers {
        let Some(models) = models.as_object() else {
            return Err(format!("Invalid pricing for provider: {provider}"));
        };
        for (model, pricing) in models {
            let Some(fields) = pricing.as_object() else {
                return Err(format!("Invalid pricing for model: {provider}/{model}"));
            };
            for (field, value) in fields {
                if !VALID_FIELDS.contains(&field.as_str()) {
                    return Err(format!(
                        "Invalid pricing field: {field} for {provider}/{model}"
                    ));
                }
                if value.as_f64().is_none_or(|number| number < 0.0) {
                    return Err(format!(
                        "Invalid pricing value for {field} in {provider}/{model}: must be non-negative number"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn merge_provider_pricing(current: &Value, updates: &Value) -> Value {
    let mut merged = current.as_object().cloned().unwrap_or_default();
    if let Some(updates) = updates.as_object() {
        for (model, pricing) in updates {
            merged.insert(model.clone(), pricing.clone());
        }
    }
    Value::Object(merged)
}

pub(super) async fn route(request: Request) -> Response {
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => {
            let connection = match open_database() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/pricing] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            match user_pricing(&connection) {
                Ok(custom) => Json(merged_pricing(default_pricing(), &custom)).into_response(),
                Err(error) => {
                    eprintln!("[api/pricing] read failed: {error}");
                    admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch pricing")
                }
            }
        }
        Method::PATCH => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            if let Err(message) = validate_pricing(&payload) {
                return admin_error(StatusCode::BAD_REQUEST, &message);
            }
            let mut connection = match open_database_for_writes() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/pricing] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/pricing] update transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to update pricing",
                    );
                }
            };
            for (provider, updates) in payload.as_object().into_iter().flatten() {
                let current: Option<String> = match transaction
                    .query_row(
                        "SELECT value FROM kv WHERE scope = 'pricing' AND key = ?1",
                        params![provider],
                        |row| row.get(0),
                    )
                    .optional()
                {
                    Ok(current) => current,
                    Err(error) => {
                        eprintln!("[api/pricing] current pricing lookup failed: {error}");
                        return admin_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Failed to update pricing",
                        );
                    }
                };
                let current = match current {
                    Some(raw) => match serde_json::from_str::<Value>(&raw) {
                        Ok(value) => value,
                        Err(error) => {
                            eprintln!("[api/pricing] stored pricing is invalid: {error}");
                            return admin_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Failed to update pricing",
                            );
                        }
                    },
                    None => Value::Object(serde_json::Map::new()),
                };
                let value = merge_provider_pricing(&current, updates);
                if let Err(error) = transaction.execute(
                    "INSERT INTO kv(scope, key, value) VALUES('pricing', ?1, ?2)
                     ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
                    params![provider, value.to_string()],
                ) {
                    eprintln!("[api/pricing] update failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to update pricing",
                    );
                }
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/pricing] update commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to update pricing",
                );
            }
            match open_database().and_then(|connection| user_pricing(&connection)) {
                Ok(pricing) => Json(pricing).into_response(),
                Err(error) => {
                    eprintln!("[api/pricing] updated pricing read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to update pricing",
                    )
                }
            }
        }
        Method::DELETE => {
            let query = parts.uri.query().unwrap_or_default();
            let mut provider = None;
            let mut model = None;
            for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
                match key.as_ref() {
                    "provider" => provider = Some(value.into_owned()),
                    "model" => model = Some(value.into_owned()),
                    _ => {}
                }
            }
            let mut connection = match open_database_for_writes() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/pricing] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/pricing] reset transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to reset pricing",
                    );
                }
            };
            let result = match provider.as_deref().filter(|provider| !provider.is_empty()) {
                Some(provider) => match model.as_deref().filter(|model| !model.is_empty()) {
                    Some(model) => {
                        let current: Option<String> = match transaction
                            .query_row(
                                "SELECT value FROM kv WHERE scope = 'pricing' AND key = ?1",
                                params![provider],
                                |row| row.get(0),
                            )
                            .optional()
                        {
                            Ok(current) => current,
                            Err(error) => {
                                eprintln!("[api/pricing] reset lookup failed: {error}");
                                return admin_error(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "Failed to reset pricing",
                                );
                            }
                        };
                        match current {
                            Some(raw) => match serde_json::from_str::<Value>(&raw) {
                                Ok(mut pricing) => {
                                    if let Some(pricing) = pricing.as_object_mut() {
                                        pricing.remove(model);
                                    }
                                    if pricing.as_object().is_none_or(|pricing| pricing.is_empty())
                                    {
                                        transaction.execute(
                                            "DELETE FROM kv WHERE scope = 'pricing' AND key = ?1",
                                            params![provider],
                                        )
                                    } else {
                                        transaction.execute(
                                            "UPDATE kv SET value = ?1 WHERE scope = 'pricing' AND key = ?2",
                                            params![pricing.to_string(), provider],
                                        )
                                    }
                                }
                                Err(error) => {
                                    eprintln!("[api/pricing] stored pricing is invalid: {error}");
                                    return admin_error(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        "Failed to reset pricing",
                                    );
                                }
                            },
                            None => Ok(0),
                        }
                    }
                    None => transaction.execute(
                        "DELETE FROM kv WHERE scope = 'pricing' AND key = ?1",
                        params![provider],
                    ),
                },
                None => transaction.execute("DELETE FROM kv WHERE scope = 'pricing'", []),
            };
            if let Err(error) = result {
                eprintln!("[api/pricing] reset failed: {error}");
                return admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to reset pricing");
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/pricing] reset commit failed: {error}");
                return admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to reset pricing");
            }
            match open_database()
                .and_then(|connection| user_pricing(&connection))
                .map(|custom| merged_pricing(default_pricing(), &custom))
            {
                Ok(pricing) => Json(pricing).into_response(),
                Err(error) => {
                    eprintln!("[api/pricing] reset pricing reload failed: {error}");
                    admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to reset pricing")
                }
            }
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_invalid_pricing_shapes_and_values() {
        assert!(validate_pricing(&json!({"openai": {"m": {"input": 0.5}}})).is_ok());
        assert_eq!(
            validate_pricing(&json!({"openai": {"m": {"unknown": 1.0}}})),
            Err("Invalid pricing field: unknown for openai/m".to_owned())
        );
        assert!(validate_pricing(&json!({"openai": {"m": {"input": -1.0}}})).is_err());
        assert!(validate_pricing(&json!({"openai": {"m": {"input": "1"}}})).is_err());
        assert!(validate_pricing(&json!({"openai": []})).is_err());
    }

    #[test]
    fn pricing_merge_matches_node_provider_and_model_overrides() {
        let defaults = json!({
            "gh": {"gpt": {"input": 1.0, "output": 2.0}}
        });
        let custom = json!({
            "gh": {"gpt": {"input": 3.0}, "new": {"output": 4.0}},
            "custom": {"model": {"input": 5.0}}
        });
        assert_eq!(
            merged_pricing(&defaults, &custom),
            json!({
                "gh": {
                    "gpt": {"input": 3.0, "output": 2.0},
                    "new": {"output": 4.0}
                },
                "custom": {"model": {"input": 5.0}}
            })
        );
    }
}
