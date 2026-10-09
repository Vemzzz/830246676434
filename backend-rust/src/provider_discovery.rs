use axum::{
    Json,
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use url::Url;

const KILO_MODELS_URL: &str = "https://api.kilo.ai/api/gateway/models";
const KILO_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const NVIDIA_FREE_MODELS: &[(&str, &str)] = &[
    ("minimaxai/minimax-m2.7", "MiniMax M2.7"),
    ("minimaxai/minimax-m3", "MiniMax M3"),
    ("z-ai/glm-5.1", "GLM 5.1"),
    ("z-ai/glm-5.2", "GLM 5.2"),
    ("deepseek-ai/deepseek-v4-pro", "DeepSeek V4 Pro"),
    ("deepseek-ai/deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("moonshotai/kimi-k2.6", "Kimi K2.6"),
    ("nvidia/nemotron-3-ultra-550b-a55b", "Nemotron 3 Ultra"),
    ("nvidia/nv-embedqa-e5-v5", "NV EmbedQA E5 v5"),
];

#[derive(Default)]
struct KiloCache {
    models: Option<Vec<Value>>,
    fetched_at: Option<Instant>,
}

fn kilo_cache() -> &'static Mutex<KiloCache> {
    static CACHE: OnceLock<Mutex<KiloCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(KiloCache::default()))
}

fn response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn ipv4_blocked(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 0
        || a == 10
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 100 && (64..=127).contains(&b))
        || a >= 224
}

fn ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ipv4_blocked(ip),
        IpAddr::V6(ip) => {
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.segments()[0] & 0xffc0 == 0xfec0
                || ip.to_ipv4_mapped().is_some()
        }
    }
}

async fn checked_public_url(raw_url: &str) -> Result<(Url, Vec<SocketAddr>), &'static str> {
    let url = Url::parse(raw_url).map_err(|_| "Blocked URL: unsupported target")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Blocked URL: unsupported target");
    }
    let host = url.host_str().expect("validated URL has a host");
    let normalized_host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if matches!(
        normalized_host.as_str(),
        "localhost" | "ip6-localhost" | "ip6-loopback"
    ) || [".internal", ".local", ".localhost"]
        .iter()
        .any(|suffix| normalized_host.ends_with(suffix))
    {
        return Err("Blocked URL: internal host");
    }
    if let Ok(ip) = normalized_host.parse::<IpAddr>() {
        if ip_blocked(ip) {
            return Err("Blocked URL: private IP");
        }
        let port = url.port_or_known_default().unwrap_or(80);
        return Ok((url, vec![SocketAddr::new(ip, port)]));
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "Blocked URL: DNS lookup failed")?
        .collect::<Vec<_>>();
    if addresses.is_empty() || addresses.iter().any(|address| ip_blocked(address.ip())) {
        return Err("Blocked URL: private IP");
    }
    Ok((url, addresses))
}

