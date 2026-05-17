mod analyze;
mod cli;
mod install;
mod serve;
mod stats;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    init_tracing();
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Install { force, path } => install::run(&path, force),
        cli::Command::Analyze { path } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(analyze::run(path))
        }
        cli::Command::Serve { path } => serve::run(&path),
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
