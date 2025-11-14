mod app;
mod backend;
mod cli;
mod models;
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::Cli;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let runtime = app::AppRuntime::new(cli);
    runtime.run()
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .try_init();
}
