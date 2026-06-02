use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, mpsc};
use std::thread;
use std::time::Duration;

use std::future::IntoFuture;

use anyhow::{Context, Result, anyhow};
use axum::body::{Body, to_bytes};
use axum::error_handling::HandleErrorLayer;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clarion_core::EdgeConfidence;
use clarion_core::HttpErrorCode as ErrorCode;
use clarion_mcp::config::HttpReadConfig;
use clarion_storage::{
    CallEdgeMatch, CanonicalProjectPath, EntityVisibility, ReaderPool, SeiLookupResult,
    StorageError, call_edges_from, call_edges_targeting, entity_visibility, is_reserved_sei,
    resolve_file_catalog_entry, resolve_locator, resolve_sei, sei_lineage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    /// Resolved Loom component identity HMAC secret. When present, protected
    /// routes require `X-Loom-Component: clarion:<hmac>`.
    identity_secret: Option<Arc<String>>,
    /// Present only when `serve.http.wardline_taint_write` is true (ADR-036).
    /// `None` ⇒ the write API is disabled and returns 403 `WRITE_DISABLED`.
    taint_writer: Option<tokio::sync::mpsc::Sender<clarion_storage::WriterCmd>>,
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

fn router(state: AppState) -> Router {
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

/// Enforce configured identity on protected routes. Prefer the Loom HMAC
/// identity when `identity_token_env` is configured; otherwise preserve the
/// legacy bearer-token path for existing deployments.
async fn require_http_identity(
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
async fn require_http_identity_wardline(
    State(state): State<AppState>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    require_http_identity_with_limit(&state, WARDLINE_BODY_LIMIT_BYTES, request, next).await
}

async fn require_http_identity_with_limit(
    state: &AppState,
    body_limit: usize,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    if let Some(secret) = state.identity_secret.as_ref() {
        return require_hmac_identity(secret, body_limit, request, next).await;
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

async fn require_hmac_identity(
    secret: &str,
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
        .get("x-loom-component")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("clarion:"))
        .filter(|signature| !signature.is_empty())
        .map(str::to_owned);
    let Some(presented) = presented else {
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
    let expected = component_hmac_hex(secret.as_bytes(), &method, &path_and_query, &body_bytes);
    if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return unauthenticated_response();
    }
    next.run(Request::from_parts(parts, Body::from(body_bytes)))
        .await
}

fn unauthenticated_response() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthenticated,
        "authentication required",
    )
}

fn component_hmac_hex(secret: &[u8], method: &str, path_and_query: &str, body: &[u8]) -> String {
    hmac_sha256_hex(
        secret,
        canonical_hmac_message(method, path_and_query, body).as_bytes(),
    )
}

fn canonical_hmac_message(method: &str, path_and_query: &str, body: &[u8]) -> String {
    format!(
        "{}\n{}\n{}",
        method,
        path_and_query,
        hex_lower(&Sha256::digest(body))
    )
}

fn hmac_sha256_hex(secret: &[u8], message: &[u8]) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut key = [0_u8; BLOCK_SIZE];
    if secret.len() > BLOCK_SIZE {
        key[..32].copy_from_slice(&Sha256::digest(secret));
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut ipad = [0x36_u8; BLOCK_SIZE];
    let mut opad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        ipad[index] ^= key[index];
        opad[index] ^= key[index];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    hex_lower(&outer.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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

/// Maximum number of `BatchFileQuery` entries a single
/// `POST /api/v1/files/batch` request may carry. Pinned in the federation
/// contract; Filigree splits oversize lookup sets client-side. Lifted to a
/// constant so the contract docs, the validator, and tests all point at
/// the same number.
const BATCH_MAX_QUERIES: usize = 256;
const RESOLVE_MAX_PATHS: usize = 1000;
const HTTP_BODY_LIMIT_BYTES: usize = 16 * 1024;

/// Default / maximum page size for the call-graph linkage routes (WS2).
const LINKAGE_LIMIT_DEFAULT: u32 = 50;
const LINKAGE_LIMIT_MAX: u32 = 200;
/// Max entity ids in one linkage `:batch-get` request. Pinned in contracts.md;
/// clients split oversize sets client-side (mirrors `BATCH_MAX_QUERIES`).
const LINKAGES_BATCH_MAX: usize = 50;

/// Body limit for the Wardline taint-store routes. Batched writes/resolves
/// carry thousands of qualnames; the 16 KiB read-API limit is far too small.
/// Wardline chunks client-side against `WARDLINE_TAINT_BATCH_MAX` (mirrors how
/// Filigree splits against `BATCH_MAX_QUERIES`). Pinned in contracts.md (W.5).
const WARDLINE_BODY_LIMIT_BYTES: usize = 4 * 1024 * 1024;
/// Max qualnames/facts in one Wardline request.
const WARDLINE_TAINT_BATCH_MAX: usize = 2000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchFileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    language: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchFileRequest {
    queries: Vec<BatchFileQuery>,
}

#[derive(Debug, Serialize)]
struct BatchResolvedItem {
    requested_path: String,
    entity_id: String,
    content_hash: String,
    canonical_path: CanonicalProjectPath,
    language: String,
}

#[derive(Debug, Serialize)]
struct BatchErrorItem {
    requested_path: String,
    code: ErrorCode,
    message: String,
}

#[derive(Debug, Serialize)]
struct BatchFileResponse {
    resolved: Vec<BatchResolvedItem>,
    not_found: Vec<String>,
    briefing_blocked: Vec<String>,
    errors: Vec<BatchErrorItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveFileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    language: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveFilesRequest {
    paths: Vec<ResolveFileQuery>,
}

#[derive(Debug, Serialize)]
struct ResolveFilesResponse {
    results: Vec<ResolveFileResult>,
}

#[derive(Debug, Serialize)]
struct ResolveFileResult {
    path: String,
    response: ResolveFileItemResponse,
}

#[derive(Debug, Serialize)]
struct ResolveFileItemResponse {
    status: ResolveFileStatus,
    body: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResolveFileStatus {
    Resolved,
    NotFound,
    Blocked,
    Error,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    #[serde(default)]
    project: String,
    qualnames: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ResolveResponse {
    /// qualname -> `entity_id`, only for exact matches.
    resolved: std::collections::BTreeMap<String, String>,
    unresolved: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaintFactInput {
    qualname: String,
    /// `RawValue` captures the ORIGINAL bytes of this JSON sub-value exactly —
    /// `serde_json::Value` would normalize (object keys are a `BTreeMap`, so
    /// `{"b":2,"a":1}` would re-emit as `{"a":1,"b":2}`). The federation
    /// contract is "stored and returned verbatim", so we preserve the bytes.
    wardline_json: Box<serde_json::value::RawValue>,
    #[serde(default)]
    scan_id: Option<String>,
    #[serde(default)]
    content_hash_at_compute: Option<String>,
    /// Optional caller-supplied Stable Entity Identity (T3.4, migration 0006).
    /// Opaque — stored verbatim, never parsed. When omitted, the write path
    /// resolves the alive SEI for the resolved locator server-side. A
    /// caller-supplied value wins (Wardline already holds the SEI from its
    /// `SeiResolver`).
    #[serde(default)]
    sei: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteTaintFactsRequest {
    #[serde(default)]
    project: String,
    #[serde(default)]
    scan_id: Option<String>,
    facts: Vec<TaintFactInput>,
}

#[derive(Debug, Serialize)]
struct WriteTaintFactsResponse {
    written: usize,
    unresolved_qualnames: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaintFactQuery {
    #[serde(default)]
    project: String,
    qualname: String,
}

#[derive(Debug, Serialize)]
struct TaintFactView {
    qualname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wardline_json: Option<Box<serde_json::value::RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_content_hash: Option<String>,
    exists: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchGetRequest {
    #[serde(default)]
    project: String,
    qualnames: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchGetBySeiRequest {
    #[serde(default)]
    project: String,
    /// Opaque SEIs (`clarion:eid:<hex>`). Treated verbatim — NO locator-shape
    /// validation (SEI-shaped strings are the valid input here, the inverse of
    /// the `resolve` REQ-F-02 rejection).
    seis: Vec<String>,
}

/// One taint fact keyed by SEI (T3.4 read-by-SEI surface). Same fields as
/// [`TaintFactView`] but keyed on the opaque `sei` instead of the qualname:
/// `exists: false` when no SEI-tagged fact is stored for the SEI.
#[derive(Debug, Serialize)]
struct TaintFactBySeiView {
    sei: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wardline_json: Option<Box<serde_json::value::RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_content_hash: Option<String>,
    exists: bool,
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

/// Batch resolution endpoint. Resolves up to `BATCH_MAX_QUERIES` paths in a
/// single request, partitioning results into four lists:
///
/// - `resolved`        — paths that mapped to a file-kind entity.
/// - `not_found`       — paths Clarion does not have a catalog row for.
/// - `briefing_blocked` — paths whose entity carries a `briefing_blocked`
///   property (the partition equivalent of the single-file 403 surface).
/// - `errors`          — per-path resolution errors (`INVALID_PATH`,
///   `PATH_OUTSIDE_PROJECT`, `STORAGE_ERROR`, `INTERNAL`).
///
/// The whole batch runs inside **one** `with_reader` closure so we
/// check out one pooled connection per request, not one per query —
/// this is the perf win Filigree's `ClarionRegistry` needs for cold-
/// start hydration. `ETag` is intentionally not applied to the batch
/// surface; clients should `ETag` the single-file endpoint when they
/// want conditional fetch semantics.
async fn post_files_batch(
    State(state): State<AppState>,
    body: Result<Json<BatchFileRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"queries\": [...]}",
        );
    };
    if request.queries.len() > BATCH_MAX_QUERIES {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::BatchTooLarge,
            "queries[] exceeds the per-batch maximum of 256 entries",
        );
    }
    let project_root = state.project_root.clone();
    let queries = request.queries;
    let catalog_result = state
        .readers
        .with_reader(move |conn| {
            let mut resolved = Vec::new();
            let mut not_found = Vec::new();
            let mut briefing_blocked = Vec::new();
            let mut errors = Vec::new();
            for query in queries {
                if query.path.trim().is_empty() {
                    errors.push(BatchErrorItem {
                        requested_path: query.path.clone(),
                        code: ErrorCode::InvalidPath,
                        message: "path must not be blank".to_owned(),
                    });
                    continue;
                }
                match resolve_file_catalog_entry(conn, &project_root, &query.path, &query.language)
                {
                    Ok(Some(entry)) => match entry.into_resolved_file() {
                        Ok(file) => {
                            if file.briefing_blocked.is_some() {
                                briefing_blocked.push(query.path);
                            } else {
                                resolved.push(BatchResolvedItem {
                                    requested_path: query.path,
                                    entity_id: file.entity_id,
                                    content_hash: file.content_hash,
                                    canonical_path: file.canonical_path,
                                    language: file.language,
                                });
                            }
                        }
                        Err(err) => errors.push(classify_batch_error(query.path, &err)),
                    },
                    Ok(None) => not_found.push(query.path),
                    Err(err) => errors.push(classify_batch_error(query.path, &err)),
                }
            }
            Ok::<_, StorageError>(BatchFileResponse {
                resolved,
                not_found,
                briefing_blocked,
                errors,
            })
        })
        .await;
    match catalog_result {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => json_read_error(&err),
    }
}

async fn post_files_resolve(
    State(state): State<AppState>,
    body: Result<Json<ResolveFilesRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"paths\": [...]}",
        );
    };
    if request.paths.len() > RESOLVE_MAX_PATHS {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "paths[] exceeds the per-batch maximum of 1000 entries",
        );
    }
    let project_root = state.project_root.clone();
    let paths = request.paths;
    let catalog_result = state
        .readers
        .with_reader(move |conn| {
            let results = paths
                .into_iter()
                .map(|query| {
                    let response =
                        resolve_file_query_item(conn, &project_root, &query.path, &query.language);
                    ResolveFileResult {
                        path: query.path,
                        response,
                    }
                })
                .collect();
            Ok::<_, StorageError>(ResolveFilesResponse { results })
        })
        .await;
    match catalog_result {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => json_read_error(&err),
    }
}

// ── Call-graph linkages (Wave 0 / WS2) ──────────────────────────────────────
//
// Thin HTTP wrappers over `clarion_storage::call_edges_targeting` (callers) and
// `call_edges_from` (callees). Aggregated per neighbour entity: `call_site_count`
// is the number of call sites (across all returned confidence tiers) and
// `confidence` is the STRONGEST tier present (resolved > ambiguous > inferred) —
// a real resolved site is reported as resolved even if weaker sites also exist.
// Inferred-tier results reflect only already-persisted inferred edges; the
// read-only HTTP surface does not materialise inferred edges on demand (that is
// an MCP, writer-backed capability). Routes are HMAC-gated like `/api/v1/files`.
//
// briefing-block policy: the QUERIED entity is refused with 403 when blocked
// (mirrors `get_file`, honouring the federation "refuse blocked entities to
// siblings" contract). NEIGHBOUR entities are NOT filtered — the linkage payload
// is structural topology (ids + counts + tier), consistent with the MCP
// call-graph surface (`callers_of`/`neighborhood`), which does not filter
// briefing-blocked neighbours either.

#[derive(Debug, Serialize)]
struct LinkageEntry {
    entity_id: String,
    confidence: String,
    call_site_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkageQuery {
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkageBatchRequest {
    entity_ids: Vec<String>,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Which side of the `calls` graph a linkage query walks.
#[derive(Debug, Clone, Copy)]
enum LinkageDirection {
    /// Inbound: who calls the entity (neighbour = caller `from_id`).
    Callers,
    /// Outbound: what the entity calls (neighbour = callee `to_id`).
    Callees,
}

impl LinkageDirection {
    /// The JSON field name the page of entries is serialised under.
    fn field(self) -> &'static str {
        match self {
            LinkageDirection::Callers => "callers",
            LinkageDirection::Callees => "callees",
        }
    }
}

/// Map the `confidence` query value to the storage layer's `max_confidence`
/// CEILING (`confidence_allowed` is `actual <= max`). `all` and `inferred` both
/// admit every tier; `ambiguous` admits resolved+ambiguous; `resolved` admits
/// only resolved. Returns `None` for an unrecognised value (→ 400).
fn parse_max_confidence(raw: Option<&str>) -> Option<EdgeConfidence> {
    match raw.unwrap_or("all") {
        "all" | "inferred" => Some(EdgeConfidence::Inferred),
        "ambiguous" => Some(EdgeConfidence::Ambiguous),
        "resolved" => Some(EdgeConfidence::Resolved),
        _ => None,
    }
}

/// Rank for "strongest tier" selection: lower is more certain.
fn confidence_rank(confidence: EdgeConfidence) -> u8 {
    match confidence {
        EdgeConfidence::Resolved => 0,
        EdgeConfidence::Ambiguous => 1,
        EdgeConfidence::Inferred => 2,
    }
}

/// Aggregate raw call-edge matches into per-neighbour [`LinkageEntry`] rows,
/// deterministically ordered by `entity_id`. The neighbour is the `from_id`
/// for callers and the `to_id` for callees (`call_edges_from` already expands an
/// ambiguous edge into one match per candidate callee, so the callee count
/// reflects candidate breadth — faithful to the wrapped query).
fn aggregate_linkages(matches: &[CallEdgeMatch], direction: LinkageDirection) -> Vec<LinkageEntry> {
    use std::collections::BTreeMap;
    let mut acc: BTreeMap<&str, (EdgeConfidence, usize)> = BTreeMap::new();
    for edge in matches {
        let neighbour = match direction {
            LinkageDirection::Callers => edge.from_id.as_str(),
            LinkageDirection::Callees => edge.to_id.as_str(),
        };
        let slot = acc.entry(neighbour).or_insert((edge.confidence, 0));
        slot.1 += 1;
        if confidence_rank(edge.confidence) < confidence_rank(slot.0) {
            slot.0 = edge.confidence;
        }
    }
    acc.into_iter()
        .map(|(entity_id, (confidence, call_site_count))| LinkageEntry {
            entity_id: entity_id.to_owned(),
            confidence: confidence.as_str().to_owned(),
            call_site_count,
        })
        .collect()
}

/// Fetch + aggregate one entity's linkages on a pooled connection. Returns the
/// full (unpaginated) ordered entry list, distinguishing not-found / blocked.
fn linkages_for(
    conn: &rusqlite::Connection,
    entity_id: &str,
    direction: LinkageDirection,
    max_confidence: EdgeConfidence,
) -> Result<LinkageLookup, StorageError> {
    match entity_visibility(conn, entity_id)? {
        EntityVisibility::NotFound => Ok(LinkageLookup::NotFound),
        EntityVisibility::Blocked(_) => Ok(LinkageLookup::Blocked),
        EntityVisibility::Visible => {
            let matches = match direction {
                LinkageDirection::Callers => call_edges_targeting(conn, entity_id, max_confidence)?,
                LinkageDirection::Callees => call_edges_from(conn, entity_id, max_confidence)?,
            };
            Ok(LinkageLookup::Found(aggregate_linkages(
                &matches, direction,
            )))
        }
    }
}

enum LinkageLookup {
    NotFound,
    Blocked,
    Found(Vec<LinkageEntry>),
}

/// Single-entity linkage handler (callers or callees).
async fn linkage_single(
    state: &AppState,
    entity_id: String,
    query: Result<Query<LinkageQuery>, QueryRejection>,
    direction: LinkageDirection,
) -> Response {
    let Ok(Query(query)) = query else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "query parameters are invalid",
        );
    };
    let Some(max_confidence) = parse_max_confidence(query.confidence.as_deref()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "confidence must be one of: resolved, ambiguous, inferred, all",
        );
    };
    let limit = query
        .limit
        .unwrap_or(LINKAGE_LIMIT_DEFAULT)
        .min(LINKAGE_LIMIT_MAX) as usize;
    let offset = query.offset.unwrap_or(0) as usize;

    let lookup_id = entity_id.clone();
    let result = state
        .readers
        .with_reader(move |conn| linkages_for(conn, &lookup_id, direction, max_confidence))
        .await;

    match result {
        Ok(LinkageLookup::NotFound) => json_error(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "entity is not known to Clarion",
        ),
        Ok(LinkageLookup::Blocked) => json_error(
            StatusCode::FORBIDDEN,
            ErrorCode::BriefingBlocked,
            "entity is briefing-blocked and cannot be exposed",
        ),
        Ok(LinkageLookup::Found(entries)) => {
            let total = entries.len();
            let page: Vec<LinkageEntry> = entries.into_iter().skip(offset).take(limit).collect();
            let truncated = offset.saturating_add(page.len()) < total;
            let body = serde_json::json!({
                "entity_id": entity_id,
                direction.field(): page,
                "total": total,
                "truncated": truncated,
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(err) => json_read_error(&err),
    }
}

/// Batch linkage handler. Returns `{ results: { entity_id: [LinkageEntry] } }`
/// for the VISIBLE requested ids only; not-found and briefing-blocked ids are
/// omitted (the caller diffs requested vs returned keys). `limit` caps entries
/// per entity; there is no offset on the batch surface.
async fn linkage_batch(
    state: &AppState,
    body: Result<Json<LinkageBatchRequest>, axum::extract::rejection::JsonRejection>,
    direction: LinkageDirection,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"entity_ids\": [...]}",
        );
    };
    if request.entity_ids.len() > LINKAGES_BATCH_MAX {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::BatchTooLarge,
            "entity_ids[] exceeds the per-batch maximum of 50 entries",
        );
    }
    let Some(max_confidence) = parse_max_confidence(request.confidence.as_deref()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "confidence must be one of: resolved, ambiguous, inferred, all",
        );
    };
    let limit = request
        .limit
        .unwrap_or(LINKAGE_LIMIT_DEFAULT)
        .min(LINKAGE_LIMIT_MAX) as usize;
    let entity_ids = request.entity_ids;

    let result = state
        .readers
        .with_reader(move |conn| {
            // BTreeMap → deterministic JSON object key order.
            let mut results: std::collections::BTreeMap<String, Vec<LinkageEntry>> =
                std::collections::BTreeMap::new();
            for entity_id in entity_ids {
                if let LinkageLookup::Found(mut entries) =
                    linkages_for(conn, &entity_id, direction, max_confidence)?
                {
                    entries.truncate(limit);
                    results.insert(entity_id, entries);
                }
            }
            Ok::<_, StorageError>(results)
        })
        .await;

    match result {
        Ok(results) => (
            StatusCode::OK,
            Json(serde_json::json!({ "results": results })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

async fn get_callers(
    State(state): State<AppState>,
    Path(entity_id): Path<String>,
    query: Result<Query<LinkageQuery>, QueryRejection>,
) -> Response {
    linkage_single(&state, entity_id, query, LinkageDirection::Callers).await
}

async fn get_callees(
    State(state): State<AppState>,
    Path(entity_id): Path<String>,
    query: Result<Query<LinkageQuery>, QueryRejection>,
) -> Response {
    linkage_single(&state, entity_id, query, LinkageDirection::Callees).await
}

async fn post_callers_batch(
    State(state): State<AppState>,
    body: Result<Json<LinkageBatchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    linkage_batch(&state, body, LinkageDirection::Callers).await
}

async fn post_callees_batch(
    State(state): State<AppState>,
    body: Result<Json<LinkageBatchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    linkage_batch(&state, body, LinkageDirection::Callees).await
}

// ── SEI identity resolution (Wave 1 / WS1, ADR-038 §4 / SEI spec §4) ─────────
//
// `resolve(locator)`     → the alive SEI for a locator, or { alive: false }.
// `resolve_sei(sei)`     → the alive record, or { alive: false, lineage }.
// `lineage(sei)`         → the ordered event list.
// All HMAC-gated like `/api/v1/files`. Identity is read from `sei_bindings`
// (the source of truth); `entities` is joined only for `content_hash`.
//
// REQ-F-02 (fail-closed): `resolve(locator)` MUST reject an SEI-shaped input
// (reserved `clarion:eid:` prefix) — never silently mis-resolve. A colon-count
// check is insufficient (an SEI carries the same two colons a locator does), so
// the rejection keys on the reserved prefix. This is what makes the idempotent,
// resumable cross-tool backfill safe (an already-migrated SEI is rejected).

/// Max locators in one `resolve:batch` request (mirrors `BATCH_MAX_QUERIES`).
const IDENTITY_BATCH_MAX: usize = 256;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveLocatorRequest {
    locator: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveLocatorBatchRequest {
    locators: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SeiLineageEventBody {
    event: String,
    old_locator: Option<String>,
    new_locator: Option<String>,
    run_id: String,
    recorded_at: String,
}

/// Validate a locator for `resolve` (REQ-F-02). Rejects the reserved SEI prefix
/// and anything that is not a 3-segment `{plugin}:{kind}:{qualname}` with no
/// empty segment. Returns the documented client message on rejection.
fn validate_locator(locator: &str) -> Result<(), &'static str> {
    if is_reserved_sei(locator) {
        return Err("not a valid locator: input is an SEI (reserved clarion:eid: prefix)");
    }
    let segments: Vec<&str> = locator.splitn(3, ':').collect();
    if segments.len() != 3 || segments.iter().any(|s| s.is_empty()) {
        return Err("not a valid locator: expected {plugin}:{kind}:{qualname}");
    }
    Ok(())
}

fn lineage_rows_to_body(rows: Vec<clarion_storage::SeiLineageRow>) -> Vec<SeiLineageEventBody> {
    rows.into_iter()
        .map(|r| SeiLineageEventBody {
            event: r.event,
            old_locator: r.old_locator,
            new_locator: r.new_locator,
            run_id: r.run_id,
            recorded_at: r.recorded_at,
        })
        .collect()
}

async fn post_identity_resolve(
    State(state): State<AppState>,
    body: Result<Json<ResolveLocatorRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"locator\": \"...\"}",
        );
    };
    if let Err(message) = validate_locator(&request.locator) {
        return json_error(StatusCode::BAD_REQUEST, ErrorCode::InvalidPath, message);
    }
    let locator = request.locator;
    let result = state
        .readers
        .with_reader(move |conn| resolve_locator(conn, &locator))
        .await;
    match result {
        Ok(Some(record)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": record.sei,
                "current_locator": record.current_locator,
                "content_hash": record.content_hash,
                "alive": true,
            })),
        )
            .into_response(),
        Ok(None) => (StatusCode::OK, Json(serde_json::json!({ "alive": false }))).into_response(),
        Err(err) => json_read_error(&err),
    }
}

async fn post_identity_resolve_batch(
    State(state): State<AppState>,
    body: Result<Json<ResolveLocatorBatchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"locators\": [...]}",
        );
    };
    if request.locators.len() > IDENTITY_BATCH_MAX {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::BatchTooLarge,
            "locators[] exceeds the per-batch maximum of 256 entries",
        );
    }
    let locators = request.locators;
    let result = state
        .readers
        .with_reader(move |conn| {
            // BTreeMap → deterministic key order. Invalid (SEI-shaped or
            // malformed) inputs are collected separately, never mis-resolved.
            let mut resolved: std::collections::BTreeMap<String, serde_json::Value> =
                std::collections::BTreeMap::new();
            let mut invalid: Vec<String> = Vec::new();
            let mut not_found: Vec<String> = Vec::new();
            for locator in locators {
                if validate_locator(&locator).is_err() {
                    invalid.push(locator);
                    continue;
                }
                match resolve_locator(conn, &locator)? {
                    Some(record) => {
                        resolved.insert(
                            locator,
                            serde_json::json!({
                                "sei": record.sei,
                                "current_locator": record.current_locator,
                                "content_hash": record.content_hash,
                                "alive": true,
                            }),
                        );
                    }
                    None => not_found.push(locator),
                }
            }
            Ok::<_, StorageError>((resolved, invalid, not_found))
        })
        .await;
    match result {
        Ok((resolved, invalid, not_found)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "resolved": resolved,
                "invalid": invalid,
                "not_found": not_found,
            })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

async fn get_identity_sei(State(state): State<AppState>, Path(sei): Path<String>) -> Response {
    let lookup_sei = sei.clone();
    let result = state
        .readers
        .with_reader(move |conn| resolve_sei(conn, &lookup_sei))
        .await;
    match result {
        Ok(SeiLookupResult::Alive(record)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": sei,
                "current_locator": record.current_locator,
                "content_hash": record.content_hash,
                "alive": true,
            })),
        )
            .into_response(),
        Ok(SeiLookupResult::NotAlive { lineage }) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": sei,
                "alive": false,
                "lineage": lineage_rows_to_body(lineage),
            })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

