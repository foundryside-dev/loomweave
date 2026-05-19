use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use axum::error_handling::HandleErrorLayer;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clarion_mcp::config::HttpReadConfig;
use clarion_storage::{CanonicalProjectPath, ReaderPool, StorageError, resolve_file_catalog_entry};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tower::limit::ConcurrencyLimitLayer;
use tower::load_shed;
use tower::timeout;
use tower::{BoxError, ServiceBuilder};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

static HTTP_ERROR_DISPATCH: LazyLock<tracing::Dispatch> = LazyLock::new(|| {
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_ansi(false)
        .finish();
    tracing::Dispatch::new(subscriber)
});

#[derive(Debug)]
pub struct HttpReadServer {
    shutdown: Option<oneshot::Sender<()>>,
    failure_rx: mpsc::Receiver<String>,
    join: Option<thread::JoinHandle<Result<()>>>,
    /// `ReaderPool::identity()` captured **inside the HTTP thread**, after
    /// the pool that the runtime actually uses has been moved into place.
    /// Callers can `Arc::ptr_eq` this against their own `ReaderPool` to
    /// catch a refactor that re-opens the pool inside this module.
    readers_identity: Arc<()>,
}

impl HttpReadServer {
    /// Borrow the in-thread `ReaderPool` identity tag. See the field comment.
    #[must_use]
    pub fn readers_identity(&self) -> &Arc<()> {
        &self.readers_identity
    }
}

impl HttpReadServer {
    pub fn check_running(&mut self) -> Result<()> {
        match self.failure_rx.try_recv() {
            Ok(error) => {
                let join_result = self.join_finished();
                if let Some(Err(join_error)) = join_result {
                    return Err(join_error.context(error));
                }
                return Err(anyhow!(error).context("HTTP read API server failed"));
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                if self
                    .join
                    .as_ref()
                    .is_some_and(thread::JoinHandle::is_finished)
                {
                    match self.join_finished() {
                        Some(Ok(())) => {
                            return Err(anyhow!("HTTP read API server exited unexpectedly"));
                        }
                        Some(Err(err)) => return Err(err),
                        None => {}
                    }
                }
            }
        }
        Ok(())
    }

    pub fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| anyhow!("HTTP read server thread panicked"))??;
        }
        Ok(())
    }

    fn join_finished(&mut self) -> Option<Result<()>> {
        let finished = self
            .join
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished);
        if !finished {
            return None;
        }
        let join = self.join.take()?;
        Some(
            join.join()
                .map_err(|_| anyhow!("HTTP read server thread panicked"))
                .and_then(|result| result),
        )
    }
}

#[derive(Clone)]
struct AppState {
    project_root: PathBuf,
    readers: ReaderPool,
    instance_id: crate::instance::InstanceId,
    /// Resolved inbound auth token. `Some` when the configured `token_env`
    /// was set at spawn time, `None` when it was unset (loopback v0.1 trust
    /// mode). All `/api/v1/files`-family requests require
    /// `Authorization: Bearer <this>` when `Some`. `/api/v1/_capabilities`
    /// is always unauthenticated so siblings can probe pre-auth.
    auth_token: Option<Arc<String>>,
}

/// Ready-signal payload returned from the HTTP thread back to `spawn`.
///
/// `readers_identity` is captured **inside** `run_http_read_server`, after
/// `readers` has been moved into the runtime. A refactor that re-opens the
/// pool inside the thread ships the new pool's identity back, and the
/// caller-side `Arc::ptr_eq` check fires. Capturing on the caller side
/// (before the move into the thread) would silently miss that refactor.
struct HttpReadReady {
    local_addr: std::net::SocketAddr,
    readers_identity: Arc<()>,
}

pub fn spawn(
    project_root: PathBuf,
    readers: ReaderPool,
    instance_id: crate::instance::InstanceId,
    config: &HttpReadConfig,
) -> Result<Option<HttpReadServer>> {
    spawn_with_env(project_root, readers, instance_id, config, |name| {
        std::env::var(name).ok()
    })
}

