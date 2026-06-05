//! Inbound identity / HMAC authentication middleware for the HTTP read API.
//!
//! Split out of `http_read.rs` (mechanical relocation; behaviour unchanged).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use loomweave_core::HttpErrorCode as ErrorCode;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use tower::BoxError;
use tower::load_shed;
use tower::timeout;

use super::errors::format_dyn_error_chain;
use super::{AppState, HTTP_BODY_LIMIT_BYTES, WARDLINE_BODY_LIMIT_BYTES, json_error};

type HmacSha256 = Hmac<Sha256>;
pub(crate) type SharedHmacReplayCache = Arc<Mutex<HmacReplayCache>>;

/// Wire-pinned HMAC freshness window.
///
/// Basis: local sibling HTTP calls should complete in milliseconds; five
/// minutes tolerates moderate clock skew without making a captured request
/// useful for long. Override: none, this is part of the federation auth wire
/// contract. Retune: successor ADR if sibling deployments demonstrate a wider
/// skew requirement.
const HMAC_FRESHNESS_WINDOW_SECONDS: i64 = 300;
const HMAC_NONCE_MAX_LEN: usize = 128;

#[derive(Debug, Default)]
pub(crate) struct HmacReplayCache {
    seen: HashMap<String, i64>,
}

pub(crate) fn new_hmac_replay_cache() -> SharedHmacReplayCache {
    Arc::new(Mutex::new(HmacReplayCache::default()))
}

impl HmacReplayCache {
    fn check_and_record(
        &mut self,
        nonce: &str,
        request_timestamp: i64,
        now_timestamp: i64,
    ) -> bool {
        let oldest_allowed = now_timestamp.saturating_sub(HMAC_FRESHNESS_WINDOW_SECONDS);
        self.seen.retain(|_, seen_at| *seen_at >= oldest_allowed);
        if request_timestamp < oldest_allowed
            || request_timestamp > now_timestamp.saturating_add(HMAC_FRESHNESS_WINDOW_SECONDS)
        {
            return false;
        }
        if self.seen.contains_key(nonce) {
            return false;
        }
        self.seen.insert(nonce.to_owned(), request_timestamp);
        true
    }
}

/// Enforce configured identity on protected routes. Prefer the Weft HMAC
/// identity when `identity_token_env` is configured; otherwise preserve the
/// legacy bearer-token path for existing deployments.
pub(crate) async fn require_http_identity(
    State(state): State<AppState>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    require_http_identity_with_limit(&state, HTTP_BODY_LIMIT_BYTES, request, next).await
}

/// Wardline-route identity guard. Identical to [`require_http_identity`] but
/// reads up to `WARDLINE_BODY_LIMIT_BYTES` when verifying the HMAC signature,
/// so a multi-MiB taint-store body is not rejected by the signature-read step
/// before the route's own larger body limit applies.
pub(crate) async fn require_http_identity_wardline(
    State(state): State<AppState>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    require_http_identity_with_limit(&state, WARDLINE_BODY_LIMIT_BYTES, request, next).await
}

pub(crate) async fn require_http_identity_with_limit(
    state: &AppState,
    body_limit: usize,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    if let Some(secret) = state.identity_secret.as_ref() {
        return require_hmac_identity(secret, &state.hmac_replay_cache, body_limit, request, next)
            .await;
    }
    let Some(expected) = state.auth_token.as_ref() else {
        return next.run(request).await;
    };
    let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let Some(presented) = presented else {
        return unauthenticated_response();
    };
    // Constant-time compare so a wrong-length-token client can't trivially
    // distinguish "header absent" from "token mismatch" via timing.
    if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return unauthenticated_response();
    }
    next.run(request).await
}

