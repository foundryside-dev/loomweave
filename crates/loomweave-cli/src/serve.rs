use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use loomweave_federation::config::{
    ConfigTrust, LlmConfig, McpConfig, ProviderSelection, SemanticProviderKind,
    SemanticSearchConfig, log_config_trust_once, select_provider_with_env,
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

#[derive(Debug, Clone)]
struct WorktreeServeGate {
    fallback_argv: Vec<String>,
    bootstrap_spawn_failed: bool,
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

/// Process-wide SIGINT/SIGTERM latch for `serve` (clarion-7ad374bac4).
///
/// `serve` publishes its actually-bound HTTP port to
/// `<store>/ephemeral.port` (ADR-044) and relies on
/// `http_read::PublishedPortGuard`'s Drop to compare-and-delete it — which
/// covers graceful shutdown, error return, and panic-unwind, but NOT the
/// default SIGINT/SIGTERM dispositions (Ctrl-C on a foreground serve, or a
/// supervisor such as Codex SIGTERM-ing its MCP children), which terminate
/// the process without running destructors and strand the marker; consumers
/// then classify HTTP as configured from a dead port. The handler is
/// async-signal-safe: it only stores the signal number into a static atomic
/// (`signal-hook`'s `flag::register_usize`), which the supervision loop in
/// [`supervise_stdio_with_http`] polls on its existing 100ms tick.
#[cfg(unix)]
mod termination_signal {
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, LazyLock, Once};

    /// `0` = no termination signal observed yet; otherwise the raw signal
    /// number (SIGINT = 2, SIGTERM = 15).
    static RECEIVED: LazyLock<Arc<AtomicUsize>> = LazyLock::new(|| Arc::new(AtomicUsize::new(0)));
    static INSTALL: Once = Once::new();

    /// Install the latch handlers. Idempotent, and called ONLY from the real
    /// `serve` entry ([`super::run`]) — never from library code paths that
    /// unit tests exercise concurrently, so tests never mutate process-wide
    /// signal dispositions.
    pub(super) fn install() {
        INSTALL.call_once(|| {
            for signal in [
                signal_hook::consts::signal::SIGINT,
                signal_hook::consts::signal::SIGTERM,
            ] {
                let observed =
                    usize::try_from(signal).expect("SIGINT/SIGTERM numbers are positive");
                if let Err(err) =
                    signal_hook::flag::register_usize(signal, Arc::clone(&RECEIVED), observed)
                {
                    // Degraded, not fatal: serve still works; only the
                    // signal-time ephemeral.port cleanup is lost (the same
                    // posture as SIGKILL, which read-side validation
                    // tolerates).
                    tracing::warn!(
                        signal,
                        error = %err,
                        "could not install termination-signal handler; ephemeral.port \
                         cleanup on this signal is unavailable"
                    );
                }
            }
        });
    }

    pub(super) fn flag() -> &'static AtomicUsize {
        &RECEIVED
    }
}

/// Non-unix stub: Linux is the supported platform
/// (`docs/operator/README.md`); elsewhere the latch never fires and `serve`
/// keeps its pre-existing behavior (Drop-guard cleanup on graceful paths
/// only).
#[cfg(not(unix))]
mod termination_signal {
    use std::sync::atomic::AtomicUsize;

    static RECEIVED: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn install() {}

    pub(super) fn flag() -> &'static AtomicUsize {
        &RECEIVED
    }
}

pub fn run(path: &Path, config_path: Option<&Path>) -> Result<()> {
    // Latch SIGINT/SIGTERM before any route can publish an ephemeral.port
    // marker, so a signal during the whole serve lifetime reaches the
    // supervision loop's cleanup instead of the destructor-skipping default
    // disposition (clarion-7ad374bac4).
    termination_signal::install();
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
        ServeRoute::Full => run_server(db_path, config_path, &worktree_ctx, None),
    }
}

/// Bootstrap and serve a linked worktree's isolated index (worktree-indexes
/// design, "Bootstrap"): ensure the isolated store exists (Task 3), spawn a
/// **detached** `loomweave worktree analyze` for it if (and only if) no prior
/// analyze attempt has written a run row, then serve the full `ServerState` path
/// immediately with the bootstrap gate active. Graph tools answer
/// `index-building` (or `index-build-failed`) until a completed run row
/// appears — no waiting here, no reconnect required once it does
/// (`loomweave_mcp::worktree_bootstrap`'s per-call readiness consult).
fn run_linked_worktree(
    config_path: Option<&Path>,
    worktree_ctx: &loomweave_core::worktree::WorktreeContext,
) -> Result<()> {
    // An un-`install`ed primary cannot back this worktree's isolated store.
    // Mirror the primary route's degraded session (clarion-ac36f51c2b)
    // instead of exiting 1 with the reason buried in stderr
    // (clarion-459bf2ac3b) — and anchor the recovery hint at the PRIMARY
    // root: installing there creates the repository store this worktree's
    // isolated store nests under, while a worktree-root hint would
    // materialize the forbidden decoy store (clarion-f8b577dc48).
    if !worktree_ctx.repository_store.exists() {
        return serve_no_index(
            &worktree_ctx.primary_root,
            &worktree_ctx.repository_store.join("loomweave.db"),
        );
    }
    let bootstrap_spawn_failed = bootstrap_linked_worktree(worktree_ctx, None, config_path)?;
    let db_path = worktree_ctx.store_paths.db.clone();
    run_server(
        db_path,
        config_path,
        worktree_ctx,
        Some(WorktreeServeGate {
            fallback_argv: loomweave_mcp::worktree_bootstrap::fallback_argv(
                &worktree_ctx.source_root,
                config_path,
            ),
            bootstrap_spawn_failed,
        }),
    )
}

