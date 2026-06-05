mod analyze;
mod analyze_lock;
mod cli;
mod config;
mod db;
mod doctor;
mod guidance;
mod hook;
mod hooks_settings;
mod http_read;
mod install;
mod instance;
mod integration_bindings;
mod mcp_registration;
mod run_lifecycle;
mod sarif;
mod secret_scan;
mod sei_git;
mod serve;
mod skill_pack;
mod stats;
mod wardline_guidance;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    // Load .env before tracing setup for operator-facing commands so a
    // .env-supplied RUST_LOG is in effect by the time the filter is built.
    if should_load_dotenv(&cli.command) {
        let _ = dotenvy::dotenv();
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
            rt.block_on(analyze::run_with_options(
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
                },
            ))
        }
        cli::Command::Serve { path, config } => serve::run(&path, config.as_deref()),
        cli::Command::Hook { command } => match command {
            cli::HookCommand::SessionStart { path } => hook::session_start(&path),
        },
        cli::Command::Db { command } => match command {
            cli::DbCommand::Backup {
                output,
                path,
                force,
            } => db::backup(&path, &output, force),
        },
        cli::Command::Guidance { command } => guidance::run(command),
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
    }
}

/// Whether to load a repository-controlled `.env` for this command.
///
/// Most operator commands want `.env` loaded (e.g. a `.env`-supplied `RUST_LOG`,
/// or a Filigree `token_env` consumed by `guidance promote` / `sarif import`).
/// Two cases must NOT load it, because they would import repository-controlled
/// values into a subprocess environment before those values have been vetted:
///
/// - `analyze`: project `.env` contents are scanned as source sidecars by the
///   pre-ingest secret scanner and must not reach plugin subprocess
///   environments before that gate runs.
/// - `guidance create` / `guidance edit`: authoring spawns `$VISUAL`/`$EDITOR`
///   (see `guidance::edit_in_editor`), so a repository `.env` supplying
///   `VISUAL`/`EDITOR` — or `PATH`, etc. — would execute attacker-controlled
///   code as the operator who merely opened an untrusted checkout. Only these
///   two `guidance` subcommands spawn an editor; the rest (`promote`, `show`,
///   `list`, `export`, `import`, `delete`) keep `.env` so a `.env`-supplied
///   Filigree token still resolves.
fn should_load_dotenv(command: &cli::Command) -> bool {
    !matches!(
        command,
        cli::Command::Analyze { .. }
            | cli::Command::Guidance {
                command: cli::GuidanceCommand::Create { .. } | cli::GuidanceCommand::Edit { .. },
            }
    )
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
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
        assert!(!loads(&["clarion", "analyze", "."]));
    }

    #[test]
    fn guidance_editor_subcommands_do_not_load_dotenv() {
        // create/edit spawn $VISUAL/$EDITOR; a repo .env must not feed them.
        assert!(!loads(&[
            "clarion",
            "guidance",
            "create",
            "--scope-level",
            "module",
            "--match",
            "kind:function",
        ]));
        assert!(!loads(&["clarion", "guidance", "edit", "core:guidance:x"]));
    }

    #[test]
    fn non_editor_guidance_subcommands_keep_dotenv() {
        // promote resolves a Filigree token from a .env-supplied token_env;
        // excluding it would regress authenticated promotion. These commands
        // never spawn an editor, so loading .env is safe.
        assert!(loads(&["clarion", "guidance", "promote", "obs-123"]));
        assert!(loads(&["clarion", "guidance", "show", "core:guidance:x"]));
        assert!(loads(&["clarion", "guidance", "list"]));
        assert!(loads(&["clarion", "guidance", "export", "--to", "out"]));
    }

    #[test]
    fn other_commands_load_dotenv() {
        assert!(loads(&["clarion", "doctor"]));
    }
}