/// Spawn variant that takes an explicit env lookup so tests can drive the
/// auth-trust gate (and the resolved-bearer-token plumbing) without
/// mutating process environment.
pub fn spawn_with_env<F>(
    project_root: PathBuf,
    readers: ReaderPool,
    instance_id: crate::instance::InstanceId,
    config: &HttpReadConfig,
    env_lookup: F,
) -> Result<Option<HttpReadServer>>
where
    F: Fn(&str) -> Option<String>,
{
    if !config.enabled {
        return Ok(None);
    }
    config
        .validate_loopback_trust()
        .context("validate HTTP read API trust model")?;
    config
        .validate_auth_trust(&env_lookup)
        .context("validate HTTP read API auth trust model")?;
    let auth_token = env_lookup(&config.token_env)
        .map(|raw| raw.trim().to_owned())
        .filter(|trimmed| !trimmed.is_empty())
        .map(Arc::new);
    let bind = config.bind;
    let warn_unauthenticated_non_loopback = config.allow_non_loopback
        && !config.is_loopback_bind()
        && auth_token.is_none();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<HttpReadReady>>();
    let (failure_tx, failure_rx) = mpsc::channel();
    let auth_token_thread = auth_token.clone();
    let join = thread::Builder::new()
        .name("clarion-http-read".to_owned())
        .spawn(move || -> Result<()> {
            let result = run_http_read_server(
                project_root,
                readers,
                instance_id,
                auth_token_thread,
                bind,
                shutdown_rx,
                ready_tx,
            );
            if let Err(err) = &result {
                let _ = failure_tx.send(format!("{err:#}"));
            }
            result
        })
        .context("spawn HTTP read server thread")?;
    let ready = ready_rx
        .recv()
        .context("wait for HTTP read API bind result")??;
    let local_addr = ready.local_addr;
    let auth = if auth_token.is_some() { "bearer" } else { "none" };
    if warn_unauthenticated_non_loopback {
        tracing::warn!(
            bind = %local_addr,
            auth = %auth,
            "Clarion HTTP read API listening on non-loopback interface without authentication"
        );
    }
    tracing::info!(bind = %local_addr, auth = %auth, "Clarion HTTP read API listening");
    Ok(Some(HttpReadServer {
        shutdown: Some(shutdown_tx),
        failure_rx,
        join: Some(join),
        readers_identity: ready.readers_identity,
    }))
}

fn run_http_read_server(
    project_root: PathBuf,
    readers: ReaderPool,
    instance_id: crate::instance::InstanceId,
    auth_token: Option<Arc<String>>,
    bind: std::net::SocketAddr,
    shutdown_rx: oneshot::Receiver<()>,
    ready_tx: mpsc::Sender<Result<HttpReadReady>>,
) -> Result<()> {
    // Capture identity here, after `readers` has been moved in. A refactor
    // that opens a fresh pool inside this function would ship its new
    // identity back to the caller, who will `Arc::ptr_eq`-fail. Capturing
    // before the move (in `spawn`) would silently miss that refactor.
    let readers_identity = readers.identity().clone();
    let runtime = build_http_runtime()?;
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(err) => {
                let _ = ready_tx.send(Err(anyhow!("bind HTTP read API on {bind}: {err}")));
                return Err(anyhow!("bind HTTP read API on {bind}: {err}"));
            }
        };
        let local_addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(err) => {
                let _ = ready_tx.send(Err(anyhow!("read HTTP read API local addr: {err}")));
                return Err(anyhow!("read HTTP read API local addr: {err}"));
            }
        };
        let _ = ready_tx.send(Ok(HttpReadReady {
            local_addr,
            readers_identity,
        }));
        let state = AppState {
            project_root,
            readers,
            instance_id,
            auth_token,
        };
        use std::future::IntoFuture;
        let serve_future = axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .into_future();
        run_serve_future(serve_future).await
    })
}

#[cfg(not(test))]
async fn run_serve_future<F>(serve_future: F) -> Result<()>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    serve_future.await.context("serve HTTP read API")
}

/// Test-only cooperative panic hook. Setting [`HTTP_THREAD_PANIC_TRIGGER`]
/// to `true` causes the HTTP thread's `block_on` future to panic on its
/// next 5 ms tick. The panic propagates up through `block_on`, the thread's
/// `JoinHandle::join()` returns `Err(panic_payload)`, and
/// `HttpReadServer::check_running` then surfaces `"HTTP read server thread
/// panicked"` to the supervisor. This is the only path that still exercises
/// the supervisor's runtime-internal-panic arm after `CatchPanicLayer` was
/// introduced (which absorbs per-request handler panics into 500 envelopes).
#[cfg(test)]
pub(crate) static HTTP_THREAD_PANIC_TRIGGER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
async fn run_serve_future<F>(serve_future: F) -> Result<()>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    tokio::select! {
        () = panic_trigger_watcher() => unreachable!("panic_trigger_watcher must panic, not return"),
        result = serve_future => result.context("serve HTTP read API"),
    }
}

