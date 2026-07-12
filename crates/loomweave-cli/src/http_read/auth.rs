//! Inbound identity / HMAC authentication middleware for the HTTP read API.
//!
//! Split out of `http_read.rs` (mechanical relocation; behaviour unchanged).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use hmac::{Hmac, Mac};
use loomweave_core::HttpErrorCode as ErrorCode;
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

/// Validate freshness and record a nonce against an explicit clock value.
/// Keeping time outside this helper makes the wire contract deterministic for
/// producer-owned vectors. A poisoned mutex is a trust-boundary failure and is
/// therefore rejected rather than recovered.
pub(crate) fn validate_hmac_replay_at(
    replay_cache: &SharedHmacReplayCache,
    nonce: &str,
    request_timestamp: i64,
    now_timestamp: i64,
) -> bool {
    replay_cache
        .lock()
        .is_ok_and(|mut cache| cache.check_and_record(nonce, request_timestamp, now_timestamp))
}

fn parse_bearer_credential(value: &str) -> Option<&str> {
    let token = value.strip_prefix("Bearer ")?;
    (!token.is_empty() && token.trim() == token).then_some(token)
}

/// Parse the exact Weft component wire shape. Callers may use successful
/// parsing only as a boolean signal; the signature itself remains sensitive.
pub(super) fn parse_weft_component_signature(value: &str) -> Option<&str> {
    let signature = value.strip_prefix("loomweave:")?;
    (signature.len() == 64
        && signature
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
    .then_some(signature)
}

fn parse_hmac_timestamp(value: &str) -> Option<i64> {
    let parsed = value.parse::<i64>().ok()?;
    (parsed >= 0 && parsed.to_string() == value).then_some(parsed)
}

fn parse_hmac_nonce(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= HMAC_NONCE_MAX_LEN
        && !value.chars().all(char::is_whitespace))
    .then_some(value)
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
        .and_then(parse_bearer_credential);
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
        .and_then(parse_weft_component_signature)
        .map(str::to_owned);
    let Some(presented) = presented else {
        return unauthenticated_response();
    };
    let timestamp = parts
        .headers
        .get("x-weft-timestamp")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_hmac_timestamp);
    let Some(timestamp) = timestamp else {
        return unauthenticated_response();
    };
    let nonce = parts
        .headers
        .get("x-weft-nonce")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_hmac_nonce)
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
    let fresh_and_unseen = validate_hmac_replay_at(replay_cache, &nonce, timestamp, now);
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
        method.to_ascii_uppercase(),
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
    fn canonical_hmac_vector_normalizes_method_and_pins_every_signing_field() {
        let body = br#"{"locator":"python:function:demo.run"}"#;
        let message = canonical_hmac_message(
            "post",
            "/api/v1/identity/resolve?trace=1",
            body,
            1_900_000_000,
            "nonce-vector-001",
        );

        assert_eq!(
            message,
            "POST\n/api/v1/identity/resolve?trace=1\n591e7e32c043fb1bc8f070f7f65d5e262c01e468a7d4a4cf48c1fd997a7211e0\n1900000000\nnonce-vector-001"
        );
        assert_eq!(
            component_hmac_hex(
                b"federation-secret",
                "post",
                "/api/v1/identity/resolve?trace=1",
                body,
                1_900_000_000,
                "nonce-vector-001",
            ),
            "38898838931bd6b292917a8734975472bda49ae679960610df1d0db62f4a00c5"
        );
    }

    #[test]
    fn producer_auth_golden_rechecks_production_canonicalizer_and_freshness() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/federation/fixtures/loomweave-http-auth-v1.golden.json"
        ))
        .expect("parse producer auth golden");
        let vector = &fixture["canonical_hmac"];
        let body = vector["body_utf8"].as_str().expect("body").as_bytes();
        let method = vector["method_input"].as_str().expect("input method");
        let path = vector["path_and_query"].as_str().expect("path and query");
        let timestamp = vector["timestamp"].as_i64().expect("timestamp");
        let nonce = vector["nonce"].as_str().expect("nonce");
        assert_eq!(
            canonical_hmac_message(method, path, body, timestamp, nonce),
            vector["signing_bytes_utf8"]
                .as_str()
                .expect("signing bytes")
        );
        assert_eq!(
            format!(
                "loomweave:{}",
                component_hmac_hex(
                    vector["secret"].as_str().expect("secret").as_bytes(),
                    method,
                    path,
                    body,
                    timestamp,
                    nonce,
                )
            ),
            vector["component_signature"]
                .as_str()
                .expect("component signature")
        );

        let now = fixture["freshness"]["now"].as_i64().expect("now");
        for accepted in fixture["freshness"]["accepted_timestamps"]
            .as_array()
            .expect("accepted timestamps")
        {
            assert!(validate_hmac_replay_at(
                &new_hmac_replay_cache(),
                &format!("accepted-{accepted}"),
                accepted.as_i64().expect("accepted timestamp"),
                now,
            ));
        }
        for rejected in fixture["freshness"]["rejected_timestamps"]
            .as_array()
            .expect("rejected timestamps")
        {
            assert!(!validate_hmac_replay_at(
                &new_hmac_replay_cache(),
                &format!("rejected-{rejected}"),
                rejected.as_i64().expect("rejected timestamp"),
                now,
            ));
        }
    }

    #[test]
    fn wire_auth_fields_are_exact_and_never_trimmed() {
        let signature = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            parse_bearer_credential("Bearer exact-token"),
            Some("exact-token")
        );
        assert_eq!(parse_bearer_credential("Bearer  exact-token"), None);
        assert_eq!(parse_bearer_credential("Bearer exact-token "), None);
        assert_eq!(parse_bearer_credential(" bearer exact-token"), None);

        let component = format!("loomweave:{signature}");
        assert_eq!(parse_weft_component_signature(&component), Some(signature));
        assert_eq!(
            parse_weft_component_signature(&format!("loomweave:{signature} ")),
            None
        );
        assert_eq!(
            parse_weft_component_signature(&format!(" loomweave:{signature}")),
            None
        );
        assert_eq!(
            parse_weft_component_signature(&format!("loomweave:{}", signature.to_uppercase())),
            None
        );

        assert_eq!(parse_hmac_timestamp("1900000000"), Some(1_900_000_000));
        assert_eq!(parse_hmac_timestamp(" 1900000000"), None);
        assert_eq!(parse_hmac_timestamp("1900000000 "), None);
        assert_eq!(parse_hmac_timestamp("+1900000000"), None);
        assert_eq!(parse_hmac_timestamp("01900000000"), None);
        assert_eq!(parse_hmac_timestamp("-1"), None);

        assert_eq!(
            parse_hmac_nonce(" nonce-with-padding "),
            Some(" nonce-with-padding ")
        );
        assert_eq!(parse_hmac_nonce(""), None);
        assert_eq!(parse_hmac_nonce("   "), None);
    }

    #[test]
    fn canonical_hmac_signs_exact_nonce_bytes() {
        let padded = component_hmac_hex(
            b"federation-secret",
            "GET",
            "/api/v1/identity/sei/example",
            b"",
            1_900_000_000,
            " nonce ",
        );
        let trimmed = component_hmac_hex(
            b"federation-secret",
            "GET",
            "/api/v1/identity/sei/example",
            b"",
            1_900_000_000,
            "nonce",
        );
        assert_ne!(
            padded, trimmed,
            "nonce whitespace must be signed, not trimmed"
        );
        assert!(
            canonical_hmac_message(
                "GET",
                "/api/v1/identity/sei/example",
                b"",
                1_900_000_000,
                " nonce ",
            )
            .ends_with("\n nonce ")
        );
    }

    #[test]
    fn hmac_freshness_window_is_inclusive_and_poisoned_cache_fails_closed() {
        let now = 1_900_000_000;
        let cache = new_hmac_replay_cache();

        assert!(validate_hmac_replay_at(
            &cache,
            "old-boundary",
            now - HMAC_FRESHNESS_WINDOW_SECONDS,
            now,
        ));
        assert!(validate_hmac_replay_at(
            &cache,
            "future-boundary",
            now + HMAC_FRESHNESS_WINDOW_SECONDS,
            now,
        ));
        assert!(!validate_hmac_replay_at(
            &cache,
            "too-old",
            now - HMAC_FRESHNESS_WINDOW_SECONDS - 1,
            now,
        ));
        assert!(!validate_hmac_replay_at(
            &cache,
            "too-new",
            now + HMAC_FRESHNESS_WINDOW_SECONDS + 1,
            now,
        ));

        let poisoned = new_hmac_replay_cache();
        let poison_clone = poisoned.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poison_clone.lock().expect("lock replay cache");
            panic!("poison replay cache");
        });
        assert!(!validate_hmac_replay_at(
            &poisoned,
            "must-fail-closed",
            now,
            now,
        ));
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
                .header(
                    "X-Weft-Component",
                    "loomweave:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
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
