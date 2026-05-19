use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use clarion_core::{
    ClaudeCliProvider, ClaudeCliProviderConfig, CodexCliProvider, CodexCliProviderConfig,
    LlmProvider, OpenRouterProvider, OpenRouterProviderConfig, Recording, RecordingProvider,
};
use clarion_mcp::config::{LlmConfig, McpConfig, ProviderSelection, select_provider_with_env};
use clarion_mcp::filigree::FiligreeHttpClient;
use clarion_storage::{DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, ReaderPool, Writer};

pub fn run(path: &Path, config_path: Option<&Path>) -> Result<()> {
    let db_path = path.join(".clarion").join("clarion.db");
    ensure!(
        db_path.exists(),
        "Clarion database not found at {}; run `clarion install --path {}` first",
        db_path.display(),
        path.display()
    );

    let project_root = path
        .canonicalize()
        .with_context(|| format!("canonicalize project path {}", path.display()))?;
    let instance_id = crate::instance::load_or_create(&project_root)
        .context("load Clarion project instance ID")?;
    let default_config_path = path.join("clarion.yaml");
    let config_path = config_path.unwrap_or(&default_config_path);
    let config = if config_path.exists() {
        McpConfig::from_path(config_path)
            .with_context(|| format!("load MCP config {}", config_path.display()))?
    } else {
        McpConfig::default()
    };
    let provider_selection = select_provider_with_env(&config, |name| std::env::var(name).ok())?;
    let llm_provider = build_llm_provider(&config, provider_selection, &project_root)?;
    let filigree_client = FiligreeHttpClient::from_config(&config.integrations.filigree, |name| {
        std::env::var(name).ok()
    })
    .context("build Filigree HTTP client")?;

    let readers = ReaderPool::open(&db_path, 16)
        .map_err(|err| anyhow!("open reader pool for {}: {err}", db_path.display()))?;
    let http_project_root = project_root.clone();
    let http_server = crate::http_read::spawn(
        http_project_root,
        readers.clone(),
        instance_id.to_string(),
        &config.serve.http,
    )
    .context("start HTTP read API")?;
    let stdio = spawn_mcp_stdio(
        project_root,
        db_path,
        readers,
        config.llm.clone(),
        llm_provider,
        filigree_client,
    )?;
    supervise_stdio_with_http(stdio, http_server)
}

struct StdioServe {
    result_rx: mpsc::Receiver<Result<()>>,
    join: thread::JoinHandle<()>,
}

fn spawn_mcp_stdio(
    project_root: PathBuf,
    db_path: PathBuf,
    readers: ReaderPool,
    llm_config: LlmConfig,
    llm_provider: Option<Arc<dyn LlmProvider>>,
    filigree_client: Option<FiligreeHttpClient>,
) -> Result<StdioServe> {
    let (result_tx, result_rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("clarion-mcp-stdio".to_owned())
        .spawn(move || {
            let result = run_mcp_stdio(
                project_root,
                &db_path,
                readers,
                llm_config,
                llm_provider,
                filigree_client,
            );
            let _ = result_tx.send(result);
        })
        .context("spawn MCP stdio server thread")?;
    Ok(StdioServe { result_rx, join })
}

fn run_mcp_stdio(
    project_root: PathBuf,
    db_path: &Path,
    readers: ReaderPool,
    llm_config: LlmConfig,
    llm_provider: Option<Arc<dyn LlmProvider>>,
    filigree_client: Option<FiligreeHttpClient>,
) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create MCP runtime")?;
    let _runtime_guard = runtime.enter();
    let mut state = clarion_mcp::ServerState::new(project_root, readers);
    let mut llm_writer = None;
    let mut llm_writer_join = None;
    if let Some(provider) = llm_provider {
        let (writer, handle) = Writer::spawn(
            db_path.to_owned(),
            DEFAULT_BATCH_SIZE,
            DEFAULT_CHANNEL_CAPACITY,
        )
        .map_err(|err| anyhow!("spawn MCP LLM writer for {}: {err}", db_path.display()))?;
        state = state.with_summary_llm(writer.sender(), llm_config, provider);
        llm_writer = Some(writer);
        llm_writer_join = Some(handle);
    }
    if let Some(client) = filigree_client {
        state = state.with_filigree_client(Arc::new(client));
    }

    let serve_result =
        clarion_mcp::serve_stdio_with_state_on_runtime(&runtime, &state, &mut reader, &mut writer)
            .context("serve MCP stdio");
    drop(state);
    drop(llm_writer);
    let writer_result = if let Some(handle) = llm_writer_join {
        Some(
            runtime
                .block_on(handle)
                .context("join MCP LLM writer")?
                .map_err(|err| anyhow!("MCP LLM writer failed: {err}")),
        )
    } else {
        None
    };

    serve_result?;
    if let Some(result) = writer_result {
        result?;
    }
    Ok(())
}

