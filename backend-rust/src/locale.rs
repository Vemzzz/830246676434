use axum::{
    body::{Body, to_bytes},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::{Value, json};

const SUPPORTED_LOCALES: &[&str] = &[
    "en", "vi", "zh-CN", "zh-TW", "ja", "pt-BR", "pt-PT", "ko", "es", "de", "fr", "he", "ar", "ru",
    "pl", "cs", "nl", "tr", "uk", "tl", "id", "th", "hi", "bn", "ur", "fa", "ro", "sv", "it", "el",
    "hu", "fi", "da", "no",
];

fn normalize_locale(locale: &str) -> Option<&str> {
    if SUPPORTED_LOCALES.contains(&locale) {
        Some(locale)
    } else {
        None
    }
}

pub async fn set_locale(request: axum::http::Request<Body>) -> Response {
    let bytes = match to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({ "error": "Failed to set locale" })),
            )
                .into_response();
        }
    };
    let body = match serde_json::from_slice::<Value>(&bytes) {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to set locale" })),
            )
                .into_response();
        }
    };
    let Some(locale) = body.get("locale").and_then(Value::as_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid locale" })),
        )
            .into_response();
    };
    let Some(locale) = normalize_locale(locale) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid locale" })),
        )
            .into_response();
    };

    let mut response = (
        StatusCode::OK,
        Json(json!({ "success": true, "locale": locale })),
    )
        .into_response();
    response.headers_mut().append(
        "set-cookie",
        format!("locale={locale}; Path=/; Max-Age=31536000")
            .parse()
            .expect("supported locale cookie is a valid header"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_configured_locales_without_rewriting() {
        assert_eq!(normalize_locale("id"), Some("id"));
        assert_eq!(normalize_locale("zh-CN"), Some("zh-CN"));
        assert_eq!(normalize_locale("zh"), None);
        assert_eq!(normalize_locale("en-US"), None);
    }

    #[tokio::test]
    async fn locale_endpoint_sets_cookie_and_rejects_unsupported_values() {
        let response = set_locale(
            axum::http::Request::builder()
                .body(Body::from(r#"{"locale":"id"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("set-cookie").unwrap(),
            "locale=id; Path=/; Max-Age=31536000"
        );

        let response = set_locale(
            axum::http::Request::builder()
                .body(Body::from(r#"{"locale":"xx"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
