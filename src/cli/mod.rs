use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser, Clone)]
#[command(name = "sabantui", version, about = "Universal display configuration CLI/TUI", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// List outputs for the detected or specified backend
    List {
        /// Override backend detection logic
        #[arg(short, long)]
        backend: Option<BackendSelector>,
    },
    /// Apply configuration change (mode, position, etc.)
    Apply {
        #[arg(short, long)]
        backend: Option<BackendSelector>,
        #[arg(long)]
        output: String,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        refresh: Option<u32>,
        /// Target brightness value in range 0.0 - 1.0
        #[arg(long)]
        brightness: Option<f32>,
        /// Gamma multiplier (e.g. 1.0 is neutral)
        #[arg(long)]
        gamma: Option<f32>,
        /// Color temperature in mireds (1000 - 10000)
        #[arg(long, alias = "temp")]
        temperature: Option<u16>,
        #[arg(long)]
        position: Option<String>,
        #[arg(long)]
        orientation: Option<String>,
        #[arg(long)]
        mirror: Option<String>,
        #[arg(long)]
        enabled: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BackendSelector {
    X11,
    Wlroots,
}