fn supervise_stdio_with_http(
    stdio: StdioServe,
    mut http_server: Option<crate::http_read::HttpReadServer>,
) -> Result<()> {
    let serve_result = loop {
        match stdio.result_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(server) = http_server.as_mut()
                    && let Err(err) = server.check_running()
                {
                    if let Some(server) = http_server.take()
                        && let Err(stop_err) = server.shutdown()
                    {
                        tracing::warn!(
                            error = %stop_err,
                            "failed to stop HTTP read API after supervised failure"
                        );
                    }
                    return Err(err.context("HTTP read API failed while MCP stdio was running"));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stdio
                    .join
                    .join()
                    .map_err(|_| anyhow!("MCP stdio server thread panicked"))?;
                return Err(anyhow!("MCP stdio server thread exited without a result"));
            }
        }
    };
    stdio
        .join
        .join()
        .map_err(|_| anyhow!("MCP stdio server thread panicked"))?;
    let shutdown_result = match http_server {
        Some(server) => server.shutdown().context("stop HTTP read API"),
        None => Ok(()),
    };
    finish_supervised_result(serve_result, shutdown_result)
}

fn finish_supervised_result(serve_result: Result<()>, shutdown_result: Result<()>) -> Result<()> {
    match (serve_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(shutdown_err)) => Err(shutdown_err),
        (Err(serve_err), Ok(())) => Err(serve_err),
        (Err(serve_err), Err(shutdown_err)) => {
            tracing::warn!(
                error = %shutdown_err,
                "failed to stop HTTP read API after MCP stdio failure"
            );
            Err(serve_err)
        }
    }
}

fn build_llm_provider(
    config: &McpConfig,
    selection: ProviderSelection,
    project_root: &Path,
) -> Result<Option<Arc<dyn LlmProvider>>> {
    match selection {
        ProviderSelection::Disabled => Ok(None),
        ProviderSelection::Recording => {
            let recordings = load_recording_fixture(config, project_root)?;
            Ok(Some(Arc::new(RecordingProvider::from_recordings(
                recordings,
            ))))
        }
        ProviderSelection::OpenRouter { api_key_env } => {
            let api_key = std::env::var(&api_key_env).ok();
            let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
                api_key,
                allow_live_provider: true,
                model_id: config.llm.model_id.clone(),
                endpoint_url: config.llm.openrouter.endpoint_url.clone(),
                referer: config.llm.openrouter.attribution.referer.clone(),
                title: config.llm.openrouter.attribution.title.clone(),
            })
            .context("build OpenRouter LLM provider")?;
            Ok(Some(Arc::new(provider)))
        }
        ProviderSelection::CodexCli => {
            let provider = CodexCliProvider::from_config(CodexCliProviderConfig {
                executable: config.llm.codex_cli.executable.clone(),
                project_root: project_root.to_owned(),
                model_id: config.llm.model_id.clone(),
                model: config.llm.codex_cli.model.clone(),
                profile: config.llm.codex_cli.profile.clone(),
                sandbox: config.llm.codex_cli.sandbox.as_str().to_owned(),
                timeout_seconds: config.llm.codex_cli.timeout_seconds,
            })
            .context("build Codex CLI LLM provider")?;
            Ok(Some(Arc::new(provider)))
        }
        ProviderSelection::ClaudeCli => {
            let provider = ClaudeCliProvider::from_config(ClaudeCliProviderConfig {
                executable: config.llm.claude_cli.executable.clone(),
                project_root: project_root.to_owned(),
                model_id: config.llm.model_id.clone(),
                model: config.llm.claude_cli.model.clone(),
                permission_mode: config.llm.claude_cli.permission_mode.as_str().to_owned(),
                tools: config.llm.claude_cli.tools.clone(),
                timeout_seconds: config.llm.claude_cli.timeout_seconds,
                max_turns: config.llm.claude_cli.max_turns,
                no_session_persistence: config.llm.claude_cli.no_session_persistence,
                exclude_dynamic_system_prompt_sections: config
                    .llm
                    .claude_cli
                    .exclude_dynamic_system_prompt_sections,
            })
            .context("build Claude CLI LLM provider")?;
            Ok(Some(Arc::new(provider)))
        }
    }
}

fn load_recording_fixture(config: &McpConfig, project_root: &Path) -> Result<Vec<Recording>> {
    let Some(path) = &config.llm.recording_fixture_path else {
        return Ok(Vec::new());
    };
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        project_root.join(path)
    };
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read RecordingProvider fixture {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parse RecordingProvider fixture {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_supervised_result_preserves_stdio_error_over_shutdown_error() {
        let err = finish_supervised_result(
            Err(anyhow!("stdio failed first")),
            Err(anyhow!("HTTP shutdown also failed")),
        )
        .expect_err("stdio failure should win");

        assert!(
            format!("{err:#}").contains("stdio failed first"),
            "unexpected error: {err:#}"
        );
    }
}
