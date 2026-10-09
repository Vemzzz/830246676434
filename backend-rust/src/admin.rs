use super::*;

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

pub(super) fn hmac_sha256_hex(secret: &[u8], message: &[u8]) -> String {
    let mut key = [0_u8; 64];
    if secret.len() > key.len() {
        key[..32].copy_from_slice(&Sha256::digest(secret));
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    hex_encode(&outer.finalize())
}

fn read_or_create_api_key_secret() -> Result<Vec<u8>, String> {
    if let Ok(secret) = env::var("API_KEY_SECRET")
        && !secret.is_empty()
    {
        return Ok(secret.into_bytes());
    }
    let path = data_dir()?.join("auth").join("api-key-secret");
    match fs::read_to_string(&path) {
        Ok(secret) if !secret.trim().is_empty() => {
            return Ok(secret.trim().as_bytes().to_vec());
        }
        Ok(_) => return Err("API key secret file is empty".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }

    let mut secret = [0_u8; 32];
    getrandom::getrandom(&mut secret).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "API key secret path has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            let encoded = hex_encode(&secret);
            file.write_all(encoded.as_bytes())
                .map_err(|error| error.to_string())?;
            Ok(encoded.into_bytes())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let value = fs::read_to_string(path).map_err(|error| error.to_string())?;
            if value.trim().is_empty() {
                return Err("API key secret file is empty".to_owned());
            }
            Ok(value.trim().as_bytes().to_vec())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn consistent_machine_id() -> Result<String, String> {
    let raw = fs::read_to_string(data_dir()?.join("machine-id")).map_err(|_| {
        "Machine identity is not initialized on the shared data directory".to_owned()
    })?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Machine identity is empty".to_owned());
    }
    let salt = env::var("MACHINE_ID_SALT")
        .ok()
        .filter(|salt| !salt.is_empty())
        .unwrap_or_else(|| "endpoint-proxy-salt".to_owned());
    let digest = Sha256::digest(format!("{raw}{salt}").as_bytes());
    Ok(hex_encode(&digest)[..16].to_owned())
}

pub(super) fn generate_api_key(machine_id: &str, secret: &[u8]) -> Result<String, String> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut key_id = String::with_capacity(6);
    while key_id.len() < 6 {
        let mut random = [0_u8; 16];
        getrandom::getrandom(&mut random).map_err(|error| error.to_string())?;
        for byte in random {
            if byte < 252 {
                key_id.push(ALPHABET[(byte % 36) as usize] as char);
                if key_id.len() == 6 {
                    break;
                }
            }
        }
    }
    let crc = hmac_sha256_hex(secret, format!("{machine_id}{key_id}").as_bytes());
    Ok(format!("sk-{machine_id}-{key_id}-{}", &crc[..8]))
}

pub(super) fn api_key_limit(value: Option<&Value>, field: &str) -> Result<Option<i64>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() || value.as_str().is_some_and(|value| value.is_empty()) {
        return Ok(None);
    }
    let parsed = if let Some(text) = value.as_str() {
        text.parse::<f64>().ok()
    } else {
        value.as_f64()
    };
    let Some(parsed) = parsed.filter(|number| {
        number.is_finite()
            && number.fract() == 0.0
            && (1.0..=9_007_199_254_740_991.0).contains(number)
    }) else {
        return Err(format!(
            "{field} must be a positive whole number or left blank"
        ));
    };
    Ok(Some(parsed as i64))
}

fn normalize_ip_address(value: &str) -> Option<String> {
    let value = value.trim();
    if value.to_ascii_lowercase().starts_with("::ffff:") {
        return value[7..]
            .parse::<std::net::Ipv4Addr>()
            .ok()
            .map(|address| address.to_string());
    }
    value
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|address| address.to_string().to_ascii_lowercase())
}

pub(super) fn normalize_ip_whitelist(value: Option<&Value>) -> Result<Option<Vec<String>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() || value.as_str().is_some_and(|value| value.is_empty()) {
        return Ok(None);
    }
    let candidates: Vec<&str> = if let Some(values) = value.as_array() {
        values.iter().filter_map(Value::as_str).collect()
    } else if let Some(value) = value.as_str() {
        value.split([',', '\n']).collect()
    } else {
        return Err("ipWhitelist must be a list of IP addresses".to_owned());
    };
    let mut addresses = Vec::new();
    for candidate in candidates {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        let Some(address) = normalize_ip_address(candidate) else {
            return Err(format!("Invalid IP address: {candidate}"));
        };
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    Ok((!addresses.is_empty()).then_some(addresses))
}

fn permission_list_value(value: Option<&Value>) -> Value {
    value
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or(Value::Null)
}

pub(super) fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn api_key_value(connection: &Connection, id: &str) -> Result<Option<Value>, String> {
    let row = connection
        .query_row(
            "SELECT id, key, name, machineId, isActive, createdAt,
                    allowedProviders, allowedCombos, allowedKinds,
                    maxInputTokens, maxOutputTokens, requestsPerMinute, ipWhitelist
             FROM apiKeys WHERE id = ?1",
            params![id],
            |row| {
                let parse_list = |index| -> rusqlite::Result<Value> {
                    let raw: Option<String> = row.get(index)?;
                    Ok(raw
                        .as_deref()
                        .and_then(|value| serde_json::from_str(value).ok())
                        .unwrap_or(Value::Null))
                };
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "key": row.get::<_, String>(1)?,
                    "name": row.get::<_, Option<String>>(2)?,
                    "machineId": row.get::<_, Option<String>>(3)?,
                    "isActive": row.get::<_, Option<i64>>(4)?.unwrap_or_default() == 1,
                    "createdAt": row.get::<_, String>(5)?,
                    "allowedProviders": parse_list(6)?,
                    "allowedCombos": parse_list(7)?,
                    "allowedKinds": parse_list(8)?,
                    "maxInputTokens": row.get::<_, Option<i64>>(9)?,
                    "maxOutputTokens": row.get::<_, Option<i64>>(10)?,
                    "requestsPerMinute": row.get::<_, Option<i64>>(11)?,
                    "ipWhitelist": parse_list(12)?,
                }))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    Ok(row)
}

fn api_keys_list_response(connection: &Connection) -> Result<Value, String> {
    let mut statement = connection
        .prepare("SELECT id FROM apiKeys ORDER BY createdAt ASC")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut keys = Vec::new();
    for row in rows {
        if let Some(key) = api_key_value(connection, &row.map_err(|error| error.to_string())?)? {
            keys.push(key);
        }
    }
    Ok(json!({ "keys": keys }))
}

pub(super) async fn api_keys_collection(request: Request) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => match open_database_for_admin()
            .and_then(|connection| api_keys_list_response(&connection))
        {
            Ok(value) => Json(value).into_response(),
            Err(error) => {
                eprintln!("[api/keys] list failed: {error}");
                admin_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                )
            }
        },
        Method::POST => {
            let payload = match parse_admin_json(body).await {
                Ok(Value::Object(payload)) => Value::Object(payload),
                Ok(_) => {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "Request body must be a JSON object",
                    );
                }
                Err((status, message)) => return admin_error(status, message),
            };
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if name.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Name is required");
            }
            let input_limit = match api_key_limit(payload.get("maxInputTokens"), "maxInputTokens") {
                Ok(value) => value,
                Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
            };
            let output_limit =
                match api_key_limit(payload.get("maxOutputTokens"), "maxOutputTokens") {
                    Ok(value) => value,
                    Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
                };
            let requests_per_minute =
                match api_key_limit(payload.get("requestsPerMinute"), "requestsPerMinute") {
                    Ok(value) => value,
                    Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
                };
            let ip_whitelist = match normalize_ip_whitelist(payload.get("ipWhitelist")) {
                Ok(value) => value,
                Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
            };
            let machine_id = match consistent_machine_id() {
                Ok(machine_id) => machine_id,
                Err(error) => {
                    eprintln!("[api/keys] machine identity unavailable: {error}");
                    return admin_error(StatusCode::SERVICE_UNAVAILABLE, &error);
                }
            };
            let secret = match read_or_create_api_key_secret() {
                Ok(secret) => secret,
                Err(error) => {
                    eprintln!("[api/keys] key secret unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "API key secret unavailable",
                    );
                }
            };
            let key = match generate_api_key(&machine_id, &secret) {
                Ok(key) => key,
                Err(error) => {
                    eprintln!("[api/keys] key generation failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "API key could not be generated",
                    );
                }
            };
            let mut id_bytes = [0_u8; 16];
            if let Err(error) = getrandom::getrandom(&mut id_bytes) {
                eprintln!("[api/keys] id generation failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "API key could not be generated",
                );
            }
            let id = hex_encode(&id_bytes);
            let created_at = utc_timestamp();
            let ip_whitelist_json = ip_whitelist.as_ref().map(|value| json!(value).to_string());
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/keys] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let inserted = connection.execute(
                "INSERT INTO apiKeys
                 (id, key, name, machineId, isActive, createdAt, allowedProviders,
                  allowedCombos, allowedKinds, maxInputTokens, maxOutputTokens,
                  requestsPerMinute, ipWhitelist)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, NULL, NULL, NULL, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    key,
                    name,
                    machine_id,
                    created_at,
                    input_limit,
                    output_limit,
                    requests_per_minute,
                    ip_whitelist_json
                ],
            );
            if let Err(error) = inserted {
                eprintln!("[api/keys] create failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "API key could not be created",
                );
            }
            (
                StatusCode::CREATED,
                Json(json!({
                    "key": key,
                    "name": name,
                    "id": id,
                    "machineId": machine_id,
                    "maxInputTokens": input_limit,
                    "maxOutputTokens": output_limit,
                    "requestsPerMinute": requests_per_minute,
                    "ipWhitelist": ip_whitelist,
                })),
            )
                .into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