fn filter_suggested_models(kind: &str, raw: &Value) -> Option<Vec<Value>> {
    let models = raw.as_array()?;
    let mut filtered = match kind {
        "openrouter-free" => models
            .iter()
            .filter(|model| {
                let context_length = model.get("context_length");
                let numeric_context_length = context_length
                    .and_then(Value::as_f64)
                    .or_else(|| context_length.and_then(Value::as_str)?.parse().ok());
                model.pointer("/pricing/prompt").and_then(Value::as_str) == Some("0")
                    && model.pointer("/pricing/completion").and_then(Value::as_str) == Some("0")
                    && numeric_context_length.is_some_and(|context| context >= 200_000.0)
            })
            .map(|model| {
                let mut result = serde_json::Map::new();
                for (source, target) in [
                    ("id", "id"),
                    ("name", "name"),
                    ("context_length", "contextLength"),
                ] {
                    if let Some(value) = model.get(source) {
                        result.insert(target.to_owned(), value.clone());
                    }
                }
                Value::Object(result)
            })
            .collect::<Vec<_>>(),
        "opencode-free" => models
            .iter()
            .filter_map(|model| {
                let id = model.get("id")?.as_str()?;
                (id.ends_with("-free") || id == "big-pickle")
                    .then(|| json!({ "id": id, "name": id }))
            })
            .collect(),
        "opencode-go" => models
            .iter()
            .filter_map(|model| {
                let id = model.get("id")?.as_str()?;
                Some(json!({ "id": id, "name": id }))
            })
            .collect(),
        "mimo-free" => models
            .iter()
            .filter_map(|model| {
                let id = model.get("id").and_then(Value::as_str).unwrap_or("");
                let name = model.get("name").and_then(Value::as_str).unwrap_or(id);
                (id.starts_with("mimo") || name.to_lowercase().contains("mimo"))
                    .then(|| json!({ "id": id, "name": name }))
            })
            .collect(),
        "nvidia-free" => models
            .iter()
            .filter_map(|model| {
                let id = model.get("id")?.as_str()?;
                NVIDIA_FREE_MODELS
                    .iter()
                    .find(|(allowed_id, _)| *allowed_id == id)
                    .map(|(_, name)| json!({ "id": id, "name": name }))
            })
            .collect(),
        _ => return None,
    };
    if kind == "openrouter-free" {
        filtered.sort_by(|left, right| {
            let as_number = |value: &Value| {
                value
                    .as_f64()
                    .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
            };
            as_number(&right["contextLength"])
                .partial_cmp(&as_number(&left["contextLength"]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    Some(filtered)
}

fn filter_kilo_free_models(payload: &Value) -> Vec<Value> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| model.get("isFree") == Some(&Value::Bool(true)))
        .map(|model| {
            let mut value = serde_json::Map::new();
            for field in ["id", "name"] {
                if let Some(field_value) = model.get(field) {
                    value.insert(field.to_owned(), field_value.clone());
                }
            }
            value.insert("isFree".to_owned(), Value::Bool(true));
            let context_length = model
                .get("context_length")
                .filter(|value| {
                    !value.is_null() && value.as_bool() != Some(false) && value.as_str() != Some("")
                })
                .cloned()
                .unwrap_or_else(|| Value::from(0));
            value.insert("context_length".to_owned(), context_length);
            Value::Object(value)
        })
        .collect()
}

pub(super) async fn suggested_models(request: Request) -> Response {
    if request.method() != Method::GET {
        return response(
            StatusCode::METHOD_NOT_ALLOWED,
            json!({ "error": "Method not allowed" }),
        );
    }
    let query = request.uri().query().unwrap_or_default();
    let mut params = std::collections::HashMap::new();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        params
            .entry(name.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    let (Some(raw_url), Some(kind)) = (params.get("url"), params.get("type")) else {
        return response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "Missing url or type" }),
        );
    };
    if filter_suggested_models(kind, &Value::Array(Vec::new())).is_none() {
        return response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "Unknown filter type" }),
        );
    }
    let (url, addresses) = match checked_public_url(raw_url).await {
        Ok(target) => target,
        Err(_) => return response(StatusCode::OK, json!({ "data": [] })),
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(url.host_str().unwrap_or_default(), &addresses)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("[api/providers/suggested-models] HTTP client setup failed: {error}");
            return response(StatusCode::OK, json!({ "data": [] }));
        }
    };
    let upstream = match client.get(url).send().await {
        Ok(upstream) => upstream,
        Err(_) => return response(StatusCode::OK, json!({ "data": [] })),
    };
    if !upstream.status().is_success() {
        return response(StatusCode::OK, json!({ "data": [] }));
    }
    let data = match upstream.json::<Value>().await {
        Ok(value) => value,
        Err(_) => return response(StatusCode::OK, json!({ "data": [] })),
    };
    let raw = data
        .get("data")
        .filter(|value| !value.is_null())
        .or_else(|| data.get("models").filter(|value| !value.is_null()))
        .unwrap_or(&data);
    let models = filter_suggested_models(kind, raw).unwrap_or_default();
    response(StatusCode::OK, json!({ "data": models }))
}

