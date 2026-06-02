use std::path::PathBuf;
use std::sync::{Arc, LazyLock, mpsc};
use std::thread;
use std::time::Duration;

use std::future::IntoFuture;

use anyhow::{Context, Result, anyhow};
use axum::error_handling::HandleErrorLayer;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clarion_core::HttpErrorCode as ErrorCode;
use clarion_mcp::config::HttpReadConfig;
use clarion_storage::ReaderPool;
use serde::Serialize;
use tokio::sync::oneshot;
use tower::ServiceBuilder;
use tower::limit::ConcurrencyLimitLayer;
use tower::load_shed;
use tower::timeout;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

mod auth;
mod errors;
mod files;
mod identity;
mod linkages;
#[cfg(test)]
mod test_support;
mod wardline;

use auth::{handle_middleware_error, require_http_identity, require_http_identity_wardline};
use errors::catch_panic_response;
use files::{get_file, post_files_batch, post_files_resolve};
use identity::{
    get_identity_lineage, get_identity_sei, post_identity_resolve, post_identity_resolve_batch,
};
use linkages::{get_callees, get_callers, post_callees_batch, post_callers_batch};
use wardline::{
    get_wardline_taint_fact, post_wardline_resolve, post_wardline_taint_facts,
    post_wardline_taint_facts_batch_get, post_wardline_taint_facts_batch_get_by_sei,
};

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
pub(crate) struct AppState {
    pub(crate) project_root: PathBuf,
    pub(crate) readers: ReaderPool,
    pub(crate) instance_id: crate::instance::InstanceId,
    /// Resolved inbound auth token. `Some` when the configured `token_env`
    /// was set at spawn time, `None` when it was unset (loopback v0.1 trust
    /// mode). All `/api/v1/files`-family requests require
    /// `Authorization: Bearer <this>` when `Some`. `/api/v1/_capabilities`
    /// is always unauthenticated so siblings can probe pre-auth.
    pub(crate) auth_token: Option<Arc<String>>,
    /// Resolved Loom component identity HMAC secret. When present, protected
    /// routes require `X-Loom-Component: clarion:<hmac>`.
    pub(crate) identity_secret: Option<Arc<String>>,
    /// Present only when `serve.http.wardline_taint_write` is true (ADR-036).
    /// `None` ⇒ the write API is disabled and returns 403 `WRITE_DISABLED`.
    pub(crate) taint_writer: Option<tokio::sync::mpsc::Sender<clarion_storage::WriterCmd>>,
}

impl AppState {
    /// The `project` request field is a guard, not a selector: one `serve`
    /// serves exactly one project. An empty field is permitted (Wardline may
    /// omit it); a non-empty mismatch is rejected. The canonical project
    /// handle for v1 is the project-root directory name (cheapest, no new
    /// config). Pinned in contracts.md (W.5).
    fn reject_project_mismatch(&self, requested: &str) -> Option<Response> {
        if requested.is_empty() {
            return None;
        }
        let served = self
            .project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if requested == served {
            None
        } else {
            Some(json_error(
                StatusCode::FORBIDDEN,
                ErrorCode::ProjectMismatch,
                "project guard mismatch: this server serves a different project",
            ))
        }
    }
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
    db_path: PathBuf,
    readers: ReaderPool,
    instance_id: crate::instance::InstanceId,
    config: &HttpReadConfig,
) -> Result<Option<HttpReadServer>> {
    spawn_with_env(
        project_root,
        db_path,
        readers,
        instance_id,
        config,
        |name| std::env::var(name).ok(),
    )
}