pub(super) async fn api_key_resource(request: Request, id: String) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    let mut connection = match open_database_for_admin() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("[api/keys/{id}] database unavailable: {error}");
            return admin_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable",
            );
        }
    };
    match parts.method {
        Method::GET => match api_key_value(&connection, &id) {
            Ok(Some(key)) => Json(json!({ "key": key })).into_response(),
            Ok(None) => admin_error(StatusCode::NOT_FOUND, "Key not found"),
            Err(error) => {
                eprintln!("[api/keys/{id}] read failed: {error}");
                admin_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                )
            }
        },
        Method::PUT => {
            let payload = match parse_admin_json(body).await {
                Ok(Value::Object(payload)) => Value::Object(payload),
                Ok(_) => {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "Request body must be a JSON object",
                    );
                }
                Err((status, message)) => return admin_error(status, message),
            };
            let existing = match api_key_value(&connection, &id) {
                Ok(Some(key)) => key,
                Ok(None) => return admin_error(StatusCode::NOT_FOUND, "Key not found"),
                Err(error) => {
                    eprintln!("[api/keys/{id}] read failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map_or_else(
                    || existing["name"].clone(),
                    |name| Value::String(name.to_owned()),
                );
            let is_active = match payload.get("isActive") {
                Some(value) => json_truthy(value),
                None => existing["isActive"].as_bool().unwrap_or(false),
            };
            let permission = |field: &str| {
                if payload.get(field).is_some() {
                    permission_list_value(payload.get(field))
                } else {
                    existing[field].clone()
                }
            };
            let allowed_providers = permission("allowedProviders");
            let allowed_combos = permission("allowedCombos");
            let allowed_kinds = permission("allowedKinds");
            let input_limit = if payload.get("maxInputTokens").is_some() {
                match api_key_limit(payload.get("maxInputTokens"), "maxInputTokens") {
                    Ok(value) => value,
                    Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
                }
            } else {
                existing["maxInputTokens"].as_i64()
            };
            let output_limit = if payload.get("maxOutputTokens").is_some() {
                match api_key_limit(payload.get("maxOutputTokens"), "maxOutputTokens") {
                    Ok(value) => value,
                    Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
                }
            } else {
                existing["maxOutputTokens"].as_i64()
            };
            let requests_per_minute = if payload.get("requestsPerMinute").is_some() {
                match api_key_limit(payload.get("requestsPerMinute"), "requestsPerMinute") {
                    Ok(value) => value,
                    Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
                }
            } else {
                existing["requestsPerMinute"].as_i64()
            };
            let ip_whitelist = if payload.get("ipWhitelist").is_some() {
                match normalize_ip_whitelist(payload.get("ipWhitelist")) {
                    Ok(value) => value,
                    Err(error) => return admin_error(StatusCode::BAD_REQUEST, &error),
                }
            } else {
                existing["ipWhitelist"].as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
            };
            let serialize_list = |value: &Value| value.as_array().map(|_| value.to_string());
            let ip_whitelist_json = ip_whitelist.as_ref().map(|value| json!(value).to_string());
            if let Err(error) = connection.execute(
                "UPDATE apiKeys SET name = ?1, isActive = ?2, allowedProviders = ?3,
                 allowedCombos = ?4, allowedKinds = ?5, maxInputTokens = ?6,
                 maxOutputTokens = ?7, requestsPerMinute = ?8, ipWhitelist = ?9
                 WHERE id = ?10",
                params![
                    name.as_str().or_else(|| existing["name"].as_str()),
                    is_active,
                    serialize_list(&allowed_providers),
                    serialize_list(&allowed_combos),
                    serialize_list(&allowed_kinds),
                    input_limit,
                    output_limit,
                    requests_per_minute,
                    ip_whitelist_json,
                    id,
                ],
            ) {
                eprintln!("[api/keys/{id}] update failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "API key could not be updated",
                );
            }
            match api_key_value(&connection, &id) {
                Ok(Some(key)) => Json(json!({ "key": key })).into_response(),
                Ok(None) => admin_error(StatusCode::NOT_FOUND, "Key not found"),
                Err(error) => {
                    eprintln!("[api/keys/{id}] updated key read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "API key could not be loaded",
                    )
                }
            }
        }
        Method::DELETE => {
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/keys/{id}] delete transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "API key could not be deleted",
                    );
                }
            };
            if let Err(error) = transaction.execute(
                "DELETE FROM apiKeyRateLimits WHERE apiKeyId = ?1",
                params![id],
            ) {
                eprintln!("[api/keys/{id}] rate-limit cleanup failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "API key could not be deleted",
                );
            }
            let deleted =
                match transaction.execute("DELETE FROM apiKeys WHERE id = ?1", params![id]) {
                    Ok(deleted) => deleted,
                    Err(error) => {
                        eprintln!("[api/keys/{id}] delete failed: {error}");
                        return admin_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "API key could not be deleted",
                        );
                    }
                };
            if let Err(error) = transaction.commit() {
                eprintln!("[api/keys/{id}] delete transaction commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "API key could not be deleted",
                );
            }
            if deleted == 0 {
                return admin_error(StatusCode::NOT_FOUND, "Key not found");
            }
            Json(json!({ "message": "Key deleted successfully" })).into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

pub(super) fn kv_get_all(connection: &Connection, scope: &str) -> Result<Value, String> {
    let mut statement = connection
        .prepare("SELECT key, value FROM kv WHERE scope = ?1")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut values = serde_json::Map::new();
    for row in rows {
        let (key, value) = row.map_err(|error| error.to_string())?;
        let value = serde_json::from_str(&value).map_err(|error| error.to_string())?;
        values.insert(key, value);
    }
    Ok(Value::Object(values))
}

pub(super) fn kv_set(
    connection: &Connection,
    scope: &str,
    key: &str,
    value: &Value,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO kv(scope, key, value) VALUES(?1, ?2, ?3)
             ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
            params![scope, key, value.to_string()],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn custom_model_key(provider_alias: &str, id: &str, kind: &str) -> String {
    format!("{provider_alias}|{id}|{kind}")
}

fn sanitize_custom_model(body: &Value, bulk: bool) -> Option<(String, Value)> {
    let provider_alias = body.get("providerAlias")?.as_str()?.to_owned();
    let id = body.get("id")?.as_str()?.to_owned();
    if provider_alias.is_empty() || id.is_empty() {
        return None;
    }
    let kind = body
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("llm");
    let mut model = serde_json::Map::new();
    model.insert("providerAlias".to_owned(), json!(provider_alias));
    model.insert("id".to_owned(), json!(id));
    model.insert("type".to_owned(), json!(kind));
    if bulk {
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(&id);
        model.insert("name".to_owned(), json!(name));
    } else if let Some(name) = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        model.insert("name".to_owned(), json!(name));
    }

    if !bulk && let Some(caps) = body.get("caps").and_then(Value::as_object) {
        let mut clean_caps = serde_json::Map::new();
        for key in ["vision", "reasoning"] {
            if let Some(value @ Value::Bool(_)) = caps.get(key) {
                clean_caps.insert(key.to_owned(), value.clone());
            }
        }
        if !clean_caps.is_empty() {
            model.insert("caps".to_owned(), Value::Object(clean_caps));
        }
    }
    if !bulk
        && kind == "stt"
        && body.get("transport").and_then(Value::as_str) == Some("gemini-live")
    {
        model.insert("transport".to_owned(), json!("gemini-live"));
    }
    Some((
        custom_model_key(&provider_alias, &id, kind),
        Value::Object(model),
    ))
}

pub(super) async fn model_aliases_route(request: Request) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => match open_database_for_admin()
            .and_then(|connection| kv_get_all(&connection, "modelAliases"))
        {
            Ok(aliases) => Json(json!({ "aliases": aliases })).into_response(),
            Err(error) => {
                eprintln!("[api/models/alias] read failed: {error}");
                admin_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                )
            }
        },
        Method::PUT => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            let model = payload.get("model").and_then(Value::as_str).unwrap_or("");
            let alias = payload.get("alias").and_then(Value::as_str).unwrap_or("");
            if model.is_empty() || alias.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Model and alias required");
            }
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/alias] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            if let Err(error) = kv_set(&connection, "modelAliases", alias, &json!(model)) {
                eprintln!("[api/models/alias] write failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Alias could not be saved",
                );
            }
            Json(json!({ "success": true, "model": model, "alias": alias })).into_response()
        }
        Method::DELETE => {
            let query = match parts.uri.query() {
                Some(query) => query,
                None => return admin_error(StatusCode::BAD_REQUEST, "Alias required"),
            };
            let alias = url::form_urlencoded::parse(query.as_bytes())
                .find_map(|(key, value)| (key == "alias").then(|| value.into_owned()));
            let Some(alias) = alias.filter(|alias| !alias.is_empty()) else {
                return admin_error(StatusCode::BAD_REQUEST, "Alias required");
            };
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/alias] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            if let Err(error) = connection.execute(
                "DELETE FROM kv WHERE scope = 'modelAliases' AND key = ?1",
                params![alias],
            ) {
                eprintln!("[api/models/alias] delete failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Alias could not be deleted",
                );
            }
            Json(json!({ "success": true })).into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

