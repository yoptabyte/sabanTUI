use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::env;
use std::io::ErrorKind;
use std::process::{Command, Stdio};
use tracing::{debug, warn, info, error};
use zbus::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::InterfaceName;
use zbus::zvariant::Value;
use zbus::{Connection, Error as ZbusError};
use tokio::time::{sleep, Duration};

use crate::models::{
    DisplayColorCapabilities, DisplayColorSettings, DisplayMode, DisplayOutput,
};

const GAMMARELAY_DESTINATION: &str = "rs.wl-gammarelay";
const GAMMARELAY_PATH: &str = "/";
const GAMMARELAY_INTERFACE: &str = "rs.wl.gammarelay";
const GAMMARELAY_MAX_ATTEMPTS: u8 = 25;
const GAMMARELAY_RETRY_DELAY: Duration = Duration::from_millis(200);

pub struct WlrootsBackend;

impl WlrootsBackend {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    fn run_command(args: &[&str]) -> Result<String> {
        let output = Command::new("wlr-randr")
            .args(args)
            .output()
            .context("failed to execute wlr-randr")?;

        if !output.status.success() {
            bail!(
                "wlr-randr exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn build_properties_proxy<'a>(
        connection: &'a Connection,
        path: &'a str,
    ) -> Result<PropertiesProxy<'a>, ZbusError> {
        PropertiesProxy::builder(connection)
            .destination(GAMMARELAY_DESTINATION)?
            .path(path)?
            .build()
            .await
    }

    fn gammarelay_output_path(name: &str) -> String {
        let sanitized: String = name
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect();
        format!("/outputs/{sanitized}")
    }

    async fn ensure_gammarelay_running() -> Result<()> {
        let mut attempts: u8 = 0;
        let mut spawned = false;

        loop {
            let conn = match Connection::session().await {
                Ok(conn) => conn,
                Err(err) => {
                    attempts += 1;
                    debug!(attempt = attempts, ?err, "failed to open session bus for wl-gammarelay");
                    if attempts >= GAMMARELAY_MAX_ATTEMPTS {
                        error!(?err, "giving up opening session bus for wl-gammarelay");
                        return Err(err.into());
                    }
                    sleep(GAMMARELAY_RETRY_DELAY).await;
                    continue;
                }
            };

            let dbus = match DBusProxy::new(&conn).await {
                Ok(proxy) => proxy,
                Err(err) => {
                    attempts += 1;
                    debug!(attempt = attempts, ?err, "failed to build DBus proxy for wl-gammarelay");
                    if attempts >= GAMMARELAY_MAX_ATTEMPTS {
                        error!(?err, "giving up building DBus proxy for wl-gammarelay");
                        return Err(err.into());
                    }
                    sleep(GAMMARELAY_RETRY_DELAY).await;
                    continue;
                }
            };

            match dbus
                .name_has_owner(zbus::names::BusName::try_from(GAMMARELAY_DESTINATION)?)
                .await
            {
                Ok(true) => {
                    if spawned {
                        info!(attempts = attempts + 1, "wl-gammarelay appeared on D-Bus");
                    } else {
                        debug!("wl-gammarelay already present on D-Bus");
                    }
                    return Ok(());
                }
                Ok(false) => {
                    if attempts >= GAMMARELAY_MAX_ATTEMPTS {
                        error!(attempts, "wl-gammarelay service did not appear after retries");
                        bail!("wl-gammarelay service is unavailable");
                    }

                    if !spawned {
                        debug!("wl-gammarelay service not running; attempting to launch");
                        drop(dbus);
                        drop(conn);
                        Self::start_gammarelay_process().await?;
                        spawned = true;
                        attempts = 0;
                        continue;
                    }

                    attempts += 1;
                    debug!(attempt = attempts, "waiting for wl-gammarelay to appear on D-Bus");
                    sleep(GAMMARELAY_RETRY_DELAY).await;
                }
                Err(err) => {
                    attempts += 1;
                    debug!(attempt = attempts, ?err, "error querying wl-gammarelay owner");
                    if attempts >= GAMMARELAY_MAX_ATTEMPTS {
                        error!(?err, "giving up querying wl-gammarelay owner");
                        return Err(err.into());
                    }
                    sleep(GAMMARELAY_RETRY_DELAY).await;
                }
            }
        }
    }

