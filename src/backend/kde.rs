use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::process::Command;

#[cfg(feature = "backend-kde")]
use zbus::Connection;
#[cfg(feature = "backend-kde")]
use zbus::Proxy;

use crate::models::{
    DisplayColorCapabilities, DisplayMode, DisplayOutput, RelativePosition,
};

#[derive(Deserialize, Debug)]
struct KScreenDoctorOutput {
    #[serde(default)]
    outputs: Vec<KScreenOutput>,
}

#[derive(Deserialize, Debug)]
struct KScreenOutput {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    enabled: bool,
    #[serde(rename = "currentModeId", default)]
    current_mode_id: Option<String>,
    #[serde(rename = "preferredModeId", default)]
    _preferred_mode_id: Option<String>,
    #[serde(default)]
    modes: Vec<KScreenMode>,
    #[serde(default)]
    pos: KScreenPos,
    #[serde(default = "default_scale")]
    scale: f64,
    #[serde(default)]
    rotation: KScreenRotation,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum KScreenPos {
    Object { x: i32, y: i32 },
    // Some versions might output as array [x, y] or string "x,y" - handle what we can
    Array([i32; 2]),
}

impl KScreenPos {
    fn to_xy(&self) -> (i32, i32) {
        match self {
            Self::Object { x, y } => (*x, *y),
            Self::Array([x, y]) => (*x, *y),
        }
    }
}

fn default_scale() -> f64 {
    1.0
}

impl Default for KScreenPos {
    fn default() -> Self {
        Self::Object { x: 0, y: 0 }
    }
}


#[derive(Deserialize, Debug)]
struct KScreenMode {
    id: String,
    size: KScreenSize,
    #[serde(default, alias = "refreshRate")]
    refresh: Option<f32>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum KScreenSize {
    Object { width: u32, height: u32 },
    Array([u32; 2]),
}

impl KScreenSize {
    fn to_wh(&self) -> (u32, u32) {
        match self {
            Self::Object { width, height } => (*width, *height),
            Self::Array([w, h]) => (*w, *h),
        }
    }
}

impl Default for KScreenSize {
    fn default() -> Self {
        Self::Object {
            width: 0,
            height: 0,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum KScreenRotation {
    Number(u32),
    String(String),
}

impl KScreenRotation {
    fn to_transform(&self) -> String {
        match self {
            Self::Number(n) => match n {
                0 => "normal",
                1 => "90",
                2 => "180",
                3 => "270",
                4 => "flipped",
                5 => "flipped-90",
                6 => "flipped-180",
                7 => "flipped-270",
                _ => "normal",
            }
            .to_string(),
            Self::String(s) => match s.to_lowercase().as_str() {
                "none" | "0" => "normal",
                "left" | "90" => "90",
                "inverted" | "180" => "180",
                "right" | "270" => "270",
                "flipped" | "flipped0" | "4" => "flipped",
                "flippedleft" | "flipped90" | "5" => "flipped-90",
                "flippedinverted" | "flipped180" | "6" => "flipped-180",
                "flippedright" | "flipped270" | "7" => "flipped-270",
                _ => "normal",
            }
            .to_string(),
        }
    }
}

impl Default for KScreenRotation {
    fn default() -> Self {
        Self::Number(0)
    }
}

pub struct KdeBackend;

impl KdeBackend {
    pub fn new() -> Result<Self> {
        #[cfg(not(feature = "backend-kde"))]
        {
            bail!("KDE backend is disabled at compile time (enable feature backend-kde)");
        }
        #[cfg(feature = "backend-kde")]
        {
            Ok(Self)
        }
    }

    fn run_kscreen_doctor(args: &[&str]) -> Result<String> {
        let output = Command::new("kscreen-doctor")
            .args(args)
            .output()
            .context("failed to execute kscreen-doctor")?;

        if !output.status.success() {
            bail!(
                "kscreen-doctor exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[cfg(feature = "backend-kde")]
    async fn query_global_color_state() -> (Option<u16>, Option<f32>) {
        let conn = match Connection::session().await {
            Ok(conn) => conn,
            Err(_) => return (None, None),
        };

        let mut temp = None;
        let mut brightness = None;

        if let Ok(proxy) = Proxy::new(&conn, "org.kde.KWin", "/ColorCorrect", "org.kde.kwin.ColorCorrect").await {
            temp = proxy
                .get_property::<u32>("currentTemperature")
                .await
                .ok()
                .map(|value| value as u16);
        }

        if let Ok(proxy) = Proxy::new(
            &conn,
            "org.kde.Solid.PowerManagement",
            "/org/kde/Solid/PowerManagement/Actions/BrightnessControl",
            "org.kde.Solid.PowerManagement.Actions.BrightnessControl",
        ).await {
            let current = proxy.get_property::<u32>("brightness").await.ok();
            let max = proxy.get_property::<u32>("brightnessMax").await.ok();
            if let (Some(current), Some(max)) = (current, max) {
                brightness = Some((current as f32 / max.max(1) as f32).clamp(0.0, 1.0));
            }
        }

        (temp, brightness)
    }
}

#[cfg(test)]
mod tests {
    use super::{KScreenDoctorOutput, KScreenRotation};

    #[test]
    fn parses_outputs_when_mode_refresh_is_missing() {
        let sample = r#"{
            "outputs": [
                {
                    "name": "HDMI-1",
                    "enabled": true,
                    "currentModeId": "1",
                    "modes": [
                        { "id": "1", "size": { "width": 1920, "height": 1080 } }
                    ],
                    "pos": { "x": 0, "y": 0 },
                    "scale": 1.0,
                    "rotation": 0
                }
            ]
        }"#;

        let parsed: KScreenDoctorOutput = serde_json::from_str(sample).unwrap();
        let output = &parsed.outputs[0];
        let mode = &output.modes[0];

        assert_eq!(output.name, "HDMI-1");
        assert_eq!(mode.refresh, None);
        assert_eq!(mode.size.to_wh(), (1920, 1080));
    }

    #[test]
    fn defaults_optional_output_fields() {
        let sample = r#"{
            "outputs": [
                {
                    "name": "DP-1"
                }
            ]
        }"#;

        let parsed: KScreenDoctorOutput = serde_json::from_str(sample).unwrap();
        let output = &parsed.outputs[0];

        assert!(!output.enabled);
        assert!(output.modes.is_empty());
        assert_eq!(output.pos.to_xy(), (0, 0));
        assert_eq!(output.scale, 1.0);
        assert_eq!(output.rotation.to_transform(), KScreenRotation::Number(0).to_transform());
    }
}

#[async_trait]
impl crate::backend::DisplayBackend for KdeBackend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>> {
        let stdout = Self::run_kscreen_doctor(&["-j"])?;
        let kscreen: KScreenDoctorOutput = serde_json::from_str(&stdout)
            .context("failed to parse kscreen-doctor JSON output")?;

        let mut outputs = Vec::with_capacity(kscreen.outputs.len());

        #[cfg(feature = "backend-kde")]
        let (global_temp, global_brightness) = Self::query_global_color_state().await;

        for (idx, k_out) in kscreen.outputs.into_iter().enumerate() {
            let mut output = DisplayOutput::simple(idx, k_out.name.clone(), k_out.description);
            let is_virtual_output = k_out.name.starts_with("Virtual-");
            output.enabled = k_out.enabled;
            output.scale = Some(k_out.scale);
            output.available_scales = Some(vec![1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5]);
            output.position = Some(k_out.pos.to_xy());
            output.transform = Some(k_out.rotation.to_transform());

            output.color_caps = DisplayColorCapabilities {
                brightness: !is_virtual_output && global_brightness.is_some(),
                gamma: false,
                temperature: !is_virtual_output && global_temp.is_some(),
            };

            #[cfg(feature = "backend-kde")]
            {
                if !is_virtual_output {
                    output.color.temperature = global_temp;
                    output.color.brightness = global_brightness;
                }
            }

            let mut modes = Vec::with_capacity(k_out.modes.len());
            for k_mode in k_out.modes {
                let (w, h) = k_mode.size.to_wh();
                let mode = DisplayMode::new(w, h, k_mode.refresh);
                if let Some(ref cur_id) = k_out.current_mode_id {
                    if *cur_id == k_mode.id {
                        output.current_mode = Some(mode.clone());
                    }
                }
                modes.push(mode);
            }
            output.available_modes = modes;


            outputs.push(output);
        }

        Ok(outputs)
    }

    async fn set_mode(&self, output: &str, mode: DisplayMode) -> Result<()> {
        let stdout = Self::run_kscreen_doctor(&["-j"])?;
        let kscreen: KScreenDoctorOutput = serde_json::from_str(&stdout)?;
        let k_out = kscreen.outputs.iter().find(|o| o.name == output)
            .ok_or_else(|| anyhow!("output {} not found", output))?;

        let k_mode = k_out.modes.iter().find(|m| {
            let (w, h) = m.size.to_wh();
            w == mode.width
                && h == mode.height
                && match (mode.refresh_hz, m.refresh) {
                    (None, _) => true,
                    (Some(_), None) => false,
                    (Some(requested), Some(actual)) => (actual - requested).abs() < 0.1,
                }
        }).ok_or_else(|| anyhow!("mode {} not found for output {}", mode, output))?;

        Self::run_kscreen_doctor(&[
            &format!("output.{}.mode.{}", output, k_mode.id)
        ])?;
        Ok(())
    }

    async fn set_scale(&self, output: &str, scale: f64) -> Result<()> {
        Self::run_kscreen_doctor(&[
            &format!("output.{}.scale.{}", output, scale)
        ])?;
        Ok(())
    }

    async fn set_enabled(&self, output: &str, enabled: bool) -> Result<()> {
        let action = if enabled { "enable" } else { "disable" };
        Self::run_kscreen_doctor(&[
            &format!("output.{}.{}", output, action)
        ])?;
        Ok(())
    }

    async fn set_position(&self, output: &str, x: i32, y: i32) -> Result<()> {
        Self::run_kscreen_doctor(&[&format!("output.{}.position.{},{}", output, x, y)])?;
        Ok(())
    }

    async fn set_position_relative(&self, output: &str, direction: RelativePosition) -> Result<()> {
        let (flag, relative_to) = match &direction {
            RelativePosition::LeftOf(s) => ("left-of", s),
            RelativePosition::RightOf(s) => ("right-of", s),
            RelativePosition::Above(s) => ("above", s),
            RelativePosition::Below(s) => ("below", s),
        };

        Self::run_kscreen_doctor(&[
            &format!("output.{}.{}.{}", output, flag, relative_to)
        ])?;
        Ok(())
    }

    async fn set_transform(&self, output: &str, transform: &str) -> Result<()> {
        // kscreen-doctor uses "rotation" instead of "transform"
        let rotation = match transform {
            "normal" | "0" | "none" => "none",
            "90" | "left" => "left",
            "180" | "inverted" => "inverted",
            "270" | "right" => "right",
            "flipped" | "flipped-0" => "flipped",
            "flipped-90" | "flipped-left" => "flipped90",
            "flipped-180" | "flipped-inverted" => "flipped180",
            "flipped-270" | "flipped-right" => "flipped270",
            _ => bail!("unsupported rotation for KDE: {}", transform),
        };

        Self::run_kscreen_doctor(&[&format!("output.{}.rotation.{}", output, rotation)])?;
        Ok(())
    }

    async fn set_brightness(&self, output: &str, value: f32) -> Result<()> {
        let val = (value.clamp(0.0, 1.0) * 100.0).round() as u32;
        let res = Self::run_kscreen_doctor(&[
            &format!("output.{}.brightness.{}", output, val)
        ]);

        if res.is_err() {
            // Fallback for older Plasma versions using D-Bus
            #[cfg(feature = "backend-kde")]
            {
                let conn = Connection::session().await?;
                let proxy = Proxy::new(
                    &conn,
                    "org.kde.Solid.PowerManagement",
                    "/org/kde/Solid/PowerManagement/Actions/BrightnessControl",
                    "org.kde.Solid.PowerManagement.Actions.BrightnessControl",
                ).await?;
                // Note: this is often global brightness in older KDE
                proxy.call::<_, _, ()>("setBrightness", &(val,)).await?;
                return Ok(());
            }
        }
        res.map(|_| ())
    }

    async fn set_mirror(&self, output: &str, target: &str) -> Result<()> {
        Self::run_kscreen_doctor(&[
            &format!("output.{}.primary", target),
            &format!("output.{}.position.0,0", target),
            &format!("output.{}.position.0,0", output),
        ])?;
        Ok(())
    }

    async fn set_temperature(&self, _output: &str, value: u16) -> Result<()> {
        #[cfg(feature = "backend-kde")]
        {
            let conn = Connection::session().await?;
            // KDE Night Color interface
            let proxy = Proxy::new(
                &conn,
                "org.kde.KWin",
                "/ColorCorrect",
                "org.kde.kwin.ColorCorrect",
            ).await?;

            // Temperature is usually global in KDE
            if let Err(err) = proxy.call::<_, _, ()>("setTemperature", &(value as u32,)).await {
                let message = err.to_string();
                if message.contains("org.freedesktop.DBus.Error.UnknownObject")
                    || message.contains("/ColorCorrect")
                {
                    bail!("temperature not supported on this KDE setup");
                }
                return Err(err.into());
            }
            Ok(())
        }
        #[cfg(not(feature = "backend-kde"))]
        {
            let _ = (value, _output);
            bail!("KDE backend is disabled at compile time");
        }
    }
}