pub(super) async fn custom_models_route(request: Request) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => {
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/custom] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            match kv_get_all(&connection, "customModels") {
                Ok(Value::Object(models)) => {
                    Json(json!({ "models": models.into_values().collect::<Vec<_>>() }))
                        .into_response()
                }
                Ok(_) => Json(json!({ "models": [] })).into_response(),
                Err(error) => {
                    eprintln!("[api/models/custom] read failed: {error}");
                    admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    )
                }
            }
        }
        Method::POST => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/custom] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            if let Some(models) = payload.get("models").and_then(Value::as_array) {
                let transaction = match connection.unchecked_transaction() {
                    Ok(transaction) => transaction,
                    Err(error) => {
                        eprintln!("[api/models/custom] transaction failed: {error}");
                        return admin_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Models could not be added",
                        );
                    }
                };
                let mut added = 0;
                for item in models {
                    let Some((key, model)) = sanitize_custom_model(item, true) else {
                        continue;
                    };
                    let exists = transaction
                        .query_row(
                            "SELECT 1 FROM kv WHERE scope = 'customModels' AND key = ?1",
                            params![key],
                            |_| Ok(()),
                        )
                        .optional();
                    match exists {
                        Ok(Some(())) => continue,
                        Ok(None) => {}
                        Err(error) => {
                            eprintln!("[api/models/custom] duplicate lookup failed: {error}");
                            return admin_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Models could not be added",
                            );
                        }
                    }
                    if let Err(error) = kv_set(&transaction, "customModels", &key, &model) {
                        eprintln!("[api/models/custom] bulk insert failed: {error}");
                        return admin_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Models could not be added",
                        );
                    }
                    added += 1;
                }
                if let Err(error) = transaction.commit() {
                    eprintln!("[api/models/custom] transaction commit failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Models could not be added",
                    );
                }
                return Json(json!({ "success": true, "count": added })).into_response();
            }
            let Some((key, mut model)) = sanitize_custom_model(&payload, false) else {
                return admin_error(StatusCode::BAD_REQUEST, "providerAlias and id required");
            };
            let existing_raw = connection
                .query_row(
                    "SELECT value FROM kv WHERE scope = 'customModels' AND key = ?1",
                    params![key],
                    |row| row.get::<_, String>(0),
                )
                .optional();
            let previous = match existing_raw {
                Ok(Some(raw)) => serde_json::from_str::<Value>(&raw).ok(),
                Ok(None) => None,
                Err(error) => {
                    eprintln!("[api/models/custom] lookup failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let added = previous.is_none();
            if let Some(previous) = previous.and_then(|value| value.as_object().cloned())
                && let Some(next) = model.as_object_mut()
            {
                for (field, value) in previous {
                    next.entry(field).or_insert(value);
                }
            }
            if let Some(next) = model.as_object_mut() {
                let fallback_name = next.get("id").cloned().unwrap_or(Value::Null);
                next.entry("name").or_insert(fallback_name);
            }
            if let Err(error) = kv_set(&connection, "customModels", &key, &model) {
                eprintln!("[api/models/custom] upsert failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Model could not be added",
                );
            }
            Json(json!({ "success": true, "added": added })).into_response()
        }
        Method::DELETE => {
            let query = parts.uri.query().unwrap_or_default();
            let values: HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
            let provider_alias = values
                .get("providerAlias")
                .map(String::as_str)
                .unwrap_or("");
            let id = values.get("id").map(String::as_str).unwrap_or("");
            let kind = values.get("type").map(String::as_str).unwrap_or("llm");
            if provider_alias.is_empty() || id.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "providerAlias and id required");
            }
            let key = custom_model_key(provider_alias, id, kind);
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/custom] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            if let Err(error) = connection.execute(
                "DELETE FROM kv WHERE scope = 'customModels' AND key = ?1",
                params![key],
            ) {
                eprintln!("[api/models/custom] delete failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Model could not be deleted",
                );
            }
            Json(json!({ "success": true })).into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

pub(super) async fn disabled_models_route(request: Request) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => {
            let query = parts.uri.query().unwrap_or_default();
            let provider_alias = url::form_urlencoded::parse(query.as_bytes())
                .find_map(|(key, value)| (key == "providerAlias").then(|| value.into_owned()));
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/disabled] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            match kv_get_all(&connection, "disabledModels") {
                Ok(Value::Object(mut disabled)) => {
                    if let Some(provider_alias) = provider_alias {
                        Json(json!({ "ids": disabled.remove(&provider_alias).unwrap_or_else(|| json!([])) })).into_response()
                    } else {
                        Json(json!({ "disabled": disabled })).into_response()
                    }
                }
                Ok(_) => Json(json!({ "disabled": {} })).into_response(),
                Err(error) => {
                    eprintln!("[api/models/disabled] read failed: {error}");
                    admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    )
                }
            }
        }
        Method::POST => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            let provider_alias = payload
                .get("providerAlias")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(ids) = payload.get("ids").and_then(Value::as_array) else {
                return admin_error(StatusCode::BAD_REQUEST, "providerAlias and ids[] required");
            };
            if provider_alias.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "providerAlias and ids[] required");
            }
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/disabled] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let transaction = match connection.unchecked_transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/models/disabled] transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Models could not be disabled",
                    );
                }
            };
            let current = transaction
                .query_row(
                    "SELECT value FROM kv WHERE scope = 'disabledModels' AND key = ?1",
                    params![provider_alias],
                    |row| row.get::<_, String>(0),
                )
                .optional();
            let mut merged = match current {
                Ok(Some(value)) => serde_json::from_str::<Vec<Value>>(&value).unwrap_or_default(),
                Ok(None) => Vec::new(),
                Err(error) => {
                    eprintln!("[api/models/disabled] lookup failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Models could not be disabled",
                    );
                }
            };
            for id in ids {
                if !merged.contains(id) {
                    merged.push(id.clone());
                }
            }
            if let Err(error) = kv_set(
                &transaction,
                "disabledModels",
                provider_alias,
                &json!(merged),
            ) {
                eprintln!("[api/models/disabled] write failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Models could not be disabled",
                );
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/models/disabled] commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Models could not be disabled",
                );
            }
            Json(json!({ "success": true })).into_response()
        }
        Method::DELETE => {
            let query = parts.uri.query().unwrap_or_default();
            let values: HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
            let provider_alias = values
                .get("providerAlias")
                .map(String::as_str)
                .unwrap_or("");
            if provider_alias.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "providerAlias required");
            }
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/models/disabled] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let Some(id) = values.get("id").filter(|id| !id.is_empty()) else {
                if let Err(error) = connection.execute(
                    "DELETE FROM kv WHERE scope = 'disabledModels' AND key = ?1",
                    params![provider_alias],
                ) {
                    eprintln!("[api/models/disabled] clear failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Models could not be enabled",
                    );
                }
                return Json(json!({ "success": true })).into_response();
            };
            let transaction = match connection.unchecked_transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/models/disabled] transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Models could not be enabled",
                    );
                }
            };
            let current = transaction
                .query_row(
                    "SELECT value FROM kv WHERE scope = 'disabledModels' AND key = ?1",
                    params![provider_alias],
                    |row| row.get::<_, String>(0),
                )
                .optional();
            let mut ids = match current {
                Ok(Some(value)) => serde_json::from_str::<Vec<Value>>(&value).unwrap_or_default(),
                Ok(None) => Vec::new(),
                Err(error) => {
                    eprintln!("[api/models/disabled] lookup failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Models could not be enabled",
                    );
                }
            };
            ids.retain(|value| value != id);
            let result = if ids.is_empty() {
                transaction.execute(
                    "DELETE FROM kv WHERE scope = 'disabledModels' AND key = ?1",
                    params![provider_alias],
                )
            } else {
                transaction.execute(
                    "INSERT INTO kv(scope, key, value) VALUES('disabledModels', ?1, ?2)
                     ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value",
                    params![provider_alias, json!(ids).to_string()],
                )
            };
            if let Err(error) = result {
                eprintln!("[api/models/disabled] update failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Models could not be enabled",
                );
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/models/disabled] commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Models could not be enabled",
                );
            }
            Json(json!({ "success": true })).into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

fn generate_uuid_v4() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let value = hex_encode(&bytes);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &value[..8],
        &value[8..12],
        &value[12..16],
        &value[16..20],
        &value[20..]
    ))
}

pub(super) fn sanitize_provider_node_base_url(node_type: &str, base_url: &str) -> String {
    let mut base_url = base_url.to_owned();
    if node_type == "anthropic-compatible" {
        if base_url.ends_with('/') {
            base_url.pop();
        }
        if base_url.ends_with("/messages") {
            base_url.truncate(base_url.len() - "/messages".len());
        }
        if !base_url.ends_with("/v1") {
            base_url.push_str("/v1");
        }
    } else if node_type == "custom-embedding" {
        if base_url.ends_with('/') {
            base_url.pop();
        }
        if base_url.ends_with("/embeddings") {
            base_url.truncate(base_url.len() - "/embeddings".len());
        }
    }
    base_url
}

