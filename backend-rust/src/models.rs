use axum::{
    Json,
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::collections::HashSet;

use crate::{CatalogModel, CatalogProvider, admin, model_catalog, open_database_for_admin};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const TTS_VOICES_PROVIDERS: &[&str] = &[
    "elevenlabs",
    "edge-tts",
    "deepgram",
    "inworld",
    "local-device",
];

fn query_param(request: &Request, name: &str) -> Option<String> {
    request
        .uri()
        .query()
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

fn model_kind(model: &CatalogModel) -> &str {
    match model.kind.as_deref().unwrap_or("llm") {
        "imageToText" => "llm",
        kind => kind,
    }
}

fn kind_endpoint(kind: &str) -> Option<&'static str> {
    match kind {
        "llm" | "imageToText" => Some("/v1/chat/completions"),
        "image" => Some("/v1/images/generations"),
        "tts" => Some("/v1/audio/speech"),
        "stt" => Some("/v1/audio/transcriptions"),
        "embedding" => Some("/v1/embeddings"),
        "webSearch" => Some("/v1/search"),
        "webFetch" => Some("/v1/fetch"),
        _ => None,
    }
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn build_model_info(alias: &str, provider: &CatalogProvider, model: &CatalogModel) -> Value {
    let kind = model_kind(model);
    let mut info = json!({
        "id": format!("{alias}/{}", model.id),
        "name": if model.name.is_empty() { &model.id } else { &model.name },
        "kind": kind,
        "owned_by": alias,
        "endpoint": kind_endpoint(kind),
    });
    if kind == "webSearch" {
        add_search_config(&mut info, provider);
    }
    let Some(object) = info.as_object_mut() else {
        return info;
    };
    for (field, value) in [
        ("params", model.params.as_ref()),
        (
            "capabilities",
            (!model.capabilities.is_null()).then_some(&model.capabilities),
        ),
        ("options", model.options.as_ref()),
        ("dimensions", model.dimensions.as_ref()),
        ("contextWindow", model.context_window.as_ref()),
    ] {
        if let Some(value) = value.filter(|value| js_truthy(value)) {
            object.insert(field.to_owned(), value.clone());
        }
    }
    if kind == "tts" && TTS_VOICES_PROVIDERS.contains(&provider.id.as_str()) {
        object.insert(
            "voicesUrl".to_owned(),
            Value::String(format!("/v1/audio/voices?provider={}", provider.id)),
        );
    }
    info
}

fn virtual_model_info(alias: &str, provider: &CatalogProvider, model_id: &str) -> Option<Value> {
    let provider_can_search = provider.search_config.is_some() || provider.has_search_via_chat;
    let (kind, name, params) = match model_id {
        "search" if provider_can_search => {
            let params = if provider.id == "exa" {
                json!([
                    "query",
                    "max_results",
                    "search_type",
                    "exa_options",
                    "exa_options.contents"
                ])
            } else {
                json!([
                    "query",
                    "max_results",
                    "country",
                    "language",
                    "time_range",
                    "domain_filter",
                    "search_type"
                ])
            };
            (
                "webSearch",
                format!("{} Search", provider.display_name),
                params,
            )
        }
        "fetch" if provider.has_fetch_config => (
            "webFetch",
            format!("{} Fetch", provider.display_name),
            json!(["url", "format", "max_characters"]),
        ),
        _ => return None,
    };
    let mut info = json!({
        "id": format!("{alias}/{model_id}"),
        "name": name,
        "kind": kind,
        "owned_by": alias,
        "endpoint": kind_endpoint(kind),
        "params": params,
    });
    if kind == "webSearch" {
        add_search_config(&mut info, provider);
    }
    Some(info)
}

fn add_search_config(info: &mut Value, provider: &CatalogProvider) {
    let (Some(config), Some(object)) = (provider.search_config.as_ref(), info.as_object_mut())
    else {
        return;
    };
    for (source, target) in [
        ("searchTypes", "searchTypes"),
        ("exaSearchTypes", "exaSearchTypes"),
        ("maxMaxResults", "maxResults"),
        ("requiredOptions", "required"),
    ] {
        if let Some(value) = config.get(source).filter(|value| js_truthy(value)) {
            object.insert(target.to_owned(), value.clone());
        }
    }
}

fn with_model_info_cors(mut response: Response) -> Response {
    response.headers_mut().insert(
        "access-control-allow-origin",
        "*".parse().expect("static CORS header is valid"),
    );
    response
}

pub(super) async fn model_info(request: Request) -> Response {
    if request.method() == axum::http::Method::OPTIONS {
        let response = Response::builder()
            .status(StatusCode::OK)
            .header("access-control-allow-methods", "GET, OPTIONS")
            .body(Body::empty())
            .expect("empty preflight response is valid");
        return with_model_info_cors(response);
    }
    if request.method() != axum::http::Method::GET {
        return with_model_info_cors(api_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "Method not allowed",
        ));
    }
    let Some(id) = query_param(&request, "id") else {
        return with_model_info_cors(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": "Missing required query param: id (e.g. ?id=openai/dall-e-3)",
                        "type": "invalid_request_error"
                    }
                })),
            )
                .into_response(),
        );
    };
    let requested_kind = query_param(&request, "kind");
    let Some((alias, model_id)) = id.split_once('/') else {
        return with_model_info_cors(
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Model not found: {id}"),
                        "type": "not_found"
                    }
                })),
            )
                .into_response(),
        );
    };
    let provider = model_catalog().providers.iter().find(|provider| {
        provider.id == alias
            || provider.alias == alias
            || provider.aliases.iter().any(|value| value == alias)
    });
    let Some(provider) = provider else {
        return with_model_info_cors(
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Model not found: {id}"),
                        "type": "not_found"
                    }
                })),
            )
                .into_response(),
        );
    };
    let models = if provider.all_models.is_empty() {
        &provider.models
    } else {
        &provider.all_models
    };
    let model = models.iter().find(|model| {
        model.id == model_id
            && requested_kind
                .as_deref()
                .is_none_or(|kind| model_kind(model) == kind)
    });
    let info = model
        .map(|model| build_model_info(alias, provider, model))
        .or_else(|| virtual_model_info(alias, provider, model_id));
    match info {
        Some(info) => with_model_info_cors(Json(info).into_response()),
        None => with_model_info_cors(
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("Model not found: {id}"),
                        "type": "not_found"
                    }
                })),
            )
                .into_response(),
        ),
    }
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn database_error(endpoint: &str, error: String) -> Response {
    eprintln!("[{endpoint}] database operation failed: {error}");
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to access model configuration",
    )
}

