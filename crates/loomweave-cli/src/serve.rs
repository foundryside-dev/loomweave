use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use loomweave_federation::config::{
    LlmConfig, McpConfig, ProviderSelection, SemanticProviderKind, SemanticSearchConfig,
    select_provider_with_env,
};
use loomweave_federation::filigree::FiligreeHttpClient;
use loomweave_federation::warpline::WarplineMcpClient;
use loomweave_llm::{
    ApiEmbeddingProvider, ApiEmbeddingProviderConfig, ClaudeCliProvider, ClaudeCliProviderConfig,
    CodexCliProvider, CodexCliProviderConfig, EmbeddingProvider, EmbeddingProviderError,
    LlmProvider, OpenRouterProvider, OpenRouterProviderConfig, Recording, RecordingProvider,
    TrafficLoggingProvider,
};
use loomweave_storage::{DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, ReaderPool, Writer};

/// Which of the three `serve` behaviors a resolved [`WorktreeContext`] and
/// the source root's own `db_path` existence select — pinned decision 1's
/// routing decision, extracted as a pure function so it is directly
/// unit-assertable (see `tests::linked_context_routes_to_linked_not_no_index`
/// below) without spinning up a full `serve` session. `db_exists` is only
/// consulted for a non-`Linked` context; a linked worktree always routes to
/// [`ServeRoute::Linked`] regardless of it, since Task 3's eager,
/// schema-initialized store means file existence is never the readiness
/// signal there.
///
/// [`WorktreeContext`]: loomweave_core::worktree::WorktreeContext
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeRoute {
    /// No index yet (and not a linked worktree) — degrade to
    /// `serve_no_index`.
    NoIndex,
    /// A linked worktree — always the full `ServerState` path with the
    /// bootstrap gate active, never `serve_no_index`.
    Linked,
    /// Main worktree or standalone checkout with an existing index — the
    /// full `ServerState` path, gate inactive.
    Full,
}

fn choose_serve_route(kind: loomweave_core::worktree::WorktreeKind, db_exists: bool) -> ServeRoute {
    if kind == loomweave_core::worktree::WorktreeKind::Linked {
        ServeRoute::Linked
    } else if db_exists {
        ServeRoute::Full
    } else {
        ServeRoute::NoIndex
    }
}

pub fn run(path: &Path, config_path: Option<&Path>) -> Result<()> {
    // Resolved once, before any gate (pinned decision 1, Task 5): a linked
    // worktree always routes to the full `ServerState` path with a `building`
    // readiness gate, never to `serve_no_index` — Task 3's eager,
    // schema-initialized empty DB means the effective store's `db_path`
    // already exists before any analyze completes, so file existence can no
    // longer be the readiness signal for a linked worktree; the gate's
    // per-call readiness consult carries the state instead (see
    // `loomweave_mcp::worktree_bootstrap`).
    let worktree_ctx = loomweave_core::worktree::WorktreeContext::resolve(path)
        .with_context(|| format!("resolve worktree context for {}", path.display()))?;
    // Main/standalone's own gate, byte-for-byte unchanged from before Task 5
    // — routed through the plain project root, exactly as always. Computed
    // unconditionally (a cheap `.exists()` stat) so `choose_serve_route`
    // stays a pure function of two small values.
    let db_path = loomweave_core::store::db_path(path);

    let route = choose_serve_route(worktree_ctx.kind, db_path.exists());
    // Cleanup sweep (Task 6): every `serve` startup, from every route —
    // main and standalone included, since main-checkout activity is what
    // reclaims a worktree's store once the worktree itself is gone and
    // never runs Loomweave again. `sweep_best_effort` cannot fail this
    // `serve` invocation: it has no `Result` to propagate (see
    // `loomweave_cli::worktree::sweep`).
    loomweave_cli::worktree::sweep::sweep_best_effort(&worktree_ctx);

    match route {
        ServeRoute::Linked => run_linked_worktree(config_path, &worktree_ctx),
        ServeRoute::NoIndex => {
            // No index yet. Rather than exiting 1 — which leaves the MCP
            // client staring at a server that died at startup with the
            // reason buried in stderr — serve a degraded stdio session that
            // answers `initialize` and chirps "run analyze" from every tool
            // call. clarion-ac36f51c2b.
            serve_no_index(path, &db_path)
        }
        ServeRoute::Full => run_server(db_path, config_path, &worktree_ctx, false),
    }
}