#[cfg(test)]
async fn panic_trigger_watcher() {
    loop {
        if HTTP_THREAD_PANIC_TRIGGER.swap(false, std::sync::atomic::Ordering::SeqCst) {
            panic!("synthetic HTTP runtime panic for supervisor test");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn build_http_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .thread_name("clarion-http-worker")
        .enable_all()
        .build()
        .context("create HTTP read runtime")
}

fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v1/files", get(get_file))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_bearer_token,
        ));
    let unprotected = Router::new().route("/api/v1/_capabilities", get(get_capabilities));
    protected
        .merge(unprotected)
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(CatchPanicLayer::custom(catch_panic_response))
                .layer(HandleErrorLayer::new(handle_middleware_error))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(http_request_span)
                        .on_failure(()),
                )
                .layer(timeout::TimeoutLayer::new(Duration::from_secs(10)))
                .layer(RequestBodyLimitLayer::new(16 * 1024))
                .layer(load_shed::LoadShedLayer::new())
                .layer(ConcurrencyLimitLayer::new(64)),
        )
}

/// Enforce `Authorization: Bearer <token>` on every request to a protected
/// route (currently `/api/v1/files` and the batch endpoint added by
/// CONTRACT-1). Unprotected routes — only `/api/v1/_capabilities` — are
/// served by a sibling sub-router that bypasses this middleware so siblings
/// can probe pre-auth.
///
/// Trust matrix (per `HttpReadConfig::validate_auth_trust`):
/// - loopback bind, token env unset    → `auth_token` is `None`; allow all.
/// - loopback bind, token env set      → `auth_token` is `Some`; require it.
/// - non-loopback bind, token env unset → `validate_auth_trust` refused
///   startup, this branch is unreachable.
/// - non-loopback bind, token env set   → `auth_token` is `Some`; require it.
async fn require_bearer_token(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
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
        return unauthorized_response();
    };
    // Constant-time compare so a wrong-length-token client can't trivially
    // distinguish "header absent" from "token mismatch" via timing.
    if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return unauthorized_response();
    }
    next.run(request).await
}

fn unauthorized_response() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthorized,
        "authentication required",
    )
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

async fn handle_middleware_error(err: BoxError) -> Response {
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    language: String,
}

#[derive(Debug, Serialize)]
struct FileResponse {
    entity_id: String,
    content_hash: String,
    canonical_path: CanonicalProjectPath,
    language: String,
}

#[derive(Debug, Serialize)]
struct CapabilitiesResponse {
    registry_backend: bool,
    file_registry: bool,
    api_version: u8,
    instance_id: crate::instance::InstanceId,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    code: ErrorCode,
}

#[derive(Debug, Copy, Clone, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ErrorCode {
    InvalidPath,
    PathOutsideProject,
    NotFound,
    BriefingBlocked,
    Unauthorized,
    StorageError,
    Internal,
}

async fn get_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<FileQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "query parameters are invalid",
        );
    };
    if query.path.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "path query parameter must not be blank",
        );
    }
    let project_root = state.project_root.clone();
    let file_path = query.path;
    let language = query.language;
    let catalog_result = state
        .readers
        .with_reader(move |conn| {
            resolve_file_catalog_entry(conn, &project_root, &file_path, &language)
        })
        .await;
    let result = match catalog_result {
        Ok(Some(entry)) => entry.into_resolved_file().map(Some),
        Ok(None) => Ok(None),
        Err(err) => Err(err),
    };
    match result {
        Ok(Some(file)) => {
            if let Some(reason) = file.briefing_blocked.as_deref() {
                log_briefing_blocked_refusal(file.canonical_path.as_str(), reason);
                return json_error(
                    StatusCode::FORBIDDEN,
                    ErrorCode::BriefingBlocked,
                    "entity is briefing-blocked and cannot be exposed",
                );
            }
            let etag = file_etag(&file.content_hash);
            if if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
                let mut response = StatusCode::NOT_MODIFIED.into_response();
                insert_etag(&mut response, &etag);
                return response;
            }
            let mut response = (
                StatusCode::OK,
                Json(FileResponse {
                    entity_id: file.entity_id,
                    content_hash: file.content_hash,
                    canonical_path: file.canonical_path,
                    language: file.language,
                }),
            )
                .into_response();
            insert_etag(&mut response, &etag);
            response
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "file is not known to Clarion",
        ),
        Err(err) => json_read_error(&err),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RequestLogContext {
    loom_component: Option<String>,
    filigree_actor: Option<String>,
}

