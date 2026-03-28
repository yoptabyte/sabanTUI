use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: Option<f32>,
}

#[derive(Deserialize, Default)]
struct WlrRandrJsonHead {
    name: Option<String>,
    description: Option<String>,
    make: Option<String>,
    model: Option<String>,
    serial: Option<String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    modes: Vec<WlrRandrJsonMode>,
    #[serde(default)]
    scale: Option<f64>,
}

#[derive(Deserialize, Default)]
struct WlrRandrJsonMode {
    width: Option<u32>,
    height: Option<u32>,
    refresh: Option<f64>,
    #[serde(default)]
    preferred: bool,
    #[serde(default)]
    current: bool,
}

fn parse_wlr_randr_json(stdout: &str) -> Option<Vec<DisplayOutput>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }

    let heads: Vec<WlrRandrJsonHead> = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(_) => return None,
    };

    let mut outputs = Vec::with_capacity(heads.len());

    for (idx, mut head) in heads.into_iter().enumerate() {
        let name = match head.name.take() {
            Some(name) if !name.is_empty() => name,
            _ => continue,
        };

        let make = head.make.take();
        let model = head.model.take();

        let description = head.description.take().or_else(|| match (make, model) {
            (Some(make), Some(model)) => Some(format!("{make} {model}")),
            (Some(make), None) => Some(make),
            (None, Some(model)) => Some(model),
            (None, None) => None,
        });

        let mut output = DisplayOutput::simple(idx, name, description);
        output.enabled = head.enabled;
        output.scale = head.scale;

        let mut modes = Vec::with_capacity(head.modes.len());
        let mut current_mode: Option<DisplayMode> = None;
        let mut preferred_mode: Option<DisplayMode> = None;

        for json_mode in head.modes.into_iter() {
            let width = match json_mode.width {
                Some(width) => width,
                None => continue,
            };
            let height = match json_mode.height {
                Some(height) => height,
                None => continue,
            };
            let refresh = json_mode
                .refresh
                .filter(|hz| *hz > 0.0)
                .map(|hz| hz as f32);

            let mode = DisplayMode::new(width, height, refresh);
            if json_mode.current {
                current_mode = Some(mode.clone());
            }
            if json_mode.preferred && preferred_mode.is_none() {
                preferred_mode = Some(mode.clone());
            }
            modes.push(mode);
        }

        output.available_modes = modes;
        if output.enabled {
            output.current_mode = current_mode.or(preferred_mode);
        } else {
            output.current_mode = None;
        }

        outputs.push(output);
    }

    Some(outputs)
}

