use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use clarion_mcp::config::{McpConfig, select_provider_with_env};
use clarion_storage::ReaderPool;

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
    let _provider_selection = select_provider_with_env(&config, |name| std::env::var(name).ok())?;

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
    let state = clarion_mcp::ServerState::new(project_root, readers);

    clarion_mcp::serve_stdio_with_state_on_runtime(&runtime, &state, &mut reader, &mut writer)
        .context("serve MCP stdio")
}