async fn get_identity_lineage(State(state): State<AppState>, Path(sei): Path<String>) -> Response {
    let lookup_sei = sei.clone();
    let result = state
        .readers
        .with_reader(move |conn| sei_lineage(conn, &lookup_sei))
        .await;
    match result {
        Ok(rows) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": sei,
                "lineage": lineage_rows_to_body(rows),
            })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

/// Exact-tier Wardline qualname resolve (ADR-036, W.4). Takes a batch of
/// PRE-COMPOSED dotted qualnames that Wardline has already shaped to
/// byte-match Clarion's `canonical_qualified_name`; resolution is the direct
/// existence lookup in `clarion_storage::resolve_wardline_qualnames`. No
/// `&file=` disambiguator, no normalization — the generic resolve oracle
/// remains deferred.
async fn post_wardline_resolve(
    State(state): State<AppState>,
    body: Result<Json<ResolveRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.qualnames.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many qualnames in one request",
        );
    }
    // Move only `qualnames` into the reader closure; `project` was consumed by
    // the guard above. `with_reader` runs the lookup on a pooled connection.
    let qualnames = req.qualnames;
    let result = state
        .readers
        .with_reader(move |conn| clarion_storage::resolve_wardline_qualnames(conn, &qualnames))
        .await;
    match result {
        Ok(pairs) => {
            let mut resolved = std::collections::BTreeMap::new();
            let mut unresolved = Vec::new();
            for (qualname, resolution) in pairs {
                match resolution.into_entity_id() {
                    Some(id) => {
                        resolved.insert(qualname, id);
                    }
                    None => unresolved.push(qualname),
                }
            }
            (
                StatusCode::OK,
                Json(ResolveResponse {
                    resolved,
                    unresolved,
                }),
            )
                .into_response()
        }
        Err(err) => json_read_error(&err),
    }
}

