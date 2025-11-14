use anyhow::Result;

use crate::backend::{BackendKind, BackendRegistry};
use crate::cli::{Cli, Commands};
use crate::models::DisplayMode;

pub struct AppRuntime {
    cli: Cli,
}

impl AppRuntime {
    pub fn new(cli: Cli) -> Self {
        Self { cli }
    }

    pub fn run(self) -> Result<()> {
        match &self.cli.command {
            Some(Commands::List { backend }) => {
                let backend = backend
                    .map(|value| BackendKind::from(value))
                    .unwrap_or_else(BackendKind::auto_detect);
                let registry = BackendRegistry::default();
                registry.dispatch_list(backend)
            }
            Some(Commands::Apply {
                backend,
                output,
                mode,
                refresh,
                brightness,
                gamma,
                temperature,
                position: _,
                orientation: _,
                mirror: _,
                enabled,
            }) => {
                let backend = backend
                    .map(|value| BackendKind::from(value))
                    .unwrap_or_else(BackendKind::auto_detect);
                let mode = parse_mode_and_refresh(mode.as_deref(), *refresh)?;
                let registry = BackendRegistry::default();
                registry.execute_apply(
                    backend,
                    output,
                    mode,
                    None,
                    *brightness,
                    *gamma,
                    *temperature,
                    *enabled,
                )
            }
            None => {
                // Default to interactive TUI
                crate::tui::run_tui()
            }
        }
    }
}

fn parse_mode_and_refresh(mode: Option<&str>, refresh: Option<u32>) -> Result<Option<DisplayMode>> {
    if let Some(mode_str) = mode {
        let (width, height) = parse_resolution(mode_str)?;
        let refresh = refresh.map(|hz| hz as f32);
        Ok(Some(DisplayMode::new(width, height, refresh)))
    } else {
        Ok(None)
    }
}

fn parse_resolution(input: &str) -> Result<(u32, u32)> {
    let mut parts = input.split('x');
    let width = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid mode format"))?
        .parse::<u32>()?;
    let height = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid mode format"))?
        .parse::<u32>()?;
    Ok((width, height))
}