fn request_log_context(headers: &HeaderMap) -> RequestLogContext {
    RequestLogContext {
        loom_component: log_header_value(headers, "x-loom-component"),
        filigree_actor: log_header_value(headers, "x-filigree-actor"),
    }
}

fn log_header_value(headers: &HeaderMap, name: &'static str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn http_request_span<B>(request: &Request<B>) -> tracing::Span {
    let context = request_log_context(request.headers());
    let span = tracing::info_span!(
        "http_read_request",
        method = %request.method(),
        path = %request.uri().path(),
        loom_component = tracing::field::Empty,
        filigree_actor = tracing::field::Empty,
    );
    if let Some(loom_component) = context.loom_component {
        span.record("loom_component", tracing::field::display(loom_component));
    }
    if let Some(filigree_actor) = context.filigree_actor {
        span.record("filigree_actor", tracing::field::display(filigree_actor));
    }
    span
}

fn file_etag(content_hash: &str) -> String {
    format!("\"{content_hash}\"")
}

fn if_none_match_matches(value: Option<&HeaderValue>, etag: &str) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag)
    })
}

fn insert_etag(response: &mut Response, etag: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
}

async fn get_capabilities(State(state): State<AppState>) -> Json<CapabilitiesResponse> {
    Json(CapabilitiesResponse {
        registry_backend: true,
        file_registry: true,
        api_version: 1,
        instance_id: state.instance_id,
    })
}

fn json_read_error(err: &StorageError) -> Response {
    let error = classify_read_error(err);
    if error.status.is_server_error() {
        log_read_server_error(error.code, error.status, err);
    }
    json_error(error.status, error.code, error.message)
}

struct ReadError {
    status: StatusCode,
    code: ErrorCode,
    message: &'static str,
}

fn classify_read_error(err: &StorageError) -> ReadError {
    match err {
        StorageError::InvalidQuery(_) => ReadError {
            status: StatusCode::BAD_REQUEST,
            code: ErrorCode::InvalidPath,
            message: "path query parameter is invalid",
        },
        StorageError::InvalidSourcePath(_) => ReadError {
            status: StatusCode::BAD_REQUEST,
            code: ErrorCode::PathOutsideProject,
            message: "path is outside project root",
        },
        StorageError::Pool(_) => ReadError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: ErrorCode::StorageError,
            message: "file lookup storage is unavailable",
        },
        StorageError::Sqlite(_)
        | StorageError::PoolBuild(_)
        | StorageError::PragmaInvariant(_)
        | StorageError::Migration { .. }
        | StorageError::Io(_) => ReadError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: ErrorCode::StorageError,
            message: "file lookup failed",
        },
        StorageError::PoolInteract(_)
        | StorageError::WriterGone
        | StorageError::WriterProtocol(_)
        | StorageError::WriterNoResponse => ReadError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: ErrorCode::Internal,
            message: "internal file lookup failure",
        },
    }
}

fn format_error_chain(err: &StorageError) -> String {
    format_dyn_error_chain(err)
}

fn format_dyn_error_chain(err: &(dyn StdError + 'static)) -> String {
    let mut chain = vec![err.to_string()];
    let mut source = err.source();
    let mut depth = 0;
    while let Some(err) = source {
        if depth >= 8 {
            chain.push("additional sources omitted".to_owned());
            break;
        }
        chain.push(err.to_string());
        source = err.source();
        depth += 1;
    }
    chain.join(": caused by: ")
}

fn log_briefing_blocked_refusal(canonical_path: &str, reason: &str) {
    tracing::dispatcher::with_default(&HTTP_ERROR_DISPATCH, || {
        tracing::warn!(
            path = %canonical_path,
            reason = %reason,
            "HTTP /api/v1/files refusing to expose briefing-blocked entity"
        );
    });
}

fn log_read_server_error(code: ErrorCode, status: StatusCode, err: &StorageError) {
    let error_chain = format_error_chain(err);
    tracing::dispatcher::with_default(&HTTP_ERROR_DISPATCH, || {
        tracing::error!(
            code = ?code,
            status = status.as_u16(),
            error_chain = %error_chain,
            "HTTP /api/v1/files lookup failed"
        );
    });
}

#[allow(clippy::needless_pass_by_value)] // `ResponseForPanic` requires owned payload
fn catch_panic_response(payload: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let detail = if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    };
    tracing::dispatcher::with_default(&HTTP_ERROR_DISPATCH, || {
        tracing::error!(panic = %detail, "HTTP read API handler panicked");
    });
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        "internal panic",
    )
}