/// Wardline taint-fact batch WRITE (ADR-036, W.2). Disabled by default; only
/// reachable when `serve.http.wardline_taint_write` spawned the optional
/// writer-actor (`state.taint_writer` is `Some`). Resolution is the SAME
/// exact-tier oracle the resolve endpoint uses; `wardline_json` is opaque and
/// stored verbatim. Facts whose qualname does not resolve are reported in
/// `unresolved_qualnames` and silently skipped (not an error).
async fn post_wardline_taint_facts(
    State(state): State<AppState>,
    body: Result<Json<WriteTaintFactsRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // Disabled-by-default guard fires BEFORE body parsing: a `None` writer
    // means the API is off regardless of payload shape.
    let Some(writer) = state.taint_writer.clone() else {
        return json_error(
            StatusCode::FORBIDDEN,
            ErrorCode::WriteDisabled,
            "taint-fact write API is disabled (set serve.http.wardline_taint_write: true)",
        );
    };
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.facts.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many facts in one request",
        );
    }

    // Resolve every qualname EXACT-only on the reader pool, in input order, in
    // one pooled-connection checkout. Zip results back onto the facts by index
    // (NOT a qualname->id map) so duplicate qualnames are handled correctly. In
    // the SAME checkout, batch-resolve the alive SEI for every resolved locator
    // (one chunked `IN`, not an N+1 of point lookups) so each fact can be
    // stamped with its rename-stable SEI key (T3.4).
    let qualnames: Vec<String> = req.facts.iter().map(|f| f.qualname.clone()).collect();
    let resolution = state
        .readers
        .with_reader(move |conn| {
            let resolved = clarion_storage::resolve_wardline_qualnames(conn, &qualnames)?;
            let locators: Vec<String> = resolved
                .iter()
                .filter_map(|(_, r)| r.entity_id().map(str::to_owned))
                .collect();
            let seis = clarion_storage::seis_for_locators(conn, &locators)?;
            Ok::<_, clarion_storage::StorageError>((resolved, seis))
        })
        .await;
    let (resolved, seis_by_locator) = match resolution {
        Ok(pair) => pair,
        Err(err) => return json_read_error(&err),
    };

    let batch_scan_id = req.scan_id.clone();
    let updated_at = iso8601_now();
    let mut written = 0_usize;
    let mut unresolved_qualnames = Vec::new();
    for (fact, (_, res)) in req.facts.into_iter().zip(resolved) {
        let Some(entity_id) = res.into_entity_id() else {
            unresolved_qualnames.push(fact.qualname);
            continue;
        };
        // SEI key: a caller-supplied value wins (opaque, verbatim); otherwise
        // the alive binding resolved for this locator. `None` on a pre-SEI DB /
        // unbound locator — the fact is still written, locator-keyed only.
        let sei = fact
            .sei
            .clone()
            .or_else(|| seis_by_locator.get(&entity_id).cloned());
        let taint_fact = clarion_storage::TaintFact {
            entity_id,
            // Opaque + byte-verbatim: `RawValue::get()` returns the original
            // bytes of the blob exactly as the client sent them (no key
            // reordering). Do NOT parse out scan_id/content_hash from inside
            // the blob; do NOT validate it.
            wardline_json: fact.wardline_json.get().to_owned(),
            scan_id: fact.scan_id.or_else(|| batch_scan_id.clone()),
            content_hash_at_compute: fact.content_hash_at_compute.clone(),
            updated_at: updated_at.clone(),
            sei,
        };
        let (ack_tx, ack_rx) = oneshot::channel();
        let cmd = clarion_storage::WriterCmd::UpsertWardlineTaintFact {
            fact: Box::new(taint_fact),
            ack: ack_tx,
        };
        if writer.send(cmd).await.is_err() {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::StorageError,
                "taint-fact writer is unavailable",
            );
        }
        match ack_rx.await {
            Ok(Ok(())) => written += 1,
            Ok(Err(err)) => {
                log_taint_write_error(&err);
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "taint-fact write failed",
                );
            }
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "taint-fact writer dropped the response channel",
                );
            }
        }
    }

    (
        StatusCode::OK,
        Json(WriteTaintFactsResponse {
            written,
            unresolved_qualnames,
        }),
    )
        .into_response()
}