/// How long the Held-lock wait tolerates a lock-holding analyze whose store
/// generation never matches this binary's expectations before serving the
/// existing (schema-bearing) store degraded behind the index gate. Generous:
/// a healthy same-version holder publishes metadata + schema within
/// milliseconds-to-seconds of taking the lock (even a stale-store
/// delete-and-rebuild is one directory), so only a genuine version skew ever
/// reaches this cap — and the cap converts it from a dead session into a
/// gated one.
const HELD_WAIT_DEGRADED_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// One iteration's decision in `bootstrap_linked_worktree`'s Held-lock wait —
/// a pure function of three observations so the policy is directly
/// unit-assertable (see `tests::held_wait_step_*`) without staging a real
/// cross-process lock race.
///
/// Invariants pinned here:
/// - A bare schema probe (`runs_table_ready`) alone NEVER breaks the wait:
///   without `metadata_current` it can be the old, about-to-be-deleted store
///   generation of a holder mid-`ensure_isolated_store` delete-and-rebuild.
/// - The wait NEVER errors. Transient states (slow holder, crashed holder)
///   resolve by the caller's lock retry; the only terminal fallback is
///   serving degraded behind the gate after [`HELD_WAIT_DEGRADED_CAP`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldWaitStep {
    /// The on-disk generation matches this binary AND the schema milestone is
    /// published: safe to open the reader pool, holder keeps building.
    ServeCurrent,
    /// Cap exceeded with a schema-bearing db whose metadata never matched
    /// (version-skewed holder): serve it behind the index gate rather than
    /// failing the session.
    ServeDegraded,
    /// Keep waiting — and keep re-trying the lock, which self-heals a holder
    /// that crashed before initializing anything.
    Wait,
}

fn held_wait_step(
    metadata_current: bool,
    runs_table_ready: bool,
    past_degraded_cap: bool,
) -> HeldWaitStep {
    if metadata_current && runs_table_ready {
        HeldWaitStep::ServeCurrent
    } else if past_degraded_cap && runs_table_ready {
        HeldWaitStep::ServeDegraded
    } else {
        HeldWaitStep::Wait
    }
}

/// The bootstrap-milestone probe: the holder's `initialise_db` (schema
/// applied, empty) has run against the db at `db_path`. Read-only open so an
/// absent file is never lazily created as an empty db.
fn runs_table_exists(db_path: &Path) -> bool {
    rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|conn| {
            conn.query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'runs'",
                [],
                |_row| Ok(()),
            )
        })
        .is_ok()
}