fn read_scope(connection: &Connection, scope: &str) -> Result<Value, String> {
    admin::kv_get_all(connection, scope)
}

pub(super) async fn api_models(request: Request) -> Response {
    match request.method() {
        &Method::GET => list_api_models(),
        &Method::PUT => update_model_alias(request).await,
        &Method::OPTIONS => Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .expect("empty preflight response is valid"),
        _ => api_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

fn list_api_models() -> Response {
    let connection = match open_database_for_admin() {
        Ok(connection) => connection,
        Err(error) => return database_error("api/models", error),
    };
    let aliases = match read_scope(&connection, "modelAliases") {
        Ok(aliases) => aliases,
        Err(error) => return database_error("api/models", error),
    };
    let disabled = match read_scope(&connection, "disabledModels") {
        Ok(disabled) => disabled,
        Err(error) => return database_error("api/models", error),
    };

    let mut models = Vec::new();
    for provider in &model_catalog().providers {
        for model in &provider.models {
            let provider_disabled = disabled
                .get(&provider.alias)
                .or_else(|| disabled.get(&provider.id))
                .and_then(Value::as_array);
            if provider_disabled
                .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(&model.id)))
            {
                continue;
            }

            let full_model = format!("{}/{}", provider.id, model.id);
            let routed_model = format!("{}/{}", provider.alias, model.id);
            let alias = aliases
                .get(&full_model)
                .and_then(Value::as_str)
                .unwrap_or(&model.id);
            let mut caps = Map::new();
            for key in [
                "vision",
                "search",
                "reasoning",
                "contextWindow",
                "maxOutput",
            ] {
                if let Some(value) = model.capabilities.get(key) {
                    caps.insert(key.to_owned(), value.clone());
                }
            }
            models.push(json!({
                "provider": provider.id,
                "model": model.id,
                "name": if model.name.is_empty() { &model.id } else { &model.name },
                "fullModel": full_model,
                "routedModel": routed_model,
                "alias": alias,
                "caps": caps,
            }));
        }
    }
    Json(json!({ "models": models })).into_response()
}