/// Shared read builder for the GET and `:batch-get` taint-fact endpoints.
/// Runs ALL DB work and the live file hashing inside ONE pooled-connection
/// checkout (the `with_reader` closure is the blocking context). For each
/// qualname:
///
/// - resolve exact-tier → entity id (unresolved → `exists: false`);
/// - `get_taint_facts` for the resolved ids; map back by entity id;
/// - for rows that exist, parse the stored blob byte-faithfully via
///   `RawValue::from_string` (W.2 wrote it from a `RawValue`, so it
///   round-trips) and derive `current_content_hash` live from the row's
///   `source_file_path` via `clarion_storage::current_file_hash`.
///
/// File hashing is DEDUPED per request by `source_file_path`: a chain-walk
/// batch hits many functions sharing one file, and a 425k-LOC project must
/// not re-hash the same file N times. A deleted/renamed/unreadable file →
/// `current_content_hash: None` (a stale signal, not a 500).
///
/// Returns `Err(Response)` only when the DB read itself fails; per-qualname
/// "not found" is conveyed in-band via `exists: false`.
async fn respond_taint_facts(
    state: &AppState,
    qualnames: Vec<String>,
) -> Result<Vec<TaintFactView>, Response> {
    let project_root = state.project_root.clone();
    let result = state
        .readers
        .with_reader(move |conn| {
            // 1. Resolve every qualname (exact tier), in input order.
            let resolved = clarion_storage::resolve_wardline_qualnames(conn, &qualnames)?;

            // 2. Fetch facts for the resolved entity ids; map back by id.
            let entity_ids: Vec<String> = resolved
                .iter()
                .filter_map(|(_, r)| r.entity_id().map(str::to_owned))
                .collect();
            let rows = clarion_storage::get_taint_facts(conn, &entity_ids)?;
            let by_entity: std::collections::HashMap<String, clarion_storage::TaintFactRow> = rows
                .into_iter()
                .map(|row| (row.entity_id.clone(), row))
                .collect();

            // 3. Build a view per qualname, deduping file hashing by path.
            let mut file_hash_cache: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            let mut views = Vec::with_capacity(resolved.len());
            for (qualname, resolution) in resolved {
                let view = match resolution
                    .into_entity_id()
                    .and_then(|id| by_entity.get(&id))
                {
                    Some(row) => {
                        // Byte-faithful: the stored string is exactly what W.2
                        // wrote from a RawValue, so it re-parses. A parse error
                        // is a storage-integrity failure, not a 404.
                        let wardline_json =
                            serde_json::value::RawValue::from_string(row.wardline_json.clone())
                                .map_err(|e| {
                                    StorageError::Corruption(format!(
                                        "stored wardline_json for {} is not valid JSON: {e}",
                                        row.entity_id
                                    ))
                                })?;
                        let current_content_hash = match &row.source_file_path {
                            Some(path) => file_hash_cache
                                .entry(path.clone())
                                .or_insert_with(|| {
                                    clarion_storage::current_file_hash(&project_root, path)
                                })
                                .clone(),
                            None => None,
                        };
                        TaintFactView {
                            qualname,
                            wardline_json: Some(wardline_json),
                            current_content_hash,
                            exists: true,
                        }
                    }
                    // Unresolved qualname OR resolved-but-no-stored-fact.
                    _ => TaintFactView {
                        qualname,
                        wardline_json: None,
                        current_content_hash: None,
                        exists: false,
                    },
                };
                views.push(view);
            }
            Ok(views)
        })
        .await;
    result.map_err(|err| json_read_error(&err))
}

/// Single taint-fact READ (ADR-036, W.3). Reads only — served regardless of
/// `state.taint_writer` (the write API may be disabled). Unknown qualname →
/// `exists: false` with no `wardline_json`.
async fn get_wardline_taint_fact(
    State(state): State<AppState>,
    query: Result<Query<TaintFactQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "query parameters are invalid",
        );
    };
    if let Some(resp) = state.reject_project_mismatch(&query.project) {
        return resp;
    }
    if query.qualname.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "qualname query parameter must not be blank",
        );
    }
    match respond_taint_facts(&state, vec![query.qualname]).await {
        Ok(mut views) => {
            // Exactly one input qualname → exactly one view.
            let view = views.pop().unwrap_or(TaintFactView {
                qualname: String::new(),
                wardline_json: None,
                current_content_hash: None,
                exists: false,
            });
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(resp) => resp,
    }
}

/// Batch taint-fact READ (ADR-036, W.3). One DB checkout + per-request file
/// hash dedup serves the chain-walk batch. Reads only — served regardless of
/// `state.taint_writer`.
async fn post_wardline_taint_facts_batch_get(
    State(state): State<AppState>,
    body: Result<Json<BatchGetRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.qualnames.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many qualnames in one request",
        );
    }
    match respond_taint_facts(&state, req.qualnames).await {
        Ok(views) => (StatusCode::OK, Json(views)).into_response(),
        Err(resp) => resp,
    }
}

/// Shared read builder for the read-by-SEI taint-fact endpoint (T3.4).
/// Mirrors [`respond_taint_facts`] but keys on the opaque SEI: for each SEI,
/// fetch the most-recent SEI-tagged fact (regardless of the locator it was
/// written under), parse the stored blob byte-faithfully, and derive the live
/// whole-file `current_content_hash` from the fact's `source_file_path`. File
/// hashing is DEDUPED per request by path. All DB work + hashing run in ONE
/// pooled-connection checkout. A SEI with no stored fact → `exists: false`.
async fn respond_taint_facts_by_sei(
    state: &AppState,
    seis: Vec<String>,
) -> Result<Vec<TaintFactBySeiView>, Response> {
    let project_root = state.project_root.clone();
    let result = state
        .readers
        .with_reader(move |conn| {
            // Most-recent fact per SEI (rename-stable lookup).
            let rows = clarion_storage::get_taint_facts_by_sei(conn, &seis)?;
            let by_sei: std::collections::HashMap<String, clarion_storage::TaintFactRow> = rows
                .into_iter()
                .filter_map(|row| row.sei.clone().map(|sei| (sei, row)))
                .collect();

            let mut file_hash_cache: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            let mut views = Vec::with_capacity(seis.len());
            // Emit one view per input SEI, in input order. A duplicate input
            // SEI yields a duplicate view (input shape is the client's).
            for sei in seis {
                let view = match by_sei.get(&sei) {
                    Some(row) => {
                        // Byte-faithful re-parse: the stored string is exactly
                        // what the write path persisted from a RawValue. A parse
                        // error is a storage-integrity failure, not a 404.
                        let wardline_json =
                            serde_json::value::RawValue::from_string(row.wardline_json.clone())
                                .map_err(|e| {
                                    StorageError::Corruption(format!(
                                        "stored wardline_json for {} is not valid JSON: {e}",
                                        row.entity_id
                                    ))
                                })?;
                        let current_content_hash = match &row.source_file_path {
                            Some(path) => file_hash_cache
                                .entry(path.clone())
                                .or_insert_with(|| {
                                    clarion_storage::current_file_hash(&project_root, path)
                                })
                                .clone(),
                            None => None,
                        };
                        TaintFactBySeiView {
                            sei,
                            wardline_json: Some(wardline_json),
                            current_content_hash,
                            exists: true,
                        }
                    }
                    None => TaintFactBySeiView {
                        sei,
                        wardline_json: None,
                        current_content_hash: None,
                        exists: false,
                    },
                };
                views.push(view);
            }
            Ok(views)
        })
        .await;
    result.map_err(|err| json_read_error(&err))
}

/// Read taint facts by SEI (T3.4). The rename-survival surface: a fact written
/// under a former locator is retrievable by its stable SEI after a rename.
/// Reads only — served regardless of `state.taint_writer`. Opaque inputs: no
/// locator-shape validation, blank SEIs are dropped.
async fn post_wardline_taint_facts_batch_get_by_sei(
    State(state): State<AppState>,
    body: Result<Json<BatchGetBySeiRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.seis.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many seis in one request",
        );
    }
    match respond_taint_facts_by_sei(&state, req.seis).await {
        Ok(views) => (StatusCode::OK, Json(views)).into_response(),
        Err(resp) => resp,
    }
}

fn log_taint_write_error(err: &StorageError) {
    let error_chain = format_error_chain(err);
    tracing::dispatcher::with_default(&HTTP_ERROR_DISPATCH, || {
        tracing::error!(
            error_chain = %error_chain,
            "HTTP /api/wardline/taint-facts write failed"
        );
    });
}

