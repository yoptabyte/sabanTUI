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
                scale,
                brightness,
                gamma,
                temperature,
                position,
                orientation,
                mirror,
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
                    *scale,
                    *brightness,
                    *gamma,
                    *temperature,
                    *enabled,
                )?;

                if let Some(pos) = position.as_deref() {
                    let (x, y) = parse_position(pos)?;
                    registry.execute_position(backend, output, x, y)?;
                }

                if let Some(orientation) = orientation.as_deref() {
                    let transform = normalize_transform(orientation)?;
                    registry.execute_transform(backend, output, &transform)?;
                }

                if let Some(target) = mirror.as_deref() {
                    registry.execute_mirror(backend, output, target)?;
                }

                Ok(())
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

fn parse_position(input: &str) -> Result<(i32, i32)> {
    let s = input.trim();
    let (a, b) = s
        .split_once(',')
        .or_else(|| s.split_once('x'))
        .ok_or_else(|| anyhow::anyhow!("invalid position format (expected X,Y)"))?;
    let x = a.trim().parse::<i32>()?;
    let y = b.trim().parse::<i32>()?;
    Ok((x, y))
}

fn normalize_transform(input: &str) -> Result<String> {
    let s = input.trim().to_ascii_lowercase().replace('°', "");
    let s = s.as_str();
    let out = match s {
        "normal" | "0" => "normal",
        "90" | "left" => "90",
        "180" | "inverted" => "180",
        "270" | "right" => "270",
        "flipped" | "flip" => "flipped",
        "flipped-90" | "flip-90" => "flipped-90",
        "flipped-180" | "flip-180" => "flipped-180",
        "flipped-270" | "flip-270" => "flipped-270",
        other => return Err(anyhow::anyhow!("unsupported orientation/transform: {other}")),
    };
    Ok(out.to_string())
}
