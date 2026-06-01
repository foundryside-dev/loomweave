use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "clarion", version, about = "Clarion code-archaeology tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialise .clarion/ and install agent-orientation assets.
    ///
    /// Bare `clarion install` does everything: .clarion/ init + skills + hooks.
    /// If .clarion/ already exists, init is skipped and skills/hooks are applied
    /// idempotently. `--skills` and `--hooks` install only the named components
    /// without touching .clarion/. `--all` is equivalent to a bare install.
    Install {
        /// Overwrite an existing .clarion/ directory.
        #[arg(long)]
        force: bool,

        /// Directory to install into (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Install the bundled clarion-workflow skill pack into
        /// .claude/skills/ and .agents/skills/.
        #[arg(long)]
        skills: bool,

        /// Merge a `SessionStart` hook into .claude/settings.json.
        #[arg(long)]
        hooks: bool,

        /// Do everything: .clarion/ init + --skills + --hooks.
        #[arg(long)]
        all: bool,
    },

    /// Run an analysis pass: walk the source tree, dispatch discovered plugins
    /// to extract entities/edges, and persist results to `.clarion/clarion.db`.
    /// Re-runs are idempotent (UPSERT on `entities.id`). If no plugins are on
    /// `$PATH`, exits 0 with a WARN and status `skipped_no_plugins` — see
    /// `docs/operator/getting-started.md` Troubleshooting.
    Analyze {
        /// Path to analyse (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Path to clarion.yaml (default: project-root/clarion.yaml if present).
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
        /// run id is the UUID a normal `clarion analyze` reports on completion.
        /// This re-walks the tree from scratch (it is not incremental recovery)
        /// and assumes the corpus is unchanged; findings that no longer fire are
        /// not pruned from the resumed run.
        #[arg(long, value_name = "RUN_ID", conflicts_with = "run_id")]
        resume: Option<String>,

        /// After emitting findings, ask Filigree to soft-archive its own
        /// `unseen_in_latest` Clarion findings older than
        /// `integrations.filigree.prune_unseen_days` (default 30)
        /// (REQ-FINDING-06). Opt-in retention sweep; enrich-only — a Filigree
        /// outage or the integration being disabled never fails the run. The
        /// sweep is `scan_source`-scoped server-side, so it only touches
        /// Clarion's findings.
        #[arg(long)]
        prune_unseen: bool,

        /// Write structured progress (phase, current plugin, processed/total
        /// files, current file, heartbeat) to this path as the run proceeds,
        /// so `analyze_status` can report progress without log scraping.
        /// Internal: set by the MCP `analyze_start` tool. Hidden from `--help`.
        #[arg(long, hide = true)]
        progress_file: Option<PathBuf>,
    },

    /// Run the MCP stdio server.
    Serve {
        /// Project directory containing .clarion/clarion.db.
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Path to clarion.yaml (default: project-root/clarion.yaml if present).
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

    /// Verify (and optionally repair) the installed agent-orientation surfaces:
    /// the `clarion-workflow` skill pack, the `SessionStart` hook, and the
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
    },
}

#[derive(Subcommand)]
pub enum DbCommand {
    /// Take a consistent, WAL-safe online backup of `.clarion/clarion.db`.
    ///
    /// Unlike `cp`, this captures outstanding WAL frames into a standalone
    /// single-file copy, so it is safe to run during a live `clarion analyze`.
    Backup {
        /// Destination file for the backup copy.
        output: PathBuf,

        /// Project directory containing .clarion/clarion.db (default: current).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Overwrite the output file if it already exists.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum HookCommand {
    /// Print a project snapshot and re-sync the skill pack on drift.
    SessionStart {
        /// Project directory containing .clarion/clarion.db.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}
