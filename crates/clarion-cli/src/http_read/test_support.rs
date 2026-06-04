//! Shared test-only helpers used across `http_read` submodule test suites.

use axum::body::to_bytes;
use axum::http::header;
use axum::response::Response;

use super::auth::component_hmac_hex;
use super::*;

pub(crate) fn hmac_request(
    secret: &str,
    method: &str,
    path_and_query: &str,
    body: &[u8],
) -> axum::http::Request<axum::body::Body> {
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = component_hmac_hex(
        secret.as_bytes(),
        method,
        path_and_query,
        body,
        timestamp,
        &nonce,
    );
    axum::http::Request::builder()
        .method(method)
        .uri(path_and_query)
        .header("X-Loom-Component", format!("clarion:{signature}"))
        .header("X-Loom-Timestamp", timestamp.to_string())
        .header("X-Loom-Nonce", nonce)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_vec()))
        .expect("build request")
}

pub(crate) async fn json_body(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("response body is JSON")
}
