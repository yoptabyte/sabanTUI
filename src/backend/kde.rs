use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::process::Command;

#[cfg(feature = "backend-kde")]
use zbus::Connection;
#[cfg(feature = "backend-kde")]
use zbus::Proxy;

use crate::models::{
    DisplayColorCapabilities, DisplayMode, DisplayOutput,
};

#[derive(Deserialize, Debug)]
struct KScreenDoctorOutput {
    outputs: Vec<KScreenOutput>,
}

#[derive(Deserialize, Debug)]
struct KScreenOutput {
    name: String,
    #[serde(default)]
    description: Option<String>,
    enabled: bool,
    #[serde(rename = "currentModeId")]
    current_mode_id: Option<String>,
    #[serde(rename = "preferredModeId")]
    _preferred_mode_id: Option<String>,
    modes: Vec<KScreenMode>,
    pos: KScreenPos,
    scale: f64,
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


#[derive(Deserialize, Debug)]
struct KScreenMode {
    id: String,
    size: KScreenSize,
    refresh: f32,
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

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum KScreenRotation {
    Number(u32),
    String(String),
}

impl KScreenRotation {
    fn to_transform(&self) -> String {
        match self {
            Self::Number(0) => "normal".to_string(),
            Self::Number(1) => "90".to_string(),
            Self::Number(2) => "180".to_string(),
            Self::Number(3) => "270".to_string(),
            Self::String(s) => match s.to_lowercase().as_str() {
                "none" | "0" => "normal".to_string(),
                "left" | "90" => "90".to_string(),
                "inverted" | "180" => "180".to_string(),
                "right" | "270" => "270".to_string(),
                _ => "normal".to_string(),
            },
            _ => "normal".to_string(),
        }
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
}

#[async_trait]
impl crate::backend::DisplayBackend for KdeBackend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>> {
        let stdout = Self::run_kscreen_doctor(&["-j"])?;
        let kscreen: KScreenDoctorOutput = serde_json::from_str(&stdout)
            .context("failed to parse kscreen-doctor JSON output")?;

        let mut outputs = Vec::with_capacity(kscreen.outputs.len());

        #[cfg(feature = "backend-kde")]
        let (global_temp, global_brightness) = {
            let conn = Connection::session().await.ok();
            let mut temp = None;
            let mut brightness = None;

            if let Some(ref c) = conn {
                if let Ok(p) = Proxy::new(c, "org.kde.KWin", "/ColorCorrect", "org.kde.kwin.ColorCorrect").await {
                    temp = p.get_property::<u32>("currentTemperature").await.ok().map(|t| t as u16);
                }

                // Try to get brightness via D-Bus for older KDE or as a global value
                if let Ok(p) = Proxy::new(
                    c,
                    "org.kde.Solid.PowerManagement",
                    "/org/kde/Solid/PowerManagement/Actions/BrightnessControl",
                    "org.kde.Solid.PowerManagement.Actions.BrightnessControl",
                ).await {
                    let b = p.get_property::<u32>("brightness").await.ok();
                    let m = p.get_property::<u32>("brightnessMax").await.ok();
                    if let (Some(b), Some(m)) = (b, m) {
                        brightness = Some((b as f32 / m.max(1) as f32).clamp(0.0, 1.0));
                    }
                }
            }
            (temp, brightness)
        };

        for (idx, k_out) in kscreen.outputs.into_iter().enumerate() {
            let mut output = DisplayOutput::simple(idx, k_out.name.clone(), k_out.description);
            output.enabled = k_out.enabled;
            output.scale = Some(k_out.scale);
            output.available_scales = Some(vec![1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5]);
            output.position = Some(k_out.pos.to_xy());
            output.transform = Some(k_out.rotation.to_transform());

            output.color_caps = DisplayColorCapabilities {
                brightness: true,
                gamma: false,
                temperature: true,
            };

            #[cfg(feature = "backend-kde")]
            {
                output.color.temperature = global_temp;
                output.color.brightness = global_brightness;
            }

            let mut modes = Vec::with_capacity(k_out.modes.len());
            for k_mode in k_out.modes {
                let (w, h) = k_mode.size.to_wh();
                let mode = DisplayMode::new(w, h, Some(k_mode.refresh));
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
            w == mode.width && h == mode.height && (mode.refresh_hz.is_none() || (m.refresh - mode.refresh_hz.unwrap()).abs() < 0.1)
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
        Self::run_kscreen_doctor(&[
            &format!("output.{}.position.{},{}", output, x, y)
        ])?;
        Ok(())
    }

    async fn set_transform(&self, output: &str, transform: &str) -> Result<()> {
        // kscreen-doctor uses "rotation" instead of "transform"
        // Valid rotations: none, left (90 deg), right (270 deg), inverted (180 deg)
        let rotation = match transform {
            "normal" | "0" | "none" => "none",
            "90" | "left" => "left",
            "180" | "inverted" => "inverted",
            "270" | "right" => "right",
            _ => bail!("unsupported rotation for KDE: {}", transform),
        };

        Self::run_kscreen_doctor(&[
            &format!("output.{}.rotation.{}", output, rotation)
        ])?;
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
                proxy.call::<_, _, ()>("setBrightness", &(val)).await?;
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
            proxy.call::<_, _, ()>("setTemperature", &(value as u32)).await?;
            Ok(())
        }
        #[cfg(not(feature = "backend-kde"))]
        {
            let _ = (value, _output);
            bail!("KDE backend is disabled at compile time");
        }
    }
}