fn provider_node_value(connection: &Connection, id: &str) -> Result<Option<Value>, String> {
    let row = connection
        .query_row(
            "SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes WHERE id = ?1",
            params![id],
            |row| {
                let raw_data = row.get::<_, String>(3)?;
                let mut value = serde_json::from_str::<Value>(&raw_data)
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                value.insert("id".to_owned(), json!(row.get::<_, String>(0)?));
                value.insert(
                    "type".to_owned(),
                    row.get::<_, Option<String>>(1)?
                        .map_or(Value::Null, Value::String),
                );
                value.insert(
                    "name".to_owned(),
                    row.get::<_, Option<String>>(2)?
                        .map_or(Value::Null, Value::String),
                );
                value.insert("createdAt".to_owned(), json!(row.get::<_, String>(4)?));
                value.insert("updatedAt".to_owned(), json!(row.get::<_, String>(5)?));
                Ok(Value::Object(value))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    Ok(row)
}

fn provider_nodes_list_response(connection: &Connection) -> Result<Value, String> {
    let mut statement = connection
        .prepare("SELECT id FROM providerNodes")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut nodes = Vec::new();
    for row in rows {
        if let Some(node) =
            provider_node_value(connection, &row.map_err(|error| error.to_string())?)?
        {
            nodes.push(node);
        }
    }
    Ok(json!({ "nodes": nodes }))
}

pub(super) async fn provider_nodes_collection(request: Request) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => {
            match open_database_for_admin()
                .and_then(|connection| provider_nodes_list_response(&connection))
            {
                Ok(value) => Json(value).into_response(),
                Err(error) => {
                    eprintln!("[api/provider-nodes] list failed: {error}");
                    admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    )
                }
            }
        }
        Method::POST => {
            let payload = match parse_admin_json(body).await {
                Ok(Value::Object(payload)) => Value::Object(payload),
                Ok(_) => {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "Request body must be a JSON object",
                    );
                }
                Err((status, message)) => return admin_error(status, message),
            };
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if name.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Name is required");
            }
            let prefix = payload
                .get("prefix")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if prefix.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Prefix is required");
            }
            let node_type = payload
                .get("type")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("openai-compatible");
            let api_type = payload.get("apiType").and_then(Value::as_str);
            let (id_prefix, default_base_url) = match node_type {
                "openai-compatible" => {
                    let Some(api_type) =
                        api_type.filter(|value| matches!(*value, "chat" | "responses"))
                    else {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "Invalid OpenAI compatible API type",
                        );
                    };
                    (
                        format!("openai-compatible-{api_type}-"),
                        "https://api.openai.com/v1",
                    )
                }
                "anthropic-compatible" => (
                    "anthropic-compatible-".to_owned(),
                    "https://api.anthropic.com/v1",
                ),
                "custom-embedding" => ("custom-embedding-".to_owned(), "https://api.openai.com/v1"),
                _ => return admin_error(StatusCode::BAD_REQUEST, "Invalid provider node type"),
            };
            let raw_base_url = match payload.get("baseUrl") {
                None | Some(Value::Null) => default_base_url,
                Some(Value::String(value)) if value.is_empty() => default_base_url,
                Some(Value::String(value)) => value.as_str(),
                Some(_) => {
                    return admin_error(StatusCode::BAD_REQUEST, "Base URL must be a string");
                }
            };
            let base_url = sanitize_provider_node_base_url(node_type, raw_base_url.trim());
            let uuid = match generate_uuid_v4() {
                Ok(uuid) => uuid,
                Err(error) => {
                    eprintln!("[api/provider-nodes] id generation failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node could not be created",
                    );
                }
            };
            let id = format!("{id_prefix}{uuid}");
            let now = utc_timestamp();
            let mut data = serde_json::Map::new();
            data.insert("prefix".to_owned(), json!(prefix));
            data.insert("baseUrl".to_owned(), json!(base_url));
            if node_type == "openai-compatible" {
                data.insert("apiType".to_owned(), json!(api_type));
            }
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/provider-nodes] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            if let Err(error) = connection.execute(
                "INSERT INTO providerNodes (id, type, name, data, createdAt, updatedAt)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, node_type, name, Value::Object(data).to_string(), now],
            ) {
                eprintln!("[api/provider-nodes] create failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Provider node could not be created",
                );
            }
            match provider_node_value(&connection, &id) {
                Ok(Some(node)) => {
                    (StatusCode::CREATED, Json(json!({ "node": node }))).into_response()
                }
                Ok(None) => admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Created provider node could not be loaded",
                ),
                Err(error) => {
                    eprintln!("[api/provider-nodes] created node read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Created provider node could not be loaded",
                    )
                }
            }
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

pub(super) async fn provider_node_resource(request: Request, id: String) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::PUT => {
            let payload = match parse_admin_json(body).await {
                Ok(Value::Object(payload)) => Value::Object(payload),
                Ok(_) => {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "Request body must be a JSON object",
                    );
                }
                Err((status, message)) => return admin_error(status, message),
            };
            let mut connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let current = match connection.query_row(
                "SELECT type, data FROM providerNodes WHERE id = ?1",
                params![id],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
            ) {
                Ok(current) => current,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return admin_error(StatusCode::NOT_FOUND, "Provider node not found");
                }
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] lookup failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let node_type = current.0.as_deref().unwrap_or("");
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if name.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Name is required");
            }
            let prefix = payload
                .get("prefix")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if prefix.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Prefix is required");
            }
            let api_type = payload.get("apiType").and_then(Value::as_str);
            if node_type == "openai-compatible" && !matches!(api_type, Some("chat" | "responses")) {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    "Invalid OpenAI compatible API type",
                );
            }
            let base_url = payload
                .get("baseUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if base_url.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Base URL is required");
            }
            let base_url = sanitize_provider_node_base_url(node_type, base_url);
            let mut node_data = serde_json::from_str::<Value>(&current.1)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            node_data.insert("prefix".to_owned(), json!(prefix));
            node_data.insert("baseUrl".to_owned(), json!(base_url));
            if node_type == "openai-compatible" {
                node_data.insert("apiType".to_owned(), json!(api_type));
            } else {
                node_data.remove("apiType");
            }
            let now = utc_timestamp();
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node could not be updated",
                    );
                }
            };
            if let Err(error) = transaction.execute(
                "UPDATE providerNodes SET name = ?1, data = ?2, updatedAt = ?3 WHERE id = ?4",
                params![name, Value::Object(node_data).to_string(), now, id],
            ) {
                eprintln!("[api/provider-nodes/{id}] update failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Provider node could not be updated",
                );
            }
            let mut statement = match transaction
                .prepare("SELECT id, data FROM providerConnections WHERE provider = ?1")
            {
                Ok(statement) => statement,
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] connection query failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node connections could not be updated",
                    );
                }
            };
            let rows = match statement.query_map(params![id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }) {
                Ok(rows) => rows,
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] connection query failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node connections could not be updated",
                    );
                }
            };
            let mut connection_rows = Vec::new();
            for row in rows {
                match row {
                    Ok(row) => connection_rows.push(row),
                    Err(error) => {
                        eprintln!("[api/provider-nodes/{id}] connection row failed: {error}");
                        return admin_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Provider node connections could not be updated",
                        );
                    }
                }
            }
            drop(statement);
            for (connection_id, raw_data) in connection_rows {
                let mut connection_data = serde_json::from_str::<Value>(&raw_data)
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                let provider_specific_data = connection_data
                    .entry("providerSpecificData".to_owned())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                let Some(provider_specific_data) = provider_specific_data.as_object_mut() else {
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node connections could not be updated",
                    );
                };
                provider_specific_data.insert("prefix".to_owned(), json!(prefix));
                provider_specific_data.insert("baseUrl".to_owned(), json!(base_url));
                provider_specific_data.insert("nodeName".to_owned(), json!(name));
                if node_type == "openai-compatible" {
                    provider_specific_data.insert("apiType".to_owned(), json!(api_type));
                } else {
                    provider_specific_data.remove("apiType");
                }
                if let Err(error) = transaction.execute(
                    "UPDATE providerConnections SET data = ?1, updatedAt = ?2 WHERE id = ?3",
                    params![
                        Value::Object(connection_data).to_string(),
                        now,
                        connection_id
                    ],
                ) {
                    eprintln!("[api/provider-nodes/{id}] connection update failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node connections could not be updated",
                    );
                }
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/provider-nodes/{id}] transaction commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Provider node could not be updated",
                );
            }
            match provider_node_value(&connection, &id) {
                Ok(Some(node)) => Json(json!({ "node": node })).into_response(),
                Ok(None) => admin_error(StatusCode::NOT_FOUND, "Provider node not found"),
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] updated node read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node could not be loaded",
                    )
                }
            }
        }
        Method::DELETE => {
            let mut connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            match provider_node_value(&connection, &id) {
                Ok(Some(_)) => {}
                Ok(None) => return admin_error(StatusCode::NOT_FOUND, "Provider node not found"),
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] lookup failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/provider-nodes/{id}] delete transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider node could not be deleted",
                    );
                }
            };
            if let Err(error) = transaction.execute(
                "DELETE FROM providerConnections WHERE provider = ?1",
                params![id],
            ) {
                eprintln!("[api/provider-nodes/{id}] connection delete failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Provider node could not be deleted",
                );
            }
            if let Err(error) =
                transaction.execute("DELETE FROM providerNodes WHERE id = ?1", params![id])
            {
                eprintln!("[api/provider-nodes/{id}] delete failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Provider node could not be deleted",
                );
            }
            if let Err(error) = transaction.commit() {
                eprintln!("[api/provider-nodes/{id}] delete commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Provider node could not be deleted",
                );
            }
            Json(json!({ "success": true })).into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

