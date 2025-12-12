use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Command;
use tracing::{debug, info};

use crate::models::{DisplayColorCapabilities, DisplayColorSettings, DisplayMode, DisplayOutput};

#[cfg(feature = "backend-gnome")]
use zbus::zvariant::OwnedValue;
#[cfg(feature = "backend-gnome")]
use zbus::Connection;
#[cfg(feature = "backend-gnome")]
use zbus::Proxy;

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MonitorSpec {
    connector: String,
    vendor: String,
    product: String,
    serial: String,
}

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone)]
struct MonitorMode {
    id: String,
    mode: DisplayMode,
    preferred: bool,
    current: bool,
}

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone)]
struct MonitorInfo {
    spec: MonitorSpec,
    modes: Vec<MonitorMode>,
    _props: HashMap<String, OwnedValue>,
}

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone)]
struct LogicalMonitorInfo {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    primary: bool,
    // (monitor spec, mode id, properties)
    monitors: Vec<(MonitorSpec, String, HashMap<String, OwnedValue>)>,
}

#[cfg(feature = "backend-gnome")]
type MonitorSpecTuple = (String, String, String, String);

#[cfg(feature = "backend-gnome")]
type MonitorConfig = (MonitorSpecTuple, String, HashMap<String, OwnedValue>);

#[cfg(feature = "backend-gnome")]
type LogicalMonitorConfig = (i32, i32, f64, u32, bool, Vec<MonitorConfig>);

const MUTTER_DESTINATION: &str = "org.gnome.Mutter.DisplayConfig";
const MUTTER_PATH: &str = "/org/gnome/Mutter/DisplayConfig";
const MUTTER_INTERFACE: &str = "org.gnome.Mutter.DisplayConfig";

pub struct GnomeBackend;

