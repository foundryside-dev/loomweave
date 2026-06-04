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
    // `analyze` is deliberately excluded: project .env contents are scanned
    // as source sidecars by the pre-ingest secret scanner and must not be
    // imported into plugin subprocess environments before that gate runs.
    if !matches!(&cli.command, cli::Command::Analyze { .. }) {
        let _ = dotenvy::dotenv();
    }
    init_tracing();
    match cli.command {
        cli::Command::Install {
            force,
            path,
            skills,
            hooks,
            all,
        } => install::run(
            &path,
            force,
            install::InstallPlan::from_flags(skills, hooks, all),
        ),
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
        cli::Command::Doctor { path, fix } => {
            // doctor prints its own report; map an unhealthy result to a
            // non-zero exit so it can gate CI / pre-commit. The Result<()> arm
            // is reserved for setup errors (bad --path), which bubble normally.
            let healthy = doctor::run(&path, fix)?;
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

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