async fn validate_openai_like(
    base_url: &str,
    api_key: &str,
    model_id: Option<&str>,
) -> Result<bool, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())?;
    let models_url = format!("{}/models", base_url.trim_end_matches('/'));
    let models = client
        .get(&models_url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if models.status().is_success() {
        return Ok(true);
    }
    if matches!(models.status().as_u16(), 401 | 403) {
        return Ok(false);
    }
    let Some(model_id) = model_id.filter(|model| !model.trim().is_empty()) else {
        return Ok(false);
    };
    let chat = client
        .post(format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .body(
            json!({
                "model": model_id,
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1,
            })
            .to_string(),
        )
        .send()
        .await
        .map_err(|error| error.to_string())?;
    Ok(!matches!(chat.status().as_u16(), 401 | 403))
}

async fn validate_anthropic_like(
    base_url: &str,
    api_key: &str,
    model_id: Option<&str>,
) -> Result<bool, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())?;
    let normalized = base_url.trim().trim_end_matches('/');
    let normalized = if normalized.ends_with("/messages") {
        &normalized[..normalized.len() - "/messages".len()]
    } else {
        normalized
    };
    let normalized = if normalized.ends_with("/v1") {
        normalized.to_owned()
    } else {
        format!("{normalized}/v1")
    };
    let models_url = format!("{normalized}/models");
    let models = client
        .get(&models_url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if models.status().is_success() {
        return Ok(true);
    }
    if matches!(models.status().as_u16(), 401 | 403) {
        return Ok(false);
    }
    let Some(model_id) = model_id.filter(|model| !model.trim().is_empty()) else {
        return Ok(false);
    };
    let messages = client
        .post(format!("{normalized}/messages"))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .body(
            json!({
                "model": model_id,
                "messages": [{"role": "user", "content": "test"}],
                "max_tokens": 1,
            })
            .to_string(),
        )
        .send()
        .await
        .map_err(|error| error.to_string())?;
    Ok(!matches!(messages.status().as_u16(), 401 | 403))
}

async fn validate_embedding_like(
    base_url: &str,
    api_key: &str,
    model_id: &str,
) -> Result<bool, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())?;
    let normalized = base_url.trim().trim_end_matches('/');
    let models_url = format!("{normalized}/models");
    let models = client
        .get(&models_url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if models.status().is_success() {
        return Ok(true);
    }
    if matches!(models.status().as_u16(), 401 | 403) {
        return Ok(false);
    }
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return Ok(false);
    }
    let embeddings = client
        .post(format!("{normalized}/embeddings"))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .body(
            json!({
                "model": model_id,
                "input": "ping",
            })
            .to_string(),
        )
        .send()
        .await
        .map_err(|error| error.to_string())?;
    Ok(!matches!(embeddings.status().as_u16(), 401 | 403))
}

