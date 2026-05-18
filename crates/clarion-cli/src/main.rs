mod analyze;
mod cli;
mod clustering;
mod config;
mod install;
mod secret_scan;
mod serve;
mod stats;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    // Load .env from CWD or any ancestor directory, before tracing setup so a
    // .env-supplied RUST_LOG is in effect by the time the filter is built.
    // Existing process env vars win over .env values (dotenvy default), so an
    // explicit `OPENROUTER_API_KEY=… clarion serve` still beats a checked-in
    // dev .env. Missing .env is not an error — silently skip.
    let _ = dotenvy::dotenv();
    init_tracing();
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Install { force, path } => install::run(&path, force),
        cli::Command::Analyze {
            path,
            config,
            allow_unredacted_secrets,
            confirm_allow_unredacted_secrets,
        } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            if config.is_none()
                && !allow_unredacted_secrets
                && confirm_allow_unredacted_secrets.is_none()
            {
                return rt.block_on(analyze::run(path));
            }
            rt.block_on(analyze::run_with_options(
                path,
                analyze::AnalyzeOptions {
                    config_path: config,
                    secret_scan: secret_scan::SecretScanOptions {
                        allow_unredacted_secrets,
                        confirm_allow_unredacted_secrets,
                    },
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