/// Spawn variant that takes an explicit env lookup so tests can drive the
/// auth-trust gate (and the resolved-bearer-token plumbing) without
/// mutating process environment.
pub fn spawn_with_env<F>(
    project_root: PathBuf,
    db_path: PathBuf,
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
    let wardline_taint_write = config.wardline_taint_write;
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
    let identity_secret = config
        .identity_token_env
        .as_deref()
        .and_then(&env_lookup)
        .map(|raw| raw.trim().to_owned())
        .filter(|trimmed| !trimmed.is_empty())
        .map(Arc::new);
    let bind = config.bind;
    let warn_unauthenticated_non_loopback = config.allow_non_loopback
        && !config.is_loopback_bind()
        && auth_token.is_none()
        && identity_secret.is_none();
    // SEC-02: operator-visible signal that the HTTP API will admit any
    // local request because both auth knobs are unset and the bind is
    // loopback. On a shared developer host or CI runner this means any
    // local process can read the (non-blocked) catalogue.
    let warn_unauthenticated_loopback =
        config.is_loopback_bind() && auth_token.is_none() && identity_secret.is_none();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<HttpReadReady>>();
    let (failure_tx, failure_rx) = mpsc::channel();
    let auth_token_thread = auth_token.clone();
    let identity_secret_thread = identity_secret.clone();
    let join = thread::Builder::new()
        .name("clarion-http-read".to_owned())
        .spawn(move || -> Result<()> {
            let result = run_http_read_server(
                project_root,
                db_path,
                wardline_taint_write,
                readers,
                instance_id,
                auth_token_thread,
                identity_secret_thread,
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
    let auth = if identity_secret.is_some() {
        "hmac"
    } else if auth_token.is_some() {
        "bearer"
    } else {
        "none"
    };
    if warn_unauthenticated_non_loopback {
        tracing::warn!(
            bind = %local_addr,
            auth = %auth,
            "Clarion HTTP read API listening on non-loopback interface without authentication"
        );
    }
    if warn_unauthenticated_loopback {
        tracing::warn!(
            bind = %local_addr,
            auth = %auth,
            "[TRUST] HTTP API serving on loopback without authentication; any \
             local process on this host can read the catalogue. Set \
             identity_token_env or token_env for multi-tenant safety."
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

#[allow(clippy::too_many_arguments)] // server bootstrap fans many one-shot inputs into one thread
fn run_http_read_server(
    project_root: PathBuf,
    db_path: PathBuf,
    wardline_taint_write: bool,
    readers: ReaderPool,
    instance_id: crate::instance::InstanceId,
    auth_token: Option<Arc<String>>,
    identity_secret: Option<Arc<String>>,
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
        // Optional ADR-011 writer-actor for the Wardline taint-store WRITE API.
        // Note: when an LLM summary provider is configured, `serve` already
        // runs a second writer-actor (the MCP summary/inferred-edge writer in
        // serve.rs). Two writer-actors on one DB is a bounded relaxation of
        // ADR-011's single-writer expectation (ADR-036 §4): the streams are
        // independent and every writer opens `BEGIN IMMEDIATE` under
        // busy_timeout=5000 + capped-backoff retry, so they serialize at the
        // SQLite write lock rather than corrupting.
        // Spawned INSIDE the HTTP runtime (`Writer::spawn` uses `spawn_blocking`,
        // which needs a runtime). We keep ONLY `writer.sender()` — the `Writer`
        // handle is dropped at the end of this block so that the AppState's
        // sender clone is the last surviving sender. When `serve_future` is
        // consumed below, that clone (and every per-request/middleware clone)
        // drops, the actor's `mpsc::Receiver` sees all senders gone, and
        // `rx.blocking_recv()` returns `None` — so `taint_writer_join.await`
        // resolves instead of deadlocking. The `taint_writer_join` is held
        // OUTSIDE the AppState so it survives to be awaited at shutdown.
        let (taint_writer, taint_writer_join) = if wardline_taint_write {
            let (writer, join) = clarion_storage::Writer::spawn(
                db_path.clone(),
                clarion_storage::DEFAULT_BATCH_SIZE,
                clarion_storage::DEFAULT_CHANNEL_CAPACITY,
            )
            .map_err(|err| anyhow!("spawn taint writer-actor: {err}"))?;
            (Some(writer.sender()), Some(join))
        } else {
            (None, None)
        };
        let state = AppState {
            project_root,
            readers,
            instance_id,
            auth_token,
            identity_secret,
            taint_writer,
        };
        let serve_future = axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .into_future();
        // Consuming `serve_future` drops the AppState (and its `taint_writer`
        // sender clone) — the last sender — so the actor's channel closes.
        let serve_result = run_serve_future(serve_future).await;
        let writer_result = match taint_writer_join {
            Some(join) => join
                .await
                .context("join taint writer-actor")?
                .map_err(|err| anyhow!("taint writer-actor failed: {err}")),
            None => Ok(()),
        };
        // Propagate the serve error first (mirrors `finish_supervised_result`).
        serve_result?;
        writer_result
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
        assert!(
            !HTTP_THREAD_PANIC_TRIGGER.swap(false, std::sync::atomic::Ordering::SeqCst),
            "synthetic HTTP runtime panic for supervisor test"
        );
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

pub(crate) fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v1/files", get(get_file))
        .route("/api/v1/files:resolve", post(post_files_resolve))
        .route("/api/v1/files/batch", post(post_files_batch))
        .route("/api/v1/entities/:entity_id/callers", get(get_callers))
        .route("/api/v1/entities/:entity_id/callees", get(get_callees))
        .route(
            "/api/v1/entities/callers:batch-get",
            post(post_callers_batch),
        )
        .route(
            "/api/v1/entities/callees:batch-get",
            post(post_callees_batch),
        )
        // SEI identity resolution (Wave 1 / WS1, ADR-038 §4 / SEI spec §4).
        .route("/api/v1/identity/resolve", post(post_identity_resolve))
        .route(
            "/api/v1/identity/resolve:batch",
            post(post_identity_resolve_batch),
        )
        .route("/api/v1/identity/sei/:sei", get(get_identity_sei))
        .route("/api/v1/identity/lineage/:sei", get(get_identity_lineage))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_http_identity,
        ));
    let unprotected = Router::new().route("/api/v1/_capabilities", get(get_capabilities));
    // The 16 KiB read-API body limit is baked onto the merged `/api/v1/*`
    // group HERE — *before* `.merge(wardline)`. Tower body limits compose as
    // MIN and layer scope follows merge boundaries: applying this layer to the
    // already-merged v1 group keeps it off the wardline routes. (Flattening
    // it onto the final `protected.merge(unprotected).merge(wardline)` would
    // cap wardline at 16 KiB and defeat the larger limit.) The outer
    // `ServiceBuilder` no longer carries a body limit; each group owns its own.
    let v1 = protected
        .merge(unprotected)
        .layer(RequestBodyLimitLayer::new(HTTP_BODY_LIMIT_BYTES));
    // Wardline taint-store sub-router. Batched resolves/writes carry thousands
    // of qualnames, so this group gets the 4 MiB limit. Later tasks (5/6/7)
    // add the taint-facts read/write routes here. `DefaultBodyLimit` must also
    // be raised: axum's `Json` extractor enforces its own 2 MB default that
    // tower-http's `RequestBodyLimitLayer` does not touch, so without this the
    // 4 MiB target would be nominal only.
    let wardline = Router::new()
        .route("/api/wardline/resolve", post(post_wardline_resolve))
        .route(
            "/api/wardline/taint-facts",
            post(post_wardline_taint_facts).get(get_wardline_taint_fact),
        )
        .route(
            "/api/wardline/taint-facts:batch-get",
            post(post_wardline_taint_facts_batch_get),
        )
        // SEI read is a `by-sei` SUB-RESOURCE, not a `taint-facts:batch-get-by-sei`
        // custom method: matchit 0.7 parses the existing `taint-facts:batch-get`
        // as `[static taint-facts][param]`, whose param greedily eats every
        // suffix — so a second colon custom-method on the same resource both
        // (a) fails to register (Conflict) and (b) would be shadowed by the
        // first. A distinct slash-segment sidesteps that cleanly.
        .route(
            "/api/wardline/taint-facts/by-sei",
            post(post_wardline_taint_facts_batch_get_by_sei),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_http_identity_wardline,
        ))
        .layer(RequestBodyLimitLayer::new(WARDLINE_BODY_LIMIT_BYTES))
        .layer(axum::extract::DefaultBodyLimit::max(
            WARDLINE_BODY_LIMIT_BYTES,
        ));
    v1.merge(wardline).with_state(state).layer(
        ServiceBuilder::new()
            .layer(CatchPanicLayer::custom(catch_panic_response))
            .layer(HandleErrorLayer::new(handle_middleware_error))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(http_request_span)
                    .on_failure(()),
            )
            .layer(timeout::TimeoutLayer::new(Duration::from_secs(10)))
            .layer(load_shed::LoadShedLayer::new())
            .layer(ConcurrencyLimitLayer::new(64)),
    )
}

#[derive(Debug, Serialize)]
struct CapabilitiesResponse {
    registry_backend: bool,
    file_registry: bool,
    api_version: u8,
    instance_id: crate::instance::InstanceId,
    /// Structural call-graph linkage routes (Wave 0 / WS2). `http: true` once
    /// the `/api/v1/entities/{id}/callers|callees` routes ship.
    linkages: LinkagesCapability,
    /// Stable Entity Identity (Wave 1 / WS1, ADR-038). Consumers degrade against
    /// a pre-SEI Clarion by reading `sei.supported`.
    sei: SeiCapability,
    /// Wardline taint-store sub-capabilities (T3.4). `read_by_sei` advertises
    /// the `POST /api/wardline/taint-facts/by-sei` route discretely: an older
    /// SEI-capable Clarion has `sei.supported: true` but lacks this route, so
    /// consumers MUST gate the rename-stable taint read on this flag rather
    /// than on `sei.supported`.
    taint_store: TaintStoreCapability,
}

#[derive(Debug, Serialize)]
struct LinkagesCapability {
    http: bool,
}

#[derive(Debug, Serialize)]
struct SeiCapability {
    supported: bool,
    version: u8,
}

#[derive(Debug, Serialize)]
struct TaintStoreCapability {
    /// `POST /api/wardline/taint-facts/by-sei` is served.
    read_by_sei: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    code: ErrorCode,
}

const HTTP_BODY_LIMIT_BYTES: usize = 16 * 1024;

/// Body limit for the Wardline taint-store routes. Batched writes/resolves
/// carry thousands of qualnames; the 16 KiB read-API limit is far too small.
/// Wardline chunks client-side against `WARDLINE_TAINT_BATCH_MAX` (mirrors how
/// Filigree splits against `BATCH_MAX_QUERIES`). Pinned in contracts.md (W.5).
const WARDLINE_BODY_LIMIT_BYTES: usize = 4 * 1024 * 1024;
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

async fn get_capabilities(State(state): State<AppState>) -> Json<CapabilitiesResponse> {
    Json(CapabilitiesResponse {
        registry_backend: true,
        file_registry: true,
        api_version: 1,
        instance_id: state.instance_id,
        linkages: LinkagesCapability { http: true },
        sei: SeiCapability {
            supported: true,
            version: 1,
        },
        taint_store: TaintStoreCapability { read_by_sei: true },
    })
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
    use std::sync::mpsc;

    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    // REQ-F-02 (ADR-038 §4): `resolve(locator)` must reject an SEI-shaped input
    // by the RESERVED PREFIX, not a colon count — an SEI carries the same two
    // colons a `{plugin}:{kind}:{qualname}` locator does.
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

    /// SEC-02: when the HTTP API binds to loopback and neither
    /// `identity_token_env` nor `token_env` resolves to a non-empty
    /// secret, the surface admits any local request. The operator must
    /// see an unmissable startup warning that names "loopback" and
    /// "without authentication".
    #[test]
    fn spawn_emits_loopback_no_token_trust_warning() {
        use clarion_mcp::config::HttpReadConfig;
        use clarion_storage::ReaderPool;
        use std::io;
        use std::net::{SocketAddr, TcpListener};
        use std::sync::Mutex;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct CaptureWriter {
            buffer: Arc<Mutex<Vec<u8>>>,
        }

        impl io::Write for CaptureWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.buffer
                    .lock()
                    .expect("capture lock")
                    .extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for CaptureWriter {
            type Writer = CaptureWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = CaptureWriter {
            buffer: buffer.clone(),
        };
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_target(false)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();

        // Drive `spawn_with_env` under the capturing subscriber so the
        // startup warning lands in `buffer`. Use `with_default` so the
        // capture is scoped to this test and does not leak into peers.
        tracing::subscriber::with_default(subscriber, || {
            let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe bind");
            let bind: SocketAddr = probe.local_addr().expect("probe local addr");
            drop(probe);

            let tempdir = tempfile::tempdir().expect("temp project root");
            let db_path = tempdir.path().join("clarion.db");
            let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");

            let config = HttpReadConfig {
                enabled: true,
                bind,
                allow_non_loopback: false,
                token_env: "CLARION_LOOPBACK_NO_TOKEN_TEST_UNSET".to_owned(),
                identity_token_env: None,
                wardline_taint_write: false,
            };
            let instance_id =
                crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000002")
                    .expect("parse synthetic instance id");

            // Env lookup that returns None for every variable — emulates
            // the operator running `clarion serve` on loopback with no
            // tokens configured.
            let env_lookup = |_: &str| -> Option<String> { None };

            let server = spawn_with_env(
                tempdir.path().to_path_buf(),
                db_path.clone(),
                readers,
                instance_id,
                &config,
                env_lookup,
            )
            .expect("spawn HTTP read API")
            .expect("config.enabled = true implies Some(server)");
            // Shut down so the test thread does not leak the HTTP server.
            server.shutdown().expect("shutdown HTTP read API");
        });

        let captured = String::from_utf8(buffer.lock().expect("capture lock").clone())
            .expect("captured tracing output is UTF-8");

        assert!(
            captured.contains("loopback"),
            "expected loopback-no-token warning to mention 'loopback'; captured: {captured}"
        );
        assert!(
            captured.contains("without authentication"),
            "expected loopback-no-token warning to mention 'without authentication'; captured: {captured}"
        );
    }

    /// W.2 writer-actor lifecycle: with `wardline_taint_write: true`, `spawn`
    /// runs the FULL `run_http_read_server` path — it spawns the optional
    /// writer-actor inside the HTTP runtime, builds the `AppState` holding the
    /// only surviving sender clone, then on `shutdown()` drops `serve_future`
    /// (and that clone) so the actor's channel closes and `taint_writer_join`
    /// resolves. A drop-ordering regression (leaked sender / retained `Writer`)
    /// would deadlock `shutdown()` forever — the unit tests that build `AppState`
    /// by hand cannot catch that; this is the only test that exercises the
    /// spawn→drop→join sequence end to end.
    #[test]
    fn spawn_with_taint_writer_shuts_down_cleanly() {
        use clarion_mcp::config::HttpReadConfig;
        use clarion_storage::ReaderPool;
        use std::net::{SocketAddr, TcpListener};

        let probe = TcpListener::bind(("127.0.0.1", 0)).expect("probe bind");
        let bind: SocketAddr = probe.local_addr().expect("probe local addr");
        drop(probe);

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        // `Writer::spawn` creates the file and `verify_user_version` passes at
        // version 0; a shutdown-only test sends no commands.
        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");

        let config = HttpReadConfig {
            enabled: true,
            bind,
            allow_non_loopback: false,
            wardline_taint_write: true,
            ..HttpReadConfig::default()
        };
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000006")
                .expect("parse synthetic instance id");

        let server = spawn(
            tempdir.path().to_path_buf(),
            db_path.clone(),
            readers,
            instance_id,
            &config,
        )
        .expect("spawn HTTP read API")
        .expect("config.enabled = true implies Some(server)");

        // If the writer sender leaked, this `shutdown()` would block on the
        // join forever; CI's per-test timeout would surface the hang.
        server
            .shutdown()
            .expect("clean shutdown joins the writer-actor without error");
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

        let mut server = spawn(
            tempdir.path().to_path_buf(),
            db_path.clone(),
            readers,
            instance_id,
            &config,
        )
        .expect("spawn HTTP read API")
        .expect("config.enabled = true implies Some(server)");

        // Trip the panic. The watcher polls every 5 ms.
        HTTP_THREAD_PANIC_TRIGGER.store(true, std::sync::atomic::Ordering::SeqCst);

        // Poll check_running until it reports a failure; cap at 5 s so a
        // regression in the trigger path doesn't hang CI.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let err = loop {
            if let Err(err) = server.check_running() {
                break err;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "supervisor did not surface a runtime panic within 5s — \
                 the test panic hook may not be wired correctly"
            );
            std::thread::sleep(Duration::from_millis(20));
        };

        let message = format!("{err:#}");
        assert!(
            message.contains("HTTP read server thread panicked"),
            "supervisor must report the thread panic; got: {message}"
        );
    }

    // ----------------------------------------------------------------------
    // W.3 taint-fact READ endpoints (GET + :batch-get).
    // ----------------------------------------------------------------------
}