async fn update_model_alias(request: Request) -> Response {
    let bytes = match to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return api_error(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large"),
    };
    let payload: Value = match serde_json::from_slice(&bytes) {
        Ok(payload) => payload,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return api_error(StatusCode::BAD_REQUEST, "Model and alias required");
    };
    let Some(alias) = payload.get("alias").and_then(Value::as_str) else {
        return api_error(StatusCode::BAD_REQUEST, "Model and alias required");
    };
    if model.is_empty() || alias.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "Model and alias required");
    }

    let connection = match open_database_for_admin() {
        Ok(connection) => connection,
        Err(error) => return database_error("api/models", error),
    };
    let aliases = match read_scope(&connection, "modelAliases") {
        Ok(Value::Object(aliases)) => aliases,
        Ok(_) => Map::new(),
        Err(error) => return database_error("api/models", error),
    };
    if aliases
        .iter()
        .any(|(key, value)| key != model && value.as_str() == Some(alias))
    {
        return api_error(StatusCode::BAD_REQUEST, "Alias already in use");
    }
    if let Err(error) = admin::kv_set(
        &connection,
        "modelAliases",
        model,
        &Value::String(alias.to_owned()),
    ) {
        return database_error("api/models", error);
    }
    Json(json!({ "success": true, "model": model, "alias": alias })).into_response()
}

pub(super) async fn gemini_models(request: Request) -> Response {
    if request.method() == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .expect("empty preflight response is valid");
    }
    if request.method() != Method::GET {
        return api_error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }

    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for provider in &model_catalog().providers {
        let provider_models = if provider.all_models.is_empty() {
            &provider.models
        } else {
            &provider.all_models
        };
        for model in provider_models {
            let name = format!("models/{}/{}", provider.alias, model.id);
            add_gemini_model(
                &mut models,
                &mut seen,
                name,
                &provider.alias,
                &model.name,
                &model.id,
                false,
            );
            if provider.alias == "gemini" {
                add_gemini_model(
                    &mut models,
                    &mut seen,
                    format!("models/{}", model.id),
                    "Gemini",
                    &model.name,
                    &model.id,
                    true,
                );
            }
        }
    }
    Json(json!({ "models": models })).into_response()
}

fn add_gemini_model(
    models: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    name: String,
    provider: &str,
    display_name: &str,
    id: &str,
    gemini_native: bool,
) {
    if !seen.insert(name.clone()) {
        return;
    }
    let display_name = if display_name.is_empty() {
        id
    } else {
        display_name
    };
    models.push(json!({
        "name": name,
        "displayName": display_name,
        "description": format!("{provider} model: {display_name}"),
        "supportedGenerationMethods": if gemini_native {
            json!(["generateContent", "streamGenerateContent"])
        } else {
            json!(["generateContent"])
        },
        "inputTokenLimit": 128000,
        "outputTokenLimit": 8192,
    }));
}

#[cfg(test)]
mod tests {
    use super::{add_gemini_model, gemini_models, model_info};
    use crate::model_catalog;
    use axum::{
        body::{Body, to_bytes},
        extract::Request,
    };
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn gemini_catalog_entries_match_the_dashboard_response_shape() {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        add_gemini_model(
            &mut models,
            &mut seen,
            "models/gemini/gemini-2.5-pro".to_owned(),
            "gemini",
            "Gemini 2.5 Pro",
            "gemini-2.5-pro",
            false,
        );
        add_gemini_model(
            &mut models,
            &mut seen,
            "models/gemini-2.5-pro".to_owned(),
            "Gemini",
            "Gemini 2.5 Pro",
            "gemini-2.5-pro",
            true,
        );

        assert_eq!(models[0]["name"], "models/gemini/gemini-2.5-pro");
        assert_eq!(models[0]["description"], "gemini model: Gemini 2.5 Pro");
        assert_eq!(
            models[0]["supportedGenerationMethods"],
            json!(["generateContent"])
        );
        assert_eq!(
            models[1]["supportedGenerationMethods"],
            json!(["generateContent", "streamGenerateContent"])
        );
        assert_eq!(models[0]["inputTokenLimit"], 128000);
        assert_eq!(models[0]["outputTokenLimit"], 8192);
    }