fn parse_wlr_randr_text(stdout: &str) -> Vec<DisplayOutput> {
    fn parse_header(line: &str) -> Option<(String, Option<String>)> {
        if let Some((name, rest)) = line.split_once(' ') {
            let rest = rest.trim();
            if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
                let description = &rest[1..rest.len() - 1];
                let description = if description.is_empty() {
                    None
                } else {
                    Some(description.to_string())
                };
                Some((name.to_string(), description))
            } else if rest.is_empty() {
                Some((name.to_string(), None))
            } else {
                Some((name.to_string(), Some(rest.to_string())))
            }
        } else {
            Some((line.to_string(), None))
        }
    }

    fn parse_enabled(line: &str) -> Option<bool> {
        let rest = line.strip_prefix("Enabled:")?.trim();
        Some(rest.eq_ignore_ascii_case("yes"))
    }

    fn parse_mode_line(line: &str) -> Option<(DisplayMode, bool, bool)> {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.chars().next()?.is_ascii_digit() {
            return None;
        }

        let (resolution, remainder) = trimmed.split_once(" px")?;
        let (width_str, height_str) = resolution.split_once('x')?;
        let width: u32 = width_str.parse().ok()?;
        let height: u32 = height_str.parse().ok()?;

        let mut refresh_hz = None;
        let mut status = "";
        let mut rest = remainder.trim();

        if let Some(idx) = rest.find('(') {
            status = &rest[idx..];
            rest = rest[..idx].trim();
        }

        if let Some(stripped) = rest.strip_prefix(',') {
            let stripped = stripped.trim();
            if let Some(pos) = stripped.find(" Hz") {
                let hz_str = stripped[..pos].trim();
                if !hz_str.is_empty() {
                    if let Ok(val) = hz_str.parse::<f32>() {
                        refresh_hz = Some(val);
                    }
                }
            }
        }

        let is_current = status.contains("current");
        let is_preferred = status.contains("preferred");

        Some((DisplayMode::new(width, height, refresh_hz), is_current, is_preferred))
    }

    fn parse_scale(line: &str) -> Option<f64> {
        let rest = line.strip_prefix("Scale:")?.trim();
        rest.parse::<f64>().ok()
    }

    let mut outputs = Vec::new();
    let mut lines = stdout.lines().peekable();
    let mut id = 0usize;

    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }

        let (name, description) = match parse_header(line) {
            Some(header) => header,
            None => continue,
        };

        let mut output = DisplayOutput::simple(id, name, description);
        let mut modes = Vec::new();
        let mut current_mode: Option<DisplayMode> = None;
        let mut preferred_mode: Option<DisplayMode> = None;

        while let Some(peeked) = lines.peek() {
            if peeked.trim().is_empty() {
                lines.next();
                continue;
            }

            if !peeked.starts_with(' ') && !peeked.starts_with('\t') {
                break;
            }

            let detail_line = lines.next().unwrap().trim_start();

            if let Some(enabled) = parse_enabled(detail_line) {
                output.enabled = enabled;
                continue;
            }

            if let Some(scale) = parse_scale(detail_line) {
                output.scale = Some(scale);
                continue;
            }

            if detail_line.starts_with("Modes:") {
                while let Some(mode_line) = lines.peek() {
                    if !mode_line.starts_with("    ") && !mode_line.starts_with('\t') {
                        break;
                    }
                    let mode_line = lines.next().unwrap().trim();
                    if let Some((mode, is_current, is_preferred)) = parse_mode_line(mode_line) {
                        if is_current {
                            current_mode = Some(mode.clone());
                        }
                        if is_preferred && preferred_mode.is_none() {
                            preferred_mode = Some(mode.clone());
                        }
                        modes.push(mode);
                    }
                }
                continue;
            }

            // Other detail lines (Make, Model, Position, etc.) are ignored for now.
        }

        output.available_modes = modes;
        if output.enabled {
            output.current_mode = current_mode.or(preferred_mode);
        } else {
            output.current_mode = None;
        }

        outputs.push(output);
        id += 1;
    }

    outputs
}

impl DisplayMode {
    pub fn new(width: u32, height: u32, refresh_hz: Option<f32>) -> Self {
        Self {
            width,
            height,
            refresh_hz,
        }
    }
}