pub(super) async fn provider_node_validation_route(request: Request) -> Response {
    if request.method() != Method::POST {
        return admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    let payload = match parse_admin_json(request.into_body()).await {
        Ok(value) => value,
        Err((status, message)) => return admin_error(status, message),
    };
    let base_url = payload
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let api_key = payload
        .get("apiKey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model_id = payload.get("modelId").and_then(Value::as_str);
    let kind = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("openai-compatible");
    if base_url.is_empty() || api_key.is_empty() {
        return admin_error(StatusCode::BAD_REQUEST, "Base URL and API key required");
    }
    let valid = match kind {
        "custom-embedding" => {
            validate_embedding_like(base_url, api_key, model_id.unwrap_or("")).await
        }
        "anthropic-compatible" => validate_anthropic_like(base_url, api_key, model_id).await,
        _ => validate_openai_like(base_url, api_key, model_id).await,
    };
    match valid {
        Ok(valid) => Json(json!({
            "valid": valid,
            "error": valid.then_some(Value::Null).unwrap_or(Value::String("Invalid API key".to_owned()))
        }))
        .into_response(),
        Err(error) => admin_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(super) async fn provider_validation_route(request: Request) -> Response {
    if request.method() != Method::POST {
        return admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    let payload = match parse_admin_json(request.into_body()).await {
        Ok(value) => value,
        Err((status, message)) => return admin_error(status, message),
    };
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let api_key = payload
        .get("apiKey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if provider.is_empty() {
        return admin_error(StatusCode::BAD_REQUEST, "Provider and API key required");
    }
    let no_auth = payload
        .get("providerSpecificData")
        .and_then(Value::as_object)
        .and_then(|data| data.get("noAuth"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || payload
            .get("noAuth")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if api_key.is_empty() && !no_auth {
        return admin_error(StatusCode::BAD_REQUEST, "Provider and API key required");
    }
    let provider_specific = payload
        .get("providerSpecificData")
        .and_then(Value::as_object)
        .cloned();
    let base_url = provider_specific
        .as_ref()
        .and_then(|data| data.get("baseUrl").and_then(Value::as_str))
        .or_else(|| {
            provider_specific
                .as_ref()
                .and_then(|data| data.get("endpoint").and_then(Value::as_str))
        })
        .unwrap_or_default();

    let valid = if !base_url.is_empty() {
        match provider {
            "anthropic-compatible" | "anthropic" => {
                validate_anthropic_like(
                    base_url,
                    api_key,
                    payload.get("modelId").and_then(Value::as_str),
                )
                .await
            }
            _ if provider.contains("embedding") || provider.ends_with("-embedding") => {
                validate_embedding_like(
                    base_url,
                    api_key,
                    payload.get("modelId").and_then(Value::as_str).unwrap_or(""),
                )
                .await
            }
            _ => {
                validate_openai_like(
                    base_url,
                    api_key,
                    payload.get("modelId").and_then(Value::as_str),
                )
                .await
            }
        }
    } else if provider == "openai"
        || provider == "vercel-ai-gateway"
        || provider == "openrouter"
        || provider == "gemini"
        || provider == "deepseek"
        || provider == "groq"
        || provider == "qwen"
    {
        validate_openai_like("https://api.openai.com/v1", api_key, Some("gpt-4o-mini")).await
    } else if provider == "anthropic" {
        validate_anthropic_like(
            "https://api.anthropic.com",
            api_key,
            Some("claude-3-haiku-20240307"),
        )
        .await
    } else {
        Ok(true)
    };
    match valid {
        Ok(valid) => Json(json!({
            "valid": valid,
            "error": valid.then_some(Value::Null).unwrap_or(Value::String("Invalid API key".to_owned()))
        }))
        .into_response(),
        Err(error) => admin_error(StatusCode::BAD_REQUEST, &error),
    }
}

pub(super) fn provider_connection_value(
    connection: &Connection,
    id: &str,
) -> Result<Option<Value>, String> {
    let row = connection
        .query_row(
            "SELECT id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt FROM providerConnections WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    Ok(row.map(
        |(id, provider, auth_type, name, email, priority, active, raw_data, created, updated)| {
            let mut value = serde_json::from_str::<Value>(&raw_data)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            for secret in ["apiKey", "accessToken", "refreshToken", "idToken"] {
                value.remove(secret);
            }
            value.insert("id".to_owned(), Value::String(id));
            value.insert("provider".to_owned(), Value::String(provider.clone()));
            value.insert("authType".to_owned(), Value::String(auth_type));
            value.insert(
                "alias".to_owned(),
                model_catalog()
                    .providers
                    .iter()
                    .find(|entry| entry.id == provider)
                    .map(|entry| Value::String(entry.alias.clone()))
                    .unwrap_or(Value::Null),
            );
            value.insert("name".to_owned(), name.map_or(Value::Null, Value::String));
            value.insert("email".to_owned(), email.map_or(Value::Null, Value::String));
            value.insert(
                "priority".to_owned(),
                priority.map_or(Value::Null, Value::from),
            );
            value.insert("isActive".to_owned(), Value::Bool(active.unwrap_or(0) != 0));
            value.insert("createdAt".to_owned(), Value::String(created));
            value.insert("updatedAt".to_owned(), Value::String(updated));
            Value::Object(value)
        },
    ))
}

fn provider_list_response(connection: &Connection) -> Result<Value, String> {
    let mut statement = connection
        .prepare("SELECT id FROM providerConnections ORDER BY provider, priority, createdAt")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut connections = Vec::new();
    for row in rows {
        let id = row.map_err(|error| error.to_string())?;
        if let Some(value) = provider_connection_value(connection, &id)? {
            connections.push(value);
        }
    }

    let mut providers = Vec::new();
    let mut alias_map = serde_json::Map::new();
    for provider in &model_catalog().providers {
        providers.push(json!({
            "id": provider.id,
            "alias": provider.alias,
            "aliases": provider.aliases,
            "displayName": provider.display_name,
            "noAuth": provider.no_auth,
            "serviceKinds": provider.service_kinds,
        }));
        for alias in std::iter::once(&provider.alias).chain(provider.aliases.iter()) {
            if alias != &provider.id {
                alias_map.insert(alias.clone(), Value::String(provider.id.clone()));
            }
        }
    }
    Ok(json!({
        "connections": connections,
        "aliasMap": Value::Object(alias_map),
        "providers": providers,
    }))
}

pub(super) fn provider_definition(provider_id: &str) -> Option<&CatalogProvider> {
    let normalized = provider_id.trim();
    let slug = normalized
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    model_catalog().providers.iter().find(|provider| {
        provider.id == normalized
            || provider.alias == normalized
            || provider.aliases.iter().any(|alias| alias == normalized)
            || provider.id == slug
            || provider.alias == slug
            || provider.aliases.iter().any(|alias| alias == &slug)
            || provider.display_name.eq_ignore_ascii_case(normalized)
    })
}

pub(super) fn provider_node_definition(
    connection: &Connection,
    provider_id: &str,
) -> Result<Option<(String, String, Value)>, String> {
    connection
        .query_row(
            "SELECT type, name, data FROM providerNodes WHERE id = ?1",
            params![provider_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .map(|(kind, name, raw_data)| {
            serde_json::from_str::<Value>(&raw_data)
                .map(|data| (kind, name, data))
                .map_err(|error| error.to_string())
        })
        .transpose()
}

fn reorder_provider_priorities(connection: &mut Connection, provider: &str) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let mut statement = transaction
        .prepare("SELECT id FROM providerConnections WHERE provider = ?1 ORDER BY priority, updatedAt DESC")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![provider], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.map_err(|error| error.to_string())?);
    }
    drop(statement);
    for (index, id) in ids.iter().enumerate() {
        transaction
            .execute(
                "UPDATE providerConnections SET priority = ?1 WHERE id = ?2",
                params![index as i64 + 1, id],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

pub(super) async fn providers_collection(request: Request) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => {
            match open_database().and_then(|connection| provider_list_response(&connection)) {
                Ok(value) => Json(value).into_response(),
                Err(error) => {
                    eprintln!("[api/providers] database query failed: {error}");
                    admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    )
                }
            }
        }
        Method::POST => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            let mut connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/providers] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let provider_input = payload
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("");
            let catalog_provider = provider_definition(provider_input);
            let provider_node = if catalog_provider.is_none() {
                match provider_node_definition(&connection, provider_input.trim()) {
                    Ok(node) => node,
                    Err(error) => {
                        eprintln!("[api/providers] custom provider lookup failed: {error}");
                        return admin_error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Backend database unavailable",
                        );
                    }
                }
            } else {
                None
            };
            if catalog_provider.is_none() && provider_node.is_none() {
                return admin_error(StatusCode::BAD_REQUEST, "Invalid provider");
            }
            let provider_id = catalog_provider
                .map(|provider| provider.id.as_str())
                .unwrap_or_else(|| provider_input.trim());
            if let Some(provider) = catalog_provider
                && provider.category.as_deref() != Some("apikey")
                && !provider.auth_modes.iter().any(|mode| mode == "apikey")
                && !provider.no_auth
            {
                return admin_error(
                    StatusCode::NOT_IMPLEMENTED,
                    "This provider requires a Node-managed auth flow and is not supported by Rust CRUD",
                );
            }
            let api_key = payload.get("apiKey").and_then(Value::as_str).unwrap_or("");
            if api_key.is_empty() && provider_id != "ollama-local" {
                return admin_error(StatusCode::BAD_REQUEST, "API Key is required");
            }
            let fallback_name = catalog_provider
                .map(|provider| provider.display_name.as_str())
                .or_else(|| provider_node.as_ref().map(|(_, name, _)| name.as_str()))
                .unwrap_or(provider_id);
            let name = payload
                .get("name")
                .or_else(|| payload.get("displayName"))
                .and_then(Value::as_str)
                .unwrap_or(fallback_name)
                .trim();
            if name.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Name is required");
            }
            let priority = match payload.get("priority") {
                None | Some(Value::Null) => 1,
                Some(value) => match value.as_i64().filter(|priority| *priority > 0) {
                    Some(priority) => priority,
                    None => {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "priority must be a positive integer",
                        );
                    }
                },
            };
            let mut data = serde_json::Map::new();
            data.insert("apiKey".to_owned(), Value::String(api_key.to_owned()));
            data.insert(
                "testStatus".to_owned(),
                payload
                    .get("testStatus")
                    .cloned()
                    .unwrap_or_else(|| Value::String("unknown".to_owned())),
            );
            for field in ["globalPriority", "defaultModel"] {
                if let Some(value) = payload
                    .get(field)
                    .filter(|value| !value.is_null() && !value.as_str().is_some_and(str::is_empty))
                {
                    data.insert(field.to_owned(), value.clone());
                }
            }
            let provider_specific_data = match payload.get("providerSpecificData") {
                Some(Value::Object(data)) => data.clone(),
                Some(_) => {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "providerSpecificData must be an object",
                    );
                }
                None => serde_json::Map::new(),
            };
            let mut provider_specific_data = provider_specific_data;
            if provider_id == "ollama-local" {
                for field in ["baseUrl", "baseURL", "ollamaHostUrl"] {
                    if let Some(base_url) = payload.get(field).and_then(Value::as_str) {
                        let base_url = base_url.trim();
                        if !base_url.is_empty() {
                            provider_specific_data
                                .insert("baseUrl".to_owned(), Value::String(base_url.to_owned()));
                            break;
                        }
                    }
                }
            }
            if let Some((_, node_name, node_data)) = provider_node.as_ref() {
                for field in ["prefix", "apiType", "baseUrl"] {
                    if let Some(value) = node_data.get(field) {
                        provider_specific_data.insert(field.to_owned(), value.clone());
                    }
                }
                provider_specific_data
                    .insert("nodeName".to_owned(), Value::String(node_name.clone()));
            }
            let connection_proxy_enabled =
                payload.get("connectionProxyEnabled") == Some(&Value::Bool(true));
            let connection_proxy_url = payload
                .get("connectionProxyUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let connection_no_proxy = payload
                .get("connectionNoProxy")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if connection_proxy_enabled && connection_proxy_url.is_empty() {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    "Connection proxy URL is required when connection proxy is enabled",
                );
            }
            provider_specific_data.insert(
                "connectionProxyEnabled".to_owned(),
                Value::Bool(connection_proxy_enabled),
            );
            provider_specific_data.insert(
                "connectionProxyUrl".to_owned(),
                Value::String(connection_proxy_url.to_owned()),
            );
            provider_specific_data.insert(
                "connectionNoProxy".to_owned(),
                Value::String(connection_no_proxy.to_owned()),
            );
            let pool_id = match payload.get("proxyPoolId") {
                None | Some(Value::Null) => None,
                Some(Value::String(id)) => {
                    let id = id.trim();
                    (!id.is_empty() && id != "__none__").then_some(id)
                }
                Some(_) => {
                    return admin_error(StatusCode::BAD_REQUEST, "Invalid proxy pool id");
                }
            };
            if let Some(pool_id) = pool_id {
                let pool_exists = match connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM proxyPools WHERE id = ?1)",
                    params![pool_id],
                    |row| row.get::<_, bool>(0),
                ) {
                    Ok(exists) => exists,
                    Err(error) => {
                        eprintln!("[api/providers] proxy-pool validation failed: {error}");
                        return admin_error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Backend database unavailable",
                        );
                    }
                };
                if !pool_exists {
                    return admin_error(StatusCode::BAD_REQUEST, "Proxy pool not found");
                }
                provider_specific_data
                    .insert("proxyPoolId".to_owned(), Value::String(pool_id.to_owned()));
            }
            data.insert(
                "providerSpecificData".to_owned(),
                Value::Object(provider_specific_data),
            );
            let now = utc_timestamp();
            let transaction = match connection.transaction() {
                Ok(transaction) => transaction,
                Err(error) => {
                    eprintln!("[api/providers] create transaction failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider connection could not be created",
                    );
                }
            };
            let existing_id = match transaction
                .query_row(
                    "SELECT id FROM providerConnections WHERE provider = ?1 AND authType = 'apikey' AND name = ?2 ORDER BY rowid LIMIT 1",
                    params![provider_id, name],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            {
                Ok(id) => id,
                Err(error) => {
                    eprintln!("[api/providers] existing connection lookup failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Provider connection could not be created",
                    );
                }
            };
            let connection_id = if let Some(id) = existing_id {
                match transaction.execute(
                    "UPDATE providerConnections SET email = NULL, priority = ?1, isActive = 1, data = ?2, updatedAt = ?3 WHERE id = ?4",
                    params![priority, Value::Object(data).to_string(), now, id],
                ) {
                    Ok(_) => id,
                    Err(error) => {
                        eprintln!("[api/providers] existing connection update failed: {error}");
                        return admin_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Provider connection could not be created",
                        );
                    }
                }
            } else {
                if let Err(error) = transaction.execute(
                    "INSERT INTO providerConnections (id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt) VALUES (lower(hex(randomblob(16))), ?1, 'apikey', ?2, NULL, ?3, 1, ?4, ?5, ?5)",
                    params![provider_id, name, priority, Value::Object(data).to_string(), now],
                ) {
                    eprintln!("[api/providers] connection create failed: {error}");
                    let status = if is_sqlite_constraint(&error) {
                        StatusCode::CONFLICT
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR
                    };
                    return admin_error(status, "Provider connection could not be created");
                }
                match transaction.query_row(
                    "SELECT id FROM providerConnections WHERE provider = ?1 AND authType = 'apikey' AND name = ?2 ORDER BY rowid DESC LIMIT 1",
                    params![provider_id, name],
                    |row| row.get::<_, String>(0),
                ) {
                    Ok(id) => id,
                    Err(error) => {
                        eprintln!("[api/providers] created connection lookup failed: {error}");
                        return admin_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Provider connection was created but could not be loaded",
                        );
                    }
                }
            };
            if let Err(error) = transaction.commit() {
                eprintln!("[api/providers] create transaction commit failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Provider connection could not be created",
                );
            }
            if let Err(error) = reorder_provider_priorities(&mut connection, provider_id) {
                eprintln!("[api/providers] connection priority reorder failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to reorder connections",
                );
            }
            match provider_connection_value(&connection, &connection_id) {
                Ok(Some(value)) => {
                    (StatusCode::CREATED, Json(json!({"connection": value}))).into_response()
                }
                Ok(None) => admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Created connection not found",
                ),
                Err(error) => {
                    eprintln!("[api/providers] created connection read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to load created connection",
                    )
                }
            }
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

