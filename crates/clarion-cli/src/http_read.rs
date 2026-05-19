use std::error::Error as StdError;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::thread;

use anyhow::{Context, Result, anyhow};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clarion_mcp::config::HttpReadConfig;
use clarion_storage::{ReaderPool, StorageError, resolve_file};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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
    join: Option<thread::JoinHandle<Result<()>>>,
}

impl HttpReadServer {
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
}

#[derive(Clone)]
struct AppState {
    project_root: PathBuf,
    readers: ReaderPool,
    instance_id: String,
}

pub fn spawn(
    project_root: PathBuf,
    db_path: PathBuf,
    instance_id: String,
    config: &HttpReadConfig,
) -> Result<Option<HttpReadServer>> {
    if !config.enabled {
        return Ok(None);
    }
    let bind = config.bind;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let join = thread::spawn(move || -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("create HTTP read runtime")?;
        runtime.block_on(async move {
            let readers = ReaderPool::open(&db_path, 16)
                .map_err(|err| anyhow!("open HTTP reader pool for {}: {err}", db_path.display()))?;
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
    });
    let local_addr = ready_rx
        .recv()
        .context("wait for HTTP read API bind result")??;
    tracing::info!(bind = %local_addr, "Clarion HTTP read API listening");
    Ok(Some(HttpReadServer {
        shutdown: Some(shutdown_tx),
        join: Some(join),
    }))
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/files", get(get_file))
        .route("/api/v1/_capabilities", get(get_capabilities))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
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

async fn get_file(State(state): State<AppState>, Query(query): Query<FileQuery>) -> Response {
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
    let result = state
        .readers
        .with_reader(move |conn| resolve_file(conn, &project_root, &file_path, &language))
        .await;
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
            (
                StatusCode::OK,
                Json(FileResponse {
                    entity_id: file.entity_id,
                    content_hash: file.content_hash,
                    canonical_path: file.canonical_path,
                    language: file.language,
                }),
            )
                .into_response()
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "file is not known to Clarion",
        ),
        Err(err) => json_read_error(err),
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

fn json_read_error(err: StorageError) -> Response {
    let error = classify_read_error(&err);
    if error.status.is_server_error() {
        log_read_server_error(error.code, error.status, &err);
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
        StorageError::PoolInteract(_) => ReadError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: ErrorCode::Internal,
            message: "internal file lookup failure",
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
        StorageError::WriterGone
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