/// ISO-8601 UTC "now" with millisecond precision (`YYYY-MM-DDTHH:MM:SS.sssZ`),
/// matching the caller-side timestamps `clarion analyze` stamps onto run rows.
fn iso8601_now() -> String {
    use time::macros::format_description;
    const ISO8601_MILLIS_UTC: &[time::format_description::FormatItem<'_>] =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    time::OffsetDateTime::now_utc()
        .format(ISO8601_MILLIS_UTC)
        .expect("fixed ISO-8601 format description should format")
}

fn resolve_file_query_item(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    path: &str,
    language: &str,
) -> ResolveFileItemResponse {
    if path.trim().is_empty() {
        return resolve_error_response(
            ResolveFileStatus::Error,
            ErrorCode::InvalidPath,
            "path must not be blank",
        );
    }
    match resolve_file_catalog_entry(conn, project_root, path, language) {
        Ok(Some(entry)) => match entry.into_resolved_file() {
            Ok(file) => {
                if file.briefing_blocked.is_some() {
                    resolve_error_response(
                        ResolveFileStatus::Blocked,
                        ErrorCode::BriefingBlocked,
                        "entity is briefing-blocked and cannot be exposed",
                    )
                } else {
                    ResolveFileItemResponse {
                        status: ResolveFileStatus::Resolved,
                        body: serde_json::to_value(FileResponse {
                            entity_id: file.entity_id,
                            content_hash: file.content_hash,
                            canonical_path: file.canonical_path,
                            language: file.language,
                        })
                        .expect("FileResponse serializes"),
                    }
                }
            }
            Err(err) => resolve_read_error_response(&err),
        },
        Ok(None) => resolve_error_response(
            ResolveFileStatus::NotFound,
            ErrorCode::NotFound,
            "file is not known to Clarion",
        ),
        Err(err) => resolve_read_error_response(&err),
    }
}

fn resolve_read_error_response(err: &StorageError) -> ResolveFileItemResponse {
    let error = classify_read_error(err);
    resolve_error_response(ResolveFileStatus::Error, error.code, error.message)
}

fn resolve_error_response(
    status: ResolveFileStatus,
    code: ErrorCode,
    message: &str,
) -> ResolveFileItemResponse {
    ResolveFileItemResponse {
        status,
        body: serde_json::to_value(ErrorResponse {
            error: message.to_owned(),
            code,
        })
        .expect("ErrorResponse serializes"),
    }
}

fn classify_batch_error(requested_path: String, err: &StorageError) -> BatchErrorItem {
    let classified = classify_read_error(err);
    BatchErrorItem {
        requested_path,
        code: classified.code,
        message: classified.message.to_owned(),
    }
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
        // A stored row that failed an integrity check is Clarion's fault, not
        // the client's: 500 + logged (via `json_read_error`), never a 4xx that
        // blames the caller's request. A federation client routing on `code`
        // must see STORAGE_ERROR, not INVALID_PATH.
        StorageError::Corruption(_) => ReadError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: ErrorCode::StorageError,
            message: "stored data failed an integrity check",
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
        // STO-02 (ADR-035): the on-disk file is not a Clarion database, or
        // was written by a newer build. Either condition is fatal for the
        // server; the writer-actor refuses to spawn against it. Surfacing
        // 500 here is defensive — in practice the HTTP API does not open
        // its own writer, but the reader pool can encounter the same file
        // header mismatches and we want a clear distinct response code.
        StorageError::ForeignDatabase { .. } | StorageError::FutureUserVersion { .. } => {
            ReadError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: ErrorCode::StorageError,
                message: "file lookup storage rejected database header",
            }
        }
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
            // Route-agnostic: this helper is shared by every read surface
            // routed through `json_read_error` (files lookup AND the wardline
            // taint-fact reads). A storage-corruption breadcrumb filed under a
            // fixed "/api/v1/files" label is one an operator won't find.
            "HTTP read API storage error"
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

    // REQ-F-02 (ADR-038 §4): `resolve(locator)` must reject an SEI-shaped input
    // by the RESERVED PREFIX, not a colon count — an SEI carries the same two
    // colons a `{plugin}:{kind}:{qualname}` locator does.
    #[test]
    fn validate_locator_rejects_reserved_sei_prefix() {
        // A real SEI has the same colon count as a locator — only the reserved
        // prefix distinguishes it, which is exactly what the rejection keys on.
        let err = validate_locator("clarion:eid:0123456789abcdef0123456789abcdef")
            .expect_err("an SEI-shaped input must be rejected");
        assert!(err.contains("not a valid locator"), "message: {err}");
    }

    #[test]
    fn validate_locator_rejects_malformed_locators() {
        assert!(validate_locator("python:function").is_err(), "two segments");
        assert!(validate_locator("python::qualname").is_err(), "empty kind");
        assert!(validate_locator("::").is_err(), "all empty");
        assert!(validate_locator("").is_err(), "empty string");
    }

    #[test]
    fn validate_locator_accepts_well_formed_locator() {
        assert!(validate_locator("python:function:auth.tokens.refresh").is_ok());
        // A qualname containing colons is fine (splitn(3) keeps the tail intact).
        assert!(validate_locator("python:function:a.b::c").is_ok());
    }

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
                .header("X-Loom-Component", "clarion:deadbeef")
                .body(Body::from(oversize))
                .expect("request");

            // Drive `require_hmac_identity` directly. axum's `Next` is not
            // publicly constructible from outside middleware composition, so
            // we exercise the function via a single-route Router with the
            // middleware layered on top.
            let app: Router<()> = Router::new()
                .route("/api/v1/files/batch", post(never_called))
                .layer(axum::middleware::from_fn(|request, next| async move {
                    require_hmac_identity("test-secret", HTTP_BODY_LIMIT_BYTES, request, next).await
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

    /// Build an `AppState` over a fresh temp file DB with migrations applied
    /// and the given entity ids seeded as full `entities` rows. Returns the
    /// state plus the `TempDir` guard (drop it last). The state carries an
    /// HMAC `identity_secret` so the protected/wardline routes are exercised
    /// with real signature verification.
    fn wardline_resolve_test_state(
        secret: &str,
        seed_ids: &[&str],
    ) -> (AppState, tempfile::TempDir) {
        use clarion_storage::ReaderPool;
        use clarion_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");
        for id in seed_ids {
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, created_at, updated_at \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    id.rsplit('.').next().unwrap_or(id),
                    "{}",
                    "deadbeef",
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .expect("seed entity row");
        }
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000004")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            taint_writer: None,
        };
        (state, tempdir)
    }

    fn hmac_request(
        secret: &str,
        method: &str,
        path_and_query: &str,
        body: &[u8],
    ) -> axum::http::Request<axum::body::Body> {
        let signature = component_hmac_hex(secret.as_bytes(), method, path_and_query, body);
        axum::http::Request::builder()
            .method(method)
            .uri(path_and_query)
            .header("X-Loom-Component", format!("clarion:{signature}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_vec()))
            .expect("build request")
    }

    #[tokio::test]
    async fn wardline_resolve_returns_exact_matches_and_unresolved() {
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";
        let (state, _tempdir) = wardline_resolve_test_state(secret, &["python:function:a.b.c"]);
        let body = br#"{"qualnames":["a.b.c","x.y.z"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parsed["resolved"]["a.b.c"], "python:function:a.b.c");
        assert_eq!(
            parsed["resolved"]
                .as_object()
                .expect("resolved object")
                .len(),
            1,
            "only exact matches appear in resolved: {parsed}"
        );
        assert_eq!(parsed["unresolved"], serde_json::json!(["x.y.z"]));
    }

    #[tokio::test]
    async fn wardline_resolve_rejects_project_guard_mismatch() {
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";
        let (state, _tempdir) = wardline_resolve_test_state(secret, &[]);
        // A non-empty `project` that does not match the served project-root
        // directory name must be rejected with 403 PROJECT_MISMATCH.
        let body = br#"{"project":"some-other-project","qualnames":["a.b.c"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "PROJECT_MISMATCH");
    }

    /// Build a write-enabled `AppState` over a fresh temp migrated DB with the
    /// given entity ids seeded, plus a REAL writer-actor. Returns the state, the
    /// `db_path` (for verification on a fresh connection), the `Writer` handle
    /// (drop it last so the actor can flush), and the `TempDir` guard. The
    /// actor runs via `Writer::spawn`'s `spawn_blocking`, so the caller MUST be
    /// on a tokio runtime (`#[tokio::test]`).
    fn wardline_write_test_state(
        secret: &str,
        seed_ids: &[&str],
    ) -> (
        AppState,
        std::path::PathBuf,
        clarion_storage::Writer,
        tempfile::TempDir,
    ) {
        use clarion_storage::ReaderPool;
        use clarion_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");
        for id in seed_ids {
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, created_at, updated_at \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    id.rsplit('.').next().unwrap_or(id),
                    "{}",
                    "deadbeef",
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .expect("seed entity row");
        }
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let (writer, _join) = clarion_storage::Writer::spawn(
            db_path.clone(),
            clarion_storage::DEFAULT_BATCH_SIZE,
            clarion_storage::DEFAULT_CHANNEL_CAPACITY,
        )
        .expect("spawn taint writer-actor");
        // The join handle is dropped here: the test reads the DB on a fresh
        // connection AFTER awaiting per-upsert acks, which confirm durability
        // (query_time_write auto-commits before the ack fires).
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000005")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            taint_writer: Some(writer.sender()),
        };
        (state, db_path, writer, tempdir)
    }

    fn read_taint_blob(db_path: &std::path::Path, entity_id: &str) -> Option<String> {
        let conn = rusqlite::Connection::open(db_path).expect("open verification conn");
        conn.query_row(
            "SELECT wardline_json FROM wardline_taint_facts WHERE entity_id = ?1",
            rusqlite::params![entity_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    #[tokio::test]
    async fn wardline_taint_write_disabled_returns_403() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        // `wardline_resolve_test_state` builds a state with `taint_writer: None`.
        let (state, _tempdir) = wardline_resolve_test_state(secret, &[]);
        let body = br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "WRITE_DISABLED");
    }

    #[tokio::test]
    async fn wardline_taint_write_persists_resolved_and_reports_unresolved() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);
        // The resolved blob's keys are in NON-alphabetical order
        // (`b` before `a`, `schema` before `ret`). Under the old
        // `Value::to_string()` path serde's BTreeMap would re-emit them
        // alphabetized; `RawValue` preserves the original bytes exactly.
        let resolved_blob = r#"{"b":2,"a":1,"taint":{"ret":"RAW","schema":"w-1"}}"#;
        let body = format!(
            r#"{{"facts":[
            {{"qualname":"a.b.c","wardline_json":{resolved_blob}}},
            {{"qualname":"x.y.z","wardline_json":{{"v":2}}}}
        ]}}"#
        );
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body.as_bytes());

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["written"], 1);
        assert_eq!(parsed["unresolved_qualnames"], serde_json::json!(["x.y.z"]));

        // The ack we awaited inside the handler confirms durability; the blob
        // must round-trip BYTE-EXACT — key order preserved, NOT alphabetized.
        // This assertion fails under the old `Value::to_string()` path.
        let stored = read_taint_blob(&db_path, "python:function:a.b.c").expect("fact stored");
        assert_eq!(
            stored, resolved_blob,
            "wardline_json stored byte-verbatim (key order preserved)"
        );
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_replaces_per_entity() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);

        let send = |body: &'static [u8]| {
            let state = state.clone();
            async move {
                let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
                let response = router(state).oneshot(request).await.expect("oneshot");
                assert_eq!(response.status(), StatusCode::OK);
            }
        };
        send(br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#).await;
        send(br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":2}}]}"#).await;

        let stored = read_taint_blob(&db_path, "python:function:a.b.c").expect("fact stored");
        assert_eq!(
            stored,
            serde_json::json!({"v":2}).to_string(),
            "second write overwrites"
        );
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_rejects_project_guard_mismatch() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, writer, _tempdir) = wardline_write_test_state(secret, &[]);
        let body = br#"{"project":"some-other-project","facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "PROJECT_MISMATCH");
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_rejects_oversize_batch() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, writer, _tempdir) = wardline_write_test_state(secret, &[]);
        let facts: Vec<serde_json::Value> = (0..=WARDLINE_TAINT_BATCH_MAX)
            .map(
                |i| serde_json::json!({ "qualname": format!("pkg.mod.f{i}"), "wardline_json": {} }),
            )
            .collect();
        assert!(facts.len() > WARDLINE_TAINT_BATCH_MAX);
        let body = serde_json::to_vec(&serde_json::json!({ "facts": facts })).expect("json");
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", &body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "BATCH_TOO_LARGE");
        drop(writer);
    }

    /// Identity-guard regression lock for the wardline route group. All three
    /// wardline routes share ONE `require_http_identity_wardline` layer, so the
    /// mutating POST is a sufficient witness: if a wiring regression dropped the
    /// `.route_layer(...)`, an absent-header POST would reach the handler and
    /// return 403/200 — never 401. The trio pins:
    ///   - valid signature → clears the guard (403 `WRITE_DISABLED` on the
    ///     write-disabled state is downstream of auth, so it proves the guard
    ///     passed, independent of the write feature);
    ///   - wrong signature → 401 `UNAUTHENTICATED`;
    ///   - absent header → 401 `UNAUTHENTICATED` (the case that catches a dropped
    ///     `.route_layer`).
    #[tokio::test]
    async fn wardline_taint_write_enforces_identity() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let body = br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#;

        // (1) Valid signature clears the guard. Against the write-DISABLED state
        // (taint_writer: None) the handler then returns 403 WRITE_DISABLED,
        // which is downstream of auth — so reaching it proves the guard passed.
        let (state, _td1) = wardline_resolve_test_state(secret, &[]);
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a valid signature must clear the identity guard (403 is downstream of auth)"
        );

        // (2) Wrong signature → 401 UNAUTHENTICATED.
        let (state, _td2) = wardline_resolve_test_state(secret, &[]);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/wardline/taint-facts")
            .header("X-Loom-Component", "clarion:deadbeefdeadbeef")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_vec()))
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a wrong signature must be rejected with 401"
        );
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "UNAUTHENTICATED");

        // (3) Absent X-Loom-Component header → 401. This is the case that
        // catches a regression dropping the route_layer: with no guard, this
        // request would reach the handler and 403/200, not 401.
        let (state, _td3) = wardline_resolve_test_state(secret, &[]);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/wardline/taint-facts")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_vec()))
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an absent identity header must 401 — dropping the route_layer fails here"
        );
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "UNAUTHENTICATED");
    }

    /// Finding 3 (non-atomic batch): pins the invariant that makes partial
    /// persistence acceptable — a whole-batch re-post is idempotent. Posting a
    /// MULTI-fact batch twice converges to the same state: stable `written`, no
    /// row duplication, last-write-wins per entity. (Deterministic mid-batch
    /// fault injection has no seam in the writer-actor without a test-only hook
    /// in production code; idempotency is the contract-relevant invariant, and
    /// is exactly what `contracts.md` instructs clients to rely on after a 5xx.)
    #[tokio::test]
    async fn wardline_taint_write_batch_retry_is_idempotent() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c", "python:function:d.e.f"]);

        let body = br#"{"facts":[
            {"qualname":"a.b.c","wardline_json":{"v":1}},
            {"qualname":"d.e.f","wardline_json":{"v":2}}
        ]}"#;
        let post = |body: &'static [u8]| {
            let state = state.clone();
            async move {
                let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
                let response = router(state).oneshot(request).await.expect("oneshot");
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
                    .await
                    .expect("body");
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
            }
        };

        let first = post(body).await;
        assert_eq!(first["written"], 2);
        let second = post(body).await;
        assert_eq!(
            second["written"], 2,
            "a whole-batch re-post writes the same count"
        );

        // No duplication: exactly one row per entity, last-write-wins.
        assert_eq!(
            read_taint_blob(&db_path, "python:function:a.b.c").as_deref(),
            Some(r#"{"v":1}"#)
        );
        assert_eq!(
            read_taint_blob(&db_path, "python:function:d.e.f").as_deref(),
            Some(r#"{"v":2}"#)
        );
        let count = {
            let conn = rusqlite::Connection::open(&db_path).expect("verify conn");
            conn.query_row("SELECT COUNT(*) FROM wardline_taint_facts", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("count")
        };
        assert_eq!(count, 2, "re-post must not duplicate rows");
        drop(writer);
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

    /// The Wardline body-limit relocation is load-bearing: a >16 KiB body must
    /// be accepted on `/api/wardline/resolve` (4 MiB limit) while the SAME body
    /// is still 413'd on the 16 KiB `/api/v1/files/batch` route. A small body
    /// passes everywhere and would not catch a broken relocation.
    #[tokio::test]
    async fn wardline_resolve_accepts_large_body_but_files_batch_rejects_it() {
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";

        // Build a >16 KiB JSON body of qualnames (well under the 2000 batch
        // cap and under 4 MiB). Each entry is ~30 bytes; 2000 of them clears
        // 16 KiB comfortably.
        let qualnames: Vec<String> = (0..2000).map(|i| format!("pkg.mod.func_{i:05}")).collect();
        let wardline_body =
            serde_json::to_vec(&serde_json::json!({ "qualnames": qualnames })).expect("json");
        assert!(
            wardline_body.len() > HTTP_BODY_LIMIT_BYTES,
            "test body must exceed the 16 KiB limit to be discriminating: {}",
            wardline_body.len()
        );

        let (state, _tempdir) = wardline_resolve_test_state(secret, &[]);
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", &wardline_body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "wardline route must accept a >16 KiB body under its 4 MiB limit"
        );

        // Same-sized body shaped for the files/batch route — must be rejected
        // by the 16 KiB limit (413 from the framework's RequestBodyLimitLayer).
        let batch_body = serde_json::to_vec(&serde_json::json!({ "queries": [] })).expect("json");
        // Pad with a large dummy so the body exceeds 16 KiB but is otherwise
        // a structurally-irrelevant oversize; the limit fires before parsing.
        let mut oversize = batch_body;
        oversize.resize(HTTP_BODY_LIMIT_BYTES + 1024, b' ');
        let (state2, _tempdir2) = wardline_resolve_test_state(secret, &[]);
        let request2 = hmac_request(secret, "POST", "/api/v1/files/batch", &oversize);
        let response2 = router(state2).oneshot(request2).await.expect("oneshot");
        // In HMAC mode the v1 route has TWO oversize-body rejecters: the
        // `RequestBodyLimitLayer(16 KiB)` on the v1 group (→ 413) and the HMAC
        // middleware's own `to_bytes(_, HTTP_BODY_LIMIT_BYTES)` (→ 500). The
        // HMAC read fires first, so this half only proves the SAME body the
        // wardline route accepted is NOT accepted here — it does NOT prove the
        // v1 `RequestBodyLimitLayer` is wired. The no-auth assertion below
        // closes that gap.
        assert_ne!(
            response2.status(),
            StatusCode::OK,
            "files/batch route must reject a >16 KiB body that the wardline route accepts"
        );
        assert!(
            response2.status().is_client_error() || response2.status().is_server_error(),
            "files/batch >16 KiB body must be an error status, got {}",
            response2.status()
        );

        // Regression guard for the v1 `RequestBodyLimitLayer` itself. With NO
        // identity configured (loopback trust), the auth middleware is a plain
        // passthrough and never reads the body, so the ONLY thing that can cap
        // an oversize v1 body is the group's `RequestBodyLimitLayer(16 KiB)`.
        // If that layer were removed in a future refactor, this assertion would
        // flip from 413 to 200 (oversize read silently let through) — which the
        // HMAC-mode half above cannot detect.
        let (mut state3, _tempdir3) = wardline_resolve_test_state(secret, &[]);
        state3.identity_secret = None;
        state3.auth_token = None;
        let request3 = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/files/batch")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, oversize.len().to_string())
            .body(axum::body::Body::from(oversize.clone()))
            .expect("build request");
        let response3 = router(state3).oneshot(request3).await.expect("oneshot");
        assert_eq!(
            response3.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "v1 RequestBodyLimitLayer must 413 a >16 KiB body on the no-auth path"
        );
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

    /// A seeded function entity for a read test: its qualname, the absolute
    /// path of its containing file (written with `bytes`), and the stored taint
    /// blob (verbatim). `line_start`/`line_end` bound a span inside the file so
    /// the span-vs-whole-file distinction is observable.
    struct SeedFn {
        qualname: &'static str,
        bytes: &'static [u8],
        /// `Some(json)` stores a taint fact; `None` seeds the entity ONLY (the
        /// resolved-entity-but-no-stored-fact case the read path must report
        /// as `exists: false`).
        blob: Option<&'static str>,
    }

    /// Build a reads-only `AppState` (`taint_writer: None`) over a fresh temp
    /// migrated DB. Each `SeedFn` gets a real file written under the project
    /// root and an `entities` row whose `source_file_path` is that file's
    /// ABSOLUTE path; a `wardline_taint_facts` row carrying its blob verbatim
    /// is stored only when `blob` is `Some`. Returns the state and the
    /// `TempDir` guard (drop it last).
    fn wardline_read_test_state(secret: &str, seeds: &[SeedFn]) -> (AppState, tempfile::TempDir) {
        use clarion_storage::ReaderPool;
        use clarion_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");

        for (i, seed) in seeds.iter().enumerate() {
            let file = tempdir.path().join(format!("seed_{i}.py"));
            std::fs::write(&file, seed.bytes).expect("write seed file");
            let abs = file.to_str().expect("utf8 path").to_owned();
            let id = format!("python:function:{}", seed.qualname);
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, source_file_path, created_at, updated_at \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    seed.qualname.rsplit('.').next().unwrap_or(seed.qualname),
                    "{}",
                    // A deliberately-wrong stored hash: the read path must NOT
                    // use it (it derives the live whole-file hash instead).
                    "stored-span-hash-not-used",
                    abs,
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .expect("seed entity row");
            if let Some(blob) = seed.blob {
                conn.execute(
                    "INSERT INTO wardline_taint_facts \
                        (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at) \
                     VALUES (?1, ?2, NULL, NULL, ?3)",
                    rusqlite::params![id, blob, "2026-05-31T00:00:00.000Z"],
                )
                .expect("seed taint fact");
            }
        }
        // Two seeds may share one file for the dedup test; insert that case
        // explicitly via a shared-file seed below if needed (handled in-test).
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000007")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            taint_writer: None,
        };
        (state, tempdir)
    }

    /// blake3 (hex) of whole file bytes — the contract's `current_content_hash`.
    fn whole_file_blake3(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    #[tokio::test]
    async fn wardline_taint_get_returns_fact_with_live_whole_file_hash() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        // Key order b,a is deliberate — RawValue must return it verbatim.
        let blob = r#"{"schema_version":"wardline-taint-1","taint":{"b":2,"a":1}}"#;
        let bytes = b"def f():\n    return 1\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "a.b.c",
                bytes,
                blob: Some(blob),
            }],
        );

        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=a.b.c",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");

        assert_eq!(parsed["qualname"], "a.b.c");
        assert_eq!(parsed["exists"], true);
        assert_eq!(
            parsed["current_content_hash"],
            whole_file_blake3(bytes),
            "current_content_hash must be the LIVE whole-file blake3"
        );
        // Byte-faithful: the serialized wardline_json sub-object must preserve
        // the original {"b":2,"a":1} key order, not normalize it.
        assert!(
            text.contains(
                r#""wardline_json":{"schema_version":"wardline-taint-1","taint":{"b":2,"a":1}}"#
            ),
            "wardline_json must be byte-faithful (key order preserved): {text}"
        );
    }

    #[tokio::test]
    async fn wardline_taint_get_unknown_qualname_reports_not_exists() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(secret, &[]);
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=does.not.exist",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(parsed["qualname"], "does.not.exist");
        assert_eq!(parsed["exists"], false);
        assert!(
            parsed.get("wardline_json").is_none(),
            "absent fact must omit wardline_json"
        );
        assert!(parsed.get("current_content_hash").is_none());
    }

    /// Finding 2 (corrupt stored blob): an `exists: true` row whose stored
    /// `wardline_json` does not re-parse is a STORAGE-integrity failure, not a
    /// malformed client request. The validated write path (`RawValue` round-trip)
    /// cannot produce this — only storage corruption or an out-of-band write
    /// can — so the test injects it directly via the seed builder's verbatim
    /// blob. The read must return 500 `STORAGE_ERROR` (Clarion's fault, and 5xx
    /// so `json_read_error` logs it), NOT 400 `INVALID_PATH` (which would blame
    /// the federation client's request for Clarion's storage damage).
    #[tokio::test]
    async fn wardline_taint_get_corrupt_blob_is_500_storage_error_not_400() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "corrupt.fn",
                bytes: b"def f():\n    return 1\n",
                blob: Some("{not valid json"),
            }],
        );
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=corrupt.fn",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a corrupt stored blob is Clarion's fault → 500, never a client 400"
        );
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            parsed["code"], "STORAGE_ERROR",
            "corruption must classify as STORAGE_ERROR, not INVALID_PATH"
        );
    }

    #[tokio::test]
    async fn wardline_taint_get_whole_file_hash_not_span_hash() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        // Multi-line file with trailing newline; the function "body" is a
        // strict sub-range so the span hash differs on BOTH axes (span scope +
        // LF normalization). The regression guard for the W.3 bug.
        let bytes = b"line0\nline1\nline2\nline3\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "m.span.fn",
                bytes,
                blob: Some(r#"{"v":1}"#),
            }],
        );
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=m.span.fn",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");

        let whole = whole_file_blake3(bytes);
        // Span-hash formula (analyze.rs::content_hash_for_entity).
        let text = std::str::from_utf8(bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let span = lines[1..3].join("\n");
        let span_hash = blake3::hash(span.as_bytes()).to_hex().to_string();

        assert_eq!(parsed["current_content_hash"], whole);
        assert_ne!(
            parsed["current_content_hash"].as_str().unwrap(),
            span_hash,
            "must be whole-file hash, NOT the span/LF-normalized hash"
        );
    }

    #[tokio::test]
    async fn wardline_taint_batch_get_mixed_present_and_absent() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let bytes = b"def g():\n    pass\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "pkg.present",
                bytes,
                blob: Some(r#"{"present":true}"#),
            }],
        );
        let body = br#"{"qualnames":["pkg.present","pkg.absent"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes_out = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes_out).expect("json");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 2, "one view per input qualname, in order");
        assert_eq!(arr[0]["qualname"], "pkg.present");
        assert_eq!(arr[0]["exists"], true);
        assert_eq!(arr[0]["current_content_hash"], whole_file_blake3(bytes));
        assert_eq!(arr[1]["qualname"], "pkg.absent");
        assert_eq!(arr[1]["exists"], false);
        assert!(arr[1].get("wardline_json").is_none());
    }

    /// The qualname RESOLVES to a real entity, but that entity has no stored
    /// taint fact (`blob: None`). This is a distinct path from an unresolved
    /// qualname: both converge on the `exists: false` view, but only this one
    /// exercises `get_taint_facts` returning fewer rows than resolved ids
    /// (present-rows-only). Without this test the changed consumer arm is
    /// covered for "unresolved" but not for "resolved-but-no-fact".
    #[tokio::test]
    async fn wardline_taint_get_resolved_entity_without_fact_reports_not_exists() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "pkg.no_fact",
                bytes: b"def f():\n    pass\n",
                blob: None,
            }],
        );
        let body = br#"{"qualnames":["pkg.no_fact"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes_out = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes_out).expect("json");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["qualname"], "pkg.no_fact");
        assert_eq!(
            arr[0]["exists"], false,
            "resolved entity with no stored fact must report exists: false"
        );
        assert!(arr[0].get("wardline_json").is_none());
        // A resolved-but-no-fact view carries no freshness signal either.
        assert_eq!(arr[0]["current_content_hash"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn wardline_taint_batch_get_shared_file_yields_same_hash() {
        use clarion_storage::ReaderPool;
        use clarion_storage::schema::apply_migrations;
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        // Build state by hand so two entities share ONE file (exercises the
        // per-request file-hash dedup; both must report the same hash).
        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("migrations");
        let shared = tempdir.path().join("shared.py");
        let bytes: &[u8] = b"def a():\n    pass\n\ndef b():\n    pass\n";
        std::fs::write(&shared, bytes).expect("write shared file");
        let abs = shared.to_str().unwrap().to_owned();
        for q in ["mod.a", "mod.b"] {
            let id = format!("python:function:{q}");
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, source_file_path, created_at, updated_at \
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    q,
                    "{}",
                    "x",
                    abs,
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO wardline_taint_facts \
                    (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at) \
                 VALUES (?1, ?2, NULL, NULL, ?3)",
                rusqlite::params![id, r#"{"v":1}"#, "2026-05-31T00:00:00.000Z"],
            )
            .unwrap();
        }
        drop(conn);
        let readers = ReaderPool::open(&db_path, 4).expect("reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000008")
                .expect("instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            taint_writer: None,
        };

        let body = br#"{"qualnames":["mod.a","mod.b"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let out = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("json");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        let expected = whole_file_blake3(bytes);
        assert_eq!(arr[0]["current_content_hash"], expected);
        assert_eq!(
            arr[0]["current_content_hash"], arr[1]["current_content_hash"],
            "two functions in the same file must share one whole-file hash"
        );
    }

    #[tokio::test]
    async fn wardline_taint_batch_get_rejects_oversize_batch() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(secret, &[]);
        let qualnames: Vec<String> = (0..=WARDLINE_TAINT_BATCH_MAX)
            .map(|i| format!("pkg.mod.f{i}"))
            .collect();
        assert!(qualnames.len() > WARDLINE_TAINT_BATCH_MAX);
        let body =
            serde_json::to_vec(&serde_json::json!({ "qualnames": qualnames })).expect("json");
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", &body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "BATCH_TOO_LARGE");
    }

    #[tokio::test]
    async fn wardline_taint_read_served_with_writer_disabled() {
        use tower::ServiceExt;

        // `wardline_read_test_state` builds `taint_writer: None`. The READ
        // endpoint must still serve (only the WRITE endpoint is gated on it).
        let secret = "wardline-read-secret";
        let bytes = b"def h():\n    pass\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "x.y.z",
                bytes,
                blob: Some(r#"{"ok":true}"#),
            }],
        );
        assert!(state.taint_writer.is_none(), "write API is disabled");
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=x.y.z",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "reads must succeed even when the write API is disabled"
        );
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(parsed["exists"], true);
    }

    #[tokio::test]
    async fn wardline_taint_get_deleted_file_yields_none_hash_not_500() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let bytes = b"def gone():\n    pass\n";
        let (state, tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "gone.fn",
                bytes,
                blob: Some(r#"{"v":1}"#),
            }],
        );
        // Delete the containing file: a stale signal → current_content_hash
        // None, fact still reported (exists:true), and NOT a 500.
        std::fs::remove_file(tempdir.path().join("seed_0.py")).expect("remove file");
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=gone.fn",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(parsed["exists"], true);
        assert!(
            parsed.get("current_content_hash").is_none(),
            "deleted file → current_content_hash omitted (None), got: {parsed}"
        );
    }

    // ── Call-graph linkages (Wave 0 / WS2) ───────────────────────────────────

    #[test]
    fn aggregate_linkages_picks_strongest_tier_and_counts_sites() {
        // Pure aggregation contract: per neighbour, `call_site_count` spans tiers
        // and `confidence` is the STRONGEST present. Caller `a` has an ambiguous
        // and a resolved site → resolved wins, count 2; `b` has one inferred site.
        let mk = |from: &str, conf: EdgeConfidence, start: i64| CallEdgeMatch {
            from_id: from.to_owned(),
            to_id: "python:function:t".to_owned(),
            stored_to_id: "python:function:t".to_owned(),
            confidence: conf,
            source_file_id: None,
            source_byte_start: Some(start),
            source_byte_end: Some(start + 5),
            properties_json: None,
        };
        let matches = vec![
            mk("python:function:a", EdgeConfidence::Ambiguous, 0),
            mk("python:function:a", EdgeConfidence::Resolved, 10),
            mk("python:function:b", EdgeConfidence::Inferred, 20),
        ];
        let entries = aggregate_linkages(&matches, LinkageDirection::Callers);
        assert_eq!(entries.len(), 2);
        // BTreeMap ordering → deterministic by entity_id.
        assert_eq!(entries[0].entity_id, "python:function:a");
        assert_eq!(entries[0].confidence, "resolved");
        assert_eq!(entries[0].call_site_count, 2);
        assert_eq!(entries[1].entity_id, "python:function:b");
        assert_eq!(entries[1].confidence, "inferred");
        assert_eq!(entries[1].call_site_count, 1);
    }

    #[test]
    fn parse_max_confidence_maps_tiers_and_rejects_garbage() {
        assert_eq!(parse_max_confidence(None), Some(EdgeConfidence::Inferred));
        assert_eq!(
            parse_max_confidence(Some("all")),
            Some(EdgeConfidence::Inferred)
        );
        assert_eq!(
            parse_max_confidence(Some("inferred")),
            Some(EdgeConfidence::Inferred)
        );
        assert_eq!(
            parse_max_confidence(Some("ambiguous")),
            Some(EdgeConfidence::Ambiguous)
        );
        assert_eq!(
            parse_max_confidence(Some("resolved")),
            Some(EdgeConfidence::Resolved)
        );
        assert_eq!(parse_max_confidence(Some("bogus")), None);
    }

    /// Build an `AppState` over a temp DB seeded with entities + `calls` edges.
    /// `entities`: (id, `briefing_blocked` reason?). `calls`: (from, to,
    /// confidence, `candidate_ids`). Carries an HMAC secret so the protected
    /// routes verify real signatures.
    fn linkage_test_state(
        secret: &str,
        entities: &[(&str, Option<&str>)],
        calls: &[(&str, &str, &str, &[&str])],
    ) -> (AppState, tempfile::TempDir) {
        use clarion_storage::ReaderPool;
        use clarion_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");
        for (id, blocked) in entities {
            let properties = match blocked {
                Some(reason) => serde_json::json!({ "briefing_blocked": reason }).to_string(),
                None => "{}".to_owned(),
            };
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, created_at, updated_at \
                 ) VALUES (?1, 'python', 'function', ?1, ?1, ?2, 'deadbeef', \
                    '2026-05-31T00:00:00.000Z', '2026-05-31T00:00:00.000Z')",
                rusqlite::params![id, properties],
            )
            .expect("seed entity row");
        }
        for (idx, (from, to, confidence, candidates)) in calls.iter().enumerate() {
            let properties: Option<String> = if candidates.is_empty() {
                None
            } else {
                Some(serde_json::json!({ "candidates": candidates }).to_string())
            };
            let byte = i64::try_from(idx).expect("test edge index fits i64") * 10;
            conn.execute(
                "INSERT INTO edges ( \
                    kind, from_id, to_id, properties, source_file_id, \
                    source_byte_start, source_byte_end, confidence \
                 ) VALUES ('calls', ?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                rusqlite::params![from, to, properties, byte, byte + 5, confidence],
            )
            .expect("seed calls edge");
        }
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000005")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            taint_writer: None,
        };
        (state, tempdir)
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("response body is JSON")
    }

    #[tokio::test]
    async fn linkage_callers_returns_aggregated_neighbours() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        // a -> t (resolved); b -> t (ambiguous, t is a candidate).
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[
                ("python:function:t", None),
                ("python:function:a", None),
                ("python:function:b", None),
            ],
            &[
                ("python:function:a", "python:function:t", "resolved", &[]),
                (
                    "python:function:b",
                    "python:function:t",
                    "ambiguous",
                    &["python:function:t"],
                ),
            ],
        );
        let path = "/api/v1/entities/python:function:t/callers?confidence=all";
        let response = router(state)
            .oneshot(hmac_request(secret, "GET", path, b""))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        assert_eq!(parsed["entity_id"], "python:function:t");
        assert_eq!(parsed["total"], 2);
        assert_eq!(parsed["truncated"], false);
        let callers = parsed["callers"].as_array().expect("callers array");
        let by_id: std::collections::HashMap<&str, &serde_json::Value> = callers
            .iter()
            .map(|c| (c["entity_id"].as_str().unwrap(), c))
            .collect();
        assert_eq!(by_id["python:function:a"]["confidence"], "resolved");
        assert_eq!(by_id["python:function:a"]["call_site_count"], 1);
        assert_eq!(by_id["python:function:b"]["confidence"], "ambiguous");
    }

    #[tokio::test]
    async fn linkage_callees_returns_outbound_calls() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[("python:function:s", None), ("python:function:x", None)],
            &[("python:function:s", "python:function:x", "resolved", &[])],
        );
        let path = "/api/v1/entities/python:function:s/callees";
        let response = router(state)
            .oneshot(hmac_request(secret, "GET", path, b""))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        let callees = parsed["callees"].as_array().expect("callees array");
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0]["entity_id"], "python:function:x");
        assert_eq!(callees[0]["confidence"], "resolved");
    }

    #[tokio::test]
    async fn linkage_unknown_entity_is_404() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        let path = "/api/v1/entities/python:function:nope/callers";
        let response = router(state)
            .oneshot(hmac_request(secret, "GET", path, b""))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn linkage_confidence_filter_excludes_weaker_tiers() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let seed = |s: &str| {
            linkage_test_state(
                s,
                &[
                    ("python:function:t", None),
                    ("python:function:a", None),
                    ("python:function:b", None),
                ],
                &[
                    ("python:function:a", "python:function:t", "resolved", &[]),
                    (
                        "python:function:b",
                        "python:function:t",
                        "ambiguous",
                        &["python:function:t"],
                    ),
                ],
            )
        };

        // resolved-only: the ambiguous caller `b` must be excluded.
        let (state, _t1) = seed(secret);
        let resolved = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:t/callers?confidence=resolved",
                b"",
            ))
            .await
            .expect("oneshot");
        let resolved = json_body(resolved).await;
        assert_eq!(
            resolved["total"], 1,
            "resolved filter excludes ambiguous: {resolved}"
        );
        assert_eq!(resolved["callers"][0]["entity_id"], "python:function:a");

        // all: both tiers present.
        let (state, _t2) = seed(secret);
        let all = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:t/callers?confidence=all",
                b"",
            ))
            .await
            .expect("oneshot");
        assert_eq!(json_body(all).await["total"], 2);
    }

    #[tokio::test]
    async fn linkage_invalid_confidence_is_400() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:t/callers?confidence=bogus",
                b"",
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn linkage_briefing_blocked_queried_entity_is_403() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        // Queried entity carries a briefing-block marker → refuse (mirrors get_file).
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[("python:function:secret", Some("secret-scan"))],
            &[],
        );
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:secret/callers",
                b"",
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(json_body(response).await["code"], "BRIEFING_BLOCKED");
    }

    #[tokio::test]
    async fn linkage_requires_authentication() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        // No X-Loom-Component header → 401 (route is HMAC-gated like /api/v1/files).
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/entities/python:function:t/callers")
            .body(axum::body::Body::empty())
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn linkage_batch_returns_only_known_visible_entities() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[
                ("python:function:t", None),
                ("python:function:a", None),
                ("python:function:blocked", Some("secret-scan")),
            ],
            &[("python:function:a", "python:function:t", "resolved", &[])],
        );
        // t (known), blocked (briefing-blocked), nope (unknown) → results has t only.
        let body = br#"{"entity_ids":["python:function:t","python:function:blocked","python:function:nope"]}"#;
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "POST",
                "/api/v1/entities/callers:batch-get",
                body,
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        let results = parsed["results"].as_object().expect("results object");
        assert_eq!(
            results.len(),
            1,
            "only the known visible entity appears: {parsed}"
        );
        assert_eq!(
            results["python:function:t"][0]["entity_id"],
            "python:function:a"
        );
    }

    #[tokio::test]
    async fn linkage_batch_over_limit_is_400() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        let ids: Vec<String> = (0..=LINKAGES_BATCH_MAX)
            .map(|i| format!("python:function:e{i}"))
            .collect();
        let body = serde_json::json!({ "entity_ids": ids }).to_string();
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "POST",
                "/api/v1/entities/callees:batch-get",
                body.as_bytes(),
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["code"], "BATCH_TOO_LARGE");
    }

    #[tokio::test]
    async fn capabilities_reports_linkages_http_true() {
        use tower::ServiceExt;
        // _capabilities is unauthenticated so siblings can probe pre-auth.
        let (state, _tempdir) = linkage_test_state("linkage-secret", &[], &[]);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/_capabilities")
            .body(axum::body::Body::empty())
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["linkages"]["http"], true);
    }

    #[tokio::test]
    async fn capabilities_reports_taint_store_read_by_sei_true() {
        use tower::ServiceExt;
        // Discrete from `sei.supported`: an older SEI-capable Clarion would set
        // `sei.supported: true` yet lack this route, so consumers gate the
        // rename-stable taint read on this flag specifically.
        let (state, _tempdir) = linkage_test_state("linkage-secret", &[], &[]);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/_capabilities")
            .body(axum::body::Body::empty())
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await["taint_store"]["read_by_sei"],
            true
        );
    }

    /// T3.4 end-to-end oracle: a taint fact written before a rename is still
    /// retrievable by its stable SEI after the rename, while a read by the new
    /// locator correctly returns nothing until a re-scan writes under it.
    #[tokio::test]
    async fn wardline_taint_fact_survives_rename_via_sei() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let old = "python:function:old.pkg.fn";
        let new = "python:function:new.pkg.fn";
        let sei = "clarion:eid:rename-stable";
        // Both pre- and post-rename entity rows exist (entities is cumulative).
        let (state, db_path, _writer, _tempdir) = wardline_write_test_state(secret, &[old, new]);

        // Alive SEI binding at the OLD locator, as it stands at write time.
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open db");
            conn.execute(
                "INSERT INTO sei_bindings \
                    (sei, current_locator, body_hash, signature, status, \
                     born_run_id, updated_run_id, updated_at) \
                 VALUES (?1, ?2, NULL, NULL, 'alive', 'run-0', 'run-0', 't')",
                rusqlite::params![sei, old],
            )
            .expect("insert binding");
        }

        // 1. Write a fact for the OLD qualname. The request omits `sei`; the
        //    server resolves and stamps it from the alive binding.
        let write_body =
            br#"{"facts":[{"qualname":"old.pkg.fn","wardline_json":{"taint":"EXTERNAL"}}]}"#;
        let write = hmac_request(secret, "POST", "/api/wardline/taint-facts", write_body);
        let resp = router(state.clone()).oneshot(write).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);

        // The fact stored under the OLD locator carries the resolved SEI.
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open db");
            let stored: Option<String> = conn
                .query_row(
                    "SELECT sei FROM wardline_taint_facts WHERE entity_id = ?1",
                    rusqlite::params![old],
                    |r| r.get(0),
                )
                .expect("query stored sei");
            assert_eq!(
                stored.as_deref(),
                Some(sei),
                "write must auto-populate the SEI from the alive binding"
            );
        }

        // 2. Simulate the rename: the binding's current_locator flips OLD→NEW.
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open db");
            conn.execute(
                "UPDATE sei_bindings SET current_locator = ?1 WHERE sei = ?2",
                rusqlite::params![new, sei],
            )
            .expect("flip binding locator");
        }

        // 3. read-by-SEI still returns the fact (stored under the dead locator).
        let by_sei_body = serde_json::json!({ "seis": [sei] }).to_string();
        let by_sei = hmac_request(
            secret,
            "POST",
            "/api/wardline/taint-facts/by-sei",
            by_sei_body.as_bytes(),
        );
        let resp = router(state.clone())
            .oneshot(by_sei)
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
        let parsed = json_body(resp).await;
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1, "one view per input SEI");
        assert_eq!(arr[0]["sei"], sei);
        assert_eq!(
            arr[0]["exists"], true,
            "the fact survives the rename via its stable SEI"
        );
        assert_eq!(arr[0]["wardline_json"]["taint"], "EXTERNAL");

        // 4. read-by-NEW-locator returns nothing until a re-scan writes there.
        let by_new = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=new.pkg.fn",
            b"",
        );
        let resp = router(state).oneshot(by_new).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            json_body(resp).await["exists"],
            false,
            "no fact under the new locator until re-scan"
        );
    }

    /// An unknown SEI yields an honest `exists: false` view, in input order,
    /// never a fabricated row or an error.
    #[tokio::test]
    async fn wardline_taint_by_sei_unknown_reports_not_exists() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, _writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);
        let body = serde_json::json!({ "seis": ["clarion:eid:nope"] }).to_string();
        let request = hmac_request(
            secret,
            "POST",
            "/api/wardline/taint-facts/by-sei",
            body.as_bytes(),
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["sei"], "clarion:eid:nope");
        assert_eq!(arr[0]["exists"], false);
        assert!(arr[0].get("wardline_json").is_none());
    }

    /// The read-by-SEI route is HMAC-gated like the rest of the wardline group.
    #[tokio::test]
    async fn wardline_taint_by_sei_requires_identity() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, _writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);
        // No HMAC signature — a bare request must be rejected.
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/wardline/taint-facts/by-sei")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"seis":["clarion:eid:x"]}"#))
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