pub(super) async fn kilo_free_models(request: Request) -> Response {
    if request.method() != Method::GET {
        return response(
            StatusCode::METHOD_NOT_ALLOWED,
            json!({ "error": "Method not allowed" }),
        );
    }
    {
        let cache = kilo_cache()
            .lock()
            .expect("Kilo cache lock is not poisoned");
        if let (Some(models), Some(fetched_at)) = (&cache.models, cache.fetched_at)
            && fetched_at.elapsed() < KILO_CACHE_TTL
        {
            return response(StatusCode::OK, json!({ "models": models, "cached": true }));
        }
    }
    let result = async {
        let upstream = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?
            .get(KILO_MODELS_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !upstream.status().is_success() {
            return Err(format!("Kilo API returned {}", upstream.status()));
        }
        let payload = upstream
            .json::<Value>()
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(filter_kilo_free_models(&payload))
    }
    .await;
    match result {
        Ok(models) => {
            let mut cache = kilo_cache()
                .lock()
                .expect("Kilo cache lock is not poisoned");
            cache.models = Some(models.clone());
            cache.fetched_at = Some(Instant::now());
            response(StatusCode::OK, json!({ "models": models, "cached": false }))
        }
        Err(error) => {
            eprintln!("[api/providers/kilo/free-models] upstream request failed: {error}");
            let cache = kilo_cache()
                .lock()
                .expect("Kilo cache lock is not poisoned");
            if let Some(models) = &cache.models {
                response(
                    StatusCode::OK,
                    json!({ "models": models, "cached": true, "warning": error }),
                )
            } else {
                response(
                    StatusCode::BAD_GATEWAY,
                    json!({ "models": [], "error": format!("Failed to fetch Kilo models: {error}") }),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{checked_public_url, filter_kilo_free_models, filter_suggested_models, ip_blocked};
    use serde_json::json;
    use std::net::IpAddr;

    #[test]
    fn suggested_model_filters_preserve_supported_shapes() {
        let models = json!([
            {
                "id": "a",
                "name": "A",
                "context_length": 200000,
                "pricing": { "prompt": "0", "completion": "0" }
            },
            {
                "id": "b",
                "name": "B",
                "context_length": 199999,
                "pricing": { "prompt": "0", "completion": "0" }
            }
        ]);
        assert_eq!(
            json!(filter_suggested_models("openrouter-free", &models).unwrap()),
            json!([{ "id": "a", "name": "A", "contextLength": 200000 }])
        );
        assert_eq!(
            json!(
                filter_suggested_models(
                    "nvidia-free",
                    &json!([{ "id": "z-ai/glm-5.1" }, { "id": "other" }])
                )
                .unwrap()
            ),
            json!([{ "id": "z-ai/glm-5.1", "name": "GLM 5.1" }])
        );
        assert_eq!(
            json!(filter_suggested_models("opencode-go", &json!([{ "id": "model" }, {}])).unwrap()),
            json!([{ "id": "model", "name": "model" }])
        );
    }

    #[test]
    fn ssrf_address_filter_blocks_private_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.20.1.2",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.2.3",
            "::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "fec0::1",
        ] {
            assert!(ip_blocked(ip.parse::<IpAddr>().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!ip_blocked(ip.parse::<IpAddr>().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn url_guard_rejects_private_literals_and_non_http_targets() {
        for url in [
            "http://127.0.0.1/models",
            "http://[::1]/models",
            "http://localhost/models",
            "file:///etc/passwd",
            "http://user:password@example.com/models",
        ] {
            assert!(checked_public_url(url).await.is_err(), "{url}");
        }
    }

    #[test]
    fn kilo_model_filter_matches_response_contract() {
        assert_eq!(
            filter_kilo_free_models(&json!({
                "data": [
                    { "id": "free", "name": "Free", "isFree": true, "context_length": 4096 },
                    { "id": "paid", "name": "Paid", "isFree": false },
                    { "id": "free-no-context", "isFree": true }
                ]
            })),
            vec![
                json!({
                    "id": "free",
                    "name": "Free",
                    "isFree": true,
                    "context_length": 4096
                }),
                json!({
                    "id": "free-no-context",
                    "isFree": true,
                    "context_length": 0
                })
            ]
        );
    }
}