impl GnomeBackend {
    pub fn new() -> Result<Self> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }
        #[cfg(feature = "backend-gnome")]
        {
            Ok(Self)
        }
    }

    fn run_gammastep(args: &[&str]) -> Result<()> {
        let output = Command::new("gammastep")
            .args(args)
            .output()
            .context("failed to execute gammastep")?;

        if !output.status.success() {
            bail!(
                "gammastep exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    fn gammastep_set_temperature(kelvin: u16) -> Result<()> {
        // Keep the same clamp as the X11 redshift backend.
        let kelvin = kelvin.clamp(1000, 10000);
        // -P: one-shot (do not keep running); -O: set temperature.
        // gammastep tries to pick an appropriate adjustment method.
        Self::run_gammastep(&["-P", "-O", &kelvin.to_string()])
    }

    fn gammastep_set_brightness(value: f32) -> Result<()> {
        let clamped = value.clamp(0.0, 1.0);
        // redshift/gammastep style brightness is typically "day:night".
        // We set both to the same value to behave like a single slider.
        let spec = format!("{0}:{0}", clamped);
        Self::run_gammastep(&["-P", "-b", &spec])
    }

    fn gammastep_set_gamma(value: f32) -> Result<()> {
        let gamma = (value as f64).max(0.1);
        let spec = format!("{0}:{0}:{0}", gamma);
        // redshift/gammastep style gamma is typically "R:G:B".
        Self::run_gammastep(&["-P", "-g", &spec])
    }

    #[cfg(feature = "backend-gnome")]
    async fn mutter_proxy(conn: &Connection) -> Result<Proxy<'_>> {
        Ok(Proxy::new(conn, MUTTER_DESTINATION, MUTTER_PATH, MUTTER_INTERFACE)
            .await
            .context("failed to build Mutter D-Bus proxy")?)
    }

    #[cfg(feature = "backend-gnome")]
    async fn get_current_state(
        conn: &Connection,
    ) -> Result<(u32, Vec<OwnedValue>, Vec<OwnedValue>, HashMap<String, OwnedValue>)> {
        let proxy = Self::mutter_proxy(conn).await?;
        let reply: (u32, Vec<OwnedValue>, Vec<OwnedValue>, HashMap<String, OwnedValue>) = proxy
            .call("GetCurrentState", &())
            .await
            .context("failed to call Mutter GetCurrentState")?;
        Ok(reply)
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_string(v: &OwnedValue) -> Result<String> {
        let s: String = v
            .clone()
            .try_into()
            .map_err(|_| anyhow!("unexpected D-Bus type; expected string"))?;
        Ok(s)
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_i32(v: &OwnedValue) -> Result<i32> {
        let n: i32 = v
            .clone()
            .try_into()
            .map_err(|_| anyhow!("unexpected D-Bus type; expected i32"))?;
        Ok(n)
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_u32(v: &OwnedValue) -> Result<u32> {
        let n: u32 = v
            .clone()
            .try_into()
            .map_err(|_| anyhow!("unexpected D-Bus type; expected u32"))?;
        Ok(n)
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_f64(v: &OwnedValue) -> Result<f64> {
        let n: f64 = v
            .clone()
            .try_into()
            .map_err(|_| anyhow!("unexpected D-Bus type; expected f64"))?;
        Ok(n)
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_bool(v: &OwnedValue) -> Result<bool> {
        let b: bool = v
            .clone()
            .try_into()
            .map_err(|_| anyhow!("unexpected D-Bus type; expected bool"))?;
        Ok(b)
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_fields(v: &OwnedValue) -> Result<Vec<OwnedValue>> {
        let st: zbus::zvariant::Structure = v
            .clone()
            .try_into()
            .map_err(|_| anyhow!("unexpected D-Bus type; expected struct"))?;
        Ok(st.fields().iter().cloned().map(OwnedValue::from).collect())
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_array(v: &OwnedValue) -> Result<Vec<OwnedValue>> {
        let arr: zbus::zvariant::Array = v
            .clone()
            .try_into()
            .map_err(|_| anyhow!("unexpected D-Bus type; expected array"))?;
        let mut out = Vec::new();
        for item in arr.iter() {
            out.push(item.to_owned());
        }
        Ok(out)
    }

    #[cfg(feature = "backend-gnome")]
    fn ov_to_dict(v: &OwnedValue) -> Result<HashMap<String, OwnedValue>> {
        // Expecting a{sv}.
        let map: HashMap<String, OwnedValue> = v
            .clone()
            .try_into()
.map_err(|_| anyhow!("unexpected D-Bus type; expected a{{sv}}"))?;
        Ok(map)
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_monitor_spec(v: &OwnedValue) -> Result<MonitorSpec> {
        let fields = Self::ov_to_fields(v)?;
        if fields.len() < 4 {
            bail!("unexpected monitor spec format from Mutter")
        }
        Ok(MonitorSpec {
            connector: Self::ov_to_string(&fields[0])?,
            vendor: Self::ov_to_string(&fields[1])?,
            product: Self::ov_to_string(&fields[2])?,
            serial: Self::ov_to_string(&fields[3])?,
        })
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_monitor_mode(v: &OwnedValue) -> Result<MonitorMode> {
        let fields = Self::ov_to_fields(v)?;
        if fields.len() < 6 {
            bail!("unexpected monitor mode format from Mutter")
        }

        let id = Self::ov_to_string(&fields[0])?;
        let width = Self::ov_to_i32(&fields[1])?.max(0) as u32;
        let height = Self::ov_to_i32(&fields[2])?.max(0) as u32;
        let refresh = Self::ov_to_f64(&fields[3])?;
        let preferred = Self::ov_to_bool(&fields[4])?;
        let current = Self::ov_to_bool(&fields[5])?;

        Ok(MonitorMode {
            id,
            mode: DisplayMode::new(width, height, if refresh > 0.0 { Some(refresh as f32) } else { None }),
            preferred,
            current,
        })
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_monitor(v: &OwnedValue) -> Result<MonitorInfo> {
        // Expected: (monitor_spec, modes, props)
        let fields = Self::ov_to_fields(v)?;
        if fields.len() < 3 {
            bail!("unexpected monitor format from Mutter")
        }
        let spec = Self::parse_monitor_spec(&fields[0])?;
        let mode_vals = Self::ov_to_array(&fields[1])?;
        let mut modes = Vec::with_capacity(mode_vals.len());
        for mv in mode_vals {
            if let Ok(parsed) = Self::parse_monitor_mode(&mv) {
                modes.push(parsed);
            }
        }
        let props = Self::ov_to_dict(&fields[2]).unwrap_or_default();
        Ok(MonitorInfo { spec, modes, _props: props })
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_logical_monitor(v: &OwnedValue) -> Result<LogicalMonitorInfo> {
        // Expected: (x, y, scale, transform, primary, monitors)
        let fields = Self::ov_to_fields(v)?;
        if fields.len() < 6 {
            bail!("unexpected logical monitor format from Mutter")
        }
        let x = Self::ov_to_i32(&fields[0])?;
        let y = Self::ov_to_i32(&fields[1])?;
        let scale = Self::ov_to_f64(&fields[2])?;
        let transform = Self::ov_to_u32(&fields[3])?;
        let primary = Self::ov_to_bool(&fields[4])?;

        let monitor_entries = Self::ov_to_array(&fields[5])?;
        let mut monitors = Vec::with_capacity(monitor_entries.len());
        for entry in monitor_entries {
            // (monitor_spec, mode_id, props)
            let ef = Self::ov_to_fields(&entry)?;
            if ef.len() < 3 {
                continue;
            }
            let spec = Self::parse_monitor_spec(&ef[0])?;
            let mode_id = Self::ov_to_string(&ef[1])?;
            let props = Self::ov_to_dict(&ef[2]).unwrap_or_default();
            monitors.push((spec, mode_id, props));
        }

        Ok(LogicalMonitorInfo { x, y, scale, transform, primary, monitors })
    }

    #[cfg(feature = "backend-gnome")]
    fn find_output_logical<'a>(
        logicals: &'a [LogicalMonitorInfo],
        connector: &str,
    ) -> Option<(usize, &'a LogicalMonitorInfo)> {
        logicals
            .iter()
            .enumerate()
            .find(|(_, lm)| lm.monitors.iter().any(|(spec, _, _)| spec.connector == connector))
    }

    #[cfg(feature = "backend-gnome")]
    fn transform_to_mutter(transform: &str) -> Result<u32> {
        // MetaMonitorTransform in Mutter uses numeric values 0..7.
        // We accept both the wlroots-like numeric degrees and the TUI labels.
        match transform {
            "normal" | "0" => Ok(0),
            "90" => Ok(1),
            "180" => Ok(2),
            "270" => Ok(3),
            "flipped" => Ok(4),
            "flipped-90" => Ok(5),
            "flipped-180" => Ok(6),
            "flipped-270" => Ok(7),
            other => bail!("Unsupported transform for GNOME/Mutter: {other}"),
        }
    }

    #[cfg(feature = "backend-gnome")]
    fn mode_matches(requested: &DisplayMode, candidate: &DisplayMode) -> bool {
        if requested.width != candidate.width || requested.height != candidate.height {
            return false;
        }
        match (requested.refresh_hz, candidate.refresh_hz) {
            (None, _) => true,
            (Some(req), Some(can)) => (req - can).abs() < 0.5,
            (Some(_), None) => false,
        }
    }

    #[cfg(feature = "backend-gnome")]
    fn build_apply_args(
        serial: u32,
        logicals: Vec<LogicalMonitorInfo>,
    ) -> Result<(u32, u32, Vec<LogicalMonitorConfig>, HashMap<String, OwnedValue>)> {
        // method = 1 (temporary). This matches common scripting usage.
        let method: u32 = 1;

        // Build strongly-typed GVariant args to match Mutter's expected signature.
        let mut lm_values: Vec<LogicalMonitorConfig> = Vec::with_capacity(logicals.len());

        for lm in logicals {
            let mut monitors_arr: Vec<MonitorConfig> = Vec::with_capacity(lm.monitors.len());
            for (spec, mode_id, props) in lm.monitors {
                let spec_tuple: MonitorSpecTuple = (spec.connector, spec.vendor, spec.product, spec.serial);
                monitors_arr.push((spec_tuple, mode_id, props));
            }

            lm_values.push((
                lm.x,
                lm.y,
                lm.scale,
                lm.transform,
                lm.primary,
                monitors_arr,
            ));
        }

        let props: HashMap<String, OwnedValue> = HashMap::new();
        Ok((serial, method, lm_values, props))
    }

    #[cfg(feature = "backend-gnome")]
    async fn apply_logical_monitors(serial: u32, logicals: Vec<LogicalMonitorInfo>) -> Result<()> {
        let conn = Connection::session().await?;
        let proxy = Self::mutter_proxy(&conn).await?;

        let (serial, method, logical_values, props) = Self::build_apply_args(serial, logicals)?;

        debug!(serial, method, logical_count = logical_values.len(), "Applying Mutter monitor config");

        // ApplyMonitorsConfig(serial u, method u, logical_monitors a(...), properties a{sv})
        let _: () = proxy
            .call(
                "ApplyMonitorsConfig",
                &(serial, method, logical_values, props),
            )
            .await
            .context("failed to call Mutter ApplyMonitorsConfig")?;

        Ok(())
    }

    #[cfg(feature = "backend-gnome")]
    async fn load_state() -> Result<(u32, Vec<MonitorInfo>, Vec<LogicalMonitorInfo>)> {
        let conn = Connection::session().await?;
        let (serial, monitors_raw, logicals_raw, _props) = Self::get_current_state(&conn).await?;

        let mut monitors = Vec::with_capacity(monitors_raw.len());
        for m in monitors_raw {
            match Self::parse_monitor(&m) {
                Ok(mi) => monitors.push(mi),
                Err(err) => {
                    debug!(?err, "Failed to parse Mutter monitor entry");
                }
            }
        }

        let mut logicals = Vec::with_capacity(logicals_raw.len());
        for lm in logicals_raw {
            match Self::parse_logical_monitor(&lm) {
                Ok(li) => logicals.push(li),
                Err(err) => {
                    debug!(?err, "Failed to parse Mutter logical monitor entry");
                }
            }
        }

        Ok((serial, monitors, logicals))
    }
}

#[async_trait]
impl crate::backend::DisplayBackend for GnomeBackend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (_serial, monitors, logicals) = Self::load_state().await?;

            let mut outputs = Vec::with_capacity(monitors.len());

            for (idx, mon) in monitors.iter().enumerate() {
                let name = mon.spec.connector.clone();
                let description = {
                    let mut s = String::new();
                    if !mon.spec.vendor.is_empty() {
                        s.push_str(&mon.spec.vendor);
                    }
                    if !mon.spec.product.is_empty() {
                        if !s.is_empty() {
                            s.push(' ');
                        }
                        s.push_str(&mon.spec.product);
                    }
                    if s.is_empty() { None } else { Some(s) }
                };

                let mut out = DisplayOutput::simple(idx, name.clone(), description);

                // Determine whether it is enabled and its logical monitor params.
                if let Some((_i, lm)) = Self::find_output_logical(&logicals, &name) {
                    out.enabled = true;
                    out.primary = lm.primary;
                    out.scale = Some(lm.scale);

                    // Current mode: use the mode_id from logical monitor entry, then find matching mode
                    let current_mode_id = lm
                        .monitors
                        .iter()
                        .find(|(spec, _, _)| spec.connector == name)
                        .map(|(_, mode_id, _)| mode_id.clone());

                    if let Some(mode_id) = current_mode_id {
                        if let Some(mm) = mon.modes.iter().find(|m| m.id == mode_id) {
                            out.current_mode = Some(mm.mode.clone());
                        } else {
                            // Fall back to "current" flag from mode list.
                            out.current_mode = mon.modes.iter().find(|m| m.current).map(|m| m.mode.clone());
                        }
                    }
                } else {
                    out.enabled = false;
                    out.primary = false;
                    out.scale = None;
                    out.current_mode = None;
                }

                out.available_modes = mon.modes.iter().map(|m| m.mode.clone()).collect();

                // Color controls: implemented via gammastep (global). We still expose it so UI can use it.
                out.color = DisplayColorSettings::default();
                out.color_caps = DisplayColorCapabilities {
                    brightness: true,
                    gamma: true,
                    temperature: true,
                };

                outputs.push(out);
            }

            Ok(outputs)
        }
    }

    async fn set_brightness(&self, _output: &str, value: f32) -> Result<()> {
        // gammastep is global; output argument is ignored.
        Self::gammastep_set_brightness(value)
    }

    async fn set_gamma(&self, _output: &str, value: f32) -> Result<()> {
        // gammastep is global; output argument is ignored.
        Self::gammastep_set_gamma(value)
    }

    async fn set_temperature(&self, _output: &str, value: u16) -> Result<()> {
        // gammastep is global; output argument is ignored.
        Self::gammastep_set_temperature(value)
    }

    async fn set_mode(&self, output: &str, mode: DisplayMode) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, mode);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, monitors, mut logicals) = Self::load_state().await?;

            let mon = monitors
                .iter()
                .find(|m| m.spec.connector == output)
                .ok_or_else(|| anyhow!("Output {output} not found"))?;

            let new_mode_id = mon
                .modes
                .iter()
                .find(|m| Self::mode_matches(&mode, &m.mode))
                .map(|m| m.id.clone())
                .ok_or_else(|| anyhow!("Requested mode {mode} not available for {output}"))?;

            let Some((idx, _lm)) = Self::find_output_logical(&logicals, output) else {
                bail!("Output {output} is disabled; enable it before setting mode");
            };

            let lm = &mut logicals[idx];
            for (spec, mode_id, _props) in &mut lm.monitors {
                if spec.connector == output {
                    *mode_id = new_mode_id.clone();
                }
            }

            Self::apply_logical_monitors(serial, logicals).await
        }
    }

    async fn set_scale(&self, output: &str, scale: f64) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, scale);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, _monitors, mut logicals) = Self::load_state().await?;

            let Some((idx, _lm)) = Self::find_output_logical(&logicals, output) else {
                bail!("Output {output} is disabled; enable it before setting scale");
            };

            logicals[idx].scale = scale;
            Self::apply_logical_monitors(serial, logicals).await
        }
    }

    async fn set_enabled(&self, output: &str, enabled: bool) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, enabled);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, monitors, mut logicals) = Self::load_state().await?;

            let mon = monitors
                .iter()
                .find(|m| m.spec.connector == output)
                .ok_or_else(|| anyhow!("Output {output} not found"))?;

            let currently_enabled = Self::find_output_logical(&logicals, output).is_some();
            if enabled == currently_enabled {
                return Ok(());
            }

            if !enabled {
                // Remove it from any logical monitor. Remove empty logical monitors.
                logicals.iter_mut().for_each(|lm| {
                    lm.monitors.retain(|(spec, _, _)| spec.connector != output);
                });
                logicals.retain(|lm| !lm.monitors.is_empty());
                return Self::apply_logical_monitors(serial, logicals).await;
            }

            // Enabling: create a new logical monitor to the right of existing ones.
            let mode = mon
                .modes
                .iter()
                .find(|m| m.preferred)
                .or_else(|| mon.modes.iter().find(|m| m.current))
                .or_else(|| mon.modes.first())
                .ok_or_else(|| anyhow!("No modes available for {output}"))?;

            // Compute x offset: max(x + width) among existing logical monitors.
            let mut x = 0i32;
            for lm in &logicals {
                let mut width = 0i32;
                if let Some((spec, mode_id, _)) = lm.monitors.first() {
                    if let Some(mm) = monitors
                        .iter()
                        .find(|m| m.spec == *spec)
                        .and_then(|m| m.modes.iter().find(|md| md.id == *mode_id))
                    {
                        width = mm.mode.width as i32;
                    }
                }
                x = x.max(lm.x.saturating_add(width));
            }

            let new_lm = LogicalMonitorInfo {
                x,
                y: 0,
                scale: 1.0,
                transform: 0,
                primary: logicals.is_empty(),
                monitors: vec![(mon.spec.clone(), mode.id.clone(), HashMap::new())],
            };

            logicals.push(new_lm);
            Self::apply_logical_monitors(serial, logicals).await
        }
    }

    async fn set_position(&self, output: &str, x: i32, y: i32) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, x, y);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, _monitors, mut logicals) = Self::load_state().await?;

            let Some((idx, _lm)) = Self::find_output_logical(&logicals, output) else {
                bail!("Output {output} is disabled; enable it before setting position");
            };

            logicals[idx].x = x;
            logicals[idx].y = y;
            Self::apply_logical_monitors(serial, logicals).await
        }
    }

    async fn set_position_relative(&self, output: &str, relative_to: &str, direction: &str) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, relative_to, direction);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, monitors, mut logicals) = Self::load_state().await?;

            let (out_idx, _) = Self::find_output_logical(&logicals, output)
                .ok_or_else(|| anyhow!("Output {output} is disabled"))?;
            let (_rel_idx, rel_lm) = Self::find_output_logical(&logicals, relative_to)
                .ok_or_else(|| anyhow!("Output {relative_to} is disabled"))?;

            // Estimate widths/heights by the first monitor in each logical monitor.
            let out_size = logicals[out_idx]
                .monitors
                .first()
                .and_then(|(spec, mode_id, _)| {
                    monitors
                        .iter()
                        .find(|m| m.spec == *spec)
                        .and_then(|m| m.modes.iter().find(|md| md.id == *mode_id))
                        .map(|md| (md.mode.width as i32, md.mode.height as i32))
                })
                .unwrap_or((0, 0));

            let rel_size = rel_lm
                .monitors
                .first()
                .and_then(|(spec, mode_id, _)| {
                    monitors
                        .iter()
                        .find(|m| m.spec == *spec)
                        .and_then(|m| m.modes.iter().find(|md| md.id == *mode_id))
                        .map(|md| (md.mode.width as i32, md.mode.height as i32))
                })
                .unwrap_or((0, 0));

            let (new_x, new_y) = match direction {
                "left" => (rel_lm.x.saturating_sub(out_size.0), rel_lm.y),
                "right" => (rel_lm.x.saturating_add(rel_size.0), rel_lm.y),
                "above" => (rel_lm.x, rel_lm.y.saturating_sub(out_size.1)),
                "below" => (rel_lm.x, rel_lm.y.saturating_add(rel_size.1)),
                _ => bail!("Invalid direction: {direction}"),
            };

            logicals[out_idx].x = new_x;
            logicals[out_idx].y = new_y;
            Self::apply_logical_monitors(serial, logicals).await
        }
    }

    async fn set_transform(&self, output: &str, transform: &str) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, transform);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, _monitors, mut logicals) = Self::load_state().await?;

            let Some((idx, _lm)) = Self::find_output_logical(&logicals, output) else {
                bail!("Output {output} is disabled; enable it before setting transform");
            };

            logicals[idx].transform = GnomeBackend::transform_to_mutter(transform)?;
            Self::apply_logical_monitors(serial, logicals).await
        }
    }

    async fn set_mirror(&self, output: &str, target: &str) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, target);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, monitors, mut logicals) = Self::load_state().await?;

            let src = monitors
                .iter()
                .find(|m| m.spec.connector == output)
                .ok_or_else(|| anyhow!("Output {output} not found"))?;
            let dst = monitors
                .iter()
                .find(|m| m.spec.connector == target)
                .ok_or_else(|| anyhow!("Output {target} not found"))?;

            // Find a common mode by dimensions (+refresh if present) and pick the largest.
            let mut common: Vec<(&crate::models::DisplayMode, String, String)> = Vec::new();
            for sm in &src.modes {
                for dm in &dst.modes {
                    if sm.mode.width == dm.mode.width
                        && sm.mode.height == dm.mode.height
                        && match (sm.mode.refresh_hz, dm.mode.refresh_hz) {
                            (Some(a), Some(b)) => (a - b).abs() < 0.5,
                            (None, None) => true,
                            (None, Some(_)) | (Some(_), None) => false,
                        }
                    {
                        common.push((&sm.mode, sm.id.clone(), dm.id.clone()));
                    }
                }
            }
            if common.is_empty() {
                bail!("No common modes found between {output} and {target}");
            }
            common.sort_by_key(|(m, _, _)| (m.width * m.height) as i64);
            let (best_mode, src_mode_id, dst_mode_id) = common.last().unwrap();

            info!(
                "Mirroring {output} and {target} with mode {}x{}",
                best_mode.width,
                best_mode.height
            );

            // Remove both outputs from existing logical monitors.
            for lm in &mut logicals {
                lm.monitors
                    .retain(|(spec, _, _)| spec.connector != output && spec.connector != target);
            }
            logicals.retain(|lm| !lm.monitors.is_empty());

            // Place mirror at 0,0 and keep primary if either was primary.
            let primary = true;
            let mirror_lm = LogicalMonitorInfo {
                x: 0,
                y: 0,
                scale: 1.0,
                transform: 0,
                primary,
                monitors: vec![
                    (src.spec.clone(), src_mode_id.clone(), HashMap::new()),
                    (dst.spec.clone(), dst_mode_id.clone(), HashMap::new()),
                ],
            };
            logicals.push(mirror_lm);

            GnomeBackend::apply_logical_monitors(serial, logicals).await
        }
    }

    async fn set_adaptive_sync(&self, _output: &str, _enabled: bool) -> Result<()> {
        // Mutter's public DisplayConfig API does not expose VRR toggles.
        Err(anyhow!("Adaptive sync control is not supported on GNOME/Mutter"))
    }
}
