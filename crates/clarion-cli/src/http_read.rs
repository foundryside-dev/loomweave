use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::{LazyLock, mpsc};
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
use clarion_storage::{ReaderPool, StorageError, resolve_file_catalog_entry};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tower::limit::ConcurrencyLimitLayer;
use tower::load_shed;
use tower::timeout;
use tower::{BoxError, ServiceBuilder};
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
    instance_id: String,
}

pub fn spawn(
    project_root: PathBuf,
    readers: ReaderPool,
    instance_id: String,
    config: &HttpReadConfig,
) -> Result<Option<HttpReadServer>> {
    if !config.enabled {
        return Ok(None);
    }
    config
        .validate_loopback_trust()
        .context("validate HTTP read API trust model")?;
    let bind = config.bind;
    let warn_unauthenticated_non_loopback = config.allow_non_loopback && !config.is_loopback_bind();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (failure_tx, failure_rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("clarion-http-read".to_owned())
        .spawn(move || -> Result<()> {
            let result = run_http_read_server(
                project_root,
                readers,
                instance_id,
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
    let local_addr = ready_rx
        .recv()
        .context("wait for HTTP read API bind result")??;
    let auth = "none";
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
    }))
}

fn run_http_read_server(
    project_root: PathBuf,
    readers: ReaderPool,
    instance_id: String,
    bind: std::net::SocketAddr,
    shutdown_rx: oneshot::Receiver<()>,
    ready_tx: mpsc::Sender<Result<std::net::SocketAddr>>,
) -> Result<()> {
    let runtime = build_http_runtime()?;
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(err) => {
                let _ = ready_tx.send(Err(anyhow!("bind HTTP read API on {bind}: {err}")));
                return Err(anyhow!("bind HTTP read API on {bind}: {err}"));
            }
        };
        let local_addr = listener
            .local_addr()
            .context("read HTTP read API local addr")?;
        let _ = ready_tx.send(Ok(local_addr));
        let state = AppState {
            project_root,
            readers,
            instance_id,
        };
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .context("serve HTTP read API")
    })
}

fn build_http_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .thread_name("clarion-http-worker")
        .enable_all()
        .build()
        .context("create HTTP read runtime")
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/files", get(get_file))
        .route("/api/v1/_capabilities", get(get_capabilities))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
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
    tracing::error!(error = %err, "HTTP read API middleware failed");
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        "HTTP read API middleware failed",
    )
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
    canonical_path: String,
    language: String,
}

#[derive(Debug, Serialize)]
struct CapabilitiesResponse {
    registry_backend: bool,
    file_registry: bool,
    api_version: u8,
    instance_id: String,
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
                tracing::warn!(
                    path = %file.canonical_path,
                    reason = %reason,
                    "HTTP /api/v1/files refusing to expose briefing-blocked entity"
                );
                return json_error(
                    StatusCode::NOT_FOUND,
                    ErrorCode::NotFound,
                    "file is not known to Clarion",
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
}