pub(super) async fn provider_resource(request: Request, id: String) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }

    let (parts, body) = request.into_parts();
    let mut connection = match open_database_for_admin() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("[api/providers/{id}] database unavailable: {error}");
            return admin_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable",
            );
        }
    };
    match parts.method {
        Method::GET => match provider_connection_value(&connection, &id) {
            Ok(Some(value)) => Json(json!({"connection": value})).into_response(),
            Ok(None) => admin_error(StatusCode::NOT_FOUND, "Connection not found"),
            Err(error) => {
                eprintln!("[api/providers/{id}] read failed: {error}");
                admin_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                )
            }
        },
        Method::PUT => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            let row = connection
                .query_row(
                    "SELECT provider, authType, name, priority, isActive, data FROM providerConnections WHERE id = ?1",
                    params![id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<i64>>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    },
                )
                .optional();
            let (provider, auth_type, old_name, old_priority, active, raw_data) = match row {
                Ok(Some(row)) => row,
                Ok(None) => return admin_error(StatusCode::NOT_FOUND, "Connection not found"),
                Err(error) => {
                    eprintln!("[api/providers/{id}] connection lookup failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let mut data = serde_json::from_str::<Value>(&raw_data)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            for field in [
                "globalPriority",
                "defaultModel",
                "testStatus",
                "lastError",
                "lastErrorAt",
                "projectId",
                "isProjectIdManual",
            ] {
                if let Some(value) = payload.get(field) {
                    data.insert(field.to_owned(), value.clone());
                }
            }
            if payload.get("testStatus").and_then(Value::as_str) == Some("active") {
                data.insert("lastError".to_owned(), Value::Null);
                data.insert("lastErrorAt".to_owned(), Value::Null);
                data.insert("errorCode".to_owned(), Value::Null);
                data.insert("rateLimitedUntil".to_owned(), Value::Null);
                data.insert("backoffLevel".to_owned(), Value::from(0));
                let model_lock_keys: Vec<_> = data
                    .keys()
                    .filter(|key| key.starts_with("modelLock_"))
                    .cloned()
                    .collect();
                for key in model_lock_keys {
                    data.insert(key, Value::Null);
                }
            }
            if auth_type == "apikey"
                && let Some(api_key) = payload.get("apiKey").and_then(Value::as_str)
                && !api_key.is_empty()
            {
                data.insert("apiKey".to_owned(), Value::String(api_key.to_owned()));
            }
            if let Some(incoming) = payload.get("providerSpecificData") {
                let incoming = match incoming {
                    Value::Object(incoming) => incoming.clone(),
                    Value::Null => serde_json::Map::new(),
                    _ => {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "providerSpecificData must be an object",
                        );
                    }
                };
                let existing = data
                    .get("providerSpecificData")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let mut merged = existing;
                merged.extend(incoming);
                data.insert("providerSpecificData".to_owned(), Value::Object(merged));
            }
            let has_proxy_config = [
                "connectionProxyEnabled",
                "connectionProxyUrl",
                "connectionNoProxy",
            ]
            .iter()
            .any(|field| payload.get(*field).is_some());
            if has_proxy_config {
                let enabled = payload.get("connectionProxyEnabled") == Some(&Value::Bool(true));
                let url = payload
                    .get("connectionProxyUrl")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let no_proxy = payload
                    .get("connectionNoProxy")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if enabled && url.is_empty() {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "Connection proxy URL is required when connection proxy is enabled",
                    );
                }
                let provider_specific_data = data
                    .entry("providerSpecificData".to_owned())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                let Some(provider_specific_data) = provider_specific_data.as_object_mut() else {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "providerSpecificData must be an object",
                    );
                };
                provider_specific_data
                    .insert("connectionProxyEnabled".to_owned(), Value::Bool(enabled));
                provider_specific_data.insert(
                    "connectionProxyUrl".to_owned(),
                    Value::String(url.to_owned()),
                );
                provider_specific_data.insert(
                    "connectionNoProxy".to_owned(),
                    Value::String(no_proxy.to_owned()),
                );
            }
            if payload.get("proxyPoolIds").is_some() || payload.get("proxyPoolId").is_some() {
                let (pool_id, pool_ids, rotation_strategy) = if let Some(pool_ids) =
                    payload.get("proxyPoolIds")
                {
                    let Some(pool_ids) = pool_ids.as_array() else {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "proxyPoolIds must be an array",
                        );
                    };
                    let mut unique_pool_ids = HashSet::new();
                    let mut normalized_pool_ids = Vec::new();
                    for value in pool_ids {
                        let Some(pool_id) = value.as_str() else {
                            return admin_error(
                                StatusCode::BAD_REQUEST,
                                "proxyPoolIds must contain only strings",
                            );
                        };
                        let pool_id = pool_id.trim();
                        if !pool_id.is_empty() && unique_pool_ids.insert(pool_id.to_owned()) {
                            normalized_pool_ids.push(pool_id.to_owned());
                        }
                    }
                    let strategy = payload
                        .get("proxyRotationStrategy")
                        .and_then(Value::as_str)
                        .unwrap_or("none")
                        .trim()
                        .to_ascii_lowercase();
                    if !["none", "fill-first", "round-robin", "random", "smart"]
                        .contains(&strategy.as_str())
                    {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "Invalid proxy rotation strategy",
                        );
                    }
                    (None, normalized_pool_ids, Some(strategy))
                } else {
                    let pool_id = match payload.get("proxyPoolId") {
                        Some(Value::String(id))
                            if !id.trim().is_empty() && id.trim() != "__none__" =>
                        {
                            Some(id.trim().to_owned())
                        }
                        Some(Value::Null | Value::String(_)) => None,
                        Some(_) => {
                            return admin_error(StatusCode::BAD_REQUEST, "Invalid proxy pool id");
                        }
                        None => None,
                    };
                    (pool_id, Vec::new(), None)
                };
                for pool_id in pool_id.iter().chain(pool_ids.iter()) {
                    let exists = match connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM proxyPools WHERE id = ?1)",
                        params![pool_id],
                        |row| row.get::<_, bool>(0),
                    ) {
                        Ok(exists) => exists,
                        Err(error) => {
                            eprintln!("[api/providers/{id}] proxy-pool validation failed: {error}");
                            return admin_error(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "Backend database unavailable",
                            );
                        }
                    };
                    if !exists {
                        return admin_error(StatusCode::BAD_REQUEST, "Proxy pool not found");
                    }
                }
                let provider_specific_data = data
                    .entry("providerSpecificData".to_owned())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                let Some(provider_specific_data) = provider_specific_data.as_object_mut() else {
                    return admin_error(
                        StatusCode::BAD_REQUEST,
                        "providerSpecificData must be an object",
                    );
                };
                provider_specific_data.remove("proxyPoolId");
                provider_specific_data.remove("proxyPoolIds");
                provider_specific_data.remove("proxyRotationStrategy");
                if let Some(pool_id) = pool_id {
                    provider_specific_data.insert("proxyPoolId".to_owned(), Value::String(pool_id));
                }
                if !pool_ids.is_empty() {
                    provider_specific_data.insert(
                        "proxyPoolIds".to_owned(),
                        Value::Array(pool_ids.into_iter().map(Value::String).collect()),
                    );
                    if let Some(strategy) = rotation_strategy {
                        provider_specific_data
                            .insert("proxyRotationStrategy".to_owned(), Value::String(strategy));
                    }
                }
            }
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_owned)
                .or(old_name);
            let priority = match payload.get("priority") {
                Some(value) => match value.as_i64().filter(|priority| *priority > 0) {
                    Some(priority) => Some(priority),
                    None => {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "priority must be a positive integer",
                        );
                    }
                },
                None => old_priority,
            };
            let active = match payload.get("isActive") {
                Some(Value::Bool(active)) => {
                    if *active {
                        1
                    } else {
                        0
                    }
                }
                Some(_) => {
                    return admin_error(StatusCode::BAD_REQUEST, "isActive must be a boolean");
                }
                None => active.unwrap_or(0),
            };
            let result = connection.execute(
                "UPDATE providerConnections SET name = ?1, priority = ?2, isActive = ?3, data = ?4, updatedAt = ?5 WHERE id = ?6",
                params![name, priority, active, Value::Object(data).to_string(), utc_timestamp(), id],
            );
            if let Err(error) = result {
                eprintln!("[api/providers/{id}] update failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to update connection",
                );
            }
            if payload.get("priority").is_some()
                && let Err(error) = reorder_provider_priorities(&mut connection, &provider)
            {
                eprintln!("[api/providers/{id}] priority reorder failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to reorder connections",
                );
            }
            match provider_connection_value(&connection, &id) {
                Ok(Some(value)) => Json(json!({"connection": value})).into_response(),
                Ok(None) => admin_error(StatusCode::NOT_FOUND, "Connection not found"),
                Err(error) => {
                    eprintln!("[api/providers/{id}] updated connection read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to load updated connection",
                    )
                }
            }
        }
        Method::DELETE => {
            let provider = connection.query_row(
                "SELECT provider FROM providerConnections WHERE id = ?1",
                params![id],
                |row| row.get::<_, String>(0),
            );
            let provider = match provider {
                Ok(provider) => provider,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return admin_error(StatusCode::NOT_FOUND, "Connection not found");
                }
                Err(error) => {
                    eprintln!("[api/providers/{id}] connection lookup failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            if let Err(error) =
                connection.execute("DELETE FROM providerConnections WHERE id = ?1", params![id])
            {
                eprintln!("[api/providers/{id}] delete failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to delete connection",
                );
            }
            if let Err(error) = reorder_provider_priorities(&mut connection, &provider) {
                eprintln!("[api/providers/{id}] priority reorder failed: {error}");
                return admin_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to reorder connections",
                );
            }
            Json(json!({"message": "Connection deleted successfully"})).into_response()
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

fn combo_value_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let models: String = row.get("models")?;
    let models = serde_json::from_str::<Value>(&models).unwrap_or_else(|_| json!([]));
    Ok(json!({
        "id": row.get::<_, String>("id")?,
        "name": row.get::<_, String>("name")?,
        "kind": row.get::<_, Option<String>>("kind")?,
        "models": models,
        "context_length": row.get::<_, Option<i64>>("context_length")?,
        "createdAt": row.get::<_, String>("createdAt")?,
        "updatedAt": row.get::<_, String>("updatedAt")?,
    }))
}

pub(super) fn combo_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub(super) fn combo_context_length(value: Option<&Value>) -> Result<Option<i64>, &'static str> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(value) => {
            let length = value
                .as_i64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<i64>().ok()));
            length
                .filter(|length| (1..=2_000_000).contains(length))
                .map(Some)
                .ok_or("context_length must be a positive integer not exceeding 2000000")
        }
    }
}