    #[test]
    fn gemini_catalog_entries_deduplicate_names() {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        for display_name in ["First", "Duplicate"] {
            add_gemini_model(
                &mut models,
                &mut seen,
                "models/provider/model".to_owned(),
                "provider",
                display_name,
                "model",
                false,
            );
        }
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["displayName"], "First");
    }

    #[tokio::test]
    async fn gemini_endpoint_includes_the_generated_all_kinds_catalog() {
        let expected = model_catalog()
            .providers
            .iter()
            .map(|provider| provider.all_models.len())
            .sum::<usize>();
        assert!(
            model_catalog()
                .providers
                .iter()
                .any(|provider| provider.all_models.len() > provider.models.len())
        );

        let request = Request::builder()
            .method("GET")
            .body(Body::empty())
            .expect("test request is valid");
        let response = gemini_models(request).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .expect("catalog response fits the test limit");
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).expect("catalog response is valid JSON");
        assert!(body["models"].as_array().unwrap().len() >= expected);
        assert!(
            body["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model["name"] == "models/gemini/gemini-2.5-pro")
        );
    }

    #[tokio::test]
    async fn model_info_matches_catalog_metadata_and_alias() {
        let provider = model_catalog()
            .providers
            .iter()
            .find(|provider| provider.id == "openai")
            .expect("OpenAI provider must be in bundled catalog");
        let model = provider
            .all_models
            .iter()
            .find(|model| model.id == "gpt-4.1")
            .expect("gpt-4.1 metadata must be in bundled catalog");
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("id", &format!("{}/{}", provider.alias, model.id))
            .finish();
        let request = Request::builder()
            .uri(format!("/v1/models/info?{query}"))
            .body(Body::empty())
            .expect("model info request is valid");

        let response = model_info(request).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "*"
        );
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("model info response fits the limit");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("model info response is valid JSON");
        assert_eq!(body["id"], "openai/gpt-4.1");
        assert_eq!(body["kind"], "llm");
        assert_eq!(body["endpoint"], "/v1/chat/completions");
    }

    #[tokio::test]
    async fn model_info_supports_virtual_search_and_fetch_models() {
        for (id, expected_kind, expected_endpoint) in [
            ("openai/search", "webSearch", "/v1/search"),
            ("exa/search", "webSearch", "/v1/search"),
            ("exa/fetch", "webFetch", "/v1/fetch"),
        ] {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("id", id)
                .finish();
            let request = Request::builder()
                .uri(format!("/v1/models/info?{query}"))
                .body(Body::empty())
                .expect("virtual model request is valid");
            let response = model_info(request).await;
            assert_eq!(response.status(), axum::http::StatusCode::OK, "{id}");
            let body = to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("virtual model response fits the limit");
            let body: serde_json::Value =
                serde_json::from_slice(&body).expect("virtual model response is valid JSON");
            assert_eq!(body["kind"], expected_kind, "{id}");
            assert_eq!(body["endpoint"], expected_endpoint, "{id}");
            if id == "exa/search" {
                assert_eq!(body["searchTypes"], json!(["web", "news"]));
                assert_eq!(body["maxResults"], 100);
                assert_eq!(body["params"][0], "query");
            }
        }
    }

    #[tokio::test]
    async fn model_info_accepts_provider_aliases_in_model_ids() {
        let provider = model_catalog()
            .providers
            .iter()
            .find(|provider| !provider.aliases.is_empty() && !provider.all_models.is_empty())
            .expect("catalog must include an aliased provider with models");
        let alias = &provider.aliases[0];
        let model = &provider.all_models[0];
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("id", &format!("{alias}/{}", model.id))
            .finish();
        let request = Request::builder()
            .uri(format!("/v1/models/info?{query}"))
            .body(Body::empty())
            .expect("aliased model request is valid");

        let response = model_info(request).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("aliased model response fits the limit");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("aliased model response is valid JSON");
        assert_eq!(body["id"], format!("{alias}/{}", model.id));
        assert_eq!(body["owned_by"].as_str(), Some(alias.as_str()));
    }

    #[tokio::test]
    async fn model_info_preserves_missing_id_error_and_options_cors() {
        let request = Request::builder()
            .uri("/v1/models/info")
            .body(Body::empty())
            .expect("model info request is valid");
        let response = model_info(request).await;
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("error response fits the limit");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("error response is valid JSON");
        assert_eq!(body["error"]["type"], "invalid_request_error");

        let request = Request::builder()
            .method("OPTIONS")
            .uri("/v1/models/info")
            .body(Body::empty())
            .expect("preflight request is valid");
        let response = model_info(request).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-methods")
                .unwrap(),
            "GET, OPTIONS"
        );
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "*"
        );
    }
}
