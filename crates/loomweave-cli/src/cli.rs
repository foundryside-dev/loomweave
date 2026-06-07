use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "loomweave",
    version,
    about = "Loomweave code-archaeology tool",
    long_about = "Loomweave extracts a queryable graph from a codebase and serves it to \
consult-mode agents over MCP.\n\n\
Typical flow: `loomweave install` (set up .weft/loomweave/ + agent assets), `loomweave \
analyze` (build the index), `loomweave serve` (run the MCP server).\n\n\
LLM-backed entity summaries are OFF by default. To enable them set \
`llm_policy.enabled: true` + `allow_live_provider: true` in loomweave.yaml and supply \
the provider credential (e.g. OPENROUTER_API_KEY), or point at a coding-agent CLI \
(claude_cli / codex_cli). Run `loomweave config example` to print an annotated config \
and `loomweave config check` to see the effective LLM state; `loomweave doctor` \
validates the install and the config."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialise .weft/loomweave/ and install agent-orientation assets.
    ///
    /// Bare `loomweave install` does everything: .weft/loomweave/ init, Claude Code MCP,
    /// Codex MCP, Claude/Codex skills, and hooks. If .weft/loomweave/ already exists,
    /// init is skipped and the other components are applied idempotently.
    /// Component flags install only the named components without touching
    /// .weft/loomweave/. `--all` is equivalent to a bare install.
    Install {
        /// Overwrite an existing .weft/loomweave/ directory.
        #[arg(long)]
        force: bool,

        /// Directory to install into (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Install MCP config for Claude Code only.
        #[arg(long)]
        claude_code: bool,

        /// Install MCP config for Codex only.
        #[arg(long)]
        codex: bool,

        /// Path to Codex config.toml. Hidden; tests use this to avoid writing
        /// the real user-level ~/.codex/config.toml.
        #[arg(long, hide = true)]
        codex_config: Option<PathBuf>,

        /// Install the bundled loomweave-workflow skill pack into .claude/skills/.
        #[arg(long)]
        skills: bool,

        /// Install the bundled loomweave-workflow skill pack into .agents/skills/.
        #[arg(long)]
        codex_skills: bool,

        /// Merge a `SessionStart` hook into .claude/settings.json.
        #[arg(long)]
        hooks: bool,

        /// Inject the Loomweave agent-orientation block into CLAUDE.md and
        /// AGENTS.md (touching only Loomweave's own marker span).
        #[arg(long)]
        instructions: bool,

        /// Do everything: .weft/loomweave/ init + MCP config + skills + hooks +
        /// instructions.
        #[arg(long)]
        all: bool,
    },

    /// Run an analysis pass: walk the source tree, dispatch discovered plugins
    /// to extract entities/edges, and persist results to `.weft/loomweave/loomweave.db`.
    /// Re-runs are idempotent (UPSERT on `entities.id`). If no plugins are on
    /// `$PATH`, exits 0 with a WARN and status `skipped_no_plugins` — see the
    /// Troubleshooting guide at
    /// <https://github.com/foundryside-dev/loomweave/blob/main/docs/operator/getting-started.md>.
    ///
    /// To commit the index as a versioned artifact while `serve` may be running,
    /// take a consistent online copy with `loomweave db backup` rather than
    /// `git add`-ing the live file (whose pending WAL is not committable).
    Analyze {
        /// Path to analyse (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Path to loomweave.yaml (default: project-root/loomweave.yaml if present).
        #[arg(long)]
        config: Option<PathBuf>,

        /// Allow analysis of files containing unredacted secrets. Requires a
        /// confirmation step when detections are present.
        #[arg(long)]
        allow_unredacted_secrets: bool,

        /// Non-TTY confirmation token for --allow-unredacted-secrets.
        #[arg(long, value_name = "TOKEN", requires = "allow_unredacted_secrets")]
        confirm_allow_unredacted_secrets: Option<String>,

        /// Use this run id instead of generating one. Internal: set by the MCP
        /// `analyze_start` tool so it can return the handle before the run
        /// records its `runs` row. Hidden from `--help`.
        #[arg(long, hide = true)]
        run_id: Option<String>,

        /// Resume a prior run: reuse `RUN_ID` (reopening its `runs` row instead
        /// of starting fresh) and emit findings to Filigree with
        /// `mark_unseen=false`, so re-emitting does not flip the prior run's
        /// findings to `unseen_in_latest` on the peer (REQ-FINDING-05). The
        /// run id is the UUID a normal `loomweave analyze` reports on completion.
        /// This re-walks the tree from scratch (it is not incremental recovery)
        /// and assumes the corpus is unchanged; findings that no longer fire are
        /// not pruned from the resumed run.
        #[arg(long, value_name = "RUN_ID", conflicts_with = "run_id")]
        resume: Option<String>,

        /// After emitting findings, ask Filigree to soft-archive its own
        /// `unseen_in_latest` Loomweave findings older than
        /// `integrations.filigree.prune_unseen_days` (default 30)
        /// (REQ-FINDING-06). Opt-in retention sweep; enrich-only — a Filigree
        /// outage or the integration being disabled never fails the run. The
        /// sweep is `scan_source`-scoped server-side, so it only touches
        /// Loomweave's findings.
        #[arg(long)]
        prune_unseen: bool,

        /// Write structured progress (phase, current plugin, processed/total
        /// files, current file, heartbeat) to this path as the run proceeds,
        /// so `analyze_status` can report progress without log scraping.
        /// Internal: set by the MCP `analyze_start` tool. Hidden from `--help`.
        #[arg(long, hide = true)]
        progress_file: Option<PathBuf>,

        /// Skip the SEI mint pass (ADR-038 / Wave 1). A diagnostic escape hatch
        /// for runs against a pre-migration database or when stable identity is
        /// not needed; the durable entity graph is unaffected (SEI is
        /// enrich-only). Without this flag every analyze run mints/carries SEIs.
        #[arg(long)]
        no_sei: bool,

        /// Force a full re-analysis, disabling the incremental skip of files
        /// unchanged since the last run (Wave 2 / T3.1). A full re-analysis
        /// replays per-source-file edge replacement; use this for a clean
        /// graph refresh. Without this flag unchanged files are skipped.
        #[arg(long)]
        no_incremental: bool,

        /// `legis`'s read-API base URL (e.g. `http://127.0.0.1:8615`), enabling
        /// the WS9 git-rename provider seam (REQ-C-05). Enrich-only and
        /// capability-aware: the operative working-tree rename window stays on
        /// Loomweave's own git probe, so an unset or unreachable `legis` leaves
        /// behaviour byte-identical. Omit to use Loomweave's shell git source only.
        #[arg(long)]
        legis_url: Option<String>,
    },

    /// Run the MCP stdio server.
    ///
    /// Serves the code graph to MCP clients. The `entity_summary_get` tool needs
    /// a live LLM provider, which is OFF by default: set `llm_policy.enabled: true`
    /// and `allow_live_provider: true` in loomweave.yaml and supply the provider
    /// credential (`OPENROUTER_API_KEY` for the default `openrouter` provider), or
    /// switch `llm_policy.provider` to `claude_cli` / `codex_cli` for a locally
    /// authenticated coding-agent CLI. Without that, summaries are cache-only.
    /// Write-capable tools (`entity_summary_get`, `analyze_start`,
    /// `analyze_cancel`, `propose_guidance`, `promote_guidance`) require
    /// `serve.mcp.enable_write_tools: true`. The effective LLM posture is logged
    /// to stderr at startup; run `loomweave config check` to inspect it ahead of
    /// time.
    Serve {
        /// Project directory containing .weft/loomweave/loomweave.db.
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Path to loomweave.yaml (default: project-root/loomweave.yaml if present).
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Agent-lifecycle hook entrypoints. Always exit 0 (fail-soft) so a
    /// misbehaving hook never blocks session start.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },

    /// Local database maintenance.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },

    /// Author guidance sheets — institutional knowledge attached to entities
    /// that the MCP read path composes into briefings (REQ-GUIDANCE-03).
    Guidance {
        #[command(subcommand)]
        command: GuidanceCommand,
    },

    /// Inspect `loomweave.yaml`: print an annotated example, or validate the
    /// file and report the effective LLM provider state. The installed binary
    /// ships no docs, so this is the in-tool way to discover the config schema
    /// and see why live summaries are (or are not) enabled.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Verify (and optionally repair) the installed agent-orientation surfaces:
    /// the `loomweave-workflow` skill pack, the `SessionStart` hook, and the
    /// `.mcp.json` MCP registration. Prints a per-surface report plus the index
    /// snapshot; exits non-zero if any problem remains (usable as a CI /
    /// pre-commit gate).
    Doctor {
        /// Project directory to check (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Repair any problems found, in place (idempotent). Without it, doctor
        /// only reports.
        #[arg(long)]
        fix: bool,

        /// Output format.
        #[arg(long, value_enum, default_value_t = DoctorOutputFormat::Text)]
        format: DoctorOutputFormat,
    },

    /// Import external findings in SARIF format and post them to Filigree.
    Sarif {
        #[command(subcommand)]
        command: SarifCommand,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DoctorOutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
pub enum DbCommand {
    /// Take a consistent, WAL-safe online backup of `.weft/loomweave/loomweave.db`.
    ///
    /// Unlike `cp`, this captures outstanding WAL frames into a standalone
    /// single-file copy, so it is safe to run during a live `loomweave analyze`.
    Backup {
        /// Destination file for the backup copy.
        output: PathBuf,

        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Overwrite the output file if it already exists.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Print an annotated example `loomweave.yaml` to stdout — the same content
    /// `loomweave install` writes, generated so it always matches the current
    /// config schema. Redirect it to `loomweave.yaml` and edit.
    Example {
        /// Pre-select the active LLM provider block in the example
        /// (`openrouter`, `codex_cli`, or `claude_cli`). Defaults to the stub's
        /// `openrouter`.
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<String>,
    },

    /// Parse and validate `loomweave.yaml`, then print the effective LLM state
    /// (provider, enabled, live, model) and any warnings — the in-tool answer to
    /// "why are my summaries cache-only?". Exits non-zero if the file fails to
    /// parse or validate, so it works as a CI / pre-commit gate.
    Check {
        /// Project directory containing loomweave.yaml (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Path to loomweave.yaml (default: `<path>/loomweave.yaml` if present).
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum GuidanceCommand {
    /// Create a new guidance sheet (`kind: guidance`, provenance: manual).
    ///
    /// `--match` syntax is `<type>:<value>` (split on the first colon):
    /// `path:<glob>`, `tag:<tag>`, `kind:<entity-kind>`, `subsystem:<id>`,
    /// `entity:<entity-id>`. Content comes from `--content`, else stdin (when
    /// piped) or `$EDITOR`/`$VISUAL`.
    Create {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// A match rule (`<type>:<value>`); repeatable.
        #[arg(long = "match", value_name = "RULE")]
        r#match: Vec<String>,

        /// Scope level: project | subsystem | package | module | class | function.
        #[arg(long, value_name = "LEVEL")]
        scope_level: String,

        /// Guidance text (markdown). Omit to author via stdin or $EDITOR.
        #[arg(long)]
        content: Option<String>,

        /// Slug for the entity id's third segment (`core:guidance:<name>`).
        /// Defaults to a slug derived from the first match rule.
        #[arg(long)]
        name: Option<String>,

        /// Mark the sheet pinned (preserved under token-budget pressure).
        #[arg(long)]
        pinned: bool,

        /// Optional expiry. Accepts an ISO-8601 instant (e.g.
        /// `2026-12-31T23:59:59Z`), an offset form (converted to UTC), or a bare
        /// date (e.g. `2026-12-31`, taken as start-of-day UTC). Stored
        /// normalized to UTC so the read path's lexical expiry compare is
        /// correct; unparseable input is rejected.
        #[arg(long, value_name = "WHEN")]
        expires: Option<String>,
    },

    /// Edit a sheet's content in `$EDITOR`/`$VISUAL` (other properties, including
    /// `authored_at` and provenance, are preserved).
    Edit {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// The guidance sheet id (`core:guidance:<slug>`).
        id: String,
    },

    /// Print a guidance sheet (human-readable).
    Show {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// The guidance sheet id.
        id: String,
    },

    /// List guidance sheets, ordered by `scope_rank` (project → function).
    ///
    /// `--expired` and `--stale` are independent filters that compose by
    /// intersection (AND): a sheet is shown only if it passes every active
    /// filter (including `--for-entity`). Without any of them, behaves as the
    /// plain list.
    List {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Only list sheets whose `match_rules` apply to this entity id.
        #[arg(long, value_name = "ENTITY_ID")]
        for_entity: Option<String>,
        /// Only show sheets whose `expires` instant is in the past (mirrors the
        /// read path's expiry exclusion). Sheets with no `expires` are excluded.
        #[arg(long)]
        expired: bool,
        /// Only show sheets not "touched" (the later of `reviewed_at` /
        /// `authored_at`) within `--days`. This is the review-cadence/age signal
        /// (system-design §7.741), NOT the churn-based staleness finding.
        #[arg(long)]
        stale: bool,
        /// Staleness window in days for `--stale` (default: 90). Ignored without
        /// `--stale`.
        #[arg(long, value_name = "N", default_value_t = 90)]
        days: u32,
    },

    /// Delete a guidance sheet.
    Delete {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// The guidance sheet id.
        id: String,
    },

    /// Promote a reviewed Filigree guidance-proposal observation into a local
    /// guidance sheet. The observation must have been produced by MCP
    /// `propose_guidance`; arbitrary observations are rejected.
    Promote {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Path to loomweave.yaml (default: project-root/loomweave.yaml if present).
        #[arg(long)]
        config: Option<PathBuf>,
        /// The Filigree observation id to promote.
        observation_id: String,
    },

    /// Export every guidance sheet to a directory as one deterministic,
    /// diff-friendly JSON file per sheet, for committing to a shared repo
    /// (REQ-GUIDANCE-06). Output is byte-stable across runs on identical DB
    /// state. The target directory is created if absent.
    Export {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Directory to write the exported sheet files into. Export does NOT
        /// prune: a sheet deleted locally keeps its file here, and a teammate's
        /// additive `import` would resurrect it. To mirror, clear the directory
        /// before exporting.
        #[arg(long)]
        to: PathBuf,
    },

    /// Import guidance sheets from a directory of exported JSON files
    /// (REQ-GUIDANCE-06). Additive: each sheet is upserted by id, preserving ids
    /// exactly; existing local sheets not present in the directory are left
    /// untouched (never a destructive mirror). A malformed `*.json` aborts the
    /// import naming the offending file (a dropped sheet is silent data loss).
    Import {
        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Directory of exported sheet files to import.
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum HookCommand {
    /// Print a project snapshot and re-sync the skill pack on drift.
    SessionStart {
        /// Project directory containing .weft/loomweave/loomweave.db.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum SarifCommand {
    /// Translate SARIF findings and post them to Filigree.
    Import {
        /// The SARIF file path to import.
        file: PathBuf,

        /// Scan source name to tag the findings (e.g. wardline, semgrep, codeql).
        /// If omitted, defaults to the driver name from the SARIF file.
        #[arg(long)]
        scan_source: Option<String>,

        /// Project directory containing .weft/loomweave/loomweave.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}
