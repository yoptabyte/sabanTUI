use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::process::Command;

use crate::models::{DisplayColorCapabilities, DisplayColorSettings, DisplayMode, DisplayOutput};

pub struct X11Backend;

impl X11Backend {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    fn run_xrandr(args: &[&str]) -> Result<String> {
        let output = Command::new("xrandr")
            .args(args)
            .output()
            .context("failed to execute xrandr")?;

        if !output.status.success() {
            bail!(
                "xrandr exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn run_redshift(args: &[&str]) -> Result<()> {
        let output = Command::new("redshift")
            .args(args)
            .output()
            .context("failed to execute redshift")?;

        if !output.status.success() {
            bail!(
                "redshift exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    fn parse_mode_line(line: &str) -> Option<(DisplayMode, bool, bool)> {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || !trimmed.chars().next()?.is_ascii_digit() {
            return None;
        }

        // Example: "1920x1080     60.00*+ 59.94 50.00"
        let mut parts = trimmed.split_whitespace();
        let resolution = parts.next()?;
        let (width_str, height_str) = resolution.split_once('x')?;
        let width: u32 = width_str.parse().ok()?;
        let height: u32 = height_str.parse().ok()?;

        // Use the first refresh value on the line.
        let refresh_token = parts.next();
        let mut refresh_hz = None;
        let mut is_current = false;
        let mut is_preferred = false;

        if let Some(token) = refresh_token {
            is_current = token.contains('*');
            is_preferred = token.contains('+');
            let cleaned = token.trim_end_matches(['*', '+'].as_ref());
            if let Ok(val) = cleaned.parse::<f32>() {
                refresh_hz = Some(val);
            }
        }

        Some((DisplayMode::new(width, height, refresh_hz), is_current, is_preferred))
    }

    fn parse_xrandr(stdout: &str) -> Vec<DisplayOutput> {
        let mut outputs = Vec::new();
        let mut lines = stdout.lines().peekable();
        let mut id = 0usize;

        while let Some(line) = lines.next() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with("Screen") {
                continue;
            }

            // Output header lines start without indentation.
            if line.starts_with(' ') || line.starts_with('\t') {
                continue;
            }

            let mut tokens = line.split_whitespace();
            let name = match tokens.next() {
                Some(n) => n.to_string(),
                None => continue,
            };
            let status = tokens.next().unwrap_or_default();
            let mut primary = false;
            let mut current_mode: Option<DisplayMode> = None;

            let enabled = status == "connected";
            for token in tokens {
                if token == "primary" {
                    primary = true;
                    continue;
                }
                if token.contains('+') && token.contains('x') && token.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    if let Some((res, _pos)) = token.split_once('+') {
                        if let Some((w, h)) = res.split_once('x') {
                            if let (Ok(width), Ok(height)) = (w.parse::<u32>(), h.parse::<u32>()) {
                                current_mode = Some(DisplayMode::new(width, height, None));
                            }
                        }
                    }
                }
            }

            let mut output = DisplayOutput::simple(id, name.clone(), None);
            output.enabled = enabled;
            output.primary = primary;

            let mut modes = Vec::new();
            let mut preferred: Option<DisplayMode> = None;
            let mut brightness: Option<f32> = None;
            let mut gamma: Option<f32> = None;

            // Walk detail lines (indented) until the next header.
            while let Some(peeked) = lines.peek() {
                if peeked.trim().is_empty() {
                    lines.next();
                    continue;
                }
                if !peeked.starts_with(' ') && !peeked.starts_with('\t') {
                    break;
                }

                let detail = lines.next().unwrap().trim();

                if let Some(rest) = detail.strip_prefix("Brightness:") {
                    if let Ok(val) = rest.trim().parse::<f32>() {
                        brightness = Some(val);
                    }
                    continue;
                }

                if let Some(rest) = detail.strip_prefix("Gamma:") {
                    if let Some(first) = rest.trim().split(':').next() {
                        if let Ok(val) = first.parse::<f32>() {
                            gamma = Some(val);
                        }
                    }
                    continue;
                }

                if let Some((mode, is_current, is_preferred)) = Self::parse_mode_line(detail) {
                    if is_current {
                        current_mode = Some(mode.clone());
                    }
                    if is_preferred && preferred.is_none() {
                        preferred = Some(mode.clone());
                    }
                    modes.push(mode);
                }
            }

            output.available_modes = modes;
            if output.enabled {
                output.current_mode = current_mode.or(preferred);
            } else {
                output.current_mode = None;
            }

            output.color = DisplayColorSettings {
                brightness,
                gamma,
                temperature: None,
            };
            output.color_caps = DisplayColorCapabilities {
                brightness: true,
                gamma: true,
                // Temperature is handled globally through redshift. We still expose
                // the capability so CLI apply can work, even though it is not per-output.
                temperature: true,
            };

            outputs.push(output);
            id += 1;
        }

        outputs
    }
}

#[async_trait]
impl crate::backend::DisplayBackend for X11Backend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>> {
        let stdout = Self::run_xrandr(&["--query", "--verbose"])?;
        Ok(Self::parse_xrandr(&stdout))
    }