    async fn start_gammarelay_process() -> Result<()> {
        let wayland_display = env::var("WAYLAND_DISPLAY")
            .map_err(|_| anyhow!("WAYLAND_DISPLAY not set; cannot launch wl-gammarelay"))?;

        let runtime_dir = env::var("XDG_RUNTIME_DIR")
            .map_err(|_| anyhow!("XDG_RUNTIME_DIR not set; cannot launch wl-gammarelay"))?;

        debug!(%wayland_display, %runtime_dir, "spawning wl-gammarelay-rs");
        match Command::new("wl-gammarelay-rs")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("WAYLAND_DISPLAY", wayland_display)
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .spawn()
        {
            Ok(child) => {
                info!(pid = child.id(), "wl-gammarelay-rs spawned");
                Ok(())
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Err(anyhow!(
                "wl-gammarelay-rs binary not found in PATH; install it to enable color controls"
            )),
            Err(err) => {
                error!(?err, "failed to spawn wl-gammarelay-rs");
                Err(err.into())
            }
        }
    }

    async fn set_output_property_f64(output: &str, property: &str, value: f64) -> Result<()> {
        Self::ensure_gammarelay_running().await?;

        let conn = Connection::session().await?;
        let path = Self::gammarelay_output_path(output);

        let iface = InterfaceName::from_static_str(GAMMARELAY_INTERFACE)?;

        match Self::build_properties_proxy(&conn, &path).await {
            Ok(proxy) => {
                proxy.set(iface.clone(), property, &Value::from(value)).await?;
                Ok(())
            }
            Err(ZbusError::FDO(err)) if matches!(*err, zbus::fdo::Error::UnknownObject(_)) => {
                debug!(output, %path, "wl-gammarelay output path not found; falling back to global property");
                let proxy = Self::build_properties_proxy(&conn, GAMMARELAY_PATH).await?;
                proxy.set(iface, property, &Value::from(value)).await?;
                Ok(())
            }
            Err(ZbusError::FDO(err)) if matches!(*err, zbus::fdo::Error::NameHasNoOwner(_)) => {
                bail!("wl-gammarelay service is not running")
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn set_output_property_u16(output: &str, property: &str, value: u16) -> Result<()> {
        Self::ensure_gammarelay_running().await?;

        let conn = Connection::session().await?;
        let path = Self::gammarelay_output_path(output);

        let iface = InterfaceName::from_static_str(GAMMARELAY_INTERFACE)?;

        match Self::build_properties_proxy(&conn, &path).await {
            Ok(proxy) => {
                proxy.set(iface.clone(), property, &Value::from(value)).await?;
                Ok(())
            }
            Err(ZbusError::FDO(err)) if matches!(*err, zbus::fdo::Error::UnknownObject(_)) => {
                debug!(output, %path, "wl-gammarelay output path not found; falling back to global property");
                let proxy = Self::build_properties_proxy(&conn, GAMMARELAY_PATH).await?;
                proxy.set(iface, property, &Value::from(value)).await?;
                Ok(())
            }
            Err(ZbusError::FDO(err)) if matches!(*err, zbus::fdo::Error::NameHasNoOwner(_)) => {
                bail!("wl-gammarelay service is not running")
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn query_output_color_states(
        outputs: &[DisplayOutput],
    ) -> Result<Option<HashMap<String, DisplayColorSettings>>> {
        if let Err(err) = Self::ensure_gammarelay_running().await {
            warn!(?err, "Failed to initialize wl-gammarelay service; color controls disabled");
            return Ok(None);
        }

        let conn = match Connection::session().await {
            Ok(conn) => conn,
            Err(err) => {
                debug!(?err, "Failed to open session bus for wl-gammarelay");
                return Ok(None);
            }
        };

        let mut map = HashMap::with_capacity(outputs.len());

        for output in outputs {
            let path = Self::gammarelay_output_path(&output.name);
            let proxy = match Self::build_properties_proxy(&conn, &path).await {
                Ok(proxy) => proxy,
                Err(ZbusError::FDO(err)) if matches!(*err, zbus::fdo::Error::UnknownObject(_)) => {
                    debug!(name = %output.name, %path, "wl-gammarelay output missing");
                    continue;
                }
                Err(ZbusError::FDO(err)) if matches!(*err, zbus::fdo::Error::NameHasNoOwner(_)) => {
                    debug!("wl-gammarelay service disappeared");
                    return Ok(None);
                }
                Err(err) => {
                    debug!(?err, name = %output.name, %path, "Failed to build wl-gammarelay proxy for output");
                    continue;
                }
            };

            let iface = InterfaceName::from_static_str(GAMMARELAY_INTERFACE)?;

            let brightness: f64 = match proxy.get(iface.clone(), "Brightness").await {
                Ok(value) => value.try_into()?,
                Err(err) => {
                    debug!(?err, name = %output.name, "Failed to read brightness");
                    continue;
                }
            };
            let gamma: f64 = match proxy.get(iface.clone(), "Gamma").await {
                Ok(value) => value.try_into()?,
                Err(err) => {
                    debug!(?err, name = %output.name, "Failed to read gamma");
                    continue;
                }
            };
            let temperature: u16 = match proxy.get(iface.clone(), "Temperature").await {
                Ok(value) => value.try_into()?,
                Err(err) => {
                    debug!(?err, name = %output.name, "Failed to read temperature");
                    continue;
                }
            };

            map.insert(
                output.name.clone(),
                DisplayColorSettings {
                    brightness: Some(brightness as f32),
                    gamma: Some(gamma as f32),
                    temperature: Some(temperature),
                },
            );
        }

        Ok(Some(map))
    }
}

#[async_trait]
impl crate::backend::DisplayBackend for WlrootsBackend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>> {
        let stdout = Self::run_command(&["--json"])?;
        let mut outputs = DisplayOutput::parse_from_wlr_randr(&stdout);

        if let Err(err) = Self::ensure_gammarelay_running().await {
            warn!(?err, "wl-gammarelay unavailable during output listing; color controls disabled");
        }

        match Self::query_output_color_states(&outputs).await? {
            Some(color_states) if !color_states.is_empty() => {
                for output in &mut outputs {
                    if let Some(settings) = color_states.get(&output.name) {
                        output.color = settings.clone();
                        output.color_caps = DisplayColorCapabilities {
                            brightness: true,
                            gamma: true,
                            temperature: true,
                        };
                    }
                }
            }
            Some(_) | None => {
                warn!("wl-gammarelay state unavailable; color controls disabled");
            }
        }

        Ok(outputs)
    }

    async fn set_brightness(&self, _output: &str, value: f32) -> Result<()> {
        let clamped = value.clamp(0.0, 1.0);
        Self::set_output_property_f64(_output, "Brightness", clamped as f64).await
    }

    async fn set_gamma(&self, _output: &str, value: f32) -> Result<()> {
        let gamma = (value as f64).max(0.1);
        Self::set_output_property_f64(_output, "Gamma", gamma).await
    }

    async fn set_temperature(&self, _output: &str, value: u16) -> Result<()> {
        Self::set_output_property_u16(_output, "Temperature", value).await
    }

    async fn set_mode(&self, output: &str, mode: DisplayMode) -> Result<()> {
        let mode_str = if let Some(refresh) = mode.refresh_hz {
            if refresh > 0.0 {
                format!("{}x{}@{:.3}Hz", mode.width, mode.height, refresh)
            } else {
                format!("{}x{}", mode.width, mode.height)
            }
        } else {
            format!("{}x{}", mode.width, mode.height)
        };
        
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            "--mode".to_string(),
            mode_str,
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)
            .map(|_| ())
    }

    async fn set_scale(&self, output: &str, scale: f64) -> Result<()> {
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            "--scale".to_string(),
            format!("{}", scale),
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)
            .map(|_| ())
    }

    async fn set_enabled(&self, output: &str, enabled: bool) -> Result<()> {
        let flag = if enabled { "--on" } else { "--off" };
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            flag.to_string(),
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)
            .map(|_| ())
    }

    async fn set_position(&self, output: &str, x: i32, y: i32) -> Result<()> {
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            "--pos".to_string(),
            format!("{},{}", x, y),
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)
            .map(|_| ())
    }

    async fn set_position_relative(&self, output: &str, relative_to: &str, direction: &str) -> Result<()> {
        let flag = match direction {
            "left" => "--left-of",
            "right" => "--right-of",
            "above" => "--above",
            "below" => "--below",
            _ => bail!("Invalid direction: {}", direction),
        };
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            flag.to_string(),
            relative_to.to_string(),
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)
            .map(|_| ())
    }

    async fn set_transform(&self, output: &str, transform: &str) -> Result<()> {
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            "--transform".to_string(),
            transform.to_string(),
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)
            .map(|_| ())
    }

