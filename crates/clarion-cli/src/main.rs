mod analyze;
mod cli;
mod config;
// Phase 3 Task 1 lands the clustering adapter before Task 5 wires it into
// `clarion analyze`; keep the pre-integration adapter warning-clean meanwhile.
#[allow(dead_code)]
mod clustering;
mod install;
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
        cli::Command::Analyze { path, config } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            if let Some(config_path) = config {
                rt.block_on(analyze::run_with_options(
                    path,
                    analyze::AnalyzeOptions {
                        config_path: Some(config_path),
                    },
                ))
            } else {
                rt.block_on(analyze::run(path))
            }
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
