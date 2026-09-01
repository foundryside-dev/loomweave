mod analyze;
mod analyze_lock;
mod cli;
mod config;
mod db;
mod doctor;
mod git_hooks;
mod guidance;
mod hook;
mod hooks_settings;
mod http_read;
mod install;
mod instance;
mod instructions;
mod integration_bindings;
mod mcp_registration;
mod run_lifecycle;
mod sarif;
mod secret_scan;
mod sei_git;
mod serve;
mod skill_pack;
mod stats;

use anyhow::{Context, Result};
use clap::Parser;

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    // Parse `.env` into the credential sidecar — never the process
    // environment (`loomweave_core::dotenv`, ADR-062) — before tracing setup
    // so a `.env`-supplied RUST_LOG is in effect by the time the filter is built.
    if should_load_dotenv(&cli.command) {
        loomweave_core::dotenv::load_sidecar();
    }
    init_tracing();
    match cli.command {
        cli::Command::Install {
            force,
            path,
            claude_code,
            codex,
            codex_config,
            skills,
            codex_skills,
            hooks,
            instructions,
            all,
        } => {
            let mut components = Vec::new();
            if claude_code {
                components.push(install::InstallComponent::ClaudeCode);
            }
            if codex {
                components.push(install::InstallComponent::Codex);
            }
            if skills {
                components.push(install::InstallComponent::Skills);
            }
            if codex_skills {
                components.push(install::InstallComponent::CodexSkills);
            }
            if hooks {
                components.push(install::InstallComponent::Hooks);
            }
            if instructions {
                components.push(install::InstallComponent::Instructions);
            }
            install::run(
                &path,
                force,
                install::InstallPlan::from_components(all, &components),
                codex_config.as_deref(),
            )
        }
        cli::Command::Analyze {
            path,
            config,
            allow_unredacted_secrets,
            confirm_allow_unredacted_secrets,
            run_id,
            resume,
            prune_unseen,
            progress_file,
            no_sei,
            no_incremental,
            legis_url,
            json,
        } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let secret_scan = match secret_scan::SecretScanOptions::from_cli(
                allow_unredacted_secrets,
                confirm_allow_unredacted_secrets,
            ) {
                Ok(options) => options,
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(78);
                }
            };
            rt.block_on(analyze::run_with_options_draining_pending(
                path,
                analyze::AnalyzeOptions {
                    config_path: config,
                    secret_scan,
                    run_id,
                    resume_run_id: resume,
                    prune_unseen,
                    progress_file,
                    no_sei,
                    no_incremental,
                    legis_url,
                    json,
                },
            ))
        }
        cli::Command::Serve { path, config } => serve::run(&path, config.as_deref()),
        cli::Command::Hook { command } => match command {
            cli::HookCommand::SessionStart { path } => hook::session_start(&path),
            cli::HookCommand::GitSync { path } => hook::git_sync(&path),
        },
        cli::Command::Db { command } => match command {
            cli::DbCommand::Backup {
                output,
                path,
                force,
            } => db::backup(&path, &output, force),
            cli::DbCommand::Checkpoint { path } => db::checkpoint(&path),
        },
        cli::Command::Guidance { command } => guidance::run(command),
        cli::Command::Config { command } => config::run(command),
        cli::Command::Doctor { path, fix, format } => {
            // doctor prints its own report; map an unhealthy result to a
            // non-zero exit so it can gate CI / pre-commit. The Result<()> arm
            // is reserved for setup errors (bad --path), which bubble normally.
            let healthy = doctor::run(&path, fix, matches!(format, cli::DoctorOutputFormat::Json))?;
            if !healthy {
                std::process::exit(1);
            }
            Ok(())
        }
        cli::Command::Sarif { command } => match command {
            cli::SarifCommand::Import {
                file,
                scan_source,
                path,
            } => sarif::run_import(&file, scan_source, &path),
        },
        cli::Command::Worktree { command } => match command {
            cli::WorktreeCommand::Analyze {
                no_incremental,
                config,
                target,
            } => {
                let cwd = std::env::current_dir().context("determine current directory")?;
                let resolved = loomweave_cli::worktree::cmd::resolve_target(&cwd, &target)
                    .with_context(|| format!("resolve worktree analyze target {target:?}"))?;
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                rt.block_on(analyze::run_with_options_draining_pending(
                    resolved,
                    analyze::AnalyzeOptions {
                        no_incremental,
                        config_path: config,
                        ..analyze::AnalyzeOptions::default()
                    },
                ))
            }
        },
    }
}