impl fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.refresh_hz {
            Some(hz) => write!(f, "{}x{} @ {:.1}Hz", self.width, self.height, hz),
            None => write!(f, "{}x{}", self.width, self.height),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DisplayColorSettings {
    pub brightness: Option<f32>,
    pub gamma: Option<f32>,
    pub temperature: Option<u16>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DisplayColorCapabilities {
    pub brightness: bool,
    pub gamma: bool,
    pub temperature: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DisplayOutput {
    pub id: usize,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub primary: bool,
    pub current_mode: Option<DisplayMode>,
    pub available_modes: Vec<DisplayMode>,
    pub color: DisplayColorSettings,
    pub color_caps: DisplayColorCapabilities,
    pub scale: Option<f64>,
    #[serde(default)]
    pub available_scales: Option<Vec<f64>>,
    pub transform: Option<String>,
    pub position: Option<(i32, i32)>,
}

impl DisplayOutput {
    pub fn simple(id: usize, name: impl Into<String>, description: Option<String>) -> Self {
        Self {
            id,
            name: name.into(),
            description,
            enabled: true,
            primary: false,
            current_mode: None,
            available_modes: Vec::new(),
            color: DisplayColorSettings::default(),
            color_caps: DisplayColorCapabilities::default(),
            scale: None,
            available_scales: None,
            transform: None,
            position: None,
        }
    }

    pub fn with_color(mut self, color: DisplayColorSettings) -> Self {
        self.color = color;
        self
    }

    pub fn with_color_caps(mut self, caps: DisplayColorCapabilities) -> Self {
        self.color_caps = caps;
        self
    }

    pub fn parse_from_wlr_randr(stdout: &str) -> Vec<DisplayOutput> {
        if let Some(outputs) = parse_wlr_randr_json(stdout) {
            return outputs;
        }

        parse_wlr_randr_text(stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayMode, DisplayOutput};

    #[test]
    fn parses_wlr_randr_json_output() {
        let sample = r#"[
  {
    "name": "HDMI-A-1",
    "description": "Dell Inc. DELL P2715Q",
    "make": "Dell Inc.",
    "model": "DELL P2715Q",
    "serial": "ABC1234",
    "physical_size": { "width": 600, "height": 340 },
    "enabled": true,
    "modes": [
      { "width": 3840, "height": 2160, "refresh": 60.0, "preferred": true, "current": true },
      { "width": 3840, "height": 2160, "refresh": 30.0, "preferred": false, "current": false }
    ],
    "position": { "x": 0, "y": 0 },
    "transform": "normal",
    "scale": 1.0,
    "adaptive_sync": true
  },
  {
    "name": "DP-1",
    "description": null,
    "make": null,
    "model": null,
    "serial": null,
    "physical_size": { "width": 520, "height": 320 },
    "enabled": false,
    "modes": [
      { "width": 1920, "height": 1080, "refresh": 60.0, "preferred": true, "current": false }
    ],
    "adaptive_sync": null
  }
]"#;

        let outputs = DisplayOutput::parse_from_wlr_randr(sample);
        assert_eq!(outputs.len(), 2);

        let first = &outputs[0];
        assert_eq!(first.name, "HDMI-A-1");
        assert_eq!(first.description.as_deref(), Some("Dell Inc. DELL P2715Q"));
        assert!(first.enabled);
        assert_eq!(first.available_modes.len(), 2);
        assert_eq!(first.current_mode.as_ref().map(|m| (m.width, m.height)), Some((3840, 2160)));
        assert_eq!(first.current_mode.as_ref().and_then(|m| m.refresh_hz), Some(60.0));

        let second = &outputs[1];
        assert_eq!(second.name, "DP-1");
        assert!(!second.enabled);
        assert!(second.current_mode.is_none());
        assert_eq!(second.available_modes.len(), 1);
        assert_eq!(second.available_modes[0], DisplayMode::new(1920, 1080, Some(60.0)));
    }

    #[test]
    fn parses_wlr_randr_text_output() {
        let sample = r#"HDMI-A-1 \"Dell Inc. DELL P2715Q\"
  Physical size: 600x340 mm
  Enabled: yes
  Modes:
    3840x2160 px, 60.000000 Hz (preferred, current)
    3840x2160 px, 30.000000 Hz
  Position: 0,0
  Transform: normal
  Scale: 1.000000

DP-1 \"Unknown\"
  Enabled: no
  Modes:
    1920x1080 px, 60.000000 Hz (preferred)
  Transform: normal
  Scale: 1.000000
"#;

        let outputs = DisplayOutput::parse_from_wlr_randr(sample);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].name, "HDMI-A-1");
        assert!(outputs[0].enabled);
        assert_eq!(outputs[1].name, "DP-1");
        assert!(!outputs[1].enabled);
    }
}

impl fmt::Display for DisplayOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.enabled { "enabled" } else { "disabled" };
        let mut parts = Vec::new();
        if let Some(mode) = &self.current_mode {
            parts.push(format!("current mode: {}", mode));
        }
        if let Some(brightness) = self.color.brightness {
            parts.push(format!("brightness={:.2}", brightness));
        }
        if let Some(gamma) = self.color.gamma {
            parts.push(format!("gamma={:.2}", gamma));
        }
        if let Some(temp) = self.color.temperature {
            parts.push(format!("temp={}K", temp));
        }

        write!(f, "{} ({})", self.name, status)?;
        if !parts.is_empty() {
            write!(f, " - {}", parts.join(", "))?;
        }
        Ok(())
    }
}