fn json_error(status: StatusCode, code: ErrorCode, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.to_owned(),
            code,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::future::{Future, Pending, pending};
    use std::sync::mpsc;
    use std::task::{Context, Poll};

    use super::*;
    use axum::http::{HeaderMap, HeaderValue};
    use tower::Service;

    #[test]
    fn check_running_surfaces_completed_http_thread_failure_before_shutdown() {
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        failure_tx
            .send("simulated HTTP server failure".to_owned())
            .expect("send simulated failure");
        let join = thread::spawn(|| Err(anyhow!("simulated HTTP server failure")));
        let mut server = HttpReadServer {
            shutdown: Some(shutdown_tx),
            failure_rx,
            join: Some(join),
            readers_identity: Arc::new(()),
        };

        let err = server
            .check_running()
            .expect_err("HTTP failure should surface before shutdown");
        let message = format!("{err:#}");

        assert!(
            message.contains("simulated HTTP server failure"),
            "unexpected error: {message}"
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
    fn request_log_context_reads_optional_actor_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Loom-Component", HeaderValue::from_static("loom"));
        headers.insert("X-Filigree-Actor", HeaderValue::from_static("worker-f"));

        let context = request_log_context(&headers);

        assert_eq!(context.loom_component.as_deref(), Some("loom"));
        assert_eq!(context.filigree_actor.as_deref(), Some("worker-f"));
    }

    #[test]
    fn http_runtime_names_worker_threads() {
        let runtime = build_http_runtime().expect("HTTP runtime");
        let worker_name = runtime.block_on(async {
            tokio::spawn(async { std::thread::current().name().map(str::to_owned) })
                .await
                .expect("worker task")
        });

        assert_eq!(worker_name.as_deref(), Some("clarion-http-worker"));
    }

    #[test]
    fn format_dyn_error_chain_walks_box_error_sources() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("inner cause")
            }
        }
        impl StdError for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("outer failure")
            }
        }
        impl StdError for Outer {
            fn source(&self) -> Option<&(dyn StdError + 'static)> {
                Some(&self.0)
            }
        }

        let err: BoxError = Box::new(Outer(Inner));
        let chain = format_dyn_error_chain(&*err);

        assert_eq!(chain, "outer failure: caused by: inner cause");
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

    #[test]
    fn unknown_middleware_error_translated_to_internal_envelope_via_catch_panic() {
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use std::convert::Infallible;
        use std::pin::Pin;
        use tower::{Layer, Service, ServiceExt};

        #[derive(Debug)]
        struct UnknownInjected;
        impl std::fmt::Display for UnknownInjected {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("injected unknown middleware error")
            }
        }
        impl StdError for UnknownInjected {}

        #[derive(Clone)]
        struct InjectUnknownErrorLayer;

        impl<S> Layer<S> for InjectUnknownErrorLayer {
            type Service = InjectUnknownErrorService<S>;
            fn layer(&self, inner: S) -> Self::Service {
                InjectUnknownErrorService { inner }
            }
        }

        #[derive(Clone)]
        #[allow(dead_code)] // `inner` is held to satisfy Layer wiring; this service short-circuits.
        struct InjectUnknownErrorService<S> {
            inner: S,
        }

        impl<S, B> Service<Request<B>> for InjectUnknownErrorService<S>
        where
            S: Service<Request<B>, Error = Infallible> + Send + 'static,
            S::Future: Send + 'static,
            B: Send + 'static,
        {
            type Response = S::Response;
            type Error = BoxError;
            type Future =
                Pin<Box<dyn std::future::Future<Output = Result<Self::Response, BoxError>> + Send>>;

            fn poll_ready(
                &mut self,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, _req: Request<B>) -> Self::Future {
                Box::pin(async { Err(BoxError::from(Box::new(UnknownInjected))) })
            }
        }

        async fn never_called() -> Response {
            unreachable!("inner handler must not run when middleware short-circuits")
        }

        let app: Router<()> = Router::new().route("/x", get(never_called)).layer(
            ServiceBuilder::new()
                .layer(CatchPanicLayer::custom(catch_panic_response))
                .layer(HandleErrorLayer::new(handle_middleware_error))
                .layer(InjectUnknownErrorLayer),
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let (status, body) = runtime.block_on(async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/x")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("oneshot response");
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 4096)
                .await
                .expect("read response body");
            (status, bytes)
        });

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let parsed: serde_json::Value =
            serde_json::from_slice(&body).expect("response body is JSON");
        assert_eq!(parsed["error"], "internal panic");
        assert_eq!(parsed["code"], "INTERNAL");
    }

    #[test]
    fn catch_panic_response_returns_internal_envelope() {
        let payload: Box<dyn std::any::Any + Send + 'static> =
            Box::new("handler exploded".to_owned());

        let response = catch_panic_response(payload);

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn catch_panic_layer_translates_handler_panic_to_internal_envelope() {
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt;

        async fn boom() -> Response {
            panic!("synthetic handler panic");
        }

        let app: Router<()> = Router::new().route("/boom", get(boom)).layer(
            ServiceBuilder::new()
                .layer(CatchPanicLayer::custom(catch_panic_response))
                .layer(HandleErrorLayer::new(handle_middleware_error))
                .layer(timeout::TimeoutLayer::new(Duration::from_secs(1))),
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let (status, body) = runtime.block_on(async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/boom")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("oneshot response");
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 4096)
                .await
                .expect("read response body");
            (status, bytes)
        });

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let parsed: serde_json::Value =
            serde_json::from_slice(&body).expect("response body is JSON");
        assert_eq!(parsed["error"], "internal panic");
        assert_eq!(parsed["code"], "INTERNAL");
    }

    /// C8 supervisor end-to-end. Trips the test-only
    /// [`HTTP_THREAD_PANIC_TRIGGER`] after the HTTP thread has reported a
    /// successful bind. The trigger fires a panic inside the runtime's
    /// `block_on`, the thread's `JoinHandle::join()` reports
    /// `Err(panic_payload)`, and `check_running` surfaces "HTTP read server
    /// thread panicked" — the path that runs when `CatchPanicLayer` cannot
    /// absorb the panic (i.e. anything outside per-request middleware).
    #[test]
    fn check_running_surfaces_supervisor_signal_after_runtime_panic() {
        use clarion_mcp::config::HttpReadConfig;
        use clarion_storage::ReaderPool;
        use std::net::{SocketAddr, TcpListener};

        // Hold-and-drop: bind to ephemeral 0 to discover a free port, then
        // drop so the HTTP server can re-bind it. The micro-race is fine
        // here — if the port is stolen we surface a different error.
        let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe bind");
        let bind: SocketAddr = probe.local_addr().expect("probe local addr");
        drop(probe);

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        // ReaderPool::open is lazy; no connection is acquired before the
        // panic trigger fires, so the absent SQLite file is irrelevant.
        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");

        let config = HttpReadConfig {
            enabled: true,
            bind,
            allow_non_loopback: false,
            ..HttpReadConfig::default()
        };
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000001")
                .expect("parse synthetic instance id");

        // Defensive: clear any stale trigger from a prior test.
        HTTP_THREAD_PANIC_TRIGGER.store(false, std::sync::atomic::Ordering::SeqCst);

        let mut server = spawn(tempdir.path().to_path_buf(), readers, instance_id, &config)
            .expect("spawn HTTP read API")
            .expect("config.enabled = true implies Some(server)");

        // Trip the panic. The watcher polls every 5 ms.
        HTTP_THREAD_PANIC_TRIGGER.store(true, std::sync::atomic::Ordering::SeqCst);

        // Poll check_running until it reports a failure; cap at 5 s so a
        // regression in the trigger path doesn't hang CI.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let err = loop {
            match server.check_running() {
                Err(err) => break err,
                Ok(()) => {
                    if std::time::Instant::now() >= deadline {
                        panic!(
                            "supervisor did not surface a runtime panic within 5s — \
                             the test panic hook may not be wired correctly"
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        };

        let message = format!("{err:#}");
        assert!(
            message.contains("HTTP read server thread panicked"),
            "supervisor must report the thread panic; got: {message}"
        );
    }
}
