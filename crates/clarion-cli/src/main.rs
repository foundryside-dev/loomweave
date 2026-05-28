mod analyze;
mod analyze_lock;
mod cli;
mod clustering;
mod config;
mod http_read;
mod install;
mod instance;
mod run_lifecycle;
mod secret_scan;
mod serve;
mod skill_pack;
mod stats;

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
            install::InstallComponents::from_flags(skills, hooks, all),
        ),
        cli::Command::Analyze {
            path,
            config,
            allow_unredacted_secrets,
            confirm_allow_unredacted_secrets,
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
            if config.is_none()
                && matches!(
                    secret_scan.override_policy,
                    secret_scan::OverridePolicy::Forbid
                )
            {
                return rt.block_on(analyze::run(path));
            }
            rt.block_on(analyze::run_with_options(
                path,
                analyze::AnalyzeOptions {
                    config_path: config,
                    secret_scan,
                },
            ))
        }
        cli::Command::Serve { path, config } => serve::run(&path, config.as_deref()),
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