pub(crate) async fn require_hmac_identity(
    secret: &str,
    replay_cache: &SharedHmacReplayCache,
    body_limit: usize,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method.as_str().to_owned();
    let path_and_query = parts.uri.path_and_query().map_or_else(
        || parts.uri.path().to_owned(),
        |value| value.as_str().to_owned(),
    );
    let presented = parts
        .headers
        .get("x-weft-component")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("loomweave:"))
        .filter(|signature| !signature.is_empty())
        .map(str::to_owned);
    let Some(presented) = presented else {
        return unauthenticated_response();
    };
    let timestamp = parts
        .headers
        .get("x-weft-timestamp")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok());
    let Some(timestamp) = timestamp else {
        return unauthenticated_response();
    };
    let nonce = parts
        .headers
        .get("x-weft-nonce")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|nonce| !nonce.is_empty() && nonce.len() <= HMAC_NONCE_MAX_LEN)
        .map(str::to_owned);
    let Some(nonce) = nonce else {
        return unauthenticated_response();
    };
    let Ok(body_bytes) = to_bytes(body, body_limit).await else {
        // CI-02 fix: a body read failure here is not a path-validation
        // problem. The outer `RequestBodyLimitLayer` already rejects
        // oversized bodies with the framework's 413; reaching this branch
        // means a transport-layer IO failure or a body that could not be
        // collected. Surface as Internal (500) so federation clients
        // routing on `code` do not mis-classify it as a path defect.
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "request body could not be read",
        );
    };
    let expected = component_hmac_hex(
        secret.as_bytes(),
        &method,
        &path_and_query,
        &body_bytes,
        timestamp,
        &nonce,
    );
    if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return unauthenticated_response();
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let fresh_and_unseen = replay_cache
        .lock()
        .is_ok_and(|mut cache| cache.check_and_record(&nonce, timestamp, now));
    if !fresh_and_unseen {
        return unauthenticated_response();
    }
    next.run(Request::from_parts(parts, Body::from(body_bytes)))
        .await
}

pub(crate) fn unauthenticated_response() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthenticated,
        "authentication required",
    )
}

pub(crate) fn component_hmac_hex(
    secret: &[u8],
    method: &str,
    path_and_query: &str,
    body: &[u8],
    timestamp: i64,
    nonce: &str,
) -> String {
    hmac_sha256_hex(
        secret,
        canonical_hmac_message(method, path_and_query, body, timestamp, nonce).as_bytes(),
    )
}

pub(crate) fn canonical_hmac_message(
    method: &str,
    path_and_query: &str,
    body: &[u8],
    timestamp: i64,
    nonce: &str,
) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        method,
        path_and_query,
        hex_lower(&Sha256::digest(body)),
        timestamp,
        nonce
    )
}