    async fn set_mirror(&self, output: &str, target: &str) -> Result<()> {
        // True mirroring: find common mode and set both outputs to it
        let outputs = self.list_outputs().await?;
        let target_output = outputs.iter()
            .find(|o| o.name == target)
            .ok_or_else(|| anyhow::anyhow!("Target output {} not found", target))?;
        
        let source_output = outputs.iter()
            .find(|o| o.name == output)
            .ok_or_else(|| anyhow::anyhow!("Source output {} not found", output))?;
        
        // Find common modes between both outputs
        let common_modes: Vec<_> = source_output.available_modes.iter()
            .filter(|source_mode| {
                target_output.available_modes.iter().any(|target_mode| {
                    source_mode.width == target_mode.width &&
                    source_mode.height == target_mode.height &&
                    source_mode.refresh_hz == target_mode.refresh_hz
                })
            })
            .collect();
        
        if common_modes.is_empty() {
            bail!("No common modes found between {} and {}", output, target);
        }
        
        // Choose the highest resolution common mode
        let best_mode = common_modes.iter()
            .max_by_key(|m| m.width * m.height)
            .ok_or_else(|| anyhow::anyhow!("Failed to find best common mode"))?;
        
        let mode_str = if let Some(refresh) = best_mode.refresh_hz {
            format!("{}x{}@{:.0}Hz", best_mode.width, best_mode.height, refresh)
        } else {
            format!("{}x{}", best_mode.width, best_mode.height)
        };
        
        tracing::info!("Mirroring with common mode: {}", mode_str);
        
        // Set both outputs to the common mode and position 0,0
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            "--mode".to_string(),
            mode_str.clone(),
            "--pos".to_string(),
            "0,0".to_string(),
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)?;
        
        let target_args = vec![
            "--output".to_string(),
            target.to_string(),
            "--mode".to_string(),
            mode_str,
            "--pos".to_string(),
            "0,0".to_string(),
        ];
        let target_ref_args: Vec<&str> = target_args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&target_ref_args)
            .map(|_| ())
    }

    async fn set_adaptive_sync(&self, output: &str, enabled: bool) -> Result<()> {
        let flag = if enabled { "enabled" } else { "disabled" };
        let args = vec![
            "--output".to_string(),
            output.to_string(),
            "--adaptive-sync".to_string(),
            flag.to_string(),
        ];
        let ref_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_command(&ref_args)
            .map(|_| ())
    }
}
