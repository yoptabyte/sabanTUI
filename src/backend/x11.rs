use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::models::{DisplayColorCapabilities, DisplayColorSettings, DisplayMode, DisplayOutput};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct X11ColorState {
    brightness: f32,
    gamma: f32,
    temperature: u16,
}

pub struct X11Backend {
    _private: (),
}

impl X11Backend {
    fn config_path() -> Option<PathBuf> {
        if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
            return Some(
                PathBuf::from(xdg)
                    .join("sabantui")
                    .join("x11-color-state.json"),
            );
        }
        env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("sabantui")
                .join("x11-color-state.json")
        })
    }

    fn load_persisted_state() -> HashMap<String, X11ColorState> {
        let Some(path) = Self::config_path() else {
            return HashMap::new();
        };
        let Ok(raw) = fs::read_to_string(path) else {
            return HashMap::new();
        };
        serde_json::from_str::<HashMap<String, X11ColorState>>(&raw).unwrap_or_default()
    }

    fn save_persisted_state(state: &HashMap<String, X11ColorState>) -> Result<()> {
        let path = Self::config_path().ok_or_else(|| anyhow!("no config path for persistence"))?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("invalid config path"))?;
        fs::create_dir_all(parent).context("failed to create config directory")?;
        let raw =
            serde_json::to_string_pretty(state).context("failed to serialize x11 color state")?;
        fs::write(path, raw).context("failed to write x11 color state")?;
        Ok(())
    }

    fn color_state() -> &'static Mutex<HashMap<String, X11ColorState>> {
        static COLOR_STATE: OnceLock<Mutex<HashMap<String, X11ColorState>>> = OnceLock::new();
        COLOR_STATE.get_or_init(|| Mutex::new(Self::load_persisted_state()))
    }

    pub fn new() -> Result<Self> {
        Ok(Self { _private: () })
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

        let mut refresh_hz = None;
        let mut is_current = false;
        let mut is_preferred = false;

        // Handle both compact xrandr output (e.g. "60.00*+") and verbose output
        // (e.g. "(0x4b) 148.500MHz +HSync +VSync *current +preferred").
        for token in parts {
            let lower = token.to_ascii_lowercase();
            if token.contains('*') || lower.contains("current") {
                is_current = true;
            }
            if token.contains('+') || lower.contains("preferred") {
                is_preferred = true;
            }
            if refresh_hz.is_none() {
                // Refresh must look like a plain number with optional * / + markers.
                // This avoids treating verbose pixel clocks like "148.500MHz" as Hz.
                let cleaned = token.trim_end_matches(['*', '+'].as_ref());
                let is_plain_numeric = !cleaned.is_empty()
                    && cleaned.chars().all(|c| c.is_ascii_digit() || c == '.')
                    && cleaned.chars().any(|c| c.is_ascii_digit());
                if is_plain_numeric {
                    if let Ok(val) = cleaned.parse::<f32>() {
                        refresh_hz = Some(val);
                    }
                }
            }
        }

        Some((
            DisplayMode::new(width, height, refresh_hz),
            is_current,
            is_preferred,
        ))
    }

    fn parse_vertical_clock_line(line: &str) -> Option<f32> {
        let trimmed = line.trim();
        if !trimmed.starts_with("v:") {
            return None;
        }
        let (_, tail) = trimmed.rsplit_once("clock")?;
        let hz = tail.trim().strip_suffix("Hz")?.trim();
        hz.parse::<f32>().ok()
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
            let mut header_mode: Option<DisplayMode> = None;

            let connected = status == "connected";
            for token in tokens {
                if token == "primary" {
                    primary = true;
                    continue;
                }
                if token.contains('+')
                    && token.contains('x')
                    && token
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_digit())
                        .unwrap_or(false)
                {
                    if let Some((res, _pos)) = token.split_once('+') {
                        if let Some((w, h)) = res.split_once('x') {
                            if let (Ok(width), Ok(height)) = (w.parse::<u32>(), h.parse::<u32>()) {
                                header_mode = Some(DisplayMode::new(width, height, None));
                            }
                        }
                    }
                }
            }

            let mut output = DisplayOutput::simple(id, name.clone(), None);
            output.primary = primary;

            let mut modes = Vec::new();
            let mut current_mode_index: Option<usize> = None;
            let mut brightness: Option<f32> = None;
            let mut gamma: Option<f32> = None;
            let mut last_mode_index: Option<usize> = None;

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

                if let Some((mode, is_current, _is_preferred)) = Self::parse_mode_line(detail) {
                    let idx = modes.len();
                    if is_current {
                        current_mode_index = Some(idx);
                    }
                    modes.push(mode);
                    last_mode_index = Some(idx);
                    continue;
                }

                if let Some(v_clock_hz) = Self::parse_vertical_clock_line(detail) {
                    if let Some(idx) = last_mode_index {
                        if let Some(mode) = modes.get_mut(idx) {
                            mode.refresh_hz = Some(v_clock_hz);
                        }
                    }
                }
            }

            let parsed_current_mode = current_mode_index
                .and_then(|idx| modes.get(idx).cloned())
                .or_else(|| header_mode.clone());

            let mut unique_modes = Vec::new();
            for mode in modes {
                if !unique_modes.contains(&mode) {
                    unique_modes.push(mode);
                }
            }
            output.available_modes = unique_modes;

            output.enabled = connected && parsed_current_mode.is_some();
            if output.enabled {
                output.current_mode = parsed_current_mode;
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
                // Implemented via per-output xrandr gamma matrix composition.
                temperature: true,
            };

            outputs.push(output);
            id += 1;
        }

        outputs
    }

    fn get_color_state(&self, output: &str) -> Result<X11ColorState> {
        let mut state = Self::color_state()
            .lock()
            .map_err(|_| anyhow!("x11 color state lock poisoned"))?;
        let entry = state.entry(output.to_string()).or_insert(X11ColorState {
            brightness: 1.0,
            gamma: 1.0,
            temperature: 6500,
        });
        Ok(*entry)
    }

    fn update_color_state(
        &self,
        output: &str,
        updater: impl FnOnce(&mut X11ColorState),
    ) -> Result<X11ColorState> {
        let mut state = Self::color_state()
            .lock()
            .map_err(|_| anyhow!("x11 color state lock poisoned"))?;
        let entry = state.entry(output.to_string()).or_insert(X11ColorState {
            brightness: 1.0,
            gamma: 1.0,
            temperature: 6500,
        });
        updater(entry);
        let updated = *entry;
        let _ = Self::save_persisted_state(&state);
        Ok(updated)
    }

    fn kelvin_to_rgb_scales(kelvin: u16) -> (f64, f64, f64) {
        let temp = (kelvin as f64).clamp(1000.0, 10000.0) / 100.0;

        let mut red = if temp <= 66.0 {
            255.0
        } else {
            329.698_727_446 * (temp - 60.0).powf(-0.133_204_759_2)
        };

        let mut green = if temp <= 66.0 {
            99.470_802_586_1 * temp.ln() - 161.119_568_166_1
        } else {
            288.122_169_528_3 * (temp - 60.0).powf(-0.075_514_849_2)
        };

        let mut blue = if temp >= 66.0 {
            255.0
        } else if temp <= 19.0 {
            0.0
        } else {
            138.517_731_223_1 * (temp - 10.0).ln() - 305.044_792_730_7
        };

        red = red.clamp(0.0, 255.0) / 255.0;
        green = green.clamp(0.0, 255.0) / 255.0;
        blue = blue.clamp(0.0, 255.0) / 255.0;

        // Keep perceived luminance close to neutral so temperature and brightness
        // controls feel independent.
        let luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
        if luma > 0.0 {
            red /= luma;
            green /= luma;
            blue /= luma;
        }

        (red, green, blue)
    }

    fn apply_color_pipeline(&self, output: &str, state: X11ColorState) -> Result<()> {
        let (r_scale, g_scale, b_scale) = Self::kelvin_to_rgb_scales(state.temperature);
        let base_gamma = (state.gamma as f64).max(0.1);
        let r = (base_gamma * r_scale).clamp(0.1, 10.0);
        let g = (base_gamma * g_scale).clamp(0.1, 10.0);
        let b = (base_gamma * b_scale).clamp(0.1, 10.0);
        let gamma_str = format!("{r:.4}:{g:.4}:{b:.4}");
        let brightness_str = state.brightness.clamp(0.0, 1.0).to_string();
        Self::run_xrandr(&[
            "--output",
            output,
            "--brightness",
            &brightness_str,
            "--gamma",
            &gamma_str,
        ])
        .map(|_| ())
    }
}