/// Bootstrap and serve a linked worktree's isolated index (worktree-indexes
/// design, "Bootstrap"): ensure the isolated store exists (Task 3), spawn a
/// **detached** `loomweave worktree analyze` for it if (and only if) it has
/// never finished a build, then serve the full `ServerState` path
/// immediately with the bootstrap gate active. Graph tools answer
/// `index-building` (or `index-build-failed`) until a completed run row
/// appears — no waiting here, no reconnect required once it does
/// (`loomweave_mcp::worktree_bootstrap`'s per-call readiness consult).
fn run_linked_worktree(
    config_path: Option<&Path>,
    worktree_ctx: &loomweave_core::worktree::WorktreeContext,
) -> Result<()> {
    bootstrap_linked_worktree(worktree_ctx, None)?;
    let db_path = worktree_ctx.store_paths.db.clone();
    run_server(db_path, config_path, worktree_ctx, true)
}

/// Ensure the isolated store exists, then spawn `loomweave worktree analyze`
/// for it **only when it has never finished a build** — per the design,
/// "serve on a linked worktree WITH NO INDEX ... spawn". A worktree that
/// already has a completed run is not respawned on every `serve` restart
/// (that would fire a redundant 20-30 minute re-analyze every time an agent
/// session starts against an already-built worktree); keeping a built index
/// fresh is the `SessionStart` hook's job, not this one.
///
/// `analyze_program` overrides the launcher (`None` -> `current_exe()`);
/// tests inject a stub so the spawn (whether it happens at all, and its
/// exact argv) is assertable without a real multi-minute analyze — see
/// `tests` below.
fn bootstrap_linked_worktree(
    worktree_ctx: &loomweave_core::worktree::WorktreeContext,
    analyze_program: Option<&Path>,
) -> Result<()> {
    // `ensure_isolated_store`'s own contract: an un-`install`ed primary is
    // this caller's error to diagnose, not something it silently half-creates
    // (its `create_dir_all` would otherwise happily materialize the
    // primary's `.weft/loomweave/` tree, bypassing `install`'s own
    // initialization). Mirrors the identical check `analyze.rs` already runs
    // before calling the same function.
    if !worktree_ctx.repository_store.exists() {
        bail!(
            "{} has no .weft/loomweave/ store. Run `loomweave install` first.",
            worktree_ctx.primary_root.display()
        );
    }
    loomweave_cli::worktree::store::ensure_isolated_store(worktree_ctx)
        .map_err(|err| anyhow!("{err}"))
        .with_context(|| {
            format!(
                "ensure isolated store for linked worktree {}",
                worktree_ctx.source_root.display()
            )
        })?;

    let should_spawn = match rusqlite::Connection::open_with_flags(
        &worktree_ctx.store_paths.db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(conn) => loomweave_mcp::worktree_bootstrap::should_spawn_bootstrap_analyze(&conn),
        Err(err) => {
            // Ambiguous — fail toward spawning rather than risk leaving the
            // operator stuck with an unbuilt store and no automatic recovery.
            tracing::warn!(
                error = %err,
                db = %worktree_ctx.store_paths.db.display(),
                "worktree bootstrap: could not read the store to check build status; \
                 spawning `loomweave worktree analyze` to be safe"
            );
            true
        }
    };

    if !should_spawn {
        tracing::info!(
            source_root = %worktree_ctx.source_root.display(),
            "worktree bootstrap: a completed run already exists; skipping the bootstrap spawn \
             (staleness refresh is the SessionStart hook's job)"
        );
        return Ok(());
    }

    // Fire-and-forget: a spawn failure (e.g. the executable vanished) is
    // logged and swallowed inside `spawn_detached_worktree_analyze` — serve
    // must keep running in the `building` state either way, with
    // `loomweave worktree analyze` as the documented manual fallback.
    // Double-spawn (e.g. two `serve` processes racing the same worktree) is
    // tolerated by the existing per-store `analyze_lock.rs` fs2 lock the
    // spawned child acquires before writing any run row — nothing here needs
    // to check or wait for it.
    let resolved_program = match analyze_program {
        Some(program) => Ok(program.to_path_buf()),
        None => std::env::current_exe(),
    };
    match resolved_program {
        Ok(program) => loomweave_mcp::worktree_bootstrap::spawn_detached_worktree_analyze(
            &program,
            &worktree_ctx.source_root,
        ),
        Err(err) => tracing::warn!(
            error = %err,
            "worktree bootstrap: cannot resolve the loomweave executable to launch \
             `loomweave worktree analyze`; the index will stay in the building state until it \
             is run manually"
        ),
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn run_server(
    db_path: PathBuf,
    config_path: Option<&Path>,
    worktree_ctx: &loomweave_core::worktree::WorktreeContext,
    gated: bool,
) -> Result<()> {
    let project_root = worktree_ctx.source_root.clone();
    let instance_id = crate::instance::load_or_create(&worktree_ctx.store_paths.instance_id)
        .context("load Loomweave project instance ID")?;
    let default_config_path = worktree_ctx.config_path();
    let config_path = config_path.unwrap_or(&default_config_path);
    let config = if config_path.exists() {
        McpConfig::from_path(config_path)
            .with_context(|| format!("load MCP config {}", config_path.display()))?
    } else {
        McpConfig::default()
    };
    let provider_selection = select_provider_with_env(&config, |name| std::env::var(name).ok())?;
    let llm_diagnostics = llm_diagnostics(&provider_selection, &config.llm);
    // Announce the *effective* LLM posture on stderr so a misconfigured provider
    // is never silently disabled (agent-first-feedback §2.1/§2.6). stdout is the
    // JSON-RPC channel, so diagnostics must not go there.
    if llm_diagnostics.live {
        tracing::info!(
            provider = %llm_diagnostics.provider,
            model = %config.llm.effective_model_label(),
            "LLM live: entity_summary_get will dispatch to the provider"
        );
    } else {
        tracing::info!(
            provider = %llm_diagnostics.provider,
            "LLM not live: entity_summary_get is cache-only"
        );
    }
    for warning in config.llm_warnings() {
        tracing::warn!("loomweave.yaml: {warning}");
    }
    let llm_provider = build_llm_provider(&config, provider_selection, &project_root)?;
    let embedding_provider =
        build_embedding_provider(&config.semantic_search, |name| std::env::var(name).ok())?;

    // Sibling (Filigree) local-state discovery: source root first, then
    // primary root, deduplicated — a linked worktree may run its own
    // worktree-scoped Filigree instance, but the common case is one Filigree
    // scoped to the repository as a whole at the primary root. Collapses to
    // the single `project_root` list for a standalone checkout or the main
    // worktree, so behavior there is unchanged.
    let sibling_roots = loomweave_federation::filigree_url::dedup_candidate_roots(
        &worktree_ctx.source_root,
        &worktree_ctx.primary_root,
    );

    // Resolve where Filigree actually listens — prefer the live ethereal port
    // published in `.weft/filigree/ephemeral.port` over the static configured
    // port (which goes stale, the dogfood bug) — then build the client against the
    // resolved URL so `issues_for` reaches the running dashboard. The same
    // resolution is surfaced by `project_status`.
    let filigree_resolution = loomweave_federation::filigree_url::resolve_filigree_url_with_roots(
        &config.integrations.filigree,
        &sibling_roots,
        |name| std::env::var(name).ok(),
    );
    let mut filigree_config = config.integrations.filigree.clone();
    if let Some(resolved) = &filigree_resolution.resolved_url {
        filigree_config.base_url.clone_from(resolved);
    }
    // Pass the sibling-discovery roots so token resolution can reach the
    // daemon's auto-minted `.weft/filigree/federation_token` — the serve path
    // runs with an empty env in `.mcp.json`, and without the file rung every
    // weft-gated read (the wardline-findings joins) 401s (dogfood-4 A5).
    let filigree_client = FiligreeHttpClient::from_config_with_project_roots(
        &filigree_config,
        |name| std::env::var(name).ok(),
        &sibling_roots,
    )
    .context("build Filigree HTTP client")?;

    // Read-only Warpline churn consumer for the high-churn / recently-changed
    // surfaces. `None` when disabled (the default) — the surfaces degrade
    // honestly. Enrich-only, dependency-sink: nothing flows loomweave→warpline.
    let warpline_client =
        WarplineMcpClient::from_config(&config.integrations.warpline, Some(&project_root));

    let diagnostics = loomweave_mcp::DiagnosticsContext {
        llm: llm_diagnostics,
        filigree: filigree_resolution,
    };

    // Eagerly validate the DB at boot so a missing/corrupt index fails fast
    // here rather than deferring to the first reader (or lazily creating an
    // empty DB) — clarion-e74b6e69e5.
    let readers = ReaderPool::open_validated(&db_path, 16)
        .map_err(|err| anyhow!("open reader pool for {}: {err}", db_path.display()))?;
    let http_project_root = project_root.clone();
    let http_server = crate::http_read::spawn(
        http_project_root,
        db_path.clone(),
        readers.clone(),
        instance_id,
        worktree_ctx.store_paths.port.clone(),
        &config.serve.http,
    )
    .context("start HTTP read API")?;
    if let Some(server) = http_server.as_ref() {
        debug_assert!(
            std::sync::Arc::ptr_eq(server.readers_identity(), readers.identity()),
            "HTTP read API and MCP stdio must share a single ReaderPool — the HTTP \
             thread reported a different pool identity than the MCP-side handle. \
             A refactor that re-opens the pool inside http_read::spawn would \
             produce exactly this mismatch."
        );
    }
    let stdio = spawn_mcp_stdio(
        project_root,
        db_path,
        readers,
        config.llm.clone(),
        llm_provider,
        semantic_search_state(&config.semantic_search, embedding_provider),
        filigree_client,
        warpline_client,
        diagnostics,
        loomweave_mcp::McpToolPolicy {
            enable_write_tools: config.serve.mcp.enable_write_tools,
        },
        // review #12: forward serve's resolved config to analyze_start, but only
        // when it exists on disk (the McpConfig::default() fallback has no file).
        config_path.exists().then(|| config_path.to_path_buf()),
        gated,
        worktree_ctx.store_paths.clone(),
    )?;
    supervise_stdio_with_http(stdio, http_server)
}

/// Serve a degraded MCP stdio session for a project with no index. No HTTP read
/// API, no LLM / embedding providers, no Filigree client, no `ReaderPool` —
/// there is no DB to back any of them. The session answers `initialize` and
/// chirps "run `loomweave install` + `loomweave analyze`" from every tool call,
/// so the client connects and is told how to recover instead of seeing the
/// server exit. clarion-ac36f51c2b.
fn serve_no_index(project_root: &Path, db_path: &Path) -> Result<()> {
    // Goes to stderr (the CLI's tracing sink) — never stdout, which carries the
    // MCP protocol — so it lands in the MCP server log without corrupting framing.
    tracing::warn!(
        db = %db_path.display(),
        "Loomweave has no index; serving a degraded MCP session. Run \
         `loomweave analyze` to build the graph, then reconnect."
    );
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = stdout.lock();
    loomweave_mcp::serve_stdio_no_index(project_root, &mut reader, &mut writer)
        .context("serve degraded MCP stdio (no index)")
}

/// Capture the LLM policy posture for `project_status`. `live` means a provider
/// that actually dispatches (`OpenRouter` / Codex / Claude CLIs); the recording
/// fixture and the disabled state are not live.
fn llm_diagnostics(
    selection: &ProviderSelection,
    llm: &LlmConfig,
) -> loomweave_mcp::LlmDiagnostics {
    let (provider, live) = match selection {
        ProviderSelection::Disabled => ("disabled", false),
        ProviderSelection::Recording => ("recording", false),
        ProviderSelection::OpenRouter { .. } => ("openrouter", true),
        ProviderSelection::CodexCli => ("codex_cli", true),
        ProviderSelection::ClaudeCli => ("claude_cli", true),
    };
    loomweave_mcp::LlmDiagnostics {
        provider: provider.to_owned(),
        enabled: llm.enabled,
        live,
        allow_live_provider: llm.allow_live_provider,
        cache_max_age_days: llm.cache_max_age_days,
    }
}

struct StdioServe {
    result_rx: mpsc::Receiver<Result<()>>,
    join: thread::JoinHandle<()>,
}

type SemanticSearchState = (SemanticSearchConfig, Arc<dyn EmbeddingProvider>);

#[allow(clippy::too_many_arguments)]
fn spawn_mcp_stdio(
    project_root: PathBuf,
    db_path: PathBuf,
    readers: ReaderPool,
    llm_config: LlmConfig,
    llm_provider: Option<Arc<dyn LlmProvider>>,
    semantic_search: Option<SemanticSearchState>,
    filigree_client: Option<FiligreeHttpClient>,
    warpline_client: Option<WarplineMcpClient>,
    diagnostics: loomweave_mcp::DiagnosticsContext,
    tool_policy: loomweave_mcp::McpToolPolicy,
    analyze_config_path: Option<PathBuf>,
    gated: bool,
    store_paths: loomweave_core::worktree::StorePaths,
) -> Result<StdioServe> {
    let (result_tx, result_rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("loomweave-mcp-stdio".to_owned())
        .spawn(move || {
            let result = run_mcp_stdio(
                project_root,
                &db_path,
                readers,
                llm_config,
                llm_provider,
                semantic_search,
                filigree_client,
                warpline_client,
                diagnostics,
                tool_policy,
                analyze_config_path,
                gated,
                store_paths,
            );
            let _ = result_tx.send(result);
        })
        .context("spawn MCP stdio server thread")?;
    Ok(StdioServe { result_rx, join })
}

#[allow(clippy::too_many_arguments)]
fn run_mcp_stdio(
    project_root: PathBuf,
    db_path: &Path,
    readers: ReaderPool,
    llm_config: LlmConfig,
    llm_provider: Option<Arc<dyn LlmProvider>>,
    semantic_search: Option<SemanticSearchState>,
    filigree_client: Option<FiligreeHttpClient>,
    warpline_client: Option<WarplineMcpClient>,
    diagnostics: loomweave_mcp::DiagnosticsContext,
    tool_policy: loomweave_mcp::McpToolPolicy,
    analyze_config_path: Option<PathBuf>,
    gated: bool,
    store_paths: loomweave_core::worktree::StorePaths,
) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = stdout.lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create MCP runtime")?;
    let _runtime_guard = runtime.enter();
    // The gate needs both `store_paths` and `project_root` (moved into
    // `ServerState::new` next) while they are both still available — hence
    // computed up front, before either is consumed. `store_paths` carries
    // every explicit leaf path (db/embeddings/runs/...) `ServerState`'s
    // `effective_*` accessors need for a gated (linked-worktree) session
    // (worktree-index Task 7) — not just the db path.
    let worktree_gate = gated.then(|| (project_root.clone(), store_paths));
    let mut state =
        loomweave_mcp::ServerState::new(project_root, readers).with_tool_policy(tool_policy);
    if let Some((source_root, store_paths)) = worktree_gate {
        state = state.with_worktree_gate(store_paths, &source_root);
    }
    // Forward serve's config to an analyze_start-spawned analyze so the child
    // parses the same configuration (review #12). Some only when serve was
    // launched with an on-disk config — the McpConfig::default() fallback has
    // no file to forward.
    if let Some(analyze_config_path) = analyze_config_path {
        state = state.with_analyze_config(analyze_config_path);
    }
    let mut llm_writer = None;
    let mut llm_writer_join = None;
    if let Some(provider) = llm_provider {
        let (writer, handle) = Writer::spawn(db_path, DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY)
            .map_err(|err| anyhow!("spawn MCP LLM writer for {}: {err}", db_path.display()))?;
        state = state.with_summary_llm(writer.sender(), llm_config, provider);
        llm_writer = Some(writer);
        llm_writer_join = Some(handle);
    }
    if let Some((semantic_config, embedding_provider)) = semantic_search {
        state = state.with_semantic_search(semantic_config, embedding_provider);
    }
    if let Some(client) = filigree_client {
        state = state.with_filigree_client(Arc::new(client));
    }
    if let Some(client) = warpline_client {
        state = state.with_warpline_client(Arc::new(client));
    }
    state = state.with_diagnostics(diagnostics);

    let serve_result = loomweave_mcp::serve_stdio_with_state_on_runtime(
        &runtime,
        &state,
        &mut reader,
        &mut writer,
    )
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

/// Construct the embedding provider for `search_semantic` from config. Returns
/// `None` (honest degrade — the tool reports "not enabled") when semantic search
/// is disabled, or when it is enabled but live access is not opted in / no API
/// key is present. A genuine misconfiguration (e.g. zero dimensions) fails fast.
pub(crate) fn build_embedding_provider(
    config: &SemanticSearchConfig,
    read_env: impl Fn(&str) -> Option<String>,
) -> Result<Option<Arc<dyn EmbeddingProvider>>> {
    if !config.enabled {
        return Ok(None);
    }
    let api_key = read_env(&config.api_key_env);
    let (allow_live_provider, api_key) = match config.provider {
        SemanticProviderKind::Api => {
            if !config.allow_live_provider {
                return Ok(warn_inert_embedding_provider(
                    &EmbeddingProviderError::LiveProviderNotAllowed,
                ));
            }
            if api_key.as_deref().is_none_or(|key| key.trim().is_empty()) {
                return Ok(warn_inert_embedding_provider(
                    &EmbeddingProviderError::MissingApiKey,
                ));
            }
            (true, api_key)
        }
        SemanticProviderKind::LocalOpenAi => {
            config
                .validate_endpoint_trust()
                .context("validate local semantic embedding endpoint")?;
            (true, api_key)
        }
    };
    match ApiEmbeddingProvider::from_config(ApiEmbeddingProviderConfig {
        api_key,
        allow_live_provider,
        model_id: config.model_id.clone(),
        endpoint_url: config.endpoint_url.clone(),
        dimensions: config.dimensions,
        timeout_seconds: config.timeout_seconds,
    }) {
        Ok(provider) => Ok(Some(Arc::new(provider))),
        // Opt-in / key absent → degrade honestly rather than fail serve startup.
        Err(
            err @ (EmbeddingProviderError::LiveProviderNotAllowed
            | EmbeddingProviderError::MissingApiKey),
        ) => {
            tracing::warn!(
                error = %err,
                "semantic_search.enabled=true but the embedding provider could not be \
                 constructed; search_semantic will report not_enabled"
            );
            Ok(None)
        }
        Err(err) => Err(anyhow!("build embedding provider: {err}")),
    }
}

fn warn_inert_embedding_provider(
    err: &EmbeddingProviderError,
) -> Option<Arc<dyn EmbeddingProvider>> {
    tracing::warn!(
        error = %err,
        "semantic_search.enabled=true but the embedding provider could not be \
         constructed; search_semantic will report not_enabled"
    );
    None
}

/// Pair the (cloned) config with a constructed provider so `run_mcp_stdio` can
/// call `with_semantic_search`. `None` provider → semantic search stays off.
fn semantic_search_state(
    config: &SemanticSearchConfig,
    provider: Option<Arc<dyn EmbeddingProvider>>,
) -> Option<SemanticSearchState> {
    provider.map(|provider| (config.clone(), provider))
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
    let provider: Option<Arc<dyn LlmProvider>> = match selection {
        ProviderSelection::Disabled => None,
        ProviderSelection::Recording => {
            let recordings = load_recording_fixture(config, project_root)?;
            Some(Arc::new(RecordingProvider::from_recordings(recordings)) as Arc<dyn LlmProvider>)
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
                timeout_seconds: config.llm.openrouter.timeout_seconds,
            })
            .context("build OpenRouter LLM provider")?;
            Some(Arc::new(provider) as Arc<dyn LlmProvider>)
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
            Some(Arc::new(provider) as Arc<dyn LlmProvider>)
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
            Some(Arc::new(provider) as Arc<dyn LlmProvider>)
        }
    };
    Ok(provider.map(|provider| {
        Arc::new(TrafficLoggingProvider::new(
            provider,
            loomweave_core::store::llm_traffic_log_path(project_root),
        )) as Arc<dyn LlmProvider>
    }))
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
    use loomweave_core::worktree::{WorktreeContext, WorktreeKind};
    use std::process::Command as StdCommand;
    use std::time::{Duration as StdDuration, Instant};

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

    // ---------------------------------------------------------------
    // pinned decision 1's routing branch (review finding 2): deleting
    // `ServeRoute::Linked`'s arm, or the `run_linked_worktree` call it
    // selects, must fail a test here.
    // ---------------------------------------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?} in {}: {e}", dir.display()));
        assert!(
            output.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(dir: &Path, branch: &str) {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", branch]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        fs::write(dir.join("f.txt"), "hi\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "init"]);
    }

    /// A real primary repo with one linked worktree, resolved.
    fn linked_context(root: &Path) -> WorktreeContext {
        let repo = root.join("repo");
        init_repo(&repo, "main");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feature", "../linked"],
        );
        let linked = root.join("linked");
        let ctx = WorktreeContext::resolve(&linked).expect("resolves as Linked");
        assert_eq!(ctx.kind, WorktreeKind::Linked, "fixture must be Linked");
        ctx
    }

    /// `loomweave install`'s one load-bearing effect this module's own
    /// `bootstrap_linked_worktree` depends on: `repository_store` exists.
    fn install_primary(ctx: &WorktreeContext) {
        fs::create_dir_all(&ctx.repository_store).expect("create repository store");
    }

    #[test]
    fn linked_context_routes_to_linked_not_no_index() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        // No `.weft/loomweave/` under the linked worktree's own source root
        // at all — exactly the "unbuilt" state `serve` bootstraps from. If
        // `choose_serve_route` (or its caller) ever fell back to treating
        // this like a plain missing-index checkout, this would assert
        // `NoIndex` and the whole no-reconnect design breaks.
        let db_exists = loomweave_core::store::db_path(&ctx.source_root).exists();
        assert!(
            !db_exists,
            "fixture sanity: no local store should exist yet"
        );
        assert_eq!(
            choose_serve_route(ctx.kind, db_exists),
            ServeRoute::Linked,
            "a linked worktree must never route to serve_no_index"
        );
    }

    #[test]
    fn main_worktree_without_an_index_routes_to_no_index() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo, "main");
        let ctx = WorktreeContext::resolve(&repo).expect("resolves as Main");
        assert_eq!(ctx.kind, WorktreeKind::Main);

        assert_eq!(
            choose_serve_route(ctx.kind, false),
            ServeRoute::NoIndex,
            "main/standalone with no index must keep routing to serve_no_index"
        );
        assert_eq!(
            choose_serve_route(ctx.kind, true),
            ServeRoute::Full,
            "main/standalone with an index must keep routing to the full server"
        );
    }

    // ---------------------------------------------------------------
    // `bootstrap_linked_worktree`'s spawn decision + exact argv.
    // ---------------------------------------------------------------

    /// A stub standing in for `loomweave` itself: dumps its full argv (one
    /// arg per line) to `argv_dump`, mirroring the pattern in
    /// `loomweave-mcp`'s own `analyze_runs.rs` / `worktree_bootstrap.rs`
    /// tests.
    #[cfg(unix)]
    fn write_argv_dump_stub(dir: &Path, argv_dump: &Path) -> PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let script = dir.join("argv-stub.sh");
        let mut file = fs::File::create(&script).unwrap();
        writeln!(
            file,
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"{}\"\n",
            argv_dump.display()
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// Poll `path` until it exists or `deadline` elapses — the spawn is
    /// fire-and-forget, so the stub writing its argv dump races this test.
    /// Bounded, no unbounded sleep loop.
    fn wait_for_file(path: &Path, deadline: StdDuration) {
        let start = Instant::now();
        while !path.exists() {
            assert!(
                start.elapsed() <= deadline,
                "{} was not written within {deadline:?}",
                path.display()
            );
            std::thread::sleep(StdDuration::from_millis(20));
        }
    }

    #[test]
    #[cfg(unix)]
    fn bootstrap_spawns_worktree_analyze_with_exact_argv_when_unbuilt() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        install_primary(&ctx);

        let argv_dump = tmp.path().join("argv.txt");
        let stub = write_argv_dump_stub(tmp.path(), &argv_dump);

        bootstrap_linked_worktree(&ctx, Some(&stub)).expect("bootstrap on an unbuilt worktree");

        wait_for_file(&argv_dump, StdDuration::from_secs(5));
        let argv = fs::read_to_string(&argv_dump).expect("stub wrote argv");
        let forwarded: Vec<&str> = argv.lines().collect();
        assert_eq!(
            forwarded,
            vec![
                "worktree",
                "analyze",
                "--",
                ctx.source_root.to_str().unwrap()
            ],
            "the bootstrap spawn's argv must be exactly `worktree analyze -- <target>`, not the \
             hook's plain-analyze argv"
        );
    }

    #[test]
    #[cfg(unix)]
    fn bootstrap_spawns_nothing_when_a_completed_run_already_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        install_primary(&ctx);

        // Build the store once for real (no stub needed — `ensure_isolated_store`
        // is the same function `bootstrap_linked_worktree` itself calls), then
        // seed a completed run directly, simulating a worktree `serve` has
        // already successfully bootstrapped in an earlier session.
        loomweave_cli::worktree::store::ensure_isolated_store(&ctx).expect("ensure store");
        let conn = rusqlite::Connection::open(&ctx.store_paths.db).expect("open store db");
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('r1', '2026-01-01T00:00:00.000Z', '2026-01-01T00:01:00.000Z', '{}', '{}', \
             'completed')",
            [],
        )
        .expect("seed completed run");
        drop(conn);

        let argv_dump = tmp.path().join("argv.txt");
        let stub = write_argv_dump_stub(tmp.path(), &argv_dump);

        bootstrap_linked_worktree(&ctx, Some(&stub)).expect("bootstrap on a built worktree");

        // Deterministic, no wait needed: `bootstrap_linked_worktree` decides
        // whether to spawn synchronously, inside this call, before returning
        // — if it decided not to spawn, `Command::spawn` was never invoked.
        assert!(
            !argv_dump.exists(),
            "a worktree with a completed run must not be respawned on every serve restart"
        );
    }
}
