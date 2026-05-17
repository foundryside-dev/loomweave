use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, ensure};
use clarion_core::{AnthropicProvider, AnthropicProviderConfig, LlmProvider, RecordingProvider};
use clarion_mcp::config::{McpConfig, ProviderSelection, select_provider_with_env};
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
    let default_config_path = path.join("clarion.yaml");
    let config_path = config_path.unwrap_or(&default_config_path);
    let config = if config_path.exists() {
        McpConfig::from_path(config_path)
            .with_context(|| format!("load MCP config {}", config_path.display()))?
    } else {
        McpConfig::default()
    };
    let provider_selection = select_provider_with_env(&config, |name| std::env::var(name).ok())?;
    let llm_provider = build_llm_provider(&config, provider_selection)?;
    let filigree_client = FiligreeHttpClient::from_config(&config.integrations.filigree, |name| {
        std::env::var(name).ok()
    })
    .context("build Filigree HTTP client")?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create MCP runtime")?;
    let _runtime_guard = runtime.enter();
    let readers = ReaderPool::open(&db_path, 16)
        .map_err(|err| anyhow!("open reader pool for {}: {err}", db_path.display()))?;
    let mut state = clarion_mcp::ServerState::new(project_root, readers);
    let mut llm_writer = None;
    let mut llm_writer_join = None;
    if let Some(provider) = llm_provider {
        let (writer, handle) = Writer::spawn(
            db_path.clone(),
            DEFAULT_BATCH_SIZE,
            DEFAULT_CHANNEL_CAPACITY,
        )
        .map_err(|err| anyhow!("spawn MCP LLM writer for {}: {err}", db_path.display()))?;
        state = state.with_summary_llm(writer.sender(), config.llm.clone(), provider);
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

fn build_llm_provider(
    config: &McpConfig,
    selection: ProviderSelection,
) -> Result<Option<Arc<dyn LlmProvider>>> {
    match selection {
        ProviderSelection::Disabled => Ok(None),
        ProviderSelection::Recording => Ok(Some(Arc::new(RecordingProvider::from_recordings(
            Vec::new(),
        )))),
        ProviderSelection::Anthropic { api_key_env } => {
            let api_key = std::env::var(&api_key_env).ok();
            let provider = AnthropicProvider::from_config(AnthropicProviderConfig {
                api_key,
                allow_live_provider: true,
                summary_model_id: config.llm.summary_model_id.clone(),
                inferred_edges_model_id: config.llm.inferred_edges_model_id.clone(),
            })
            .context("build Anthropic LLM provider")?;
            Ok(Some(Arc::new(provider)))
        }
    }
}