pub(crate) fn hmac_sha256_hex(secret: &[u8], message: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any size");
    mac.update(message);
    hex_lower(&mac.finalize().into_bytes())
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

pub(crate) async fn handle_middleware_error(err: BoxError) -> Response {
    if err.is::<timeout::error::Elapsed>() {
        return json_error(
            StatusCode::REQUEST_TIMEOUT,
            ErrorCode::Internal,
            "HTTP request timed out",
        );
    }
    if err.is::<load_shed::error::Overloaded>() {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::StorageError,
            "HTTP read API is overloaded",
        );
    }
    // Refuse the wildcard: any middleware BoxError that is not enumerated above
    // is a programming defect, not a recoverable condition. We panic with the
    // full source chain in the payload; the outer `CatchPanicLayer` translates
    // the panic into the standard 500 INTERNAL envelope so clients still get a
    // structured response, while CI / tests surface the missing enumeration as
    // a hard failure rather than a silent 500.
    let error_chain = format_dyn_error_chain(&*err);
    panic!(
        "HTTP read API middleware produced an unhandled error type — enumerate it explicitly: {error_chain}"
    );
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::future::{Future, Pending, pending};
    use std::task::{Context, Poll};

    use axum::http::StatusCode;
    use axum::response::Response;
    use tower::limit::ConcurrencyLimitLayer;
    use tower::{BoxError, Service, ServiceBuilder, load_shed};

    use super::*;

    #[test]
    fn hmac_sha256_matches_known_vector() {
        let digest = hmac_sha256_hex(b"key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            digest,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn hmac_replay_cache_rejects_reused_and_stale_nonces() {
        let mut cache = HmacReplayCache::default();
        let now = 1_900_000_000;

        assert!(cache.check_and_record("nonce-1", now, now));
        assert!(
            !cache.check_and_record("nonce-1", now, now),
            "same nonce inside the freshness window must be rejected"
        );
        assert!(
            !cache.check_and_record("nonce-old", now - HMAC_FRESHNESS_WINDOW_SECONDS - 1, now,),
            "stale timestamps must be rejected"
        );
        assert!(
            !cache.check_and_record("nonce-future", now + HMAC_FRESHNESS_WINDOW_SECONDS + 1, now,),
            "far-future timestamps must be rejected"
        );
        assert!(
            cache.check_and_record(
                "nonce-1",
                now + HMAC_FRESHNESS_WINDOW_SECONDS + 1,
                now + HMAC_FRESHNESS_WINDOW_SECONDS + 1,
            ),
            "expired nonce entries should be pruned"
        );
    }

    #[test]
    fn load_shed_converts_concurrency_backpressure_to_overload_response() {
        #[derive(Clone)]
        struct PendingService;

        impl Service<()> for PendingService {
            type Response = ();
            type Error = BoxError;
            type Future = Pending<Result<(), BoxError>>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, _request: ()) -> Self::Future {
                pending()
            }
        }

        let mut service = ServiceBuilder::new()
            .layer(load_shed::LoadShedLayer::new())
            .layer(ConcurrencyLimitLayer::new(1))
            .service(PendingService);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert!(
            service.poll_ready(&mut cx).is_ready(),
            "first request should acquire the only concurrency permit"
        );
        let _held_permit = service.call(());

        assert!(
            service.poll_ready(&mut cx).is_ready(),
            "load-shed should stay ready when the concurrency limiter is saturated"
        );
        let mut overloaded = std::pin::pin!(service.call(()));
        let err = match Future::poll(overloaded.as_mut(), &mut cx) {
            Poll::Ready(Err(err)) => err,
            other => panic!("expected immediate overload error, got {other:?}"),
        };
        assert!(
            err.is::<load_shed::error::Overloaded>(),
            "expected load-shed overload error, got {err}"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let response = runtime.block_on(handle_middleware_error(err));
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    #[should_panic(expected = "unhandled error type")]
    fn handle_middleware_error_refuses_unenumerated_box_error() {
        #[derive(Debug)]
        struct UnknownInner;
        impl std::fmt::Display for UnknownInner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("inner unknown")
            }
        }
        impl StdError for UnknownInner {}

        #[derive(Debug)]
        struct UnknownMiddlewareError(UnknownInner);
        impl std::fmt::Display for UnknownMiddlewareError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("synthetic unknown middleware failure")
            }
        }
        impl StdError for UnknownMiddlewareError {
            fn source(&self) -> Option<&(dyn StdError + 'static)> {
                Some(&self.0)
            }
        }

        let err: BoxError = Box::new(UnknownMiddlewareError(UnknownInner));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(handle_middleware_error(err));
    }

    /// CI-02 fix: a body-read failure inside HMAC verification must not
    /// surface as `INVALID_PATH`. Federation clients switch on `code`; a
    /// transport/IO failure mis-routed as a path-validation defect would
    /// be a contract bug.
    #[test]
    fn hmac_middleware_body_read_failure_is_not_invalid_path() {
        use axum::Router;
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use axum::routing::post;
        use tower::ServiceExt;

        async fn never_called(_request: Request<Body>) -> Response {
            unreachable!("inner handler must not run when body read fails")
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let (status, body) = runtime.block_on(async {
            // Body that exceeds HTTP_BODY_LIMIT_BYTES so `to_bytes(body, HTTP_BODY_LIMIT_BYTES)`
            // returns Err with a LengthLimitError. This is the same Err path
            // a transport-level body-read failure would take.
            let oversize = vec![b'x'; HTTP_BODY_LIMIT_BYTES + 16];
            let request = Request::builder()
                .method("POST")
                .uri("/api/v1/files/batch")
                .header("X-Weft-Component", "loomweave:deadbeef")
                .header(
                    "X-Weft-Timestamp",
                    OffsetDateTime::now_utc().unix_timestamp().to_string(),
                )
                .header("X-Weft-Nonce", "body-read-failure")
                .body(Body::from(oversize))
                .expect("request");

            // Drive `require_hmac_identity` directly. axum's `Next` is not
            // publicly constructible from outside middleware composition, so
            // we exercise the function via a single-route Router with the
            // middleware layered on top.
            let app: Router<()> = Router::new()
                .route("/api/v1/files/batch", post(never_called))
                .layer(axum::middleware::from_fn(|request, next| async move {
                    let replay_cache = new_hmac_replay_cache();
                    require_hmac_identity(
                        "test-secret",
                        &replay_cache,
                        HTTP_BODY_LIMIT_BYTES,
                        request,
                        next,
                    )
                    .await
                }));

            let response = app.oneshot(request).await.expect("oneshot response");
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 4096)
                .await
                .expect("read response body");
            (status, bytes)
        });

        let parsed: serde_json::Value =
            serde_json::from_slice(&body).expect("response body is JSON");
        // The exact code is `INTERNAL` (the CI-02 fix); the load-bearing
        // assertion is that it is NOT `INVALID_PATH`.
        assert_ne!(
            parsed["code"], "INVALID_PATH",
            "body-read failure must not surface as INVALID_PATH (CI-02): got status={status}, body={parsed}"
        );
        assert_eq!(
            parsed["code"], "INTERNAL",
            "expected INTERNAL on body-read failure inside HMAC middleware"
        );
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