    async fn set_brightness(&self, output: &str, value: f32) -> Result<()> {
        let clamped = value.clamp(0.0, 1.0);
        Self::run_xrandr(&["--output", output, "--brightness", &clamped.to_string()]).map(|_| ())
    }

    async fn set_gamma(&self, output: &str, value: f32) -> Result<()> {
        let gamma = (value as f64).max(0.1);
        let gamma_str = format!("{0}:{0}:{0}", gamma);
        Self::run_xrandr(&["--output", output, "--gamma", &gamma_str]).map(|_| ())
    }

    async fn set_temperature(&self, _output: &str, value: u16) -> Result<()> {
        let kelvin = value.clamp(1000, 10000);
        // Redshift applies globally; output argument is ignored.
        Self::run_redshift(&["-O", &kelvin.to_string()])
    }

    async fn set_mode(&self, output: &str, mode: DisplayMode) -> Result<()> {
        let mut final_args = vec![
            "--output".to_string(),
            output.to_string(),
            "--mode".to_string(),
            format!("{}x{}", mode.width, mode.height),
        ];
        if let Some(refresh) = mode.refresh_hz {
            if refresh > 0.0 {
                final_args.push("--rate".to_string());
                final_args.push(format!("{:.3}", refresh));
            }
        }

        let ref_args: Vec<&str> = final_args.iter().map(|s| s.as_str()).collect();
        Self::run_xrandr(&ref_args).map(|_| ())
    }

    async fn set_scale(&self, output: &str, scale: f64) -> Result<()> {
        let scale_str = format!("{0}x{0}", scale);
        Self::run_xrandr(&["--output", output, "--scale", &scale_str]).map(|_| ())
    }

    async fn set_enabled(&self, output: &str, enabled: bool) -> Result<()> {
        let flag = if enabled { "--auto" } else { "--off" };
        Self::run_xrandr(&["--output", output, flag]).map(|_| ())
    }

    async fn set_position(&self, output: &str, x: i32, y: i32) -> Result<()> {
        let pos = format!("{}x{}", x, y);
        Self::run_xrandr(&["--output", output, "--pos", &pos]).map(|_| ())
    }

    async fn set_position_relative(&self, output: &str, relative_to: &str, direction: &str) -> Result<()> {
        let flag = match direction {
            "left" => "--left-of",
            "right" => "--right-of",
            "above" => "--above",
            "below" => "--below",
            _ => bail!("Invalid direction: {}", direction),
        };
        Self::run_xrandr(&["--output", output, flag, relative_to]).map(|_| ())
    }

    async fn set_transform(&self, output: &str, transform: &str) -> Result<()> {
        // xrandr uses "rotate" for simple transforms; for advanced transforms the
        // caller can pass a valid argument directly.
        Self::run_xrandr(&["--output", output, "--rotate", transform]).map(|_| ())
    }

    async fn set_mirror(&self, output: &str, target: &str) -> Result<()> {
        Self::run_xrandr(&["--output", output, "--same-as", target]).map(|_| ())
    }

    async fn set_adaptive_sync(&self, _output: &str, _enabled: bool) -> Result<()> {
        Err(anyhow!("Adaptive sync control is not supported on X11/xrandr"))
    }
}
