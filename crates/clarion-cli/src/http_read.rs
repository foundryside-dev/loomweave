use std::path::PathBuf;
use std::thread;

use anyhow::{Context, Result, anyhow};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clarion_mcp::config::HttpReadConfig;
use clarion_storage::{ReaderPool, resolve_file};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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
}

pub fn spawn(
    project_root: PathBuf,
    db_path: PathBuf,
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
    version: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

async fn get_file(State(state): State<AppState>, Query(query): Query<FileQuery>) -> Response {
    if query.path.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
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
        Ok(Some(file)) => (
            StatusCode::OK,
            Json(FileResponse {
                entity_id: file.entity_id,
                content_hash: file.content_hash,
                canonical_path: file.canonical_path,
                language: file.language,
            }),
        )
            .into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "file is not known to Clarion"),
        Err(err) => json_error(StatusCode::BAD_REQUEST, &err.to_string()),
    }
}

async fn get_capabilities() -> Json<CapabilitiesResponse> {
    Json(CapabilitiesResponse {
        registry_backend: true,
        file_registry: true,
        version: "0.1",
    })
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.to_owned(),
        }),
    )
        .into_response()
}