pub(super) fn utc_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let seconds = (milliseconds / 1_000) as i64;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds_of_day / 3_600,
        seconds_of_day % 3_600 / 60,
        seconds_of_day % 60,
        milliseconds % 1_000
    )
}

pub(super) fn load_combo(connection: &Connection, id: &str) -> Result<Option<Value>, String> {
    connection
        .query_row(
            "SELECT id, name, kind, models, context_length, createdAt, updatedAt FROM combos WHERE id = ?1",
            params![id],
            combo_value_from_row,
        )
        .optional()
        .map_err(|error| error.to_string())
}

pub(super) async fn combos_collection(request: Request) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    match parts.method {
        Method::GET => {
            let connection = match open_database() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/combos] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let mut statement = match connection.prepare(
                "SELECT id, name, kind, models, context_length, createdAt, updatedAt FROM combos ORDER BY createdAt ASC",
            ) {
                Ok(statement) => statement,
                Err(error) => {
                    eprintln!("[api/combos] query prepare failed: {error}");
                    return admin_error(StatusCode::SERVICE_UNAVAILABLE, "Backend database unavailable");
                }
            };
            let rows = match statement.query_map([], combo_value_from_row) {
                Ok(rows) => rows,
                Err(error) => {
                    eprintln!("[api/combos] query failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let mut combos = Vec::new();
            for row in rows {
                match row {
                    Ok(combo) => combos.push(combo),
                    Err(error) => {
                        eprintln!("[api/combos] row read failed: {error}");
                        return admin_error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Backend database unavailable",
                        );
                    }
                }
            }
            Json(json!({"combos": combos})).into_response()
        }
        Method::POST => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            let Some(name) = payload.get("name").and_then(Value::as_str) else {
                return admin_error(StatusCode::BAD_REQUEST, "Name is required");
            };
            if name.is_empty() {
                return admin_error(StatusCode::BAD_REQUEST, "Name is required");
            }
            if !combo_name_valid(name) {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    "Name can only contain letters, numbers, -, _ and .",
                );
            }
            let models = payload.get("models").cloned().unwrap_or_else(|| json!([]));
            if !models.is_array() {
                return admin_error(StatusCode::BAD_REQUEST, "models must be an array");
            }
            if payload
                .get("kind")
                .is_some_and(|kind| !kind.is_null() && !kind.is_string())
            {
                return admin_error(StatusCode::BAD_REQUEST, "kind must be a string or null");
            }
            let context_length = match combo_context_length(payload.get("context_length")) {
                Ok(value) => value,
                Err(error) => return admin_error(StatusCode::BAD_REQUEST, error),
            };
            let connection = match open_database_for_admin() {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("[api/combos] database unavailable: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            let now = utc_timestamp();
            let result = connection.execute(
                "INSERT INTO combos (id, name, kind, models, context_length, createdAt, updatedAt) VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    name,
                    payload
                        .get("kind")
                        .and_then(Value::as_str)
                        .filter(|kind| !kind.is_empty()),
                    models.to_string(),
                    context_length,
                    now
                ],
            );
            if let Err(error) = result {
                eprintln!("[api/combos] create failed: {error}");
                let status = if is_sqlite_constraint(&error) {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                let message = if status == StatusCode::BAD_REQUEST {
                    "Combo name already exists"
                } else {
                    "Failed to create combo"
                };
                return admin_error(status, message);
            }
            let id = match connection.query_row(
                "SELECT id FROM combos WHERE name = ?1",
                params![name],
                |row| row.get::<_, String>(0),
            ) {
                Ok(id) => id,
                Err(error) => {
                    eprintln!("[api/combos] created combo lookup failed: {error}");
                    return admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to load created combo",
                    );
                }
            };
            match load_combo(&connection, &id) {
                Ok(Some(combo)) => (StatusCode::CREATED, Json(combo)).into_response(),
                Ok(None) => {
                    admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Created combo not found")
                }
                Err(error) => {
                    eprintln!("[api/combos] created combo read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to load created combo",
                    )
                }
            }
        }
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

pub(super) async fn combo_resource(request: Request, id: String) -> Response {
    if let Err((status, message)) = require_dashboard_auth(&request) {
        return admin_error(status, message);
    }
    let (parts, body) = request.into_parts();
    let connection = match open_database_for_admin() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("[api/combos/{id}] database unavailable: {error}");
            return admin_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Backend database unavailable",
            );
        }
    };
    match parts.method {
        Method::GET => match load_combo(&connection, &id) {
            Ok(Some(combo)) => Json(combo).into_response(),
            Ok(None) => admin_error(StatusCode::NOT_FOUND, "Combo not found"),
            Err(error) => {
                eprintln!("[api/combos/{id}] read failed: {error}");
                admin_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Backend database unavailable",
                )
            }
        },
        Method::PUT => {
            let payload = match parse_admin_json(body).await {
                Ok(payload) => payload,
                Err((status, message)) => return admin_error(status, message),
            };
            let Some(existing) = (match load_combo(&connection, &id) {
                Ok(combo) => combo,
                Err(error) => {
                    eprintln!("[api/combos/{id}] read failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            }) else {
                return admin_error(StatusCode::NOT_FOUND, "Combo not found");
            };
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| existing["name"].as_str().unwrap_or(""));
            if !combo_name_valid(name) {
                return admin_error(
                    StatusCode::BAD_REQUEST,
                    "Name can only contain letters, numbers, -, _ and .",
                );
            }
            let name_exists = match connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM combos WHERE name = ?1 AND id <> ?2)",
                params![name, id],
                |row| row.get::<_, bool>(0),
            ) {
                Ok(exists) => exists,
                Err(error) => {
                    eprintln!("[api/combos/{id}] duplicate-name check failed: {error}");
                    return admin_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Backend database unavailable",
                    );
                }
            };
            if name_exists {
                return admin_error(StatusCode::BAD_REQUEST, "Combo name already exists");
            }
            if payload
                .get("models")
                .is_some_and(|models| !models.is_array())
            {
                return admin_error(StatusCode::BAD_REQUEST, "models must be an array");
            }
            let context_length = if payload.get("context_length").is_some() {
                match combo_context_length(payload.get("context_length")) {
                    Ok(value) => value,
                    Err(error) => return admin_error(StatusCode::BAD_REQUEST, error),
                }
            } else {
                existing["context_length"].as_i64()
            };
            let models = payload.get("models").unwrap_or(&existing["models"]);
            let kind = match payload.get("kind") {
                Some(Value::Null) => None,
                Some(value) => match value.as_str() {
                    Some(kind) => Some(kind),
                    None => {
                        return admin_error(
                            StatusCode::BAD_REQUEST,
                            "kind must be a string or null",
                        );
                    }
                },
                None => existing["kind"].as_str(),
            };
            let result = connection.execute(
                "UPDATE combos SET name = ?1, kind = ?2, models = ?3, context_length = ?4, updatedAt = ?5 WHERE id = ?6",
                params![name, kind, models.to_string(), context_length, utc_timestamp(), id],
            );
            if let Err(error) = result {
                eprintln!("[api/combos/{id}] update failed: {error}");
                return admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to update combo");
            }
            match load_combo(&connection, &id) {
                Ok(Some(combo)) => Json(combo).into_response(),
                Ok(None) => admin_error(StatusCode::NOT_FOUND, "Combo not found"),
                Err(error) => {
                    eprintln!("[api/combos/{id}] updated combo read failed: {error}");
                    admin_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to load updated combo",
                    )
                }
            }
        }
        Method::DELETE => match connection.execute("DELETE FROM combos WHERE id = ?1", params![id])
        {
            Ok(0) => admin_error(StatusCode::NOT_FOUND, "Combo not found"),
            Ok(_) => Json(json!({"success": true})).into_response(),
            Err(error) => {
                eprintln!("[api/combos/{id}] delete failed: {error}");
                admin_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete combo")
            }
        },
        _ => admin_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}