/// Ensure the isolated store exists, then spawn `loomweave worktree analyze`
/// for it **only when no analyze attempt has written a run row** — per the
/// design, "serve on a linked worktree WITH NO INDEX ... spawn". A worktree
/// with a completed or running row is not given redundant background work;
/// keeping it fresh is the `SessionStart` hook's job. A failed row likewise
/// requires the surfaced explicit recovery command instead of a doomed retry
/// on every `serve` restart.
///
/// `analyze_program` overrides the launcher (`None` -> `current_exe()`);
/// tests inject a stub so the spawn (whether it happens at all, and its
/// exact argv) is assertable without a real multi-minute analyze — see
/// `tests` below.
fn bootstrap_linked_worktree(
    worktree_ctx: &loomweave_core::worktree::WorktreeContext,
    analyze_program: Option<&Path>,
    // serve's own explicit `--config`, forwarded verbatim to the spawned
    // analyze (clarion-c39b92b868). `None` (the default ladder) needs no
    // forwarding: the child re-derives the same source→primary ladder.
    explicit_config: Option<&Path>,
    // Returns whether the bootstrap SPAWN failed (unresolvable executable or
    // spawn error) — surfaced through the gate into `project_status_get`'s
    // `index_state` as `bootstrap-spawn-failed` (clarion-917df0e1ad), so an
    // agent polling for progress learns the build will not start by itself.
    // `false` when the spawn succeeded OR was correctly skipped (already
    // built): only a failure that strands the store in `building` matters.
) -> Result<bool> {
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
    // Retry the lock in a loop rather than deciding once: the observed holder
    // can release (analyze finished) or die (fs2 releases on process exit —
    // e.g. a SessionStart analyze crashing between lock acquisition and
    // `initialise_db`), and this serve must then become the initializer
    // itself instead of failing. serve never hard-fails on a merely-slow
    // concurrent analyze; before the Held arm existed it always served with
    // the index-building gate active, and that survivability is preserved.
    let wait_started = std::time::Instant::now();
    let mut last_wait_log: Option<std::time::Instant> = None;
    let should_spawn = loop {
        match crate::analyze_lock::try_acquire_analyze_lock_for_context(worktree_ctx)? {
            crate::analyze_lock::TryAnalyzeLock::Acquired(_initialization_guard) => {
                loomweave_cli::worktree::store::ensure_isolated_store(worktree_ctx)
                    .map_err(|err| anyhow!("{err}"))
                    .with_context(|| {
                        format!(
                            "ensure isolated store for linked worktree {}",
                            worktree_ctx.source_root.display()
                        )
                    })?;

                break match rusqlite::Connection::open_with_flags(
                    &worktree_ctx.store_paths.db,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                ) {
                    Ok(conn) => {
                        loomweave_mcp::worktree_bootstrap::should_spawn_bootstrap_analyze(&conn)
                    }
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
            }
            crate::analyze_lock::TryAnalyzeLock::Held { lock_path } => {
                // A SessionStart/manual analyze owns the stable lock. It is
                // the initializer and builder; never race it with
                // ensure/migrations or launch a redundant child. But its
                // `ensure_isolated_store` call may be about to
                // delete-and-rebuild the store, so a bare "does the db have a
                // schema" probe can pass against the OLD, doomed generation —
                // and a `ReaderPool` connection opened against that file
                // before the unlink would pin the deleted generation for the
                // whole session. Probe the metadata generation FIRST (a
                // rebuilding holder deletes metadata.json before writing the
                // new one — see `isolated_store_metadata_is_current`), and
                // only then the schema milestone, so a pass proves the
                // current on-disk generation is the one the holder keeps.
                let metadata_current =
                    loomweave_cli::worktree::store::isolated_store_metadata_is_current(
                        worktree_ctx,
                    );
                let runs_table_ready = runs_table_exists(&worktree_ctx.store_paths.db);
                match held_wait_step(
                    metadata_current,
                    runs_table_ready,
                    wait_started.elapsed() > HELD_WAIT_DEGRADED_CAP,
                ) {
                    HeldWaitStep::ServeCurrent => break false,
                    HeldWaitStep::ServeDegraded => {
                        // A schema-bearing db exists but its metadata does not
                        // match this binary's expectations — most plausibly a
                        // holder built from a different Loomweave version. Its
                        // rebuild (if any) would have happened long before the
                        // cap. Serve what exists, behind the gate, rather than
                        // killing the MCP session over a version skew.
                        tracing::warn!(
                            lock = %lock_path.display(),
                            db = %worktree_ctx.store_paths.db.display(),
                            waited_secs = wait_started.elapsed().as_secs(),
                            "worktree bootstrap: analyze lock still held and the store's \
                             metadata.json does not match this binary; serving the existing \
                             store behind the index gate instead of failing"
                        );
                        break false;
                    }
                    HeldWaitStep::Wait => {
                        if last_wait_log
                            .is_none_or(|at| at.elapsed() > std::time::Duration::from_secs(5))
                        {
                            last_wait_log = Some(std::time::Instant::now());
                            tracing::info!(
                                lock = %lock_path.display(),
                                waited_secs = wait_started.elapsed().as_secs(),
                                "worktree bootstrap: waiting for the analyze holding the \
                                 worktree lock to publish the store schema (or release the \
                                 lock)"
                            );
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
            }
        }
    };

    if !should_spawn {
        tracing::info!(
            source_root = %worktree_ctx.source_root.display(),
            "worktree bootstrap: a completed run already exists; skipping the bootstrap spawn \
             (staleness refresh is the SessionStart hook's job)"
        );
        return Ok(false);
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
    let spawn_failed = match resolved_program {
        Ok(program) => !loomweave_mcp::worktree_bootstrap::spawn_detached_worktree_analyze(
            &program,
            &worktree_ctx.source_root,
            explicit_config,
            &worktree_ctx.store_paths.db,
        ),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "worktree bootstrap: cannot resolve the loomweave executable to launch \
                 `loomweave worktree analyze`; the index will stay in the building state until \
                 it is run manually"
            );
            true
        }
    };
    Ok(spawn_failed)
}

#[allow(clippy::too_many_lines)]
fn run_server(
    db_path: PathBuf,
    config_path: Option<&Path>,
    worktree_ctx: &loomweave_core::worktree::WorktreeContext,
    // `None` = ungated (main/standalone). `Some(spawn_failed)` = a linked
    // worktree with the bootstrap gate active; the bool records whether the
    // bootstrap spawn failed (clarion-917df0e1ad).
    worktree_gate: Option<WorktreeServeGate>,
) -> Result<()> {
    let project_root = worktree_ctx.source_root.clone();
    let instance_id = crate::instance::load_or_create(&worktree_ctx.store_paths.instance_id)
        .context("load Loomweave project instance ID")?;
    let default_config_path = worktree_ctx.config_path();
    let config_path = config_path.unwrap_or(&default_config_path);
    // ADR-063: a repository-tracked loomweave.yaml is corpus content; its
    // egress sections (llm_policy / semantic_search / integrations /
    // serve.http) are stripped before validation, and the verdict is announced
    // once and threaded into the server state for the read surfaces.
    let (config, config_trust) = if config_path.exists() {
        let loaded = McpConfig::load_trusted(config_path)
            .with_context(|| format!("load MCP config {}", config_path.display()))?;
        log_config_trust_once(&loaded);
        (loaded.config, loaded.trust)
    } else {
        // Absent: the built-in defaults are in effect and no repository content
        // shaped them.
        (McpConfig::default(), ConfigTrust::OperatorOwned)
    };
    let provider_selection = select_provider_with_env(&config, loomweave_core::dotenv::var)?;
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
    let llm_provider = build_llm_provider(
        &config,
        provider_selection,
        &project_root,
        &worktree_ctx.effective_store,
    )?;
    let embedding_provider =
        build_embedding_provider(&config.semantic_search, loomweave_core::dotenv::var)?;

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

    // Resolve where Filigree actually listens — prefer the live ephemeral port
    // published in `.weft/filigree/ephemeral.port` over the static configured
    // port (which goes stale, the dogfood bug) — then build the client against the
    // resolved URL so `issues_for` reaches the running dashboard. The same
    // resolution is surfaced by `project_status`.
    let filigree_resolution = loomweave_federation::filigree_url::resolve_filigree_url_with_roots(
        &config.integrations.filigree,
        &sibling_roots,
        loomweave_core::dotenv::var,
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
        loomweave_core::dotenv::var,
        &sibling_roots,
        // Paired resolution (clarion-f93e006216): the minted token must come
        // from the same root the URL's port rung won at, never a stale token
        // from another candidate root.
        filigree_resolution.winning_root.as_deref(),
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
    // The HTTP read API gates on the SAME `gated` flag as the MCP stdio gate
    // below (clarion-ecf882f230): a linked-worktree bootstrap must not serve
    // well-formed empty federation answers over the still-building store.
    // Same fallback argv as `with_worktree_gate` so both surfaces name one
    // recovery command.
    let http_worktree_gate =
        worktree_gate
            .as_ref()
            .map(|gate| crate::http_read::WorktreeHttpGate {
                fallback_argv: gate.fallback_argv.clone(),
                bootstrap_spawn_failed: gate.bootstrap_spawn_failed,
            });
    let http_server = crate::http_read::spawn(
        http_project_root,
        db_path.clone(),
        readers.clone(),
        instance_id,
        worktree_ctx.store_paths.port.clone(),
        &config.serve.http,
        http_worktree_gate,
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
        config_trust,
        // review #12: forward serve's resolved config to analyze_start, but only
        // when it exists on disk (the McpConfig::default() fallback has no file).
        config_path.exists().then(|| config_path.to_path_buf()),
        worktree_gate,
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
    config_trust: ConfigTrust,
    analyze_config_path: Option<PathBuf>,
    worktree_gate: Option<WorktreeServeGate>,
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
                config_trust,
                analyze_config_path,
                worktree_gate,
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
    config_trust: ConfigTrust,
    analyze_config_path: Option<PathBuf>,
    worktree_gate: Option<WorktreeServeGate>,
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
    let worktree_gate = worktree_gate.map(|gate| (store_paths, gate));
    let mut state = loomweave_mcp::ServerState::new(project_root, readers)
        .with_tool_policy(tool_policy)
        .with_config_trust(config_trust);
    if let Some((store_paths, gate)) = worktree_gate {
        state =
            state.with_worktree_gate(store_paths, gate.fallback_argv, gate.bootstrap_spawn_failed);
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

/// The supervision loop's terminal outcome, separated from
/// `std::process::exit` so the signal path is unit-testable without
/// delivering real signals (clarion-7ad374bac4, see
/// `tests::supervisor_signal_shuts_down_http_and_reports_signal_exit`).
#[derive(Debug)]
enum SupervisedOutcome {
    /// The MCP stdio thread finished (client closed stdin, an error, or a
    /// supervised HTTP failure) — the pre-existing return path, unchanged.
    Stdio(Result<()>),
    /// SIGINT/SIGTERM was observed: the HTTP read API has already been shut
    /// down (its `PublishedPortGuard` compare-and-deleted this instance's
    /// ephemeral.port marker) and the caller must exit the process with this
    /// conventional `128 + signo` code — the stdio thread is blocked on
    /// stdin and can never be joined.
    SignalExit(i32),
    /// The parent process went away without closing our stdin (an MCP client
    /// that died, or handed our pipes to something that outlived it —
    /// clarion-ebf404dfbb: 14 idle `serve`s on one checkout). The HTTP read
    /// API has been shut down like the signal path; the caller exits clean.
    ParentGone,
}

fn supervise_stdio_with_http(
    stdio: StdioServe,
    http_server: Option<crate::http_read::HttpReadServer>,
) -> Result<()> {
    // Parent liveness: `getppid` is a safe std call (no `prctl`/PDEATHSIG,
    // which would need unsafe FFI). A reparenting — to pid 1 or a subreaper —
    // means the process that launched us is gone.
    #[cfg(unix)]
    let initial_parent = std::os::unix::process::parent_id();
    #[cfg(unix)]
    let parent_alive = move || std::os::unix::process::parent_id() == initial_parent;
    #[cfg(not(unix))]
    let parent_alive = || true;
    match supervise_stdio_http_and_signals(
        stdio,
        http_server,
        termination_signal::flag(),
        &parent_alive,
    ) {
        SupervisedOutcome::Stdio(result) => result,
        SupervisedOutcome::SignalExit(code) => {
            // HTTP cleanup already ran inside the loop; do NOT wait on the
            // stdin-blocked stdio thread — terminate promptly with the
            // conventional signal exit code (130 SIGINT / 143 SIGTERM).
            std::process::exit(code)
        }
        // Same reasoning: the stdio thread is parked on a stdin nobody will
        // close; exit promptly and cleanly.
        SupervisedOutcome::ParentGone => std::process::exit(0),
    }
}

/// Loop iterations (100 ms each) between parent-liveness probes: ~2 s, so a
/// dead client is noticed promptly without a `getppid` per tick.
const PARENT_PROBE_EVERY_TICKS: u32 = 20;

fn supervise_stdio_http_and_signals(
    stdio: StdioServe,
    mut http_server: Option<crate::http_read::HttpReadServer>,
    signal_flag: &std::sync::atomic::AtomicUsize,
    parent_alive: &dyn Fn() -> bool,
) -> SupervisedOutcome {
    let mut tick: u32 = 0;
    let serve_result = loop {
        let signo = signal_flag.load(std::sync::atomic::Ordering::SeqCst);
        if signo != 0 {
            return handle_termination_signal(signo, http_server.take());
        }
        if tick.is_multiple_of(PARENT_PROBE_EVERY_TICKS) && !parent_alive() {
            return handle_parent_gone(http_server.take());
        }
        tick = tick.wrapping_add(1);
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
                    return SupervisedOutcome::Stdio(Err(
                        err.context("HTTP read API failed while MCP stdio was running")
                    ));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let join_result = stdio
                    .join
                    .join()
                    .map_err(|_| anyhow!("MCP stdio server thread panicked"))
                    .and_then(|()| Err(anyhow!("MCP stdio server thread exited without a result")));
                return SupervisedOutcome::Stdio(join_result);
            }
        }
    };
    if stdio.join.join().is_err() {
        return SupervisedOutcome::Stdio(Err(anyhow!("MCP stdio server thread panicked")));
    }
    let shutdown_result = match http_server {
        Some(server) => server.shutdown().context("stop HTTP read API"),
        None => Ok(()),
    };
    SupervisedOutcome::Stdio(finish_supervised_result(serve_result, shutdown_result))
}

/// The "signal observed" step of the supervision loop: shut the HTTP read
/// API down and report the exit code. Shutting down joins the HTTP thread,
/// which drops `http_read::PublishedPortGuard` — whose compare-and-delete
/// removes ONLY this instance's ephemeral.port marker. That guard stays the
/// single deletion mechanism: unlinking the file here directly would strand
/// or clobber a second serve instance's overwritten port (the two-serve
/// scenario `loomweave_port::remove_published_port_if_matches` exists for).
fn handle_termination_signal(
    signo: usize,
    http_server: Option<crate::http_read::HttpReadServer>,
) -> SupervisedOutcome {
    if let Some(server) = http_server
        && let Err(err) = server.shutdown()
    {
        tracing::warn!(
            error = %err,
            "failed to stop HTTP read API on termination signal; the published \
             ephemeral.port marker may be stranded"
        );
    }
    tracing::info!(
        signal = signo,
        "termination signal observed; HTTP read API stopped and ephemeral.port released"
    );
    SupervisedOutcome::SignalExit(termination_exit_code(signo))
}

fn handle_parent_gone(http_server: Option<crate::http_read::HttpReadServer>) -> SupervisedOutcome {
    if let Some(server) = http_server
        && let Err(err) = server.shutdown()
    {
        tracing::warn!(
            error = %err,
            "failed to stop HTTP read API after the parent process went away; the \
             published ephemeral.port marker may be stranded"
        );
    }
    tracing::info!(
        "parent process went away without closing stdin; HTTP read API stopped and \
         ephemeral.port released — exiting"
    );
    SupervisedOutcome::ParentGone
}

/// Conventional fatal-signal exit code: `128 + signo` — 130 for SIGINT,
/// 143 for SIGTERM.
fn termination_exit_code(signo: usize) -> i32 {
    128_i32.saturating_add(i32::try_from(signo).unwrap_or(0))
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
    effective_store: &Path,
) -> Result<Option<Arc<dyn LlmProvider>>> {
    let provider: Option<Arc<dyn LlmProvider>> = match selection {
        ProviderSelection::Disabled => None,
        ProviderSelection::Recording => {
            let recordings = load_recording_fixture(config, project_root)?;
            Some(Arc::new(RecordingProvider::from_recordings(recordings)) as Arc<dyn LlmProvider>)
        }
        ProviderSelection::OpenRouter { api_key_env } => {
            let api_key = loomweave_core::dotenv::var(&api_key_env);
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
            loomweave_core::store::llm_traffic_log_path_at(effective_store),
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
    // clarion-7ad374bac4: the supervisor's "signal observed" step —
    // shutdown + ephemeral.port cleanup + conventional exit code —
    // exercised via the injected flag, no real signals.
    // ---------------------------------------------------------------

    #[test]
    fn termination_exit_code_uses_the_128_plus_signo_convention() {
        assert_eq!(termination_exit_code(2), 130, "SIGINT");
        assert_eq!(termination_exit_code(15), 143, "SIGTERM");
    }

    /// A `StdioServe` whose thread blocks "on stdin" forever (a channel that
    /// never delivers), exactly like the real MCP stdio thread during an
    /// externally-signalled serve. The thread is never joined on the signal
    /// path — it dies with the (test) process.
    fn stdin_blocked_stdio() -> (StdioServe, mpsc::Sender<()>) {
        let (result_tx, result_rx) = mpsc::channel::<Result<()>>();
        let (unblock_tx, unblock_rx) = mpsc::channel::<()>();
        let join = thread::spawn(move || {
            let _ = unblock_rx.recv();
            drop(result_tx);
        });
        (StdioServe { result_rx, join }, unblock_tx)
    }

    #[test]
    fn supervisor_signal_shuts_down_http_and_reports_signal_exit() {
        use std::sync::atomic::AtomicUsize;

        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("loomweave.db");
        let readers = loomweave_storage::ReaderPool::open(&db, 4).expect("open reader pool");
        let config = loomweave_federation::config::HttpReadConfig {
            enabled: true,
            // Explicit loopback `:0` — OS-assigned, no cross-test port races,
            // and loopback means the ephemeral.port marker IS published.
            bind: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0))),
            ..loomweave_federation::config::HttpReadConfig::default()
        };
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-00000000007a")
                .expect("parse synthetic instance id");
        let port_path = loomweave_federation::loomweave_port::published_port_path(tmp.path());
        let server = crate::http_read::spawn(
            tmp.path().to_path_buf(),
            db,
            readers,
            instance_id,
            port_path.clone(),
            &config,
            None,
        )
        .expect("spawn HTTP read API")
        .expect("enabled => Some(server)");
        assert!(
            port_path.exists(),
            "fixture sanity: ephemeral.port must be published while serving"
        );

        let (stdio, _unblock_tx) = stdin_blocked_stdio();
        // SIGTERM "already delivered": the handler stored the signal number.
        let signal_flag = AtomicUsize::new(15);
        let outcome = supervise_stdio_http_and_signals(stdio, Some(server), &signal_flag, &|| true);

        match outcome {
            SupervisedOutcome::SignalExit(code) => assert_eq!(
                code, 143,
                "SIGTERM must map to the conventional 143 exit code"
            ),
            SupervisedOutcome::Stdio(result) => {
                panic!("signal must win over the stdin-blocked stdio thread, got {result:?}")
            }
            SupervisedOutcome::ParentGone => panic!("the parent probe said alive"),
        }
        assert!(
            !port_path.exists(),
            "the HTTP shutdown must have dropped PublishedPortGuard, whose \
             compare-and-delete removes this instance's ephemeral.port marker"
        );
    }

    /// clarion-ebf404dfbb: a parent that died without closing our stdin must
    /// not leave `serve` idling forever. With the liveness probe reporting
    /// the parent gone, the supervisor shuts the HTTP read API down (marker
    /// released) and reports `ParentGone` even though the stdio thread is
    /// parked on stdin.
    #[test]
    fn supervisor_exits_when_the_parent_process_goes_away() {
        use std::sync::atomic::AtomicUsize;

        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("loomweave.db");
        let readers = loomweave_storage::ReaderPool::open(&db, 4).expect("open reader pool");
        let config = loomweave_federation::config::HttpReadConfig {
            enabled: true,
            bind: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0))),
            ..loomweave_federation::config::HttpReadConfig::default()
        };
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-00000000007b")
                .expect("parse synthetic instance id");
        let port_path = loomweave_federation::loomweave_port::published_port_path(tmp.path());
        let server = crate::http_read::spawn(
            tmp.path().to_path_buf(),
            db,
            readers,
            instance_id,
            port_path.clone(),
            &config,
            None,
        )
        .expect("spawn HTTP read API")
        .expect("enabled => Some(server)");
        assert!(port_path.exists());

        let (stdio, _unblock_tx) = stdin_blocked_stdio();
        let signal_flag = AtomicUsize::new(0);
        let outcome =
            supervise_stdio_http_and_signals(stdio, Some(server), &signal_flag, &|| false);

        assert!(
            matches!(outcome, SupervisedOutcome::ParentGone),
            "a dead parent must end supervision, got {outcome:?}"
        );
        assert!(
            !port_path.exists(),
            "the HTTP shutdown must release this instance's ephemeral.port marker"
        );
    }

    /// Regression pin for the no-signal path: with the flag never set, the
    /// supervisor behaves exactly as before — the stdio result is returned
    /// once the MCP thread finishes.
    #[test]
    fn supervisor_without_signal_returns_the_stdio_result() {
        use std::sync::atomic::AtomicUsize;

        let (result_tx, result_rx) = mpsc::channel::<Result<()>>();
        let join = thread::spawn(move || {
            result_tx
                .send(Err(anyhow!("stdio finished with an error")))
                .expect("deliver stdio result");
        });
        let stdio = StdioServe { result_rx, join };

        let signal_flag = AtomicUsize::new(0);
        match supervise_stdio_http_and_signals(stdio, None, &signal_flag, &|| true) {
            SupervisedOutcome::Stdio(result) => {
                let err = result.expect_err("the stdio error must be preserved");
                assert!(
                    format!("{err:#}").contains("stdio finished with an error"),
                    "unexpected error: {err:#}"
                );
            }
            SupervisedOutcome::SignalExit(code) => {
                panic!("no signal was set, but the supervisor reported SignalExit({code})")
            }
            SupervisedOutcome::ParentGone => panic!("the parent probe said alive"),
        }
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

        bootstrap_linked_worktree(&ctx, Some(&stub), None)
            .expect("bootstrap on an unbuilt worktree");

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

    /// clarion-c39b92b868: serve's own explicit `--config` is forwarded
    /// verbatim to the spawned analyze — the one config input the child
    /// cannot re-derive. (The ladder default needs no forwarding: the child
    /// re-derives the same source→primary ladder, pinned by
    /// `worktree_analyze_uses_primary_config_when_worktree_has_none` in
    /// `tests/worktree_analyze.rs`.)
    #[test]
    #[cfg(unix)]
    fn bootstrap_spawn_forwards_serves_explicit_config() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        install_primary(&ctx);

        let argv_dump = tmp.path().join("argv.txt");
        let stub = write_argv_dump_stub(tmp.path(), &argv_dump);
        let explicit = tmp.path().join("explicit.yaml");

        bootstrap_linked_worktree(&ctx, Some(&stub), Some(&explicit))
            .expect("bootstrap with explicit config");

        wait_for_file(&argv_dump, StdDuration::from_secs(5));
        let argv = fs::read_to_string(&argv_dump).expect("stub wrote argv");
        let forwarded: Vec<&str> = argv.lines().collect();
        assert_eq!(
            forwarded,
            vec![
                "worktree",
                "analyze",
                "--config",
                explicit.to_str().unwrap(),
                "--",
                ctx.source_root.to_str().unwrap()
            ],
            "an explicit serve --config must reach the spawned analyze"
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

        bootstrap_linked_worktree(&ctx, Some(&stub), None).expect("bootstrap on a built worktree");

        // Deterministic, no wait needed: `bootstrap_linked_worktree` decides
        // whether to spawn synchronously, inside this call, before returning
        // — if it decided not to spawn, `Command::spawn` was never invoked.
        assert!(
            !argv_dump.exists(),
            "a worktree with a completed run must not be respawned on every serve restart"
        );
    }

    #[test]
    #[cfg(unix)]
    fn bootstrap_child_failure_before_begin_run_becomes_a_persisted_failed_attempt() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        install_primary(&ctx);
        let stub = tmp.path().join("failing-analyze.sh");
        let mut file = fs::File::create(&stub).unwrap();
        writeln!(file, "#!/bin/sh\nexit 17").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        drop(file);

        bootstrap_linked_worktree(&ctx, Some(&stub), None)
            .expect("serve remains available after the child exits");

        let start = Instant::now();
        loop {
            let conn = rusqlite::Connection::open(&ctx.store_paths.db).unwrap();
            let read = loomweave_mcp::worktree_bootstrap::read_worktree_readiness(&conn);
            if read.readiness == loomweave_mcp::worktree_bootstrap::WorktreeReadiness::BuildFailed {
                break;
            }
            assert!(
                start.elapsed() <= StdDuration::from_secs(5),
                "the reaper must record an early child failure instead of leaving readiness at Building: {read:?}"
            );
            std::thread::sleep(StdDuration::from_millis(20));
        }
    }

    // -----------------------------------------------------------------
    // worktree-index Task 7 fix-loop finding 1: `build_llm_provider`'s
    // `TrafficLoggingProvider` must log under the linked worktree's
    // isolated `effective_store`, never re-derive
    // `llm_traffic_log_path(project_root)` — which for a linked worktree
    // would materialize `<worktree checkout>/.weft/loomweave/diagnostics/`
    // on the first live LLM call, violating worktree-index isolation.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn linked_worktree_llm_traffic_log_lands_under_effective_store_not_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        install_primary(&ctx);

        let config = McpConfig::default();
        let provider = build_llm_provider(
            &config,
            ProviderSelection::Recording,
            &ctx.source_root,
            &ctx.effective_store,
        )
        .expect("build_llm_provider must succeed for ProviderSelection::Recording")
        .expect("Recording selection always yields a provider");

        // No recordings were seeded, so this errors — but `TrafficLoggingProvider`
        // logs both outcomes (`llm_traffic_success_event`/`llm_traffic_error_event`),
        // so the append still happens.
        let request = loomweave_llm::LlmRequest {
            purpose: loomweave_llm::LlmPurpose::Summary,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            prompt_id: "test-prompt".to_owned(),
            prompt: "irrelevant".to_owned(),
            max_output_tokens: 64,
        };
        let _ = provider.invoke(request).await;

        let correct_log = ctx
            .effective_store
            .join("diagnostics")
            .join("llm-traffic.jsonl");
        assert!(
            correct_log.exists(),
            "the traffic log must be written under the worktree's isolated effective_store: {}",
            correct_log.display()
        );

        let forbidden_log = loomweave_core::store::llm_traffic_log_path(&ctx.source_root);
        assert!(
            !forbidden_log.exists(),
            "the traffic log must NOT be written under the worktree checkout's own \
             re-derived .weft/loomweave/diagnostics/ — that is exactly the local-store \
             materialization worktree-index isolation forbids: {}",
            forbidden_log.display()
        );
    }

    // ---------------------------------------------------------------
    // The Held-analyze-lock wait: generation-aware, and it never
    // hard-fails on a transient concurrent-analyze condition.
    // ---------------------------------------------------------------

    #[test]
    fn held_wait_step_never_trusts_a_bare_schema_probe() {
        use HeldWaitStep::{ServeCurrent, ServeDegraded, Wait};
        // Current generation with the schema milestone published: serve.
        assert_eq!(held_wait_step(true, true, false), ServeCurrent);
        assert_eq!(held_wait_step(true, true, true), ServeCurrent);
        // The finding-1 regression pin: a runs table alone can belong to the
        // OLD generation of a holder mid-delete-and-rebuild — it must never
        // break the wait on its own.
        assert_eq!(held_wait_step(false, true, false), Wait);
        // ...until the degraded cap converts a persistent metadata mismatch
        // (version-skewed holder) into a gated session instead of a dead one.
        assert_eq!(held_wait_step(false, true, true), ServeDegraded);
        // No schema milestone yet: always wait — there is nothing servable,
        // and the caller's lock retry self-heals a crashed holder. No input
        // combination maps to an error.
        assert_eq!(held_wait_step(true, false, false), Wait);
        assert_eq!(held_wait_step(false, false, false), Wait);
        assert_eq!(held_wait_step(true, false, true), Wait);
        assert_eq!(held_wait_step(false, false, true), Wait);
    }

    fn hold_analyze_lock(ctx: &WorktreeContext) -> crate::analyze_lock::AnalyzeLockGuard {
        match crate::analyze_lock::try_acquire_analyze_lock_for_context(ctx)
            .expect("acquire analyze lock for test")
        {
            crate::analyze_lock::TryAnalyzeLock::Acquired(guard) => guard,
            crate::analyze_lock::TryAnalyzeLock::Held { lock_path } => {
                panic!("test fixture must own the lock at {}", lock_path.display())
            }
        }
    }

    /// Finding 1: with the lock Held and the on-disk store belonging to a
    /// STALE generation (mismatched metadata — exactly what the holder's
    /// `ensure_isolated_store` is about to delete-and-rebuild), the wait must
    /// NOT break just because the old db has a `runs` table and a completed
    /// run row. The pre-fix probe returned instantly here and pinned the
    /// doomed file into the reader pool.
    #[test]
    #[cfg(unix)]
    fn held_wait_does_not_serve_a_doomed_store_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        install_primary(&ctx);

        // A real store with a completed run row — maximally deceptive: the
        // bare schema probe passes AND the run row reads as Ready.
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
        // Mark the generation stale: the next `ensure_isolated_store` (the
        // lock holder's) will delete-and-rebuild this whole directory.
        fs::write(ctx.effective_store.join("metadata.json"), "not json").unwrap();

        let argv_dump = tmp.path().join("argv.txt");
        let stub = write_argv_dump_stub(tmp.path(), &argv_dump);
        let guard = hold_analyze_lock(&ctx);

        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tx.send(bootstrap_linked_worktree(&ctx, Some(&stub), None))
                    .expect("report bootstrap result");
            });
            assert!(
                rx.recv_timeout(StdDuration::from_millis(400)).is_err(),
                "bootstrap proceeded against a stale (doomed) store generation while the \
                 analyze lock was still held"
            );
            // The holder releases (finished or died): bootstrap must acquire
            // the lock itself and rebuild rather than trust the stale store.
            drop(guard);
            rx.recv_timeout(StdDuration::from_secs(10))
                .expect("bootstrap finishes once the lock frees")
                .expect("bootstrap succeeds after acquiring the freed lock");
        });

        assert!(
            loomweave_cli::worktree::store::isolated_store_metadata_is_current(&ctx),
            "the stale generation must have been rebuilt"
        );
        // The rebuilt store is empty (the seeded run row died with the old
        // generation), so the bootstrap spawn must fire — proving serve did
        // not trust the doomed generation's completed run row.
        wait_for_file(&argv_dump, StdDuration::from_secs(5));
    }

    /// Finding 2(a): a holder that crashes between lock acquisition and
    /// `initialise_db` releases the fs2 lock with no store on disk. The old
    /// wait probed only the db and bailed after 5 seconds with a misleading
    /// "did not finish initialization" error; it must instead re-try the
    /// now-free lock, become the initializer, and self-recover.
    #[test]
    #[cfg(unix)]
    fn held_lock_released_before_init_self_heals_instead_of_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = linked_context(tmp.path());
        install_primary(&ctx);
        // No store on disk at all — the "holder crashed before creating
        // anything" state.
        let argv_dump = tmp.path().join("argv.txt");
        let stub = write_argv_dump_stub(tmp.path(), &argv_dump);
        let guard = hold_analyze_lock(&ctx);

        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tx.send(bootstrap_linked_worktree(&ctx, Some(&stub), None))
                    .expect("report bootstrap result");
            });
            assert!(
                rx.recv_timeout(StdDuration::from_millis(300)).is_err(),
                "bootstrap must wait while the lock is genuinely held"
            );
            drop(guard); // the "crash": fs2 releases on process/file close
            rx.recv_timeout(StdDuration::from_secs(10))
                .expect("bootstrap finishes once the lock frees")
                .expect("bootstrap must self-recover by taking the freed lock, not error");
        });

        assert!(
            ctx.store_paths.db.is_file(),
            "self-recovery must have created the isolated store"
        );
        wait_for_file(&argv_dump, StdDuration::from_secs(5));
    }
}