/// Whether to consult a repository-controlled `.env` for this command.
///
/// `.env` is parsed into a credential sidecar (`loomweave_core::dotenv`,
/// ADR-062) that only Loomweave's own config-named lookups read — provider
/// keys, Filigree tokens, `RUST_LOG`. It never enters the process environment,
/// so no child process inherits it and no launcher override
/// (`LOOMWEAVE_*_MCP_COMMAND`, `$EDITOR`, `LD_PRELOAD`, …) can be supplied by
/// it. Most operator commands want the sidecar (e.g. a `.env`-supplied
/// `RUST_LOG`, or a Filigree `token_env` consumed by `guidance promote` /
/// `sarif import`). The exclusions below are defence in depth for commands
/// that must not read repository-supplied credentials at all:
///
/// - `analyze` (and `worktree analyze`, which runs the identical pipeline):
///   project `.env` contents are scanned as source sidecars by the
///   pre-ingest secret scanner and must not be consumed before that gate runs.
/// - `hook`: the session-start hook may spawn a detached `analyze`; it reads
///   no credential itself, so it has no business parsing the file.
/// - `guidance create` / `guidance edit`: authoring spawns `$VISUAL`/`$EDITOR`
///   (see `guidance::edit_in_editor`); the sidecar cannot feed those any more,
///   but an editor session is the wrong place to have repository credentials
///   resolvable at all. Only these two `guidance` subcommands spawn an editor;
///   the rest (`promote`, `show`, `list`, `export`, `import`, `delete`) keep
///   the sidecar so a `.env`-supplied Filigree token still resolves.
fn should_load_dotenv(command: &cli::Command) -> bool {
    !matches!(
        command,
        cli::Command::Analyze { .. }
            | cli::Command::Worktree {
                command: cli::WorktreeCommand::Analyze { .. },
            }
            | cli::Command::Hook { .. }
            | cli::Command::Guidance {
                command: cli::GuidanceCommand::Create { .. } | cli::GuidanceCommand::Edit { .. },
            }
    )
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    // `RUST_LOG` may come from the credential sidecar (a `.env` in the cwd),
    // which `try_from_default_env` cannot see.
    let filter = loomweave_core::dotenv::var("RUST_LOG")
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

#[cfg(test)]
mod tests {
    use super::should_load_dotenv;
    use crate::cli::Cli;
    use clap::Parser;

    fn loads(args: &[&str]) -> bool {
        let cli = Cli::try_parse_from(args).expect("valid argv");
        should_load_dotenv(&cli.command)
    }

    #[test]
    fn analyze_does_not_load_dotenv() {
        assert!(!loads(&["loomweave", "analyze", "."]));
    }

    #[test]
    fn worktree_analyze_does_not_load_dotenv() {
        assert!(!loads(&[
            "loomweave",
            "worktree",
            "analyze",
            "--",
            "some-worktree"
        ]));
    }

    #[test]
    fn session_start_hook_does_not_load_dotenv() {
        // A stale session-start hook spawns analyze; repo values must not be
        // inherited by that child before analyze's secret scan runs.
        assert!(!loads(&[
            "loomweave",
            "hook",
            "session-start",
            "--path",
            ".",
        ]));
    }

    #[test]
    fn guidance_editor_subcommands_do_not_load_dotenv() {
        // create/edit spawn $VISUAL/$EDITOR; a repo .env must not feed them.
        assert!(!loads(&[
            "loomweave",
            "guidance",
            "create",
            "--scope-level",
            "module",
            "--match",
            "kind:function",
        ]));
        assert!(!loads(&[
            "loomweave",
            "guidance",
            "edit",
            "core:guidance:x"
        ]));
    }

    #[test]
    fn non_editor_guidance_subcommands_keep_dotenv() {
        // promote resolves a Filigree token from a .env-supplied token_env;
        // excluding it would regress authenticated promotion. These commands
        // never spawn an editor, so loading .env is safe.
        assert!(loads(&["loomweave", "guidance", "promote", "obs-123"]));
        assert!(loads(&["loomweave", "guidance", "show", "core:guidance:x"]));
        assert!(loads(&["loomweave", "guidance", "list"]));
        assert!(loads(&["loomweave", "guidance", "export", "--to", "out"]));
    }

    #[test]
    fn other_commands_load_dotenv() {
        assert!(loads(&["loomweave", "doctor"]));
    }
}