#[async_trait]
impl crate::backend::DisplayBackend for X11Backend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>> {
        let stdout = Self::run_xrandr(&["--query", "--verbose"])?;
        let mut outputs = Self::parse_xrandr(&stdout);
        let mut state = Self::color_state()
            .lock()
            .map_err(|_| anyhow!("x11 color state lock poisoned"))?;
        for output in outputs.iter_mut() {
            if output.enabled && !state.contains_key(&output.name) {
                // Initialize process state from real xrandr values on first sight.
                let brightness = output.color.brightness.unwrap_or(1.0).clamp(0.0, 1.0);
                let gamma = output.color.gamma.unwrap_or(1.0).max(0.1);
                state.insert(
                    output.name.clone(),
                    X11ColorState {
                        brightness,
                        gamma,
                        temperature: 6500,
                    },
                );
                let _ = Self::save_persisted_state(&state);
            }

            if let Some(color) = state.get(&output.name).copied() {
                output.color.temperature = Some(color.temperature);
                output.color.gamma = Some(color.gamma);
                output.color.brightness = Some(color.brightness);
            }
        }
        Ok(outputs)
    }

    async fn set_brightness(&self, output: &str, value: f32) -> Result<()> {
        let clamped = value.clamp(0.0, 1.0);
        let state = self.update_color_state(output, |entry| entry.brightness = clamped)?;
        self.apply_color_pipeline(output, state)
    }

    async fn set_gamma(&self, output: &str, value: f32) -> Result<()> {
        let gamma = value.max(0.1);
        let state = self.update_color_state(output, |entry| entry.gamma = gamma)?;
        self.apply_color_pipeline(output, state)
    }

    async fn set_temperature(&self, output: &str, value: u16) -> Result<()> {
        let kelvin = value.clamp(1000, 10000);
        let state = self.update_color_state(output, |entry| entry.temperature = kelvin)?;
        self.apply_color_pipeline(output, state)
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

    async fn set_position_relative(
        &self,
        output: &str,
        relative_to: &str,
        direction: &str,
    ) -> Result<()> {
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
        Err(anyhow!(
            "Adaptive sync control is not supported on X11/xrandr"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::X11Backend;

    #[test]
    fn connected_without_current_mode_is_treated_as_disabled() {
        let sample = r#"Screen 0: minimum 8 x 8, current 2560 x 1080, maximum 32767 x 32767
eDP-1 connected primary (normal left inverted right x axis y axis)
	Identifier: 0x42
	1920x1080     60.00  59.94
DP-1 disconnected (normal left inverted right x axis y axis)
"#;
        let outputs = X11Backend::parse_xrandr(sample);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].name, "eDP-1");
        assert!(!outputs[0].enabled);
        assert!(outputs[0].current_mode.is_none());
    }

    #[test]
    fn verbose_mode_markers_set_current_and_preferred_mode() {
        let sample = r#"Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767
eDP-1 connected primary (normal left inverted right x axis y axis)
	Identifier: 0x42
	1920x1080 (0x4b) 148.500MHz +HSync +VSync *current +preferred
	1280x720  (0x4c) 74.250MHz +HSync +VSync
"#;
        let outputs = X11Backend::parse_xrandr(sample);
        assert_eq!(outputs.len(), 1);
        let output = &outputs[0];
        assert!(output.enabled);
        let mode = output.current_mode.as_ref().expect("current mode");
        assert_eq!(mode.width, 1920);
        assert_eq!(mode.height, 1080);
        assert!(mode.refresh_hz.is_none());
    }

    #[test]
    fn compact_mode_line_keeps_refresh_rate() {
        let sample = r#"Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767
DP-1 connected 1920x1080+0+0 (normal left inverted right x axis y axis)
	Identifier: 0x43
	1920x1080     74.97*+ 60.00
"#;
        let outputs = X11Backend::parse_xrandr(sample);
        assert_eq!(outputs.len(), 1);
        let mode = outputs[0].current_mode.as_ref().expect("current mode");
        assert_eq!(mode.refresh_hz, Some(74.97));
    }

    #[test]
    fn verbose_mode_uses_vertical_clock_hz() {
        let sample = r#"Screen 0: minimum 320 x 200, current 2560 x 1080, maximum 16384 x 16384
DP-1 connected 2560x1080+0+0 (0x693) normal (normal left inverted right x axis y axis)
	Identifier: 0x42
	2560x1080 (0x693) 181.250MHz +HSync -VSync *current +preferred
        h: width  2560 start 2608 end 2640 total 2720 skew    0 clock  66.64KHz
        v: height 1080 start 1083 end 1093 total 1111           clock  59.98Hz
"#;
        let outputs = X11Backend::parse_xrandr(sample);
        assert_eq!(outputs.len(), 1);
        let mode = outputs[0].current_mode.as_ref().expect("current mode");
        assert_eq!(mode.width, 2560);
        assert_eq!(mode.height, 1080);
        assert_eq!(mode.refresh_hz, Some(59.98));
    }

    #[test]
    fn connected_with_only_preferred_mode_is_treated_as_disabled() {
        let sample = r#"Screen 0: minimum 320 x 200, current 2560 x 1080, maximum 16384 x 16384
eDP-1 connected primary (normal left inverted right x axis y axis)
	Identifier: 0x41
  1920x1200 (0x46) 154.000MHz -HSync -VSync +preferred
        h: width  1920 start 1968 end 2000 total 2080 skew    0 clock  74.04KHz
        v: height 1200 start 1203 end 1209 total 1235           clock  59.95Hz
"#;
        let outputs = X11Backend::parse_xrandr(sample);
        assert_eq!(outputs.len(), 1);
        assert!(!outputs[0].enabled);
        assert!(outputs[0].current_mode.is_none());
    }

    #[test]
    fn kelvin_scales_are_finite() {
        let (r, g, b) = X11Backend::kelvin_to_rgb_scales(6500);
        assert!(r.is_finite());
        assert!(g.is_finite());
        assert!(b.is_finite());
    }

    #[test]
    fn backend_defaults_color_state() {
        let backend = X11Backend::new().expect("backend");
        let state = backend.get_color_state("DP-1").expect("state");
        assert_eq!(state.brightness, 1.0);
        assert_eq!(state.gamma, 1.0);
        assert_eq!(state.temperature, 6500);
    }
}
