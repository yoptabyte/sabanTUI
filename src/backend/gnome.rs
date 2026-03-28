use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration as StdDuration, Instant};
use tracing::{debug, info};

use crate::models::{DisplayColorCapabilities, DisplayColorSettings, DisplayMode, DisplayOutput, RelativePosition};

#[cfg(feature = "backend-gnome")]
use tokio::time::{sleep, Duration};

#[cfg(feature = "backend-gnome")]
use zbus::zvariant::OwnedValue;
#[cfg(feature = "backend-gnome")]
use zbus::Connection;
#[cfg(feature = "backend-gnome")]
use zbus::Proxy;
#[cfg(feature = "backend-gnome")]
use zbus::fdo::PropertiesProxy;
#[cfg(feature = "backend-gnome")]
use zbus::names::InterfaceName;
#[cfg(feature = "backend-gnome")]
use zbus::zvariant::Value;

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
    preferred_scale: f64,
    supported_scales: Vec<f64>,
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
    // Properties (a{sv}) returned by Mutter for this logical monitor.
    // Some Mutter versions require these to be present when applying configs.
    props: HashMap<String, OwnedValue>,
    // (monitor spec, mode id, properties)
    monitors: Vec<(MonitorSpec, String, HashMap<String, OwnedValue>)>,
}

#[cfg(feature = "backend-gnome")]
type MonitorSpecTuple = (String, String, String, String);

#[cfg(feature = "backend-gnome")]
type MonitorConfigV1 = (MonitorSpecTuple, String, HashMap<String, OwnedValue>);

#[cfg(feature = "backend-gnome")]
type LogicalMonitorConfigV1 = (i32, i32, f64, u32, bool, Vec<MonitorConfigV1>);

// Older Mutter versions expect monitor entries as just (connector, mode_id, props):
//   a(iiduba(ssa{sv}))
#[cfg(feature = "backend-gnome")]
type MonitorConfigV0 = (String, String, HashMap<String, OwnedValue>);
#[cfg(feature = "backend-gnome")]
type LogicalMonitorConfigV0 = (i32, i32, f64, u32, bool, Vec<MonitorConfigV0>);

// Newer Mutter versions use logical monitor configs without an explicit mode id:
// a(iiduba(ssss)a{sv})
#[cfg(feature = "backend-gnome")]
type LogicalMonitorConfigV2 = (i32, i32, f64, u32, bool, Vec<MonitorSpecTuple>, HashMap<String, OwnedValue>);

// NOTE: Mutter's GetCurrentState returns arrays-of-structs (not `av`).
// The concrete signature varies by Mutter version; the one we support here is:
//   u a((ssss)a(siiddada{sv})a{sv}) a(iiduba(ssss)a{sv}) a{sv}
// i.e.:
// - serial: u
// - monitors: a( (spec) (modes) (props) )
// - logical monitors: a( x, y, scale, transform, primary, a(spec), props )
// - properties: a{sv}
#[cfg(feature = "backend-gnome")]
type MutterMonitorModeState =
    (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedValue>);
#[cfg(feature = "backend-gnome")]
type MutterMonitorState = (MonitorSpecTuple, Vec<MutterMonitorModeState>, HashMap<String, OwnedValue>);
#[cfg(feature = "backend-gnome")]
type MutterLogicalMonitorState =
    (i32, i32, f64, u32, bool, Vec<MonitorSpecTuple>, HashMap<String, OwnedValue>);

const MUTTER_DESTINATION: &str = "org.gnome.Mutter.DisplayConfig";
const MUTTER_PATH: &str = "/org/gnome/Mutter/DisplayConfig";
const MUTTER_INTERFACE: &str = "org.gnome.Mutter.DisplayConfig";

const GSD_POWER_DESTINATION: &str = "org.gnome.SettingsDaemon.Power";
const GSD_POWER_PATH: &str = "/org/gnome/SettingsDaemon/Power";
const GSD_POWER_PATH_SCREEN: &str = "/org/gnome/SettingsDaemon/Power/Screen";
const GSD_POWER_SCREEN_INTERFACE: &str = "org.gnome.SettingsDaemon.Power.Screen";
const DBUS_INTROSPECTABLE_IFACE: &str = "org.freedesktop.DBus.Introspectable";

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct PersistedOutputColor {
    #[serde(default)]
    brightness: Option<f32>,
    #[serde(default)]
    gamma: Option<f32>,
    #[serde(default)]
    temperature: Option<u16>,
}

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct PersistedColorState {
    #[serde(default)]
    outputs: HashMap<String, PersistedOutputColor>,
}

#[cfg(feature = "backend-gnome")]
static DDCUTIL_AVAILABLE: OnceLock<bool> = OnceLock::new();
#[cfg(feature = "backend-gnome")]
static MUTTER_GAMMA_AVAILABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
#[cfg(feature = "backend-gnome")]
static DDC_BRIGHTNESS_CACHE: OnceLock<Mutex<HashMap<String, (Instant, f32)>>> = OnceLock::new();

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbusSetterKind {
    Method,
    Property,
}

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone)]
struct DbusSetter {
    destination: String,
    path: String,
    iface: String,
    name: String,
    arg_ty: String,
    kind: DbusSetterKind,
}

#[cfg(feature = "backend-gnome")]
#[derive(Debug, Clone)]
struct DbusMethodSig {
    destination: String,
    path: String,
    iface: String,
    method: String,
    in_sig: String, // concatenated types, e.g. "ssu"
}

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

    // ---- GNOME-native color + brightness plumbing ----
    //
    // On GNOME Wayland, per-output gamma/brightness via wlroots protocols is unavailable.
    // - Color temperature is controlled by GNOME Night Light (gsettings).
    // - Hardware backlight brightness (usually laptop internal panel) can be controlled via
    //   logind + /sys/class/backlight (global, not per-output).

    const GSETTINGS: &'static str = "gsettings";
    const NIGHTLIGHT_SCHEMA: &'static str = "org.gnome.settings-daemon.plugins.color";
    const NIGHTLIGHT_ENABLED: &'static str = "night-light-enabled";
    const NIGHTLIGHT_TEMPERATURE: &'static str = "night-light-temperature";
    const NIGHTLIGHT_SCHEDULE_AUTO: &'static str = "night-light-schedule-automatic";
    const CONFIG_DIR_NAME: &'static str = "sabantui";
    const CONFIG_FILE_NAME: &'static str = "gnome-color-state.json";

    fn run_gsettings(args: &[&str]) -> Result<String> {
        let output = Command::new(Self::GSETTINGS)
            .args(args)
            .output()
            .context("failed to execute gsettings")?;

        if !output.status.success() {
            bail!(
                "gsettings exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn gsettings_get(schema: &str, key: &str) -> Result<String> {
        Self::run_gsettings(&["get", schema, key])
    }

    fn gsettings_set(schema: &str, key: &str, value: &str) -> Result<()> {
        let _ = Self::run_gsettings(&["set", schema, key, value])?;
        Ok(())
    }

    fn parse_gsettings_bool(s: &str) -> Option<bool> {
        match s.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    fn parse_gsettings_u32(s: &str) -> Option<u32> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("uint32") {
            return rest.trim().parse::<u32>().ok();
        }
        s.parse::<u32>().ok()
    }

    fn nightlight_get_temperature() -> Option<u16> {
        let v = Self::gsettings_get(Self::NIGHTLIGHT_SCHEMA, Self::NIGHTLIGHT_TEMPERATURE).ok()?;
        let k = Self::parse_gsettings_u32(&v)?;
        Some((k.clamp(1000, 10000)) as u16)
    }

    fn nightlight_set_temperature(kelvin: u16) -> Result<()> {
        let kelvin = kelvin.clamp(1000, 10000) as u32;
        // Make changes immediate and predictable.
        let _ = Self::gsettings_set(Self::NIGHTLIGHT_SCHEMA, Self::NIGHTLIGHT_SCHEDULE_AUTO, "false");
        let _ = Self::gsettings_set(Self::NIGHTLIGHT_SCHEMA, Self::NIGHTLIGHT_ENABLED, "true");
        Self::gsettings_set(
            Self::NIGHTLIGHT_SCHEMA,
            Self::NIGHTLIGHT_TEMPERATURE,
            &format!("uint32 {kelvin}"),
        )
    }

    fn disable_night_light_best_effort() {
        let _ = Self::gsettings_set(Self::NIGHTLIGHT_SCHEMA, Self::NIGHTLIGHT_SCHEDULE_AUTO, "false");
        let _ = Self::gsettings_set(Self::NIGHTLIGHT_SCHEMA, Self::NIGHTLIGHT_ENABLED, "false");
    }

    fn config_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join(Self::CONFIG_DIR_NAME)
                .join(Self::CONFIG_FILE_NAME),
        )
    }

    fn load_persisted_state() -> PersistedColorState {
        let Some(path) = Self::config_path() else {
            return PersistedColorState::default();
        };
        let Ok(s) = fs::read_to_string(&path) else {
            return PersistedColorState::default();
        };
        serde_json::from_str(&s).unwrap_or_default()
    }

    fn save_persisted_state(state: &PersistedColorState) -> Result<()> {
        let path = Self::config_path()
            .ok_or_else(|| anyhow!("HOME is not set; cannot persist per-output color state"))?;
        let dir = path.parent().ok_or_else(|| anyhow!("invalid config path"))?;
        let _ = fs::create_dir_all(dir);
        let json = serde_json::to_string_pretty(state).context("failed to serialize persisted color state")?;
        fs::write(&path, json).context("failed to write persisted color state")?;
        Ok(())
    }

    fn read_sys_string(path: &Path) -> Option<String> {
        Some(fs::read_to_string(path).ok()?.trim().to_string())
    }

    fn backlight_dir() -> Option<PathBuf> {
        let base = Path::new("/sys/class/backlight");
        let entries = fs::read_dir(base).ok()?;
        let mut candidates: Vec<(i32, u32, String, PathBuf)> = Vec::new(); // (type_score, max, name, path)

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name() {
                Some(v) => v.to_string_lossy().to_string(),
                None => continue,
            };
            let max = match Self::read_sys_u32(&path.join("max_brightness")) {
                Some(v) if v > 0 => v,
                _ => continue,
            };

            // Skip devices that don't expose a readable current value.
            if Self::read_backlight_current(&path).is_none() {
                continue;
            }

            let score = Self::backlight_type_score(&path);
            candidates.push((score, max, name, path));
        }

        candidates.sort_by(|a, b| {
            // Desc by score, then desc by max, then asc by name for determinism.
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });

        candidates.first().map(|(_score, _max, _name, path)| path.clone())
    }

    fn read_sys_u32(path: &Path) -> Option<u32> {
        fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
    }

    fn backlight_type_score(dir: &Path) -> i32 {
        // Heuristic: prefer "raw" devices (usually the real panel backlight),
        // then "platform", then "firmware".
        match Self::read_sys_string(&dir.join("type"))
            .as_deref()
            .unwrap_or_default()
        {
            "raw" => 3,
            "platform" => 2,
            "firmware" => 1,
            _ => 0,
        }
    }

    fn read_backlight_current(dir: &Path) -> Option<u32> {
        // Prefer actual_brightness when available (reflects what hardware applied).
        Self::read_sys_u32(&dir.join("actual_brightness"))
            .or_else(|| Self::read_sys_u32(&dir.join("brightness")))
    }

    fn backlight_get() -> Option<(String, u32, u32)> {
        let dir = Self::backlight_dir()?;
        let name = dir.file_name()?.to_string_lossy().to_string();
        let cur = Self::read_backlight_current(&dir)?;
        let max = Self::read_sys_u32(&dir.join("max_brightness"))?;
        Some((name, cur, max.max(1)))
    }

    fn backlight_read_named(name: &str) -> Option<(u32, u32)> {
        let dir = Path::new("/sys/class/backlight").join(name);
        if !dir.is_dir() {
            return None;
        }
        let cur = Self::read_backlight_current(&dir)?;
        let max = Self::read_sys_u32(&dir.join("max_brightness"))?.max(1);
        Some((cur, max))
    }

    fn backlight_get_ratio() -> Option<f32> {
        let (_name, cur, max) = Self::backlight_get()?;
        Some((cur as f32 / max as f32).clamp(0.0, 1.0))
    }

    #[cfg(feature = "backend-gnome")]
    async fn logind_set_backlight(device: &str, value: u32) -> Result<()> {
        // Use logind (system bus) so we don't require direct sysfs write permissions.
        let conn = zbus::Connection::system().await?;

        // Fast path: old API on Manager (some systemd versions).
        if let Ok(proxy) = zbus::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .await
        {
            if proxy
                .call::<_, _, ()>("SetBrightness", &("backlight", device, value))
                .await
                .is_ok()
            {
                return Ok(());
            }
        }

        // Robust path: discover any SetBrightness-like methods exported by login1.
        let methods = Self::discover_login1_setbrightness(&conn).await;
        let mut attempts: Vec<String> = Vec::new();

        for m in methods {
            // We only support signatures that start with "ss" (subsystem, name).
            let sig = m.in_sig.as_str();
            if !sig.starts_with("ss") {
                continue;
            }

            let proxy = match zbus::Proxy::new(&conn, m.destination.as_str(), m.path.as_str(), m.iface.as_str()).await {
                Ok(p) => p,
                Err(e) => {
                    attempts.push(format!("proxy {} @ {}: {e}", m.iface, m.path));
                    continue;
                }
            };

            // Third arg type determines how we pass value.
            let third = sig.chars().nth(2).unwrap_or('?');
            let res: Result<(), zbus::Error> = match third {
                'u' => proxy.call(m.method.as_str(), &("backlight", device, value)).await,
                't' => proxy.call(m.method.as_str(), &("backlight", device, value as u64)).await,
                'i' => proxy.call(m.method.as_str(), &("backlight", device, value as i32)).await,
                'q' => proxy.call(m.method.as_str(), &("backlight", device, value as u16)).await,
                'y' => proxy.call(m.method.as_str(), &("backlight", device, (value.min(255) as u8))).await,
                _ => {
                    attempts.push(format!("{}::{}({sig}) @ {}: unsupported", m.iface, m.method, m.path));
                    continue;
                }
            };

            match res {
                Ok(()) => return Ok(()),
                Err(e) => attempts.push(format!("{}::{}({sig}) @ {}: {e}", m.iface, m.method, m.path)),
            }
        }

        bail!(
            "logind backlight control unavailable/denied; attempts: {}",
            attempts.join(" | ")
        )
    }

    async fn set_backlight_ratio(value: f32) -> Result<()> {
        let clamped = value.clamp(0.0, 1.0);
        let (name, _cur, max) = Self::backlight_get().ok_or_else(|| anyhow!("No backlight device found (/sys/class/backlight)"))?;
        let abs = ((clamped * (max as f32)).round() as u32).clamp(0, max);

        #[cfg(feature = "backend-gnome")]
        {
            // Prefer logind (handles permissions), but fall back to sysfs if available.
            if let Err(logind_err) = Self::logind_set_backlight(&name, abs).await {
                debug!(?logind_err, device = %name, "logind SetBrightness failed; trying sysfs write");
                let dir = Path::new("/sys/class/backlight").join(&name);
                let brightness = dir.join("brightness");
                // Only attempt sysfs write if we can actually open it for writing;
                // otherwise return the logind error (which is the actionable one).
                match OpenOptions::new().write(true).open(&brightness) {
                    Ok(mut file) => {
                        use std::io::Write;
                        file.write_all(abs.to_string().as_bytes()).ok();
                    }
                    Err(sysfs_open_err) => {
                        // If we can't write sysfs, don't hide the real logind failure behind Permission denied.
                        return Err(anyhow!(
                            "failed to set backlight brightness (device={name}) via logind: {logind_err} (sysfs not writable: {sysfs_open_err})"
                        ));
                    }
                };
            }
            // Verify that the write took effect (some setups accept calls but don't apply).
            sleep(Duration::from_millis(60)).await;
            if let Some((cur, _max2)) = Self::backlight_read_named(&name) {
                let delta = (cur as i64 - abs as i64).abs();
                if delta <= 1 {
                    return Ok(());
                }
                bail!(
                    "backlight write did not take effect (device={name}): expected {abs}, observed {cur}"
                );
            }
            // If we can't read back, assume success (best effort).
            return Ok(());
        }

        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (name, abs);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }
    }

    fn connector_looks_internal(connector: &str) -> bool {
        let c = connector.to_ascii_lowercase();
        c.starts_with("edp") || c.starts_with("lvds") || c.starts_with("dsi")
    }

    // ---- External monitor brightness via DDC/CI (ddcutil) ----
    #[cfg(feature = "backend-gnome")]
    fn ddcutil_is_available() -> bool {
        *DDCUTIL_AVAILABLE.get_or_init(|| {
            Command::new("ddcutil")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
    }

    #[cfg(feature = "backend-gnome")]
    fn connector_aliases(connector: &str) -> Vec<String> {
        let c = connector.trim();
        let mut out = vec![c.to_string()];
        // Mutter often uses "HDMI-1", kernel DRM often uses "HDMI-A-1".
        if let Some(rest) = c.strip_prefix("HDMI-") {
            out.push(format!("HDMI-A-{rest}"));
        }
        if let Some(rest) = c.strip_prefix("DVI-") {
            out.push(format!("DVI-D-{rest}"));
            out.push(format!("DVI-I-{rest}"));
        }
        if let Some(rest) = c.strip_prefix("HDMI-A-") {
            out.push(format!("HDMI-{rest}"));
        }
        out.sort();
        out.dedup();
        out
    }

    #[cfg(feature = "backend-gnome")]
    fn find_ddc_i2c_bus_for_connector(connector: &str) -> Option<u32> {
        let base = Path::new("/sys/class/drm");
        let Ok(entries) = fs::read_dir(base) else { return None };
        let aliases = Self::connector_aliases(connector);

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name() {
                Some(v) => v.to_string_lossy().to_string(),
                None => continue,
            };
            if !name.starts_with("card") || !name.contains('-') {
                continue;
            }
            // name like "card0-HDMI-A-1"
            let Some((_card, drm_conn)) = name.split_once('-') else { continue };
            if !aliases.iter().any(|a| a == drm_conn) {
                continue;
            }

            // Prefer the `ddc` symlink when present (points to the real DDC i2c adapter).
            let ddc = path.join("ddc");
            if let Ok(target) = fs::read_link(&ddc) {
                if let Some(fname) = target.file_name().map(|v| v.to_string_lossy().to_string()) {
                    if let Some(rest) = fname.strip_prefix("i2c-") {
                        if let Ok(bus) = rest.parse::<u32>() {
                            return Some(bus);
                        }
                    }
                }
            }

            let Ok(subs) = fs::read_dir(&path) else { continue };
            let mut buses: Vec<u32> = Vec::new();
            for sub in subs.flatten() {
                let sp = sub.path();
                let sname = match sp.file_name() {
                    Some(v) => v.to_string_lossy().to_string(),
                    None => continue,
                };
                if let Some(rest) = sname.strip_prefix("i2c-") {
                    if let Ok(bus) = rest.parse::<u32>() {
                        buses.push(bus);
                    }
                }
            }
            buses.sort();
            if let Some(bus) = buses.first().copied() {
                return Some(bus);
            }
        }

        None
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_ddcutil_current_max(stdout: &str) -> Option<(u32, u32)> {
        // Example:
        // "VCP code 0x10 (Brightness): current value =    50, max value =   100"
        let s = stdout;
        let cur = s
            .split("current value")
            .nth(1)
            .and_then(|t| t.split('=').nth(1))
            .and_then(|t| t.split(',').next())
            .map(|t| t.trim())
            .and_then(|t| t.parse::<u32>().ok())?;
        let max = s
            .split("max value")
            .nth(1)
            .and_then(|t| t.split('=').nth(1))
            .map(|t| t.trim())
            .and_then(|t| t.parse::<u32>().ok())?;
        Some((cur, max.max(1)))
    }

    #[cfg(feature = "backend-gnome")]
    fn ddc_get_brightness_ratio(bus: u32) -> Result<f32> {
        let output = Command::new("ddcutil")
            .args(["--bus", &bus.to_string(), "getvcp", "10", "--brief"])
            .output()
            .context("failed to execute ddcutil getvcp")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if stderr.to_ascii_lowercase().contains("permission denied") {
                bail!("ddcutil getvcp failed (permission denied). You likely need access to /dev/i2c-{bus} (udev rules / add user to i2c group / run as root). Raw error: {stderr}");
            }
            bail!("ddcutil getvcp failed: {stderr}");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (cur, max) = Self::parse_ddcutil_current_max(&stdout)
            .ok_or_else(|| anyhow!("failed to parse ddcutil output: {stdout}"))?;
        Ok((cur as f32 / max as f32).clamp(0.0, 1.0))
    }

    #[cfg(feature = "backend-gnome")]
    fn ddc_set_brightness_ratio(bus: u32, value: f32) -> Result<()> {
        let clamped = value.clamp(0.0, 1.0);

        // Read max from getvcp when possible (usually 100, but not always).
        let max = {
            let out = Command::new("ddcutil")
                .args(["--bus", &bus.to_string(), "getvcp", "10", "--brief"])
                .output()
                .ok();
            if let Some(out) = out {
                if out.status.success() {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    Self::parse_ddcutil_current_max(&stdout).map(|(_cur, max)| max).unwrap_or(100)
                } else {
                    100
                }
            } else {
                100
            }
        }
        .max(1);
        let abs = ((clamped * (max as f32)).round() as u32).clamp(0, max);
        let output = Command::new("ddcutil")
            .args(["--bus", &bus.to_string(), "setvcp", "10", &abs.to_string()])
            .output()
            .context("failed to execute ddcutil setvcp")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if stderr.to_ascii_lowercase().contains("permission denied") {
                bail!("ddcutil setvcp failed (permission denied). You likely need access to /dev/i2c-{bus} (udev rules / add user to i2c group / run as root). Raw error: {stderr}");
            }
            bail!("ddcutil setvcp failed: {stderr}");
        }
        Ok(())
    }

    #[cfg(feature = "backend-gnome")]
    fn cached_ddc_brightness(connector: &str, ttl: StdDuration) -> Option<f32> {
        let cache = DDC_BRIGHTNESS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = cache.lock().ok()?;
        if let Some((t, v)) = guard.get(connector) {
            if t.elapsed() <= ttl {
                return Some(*v);
            }
        }
        None
    }

    #[cfg(feature = "backend-gnome")]
    fn set_cached_ddc_brightness(connector: &str, value: f32) {
        let cache = DDC_BRIGHTNESS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(mut guard) = cache.lock() {
            guard.insert(connector.to_string(), (Instant::now(), value));
        }
    }

    // ---- Per-output temperature/gamma via Mutter SetCrtcGamma ----
    #[cfg(feature = "backend-gnome")]
    async fn mutter_has_set_crtc_gamma() -> bool {
        let Ok(conn) = Connection::session().await else { return false };
        let Some(xml) = Self::introspect_xml_at(&conn, MUTTER_DESTINATION, MUTTER_PATH).await else { return false };
        xml.contains("method name=\"SetCrtcGamma\"") || xml.contains("method name='SetCrtcGamma'")
    }

    #[cfg(feature = "backend-gnome")]
    async fn mutter_gamma_available() -> bool {
        *MUTTER_GAMMA_AVAILABLE
            .get_or_init(|| async { Self::mutter_has_set_crtc_gamma().await })
            .await
    }

    #[cfg(feature = "backend-gnome")]
    fn looks_like_connector_name(s: &str) -> bool {
        let up = s.trim();
        if up.is_empty() {
            return false;
        }
        let l = up.to_ascii_lowercase();
        let prefixes = ["edp", "dp", "hdmi", "dvi", "vga", "lvds", "dsi"];
        if !prefixes.iter().any(|p| l.starts_with(p)) {
            return false;
        }
        up.chars().any(|c| c.is_ascii_digit()) && up.contains('-')
    }

    #[cfg(feature = "backend-gnome")]
    async fn mutter_get_resources_outputs(conn: &Connection) -> Result<Vec<OwnedValue>> {
        let proxy = Self::mutter_proxy(conn).await?;
        // GetResources has multiple out-args; decode as a Rust tuple.
        // Signature varies slightly across versions, but the common prefix is:
        //   (serial: u32, crtcs: a(...), outputs: a(...), modes: a(...))
        //
        // We only need the `outputs` array to map connector -> CRTC.
        let (_serial, _crtcs, outputs, _modes): (u32, OwnedValue, OwnedValue, OwnedValue) = proxy
            .call("GetResources", &())
            .await
            .map_err(|e| anyhow!("Mutter GetResources call/decoding failed: {e}"))?;

        let arr = Self::ov_to_array(&outputs).context("failed to decode outputs array from GetResources")?;
        Ok(arr)
    }

    #[cfg(feature = "backend-gnome")]
    async fn mutter_find_crtc_for_output(connector: &str) -> Result<u32> {
        let conn = Connection::session().await?;
        let outputs_arr = Self::mutter_get_resources_outputs(&conn).await?;
        let aliases = Self::connector_aliases(connector);

        for out_v in outputs_arr {
            let of = match Self::ov_to_fields(&out_v) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let mut conn_name: Option<String> = None;
            let mut u32s: Vec<u32> = Vec::new();
            for f in &of {
                // Sometimes the connector name lives inside an a{sv} props dict.
                if let Ok(map) = Self::ov_to_dict(f) {
                    for key in ["connector", "connector-name", "display-name", "name"] {
                        if let Some(v) = map.get(key) {
                            if let Ok(s) = Self::ov_to_string(v) {
                                if Self::looks_like_connector_name(&s) {
                                    conn_name = Some(s);
                                }
                            }
                        }
                    }
                }
                if let Ok(s) = Self::ov_to_string(f) {
                    if Self::looks_like_connector_name(&s) {
                        conn_name = Some(s);
                    }
                }
                if let Ok(n) = Self::ov_to_u32(f) {
                    u32s.push(n);
                }
            }

            let Some(found_name) = conn_name else { continue };
            if !aliases.iter().any(|a| a == &found_name) {
                continue;
            }

            if u32s.len() >= 2 && u32s[1] != 0 {
                return Ok(u32s[1]);
            }
            for n in u32s.into_iter().skip(1) {
                if n != 0 {
                    return Ok(n);
                }
            }
        }

        bail!("failed to find CRTC for output {connector} via Mutter GetResources");
    }

    #[cfg(feature = "backend-gnome")]
    fn kelvin_to_rgb(k: u16) -> (f64, f64, f64) {
        let temp = (k as f64 / 100.0).clamp(10.0, 100.0);
        let (mut red, mut green, mut blue);

        if temp <= 66.0 {
            red = 255.0;
            green = 99.4708025861 * temp.ln() - 161.1195681661;
            if temp <= 19.0 {
                blue = 0.0;
            } else {
                blue = 138.5177312231 * (temp - 10.0).ln() - 305.0447927307;
            }
        } else {
            red = 329.698727446 * (temp - 60.0).powf(-0.1332047592);
            green = 288.1221695283 * (temp - 60.0).powf(-0.0755148492);
            blue = 255.0;
        }

        red = red.clamp(0.0, 255.0);
        green = green.clamp(0.0, 255.0);
        blue = blue.clamp(0.0, 255.0);
        (red / 255.0, green / 255.0, blue / 255.0)
    }

    #[cfg(feature = "backend-gnome")]
    fn build_gamma_ramps(gamma: f32, temperature: u16, size: usize) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
        let size = size.max(2);
        let g = gamma.max(0.1) as f64;

        let (r_k, g_k, b_k) = Self::kelvin_to_rgb(temperature.clamp(1000, 10000));
        let (r_65, g_65, b_65) = Self::kelvin_to_rgb(6500);
        let r_mul = (r_k / r_65).clamp(0.0, 2.0);
        let g_mul = (g_k / g_65).clamp(0.0, 2.0);
        let b_mul = (b_k / b_65).clamp(0.0, 2.0);

        let mut r = Vec::with_capacity(size);
        let mut gch = Vec::with_capacity(size);
        let mut b = Vec::with_capacity(size);

        for i in 0..size {
            let x = i as f64 / (size - 1) as f64;
            let y = x.powf(1.0 / g);
            let base = (y * 65535.0).clamp(0.0, 65535.0);
            r.push((base * r_mul).clamp(0.0, 65535.0).round() as u16);
            gch.push((base * g_mul).clamp(0.0, 65535.0).round() as u16);
            b.push((base * b_mul).clamp(0.0, 65535.0).round() as u16);
        }

        (r, gch, b)
    }

    #[cfg(feature = "backend-gnome")]
    async fn mutter_set_crtc_gamma(crtc: u32, r: Vec<u16>, g: Vec<u16>, b: Vec<u16>) -> Result<()> {
        let conn = Connection::session().await?;
        let proxy = Self::mutter_proxy(&conn).await?;
        proxy
            .call::<_, _, ()>("SetCrtcGamma", &(crtc, r, g, b))
            .await
            .context("failed to call Mutter SetCrtcGamma")?;
        Ok(())
    }

    #[cfg(feature = "backend-gnome")]
    async fn apply_persisted_color_for_output(output: &str) -> Result<()> {
        if !Self::mutter_gamma_available().await {
            bail!("Per-output color controls are not supported on this GNOME/Mutter version (SetCrtcGamma unavailable)");
        }

        Self::disable_night_light_best_effort();

        let state = Self::load_persisted_state();
        let entry = state.outputs.get(output).cloned().unwrap_or_default();
        let gamma = entry.gamma.unwrap_or(1.0);
        let temp = entry.temperature.unwrap_or(6500);

        let crtc = Self::mutter_find_crtc_for_output(output).await?;

        for size in [256usize, 1024, 512] {
            let (r, g, b) = Self::build_gamma_ramps(gamma, temp, size);
            match Self::mutter_set_crtc_gamma(crtc, r, g, b).await {
                Ok(()) => return Ok(()),
                Err(e) => debug!(?e, size, crtc, output = %output, "SetCrtcGamma failed; trying next LUT size"),
            }
        }
        bail!("failed to apply per-output gamma ramps for {output} (SetCrtcGamma rejected all LUT sizes)")
    }

    #[cfg(feature = "backend-gnome")]
    async fn introspect_interfaces(conn: &Connection, path: &str) -> Vec<String> {
        let proxy = match Proxy::new(conn, GSD_POWER_DESTINATION, path, DBUS_INTROSPECTABLE_IFACE).await {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        let xml: String = match proxy.call("Introspect", &()).await {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        // Extremely small parser: look for interface name="...".
        let mut out = Vec::new();
        for part in xml.split("interface name=\"").skip(1) {
            if let Some(name) = part.split('"').next() {
                if !name.is_empty() {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    #[cfg(feature = "backend-gnome")]
    async fn introspect_xml_at(conn: &Connection, destination: &str, path: &str) -> Option<String> {
        let proxy = Proxy::new(conn, destination, path, DBUS_INTROSPECTABLE_IFACE)
            .await
            .ok()?;
        proxy.call("Introspect", &()).await.ok()
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_interface_blocks(xml: &str) -> Vec<(String, String)> {
        // Return (iface_name, iface_xml_block).
        fn parse_with_sep(xml: &str, sep: &str, quote: char, out: &mut Vec<(String, String)>) {
            for part in xml.split(sep).skip(1) {
                let Some(iface) = part.split(quote).next() else { continue };
                if iface.is_empty() {
                    continue;
                }
                let block = part.split("</interface>").next().unwrap_or("").to_string();
                out.push((iface.to_string(), block));
            }
        }

        let mut pairs: Vec<(String, String)> = Vec::new();
        parse_with_sep(xml, "interface name=\"", '"', &mut pairs);
        parse_with_sep(xml, "interface name='", '\'', &mut pairs);

        // Dedup by iface name (keep first for determinism).
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (iface, block) in pairs {
            if seen.insert(iface.clone()) {
                out.push((iface, block));
            }
        }
        out
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_node_names(xml: &str) -> Vec<String> {
        fn parse_with_sep(xml: &str, sep: &str, quote: char, out: &mut Vec<String>) {
            for part in xml.split(sep).skip(1) {
                let Some(name) = part.split(quote).next() else { continue };
                if name.is_empty() {
                    continue;
                }
                out.push(name.to_string());
            }
        }
        let mut out = Vec::new();
        parse_with_sep(xml, "node name=\"", '"', &mut out);
        parse_with_sep(xml, "node name='", '\'', &mut out);
        out.sort();
        out.dedup();
        out
    }

    #[cfg(feature = "backend-gnome")]
    fn is_brightness_like(name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        (n.contains("bright") || n.contains("percent") || n.contains("backlight"))
            && !(n.contains("kbd") || n.contains("keyboard"))
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_setter_methods_from_iface(iface: &str, block: &str) -> Vec<(String, String)> {
        // Return (method, arg_ty) for brightness-like setters.
        let mut out = Vec::new();
        for method_part in block.split("<method name=\"").skip(1) {
            let Some(method) = method_part.split('"').next() else { continue };
            if !method.starts_with("Set") {
                continue;
            }
            if !Self::is_brightness_like(method) {
                continue;
            }
            let method_block = method_part.split("</method>").next().unwrap_or("");
            let mut in_types: Vec<String> = Vec::new();
            for arg_part in method_block.split("<arg ").skip(1) {
                if arg_part.contains("direction=\"out\"") || arg_part.contains("direction='out'") {
                    continue;
                }
                let ty = arg_part
                    .split("type=\"")
                    .nth(1)
                    .and_then(|t| t.split('"').next())
                    .or_else(|| arg_part.split("type='").nth(1).and_then(|t| t.split('\'').next()));
                let Some(ty) = ty else { continue };
                if !ty.is_empty() {
                    in_types.push(ty.to_string());
                }
            }
            if in_types.len() != 1 {
                continue;
            }
            let ty = in_types[0].clone();
            match ty.as_str() {
                "y" | "n" | "q" | "i" | "u" | "d" => out.push((method.to_string(), ty)),
                _ => {}
            }
        }

        // Also handle single-quoted method names if present.
        for method_part in block.split("<method name='").skip(1) {
            let Some(method) = method_part.split('\'').next() else { continue };
            if !method.starts_with("Set") {
                continue;
            }
            if !Self::is_brightness_like(method) {
                continue;
            }
            let method_block = method_part.split("</method>").next().unwrap_or("");
            let mut in_types: Vec<String> = Vec::new();
            for arg_part in method_block.split("<arg ").skip(1) {
                if arg_part.contains("direction=\"out\"") || arg_part.contains("direction='out'") {
                    continue;
                }
                let ty = arg_part
                    .split("type=\"")
                    .nth(1)
                    .and_then(|t| t.split('"').next())
                    .or_else(|| arg_part.split("type='").nth(1).and_then(|t| t.split('\'').next()));
                let Some(ty) = ty else { continue };
                if !ty.is_empty() {
                    in_types.push(ty.to_string());
                }
            }
            if in_types.len() != 1 {
                continue;
            }
            let ty = in_types[0].clone();
            match ty.as_str() {
                "y" | "n" | "q" | "i" | "u" | "d" => out.push((method.to_string(), ty)),
                _ => {}
            }
        }

        // Dedup.
        out.sort();
        out.dedup();
        let _ = iface;
        out
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_writable_props_from_iface(block: &str) -> Vec<(String, String)> {
        // Return (property, ty) for brightness-like writable properties.
        let mut out = Vec::new();
        for prop_part in block.split("<property ").skip(1) {
            if !(prop_part.contains("access=\"readwrite\"")
                || prop_part.contains("access='readwrite'")
                || prop_part.contains("access=\"write\"")
                || prop_part.contains("access='write'"))
            {
                continue;
            }
            let name = prop_part
                .split("name=\"")
                .nth(1)
                .and_then(|t| t.split('"').next())
                .or_else(|| prop_part.split("name='").nth(1).and_then(|t| t.split('\'').next()));
            let Some(name) = name else { continue };
            if name.is_empty() || !Self::is_brightness_like(name) {
                continue;
            }
            let ty = prop_part
                .split("type=\"")
                .nth(1)
                .and_then(|t| t.split('"').next())
                .or_else(|| prop_part.split("type='").nth(1).and_then(|t| t.split('\'').next()));
            let Some(ty) = ty else { continue };
            match ty {
                "y" | "n" | "q" | "i" | "u" | "d" => out.push((name.to_string(), ty.to_string())),
                _ => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }

    #[cfg(feature = "backend-gnome")]
    async fn discover_brightness_setters(
        conn: &Connection,
        destination: &str,
        root_path: &str,
    ) -> Vec<DbusSetter> {
        let mut out: Vec<DbusSetter> = Vec::new();
        let mut q: VecDeque<(String, usize)> = VecDeque::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Seed with known paths even if Introspect doesn't list child nodes.
        let mut seeds = vec![
            root_path.to_string(),
            format!("{root_path}/Screen"),
            format!("{root_path}/Backlight"),
            format!("{root_path}/KbdBacklight"),
            format!("{root_path}/Keyboard"),
        ];
        seeds.sort();
        seeds.dedup();
        for s in seeds {
            q.push_back((s, 0));
        }

        let max_depth = 3usize;
        let max_nodes = 64usize;
        let mut visited = 0usize;

        while let Some((path, depth)) = q.pop_front() {
            if depth > max_depth {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            visited += 1;
            if visited > max_nodes {
                break;
            }

            let Some(xml) = Self::introspect_xml_at(conn, destination, &path).await else {
                continue;
            };

            for (iface, block) in Self::parse_interface_blocks(&xml) {
                // Ignore obvious keyboard-only interfaces for "screen brightness" use-cases.
                let iface_l = iface.to_ascii_lowercase();
                if iface_l.contains("keyboard") || iface_l.contains("kbd") {
                    continue;
                }
                for (method, arg_ty) in Self::parse_setter_methods_from_iface(&iface, &block) {
                    out.push(DbusSetter {
                        destination: destination.to_string(),
                        path: path.clone(),
                        iface: iface.clone(),
                        name: method,
                        arg_ty,
                        kind: DbusSetterKind::Method,
                    });
                }
                for (prop, arg_ty) in Self::parse_writable_props_from_iface(&block) {
                    out.push(DbusSetter {
                        destination: destination.to_string(),
                        path: path.clone(),
                        iface: iface.clone(),
                        name: prop,
                        arg_ty,
                        kind: DbusSetterKind::Property,
                    });
                }
            }

            for node in Self::parse_node_names(&xml) {
                // Relative node names are typical in Introspect output.
                let child = if path.ends_with('/') {
                    format!("{path}{node}")
                } else {
                    format!("{path}/{node}")
                };
                q.push_back((child, depth + 1));
            }
        }

        out
    }

    #[cfg(feature = "backend-gnome")]
    fn parse_methods_by_name(block: &str, want: &dyn Fn(&str) -> bool) -> Vec<(String, String)> {
        // Return (method, in_sig).
        let mut out: Vec<(String, String)> = Vec::new();

        for sep in ["<method name=\"", "<method name='"] {
            for method_part in block.split(sep).skip(1) {
                let method = method_part
                    .split(&['"', '\''][..])
                    .next()
                    .unwrap_or_default();
                if method.is_empty() || !want(method) {
                    continue;
                }
                let method_block = method_part.split("</method>").next().unwrap_or("");
                let mut sig = String::new();
                for arg_part in method_block.split("<arg ").skip(1) {
                    if arg_part.contains("direction=\"out\"") || arg_part.contains("direction='out'") {
                        continue;
                    }
                    let ty = arg_part
                        .split("type=\"")
                        .nth(1)
                        .and_then(|t| t.split('"').next())
                        .or_else(|| arg_part.split("type='").nth(1).and_then(|t| t.split('\'').next()));
                    let Some(ty) = ty else { continue };
                    if !ty.is_empty() {
                        sig.push_str(ty);
                    }
                }
                out.push((method.to_string(), sig));
            }
        }

        out.sort();
        out.dedup();
        out
    }

    #[cfg(feature = "backend-gnome")]
    async fn discover_login1_setbrightness(conn: &Connection) -> Vec<DbusMethodSig> {
        let dest = "org.freedesktop.login1";
        let root = "/org/freedesktop/login1";

        let mut out: Vec<DbusMethodSig> = Vec::new();
        let mut q: VecDeque<(String, usize)> = VecDeque::new();
        let mut seen: HashSet<String> = HashSet::new();
        q.push_back((root.to_string(), 0));

        let max_depth = 5usize;
        let max_nodes = 160usize;
        let mut visited = 0usize;

        let want = |m: &str| {
            let ml = m.to_ascii_lowercase();
            ml == "setbrightness" || (ml.starts_with("set") && ml.contains("bright"))
        };

        while let Some((path, depth)) = q.pop_front() {
            if depth > max_depth {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            visited += 1;
            if visited > max_nodes {
                break;
            }

            let Some(xml) = Self::introspect_xml_at(conn, dest, &path).await else {
                continue;
            };

            for (iface, block) in Self::parse_interface_blocks(&xml) {
                for (method, in_sig) in Self::parse_methods_by_name(&block, &want) {
                    // We only know how to call methods that take (subsystem, name, value)
                    // as the first 3 args.
                    if in_sig.len() < 3 {
                        continue;
                    }
                    out.push(DbusMethodSig {
                        destination: dest.to_string(),
                        path: path.clone(),
                        iface: iface.clone(),
                        method,
                        in_sig,
                    });
                }
            }

            for node in Self::parse_node_names(&xml) {
                let child = if path.ends_with('/') {
                    format!("{path}{node}")
                } else {
                    format!("{path}/{node}")
                };
                q.push_back((child, depth + 1));
            }
        }

        // Prefer Manager interface if present.
        out.sort_by(|a, b| {
            let am = a.iface.contains("Manager");
            let bm = b.iface.contains("Manager");
            bm.cmp(&am)
                .then_with(|| a.method.cmp(&b.method))
                .then_with(|| a.path.cmp(&b.path))
        });
        out.dedup_by(|a, b| a.destination == b.destination && a.path == b.path && a.iface == b.iface && a.method == b.method && a.in_sig == b.in_sig);
        out
    }

    #[cfg(feature = "backend-gnome")]
    async fn gsd_power_get_percentage() -> Result<i32> {
        let conn = Connection::session().await?;
        let mut last_err: Option<anyhow::Error> = None;
        let mut remember_err = |e: anyhow::Error| {
            if last_err.is_none() {
                last_err = Some(e);
            }
        };
        let known_ifaces = [
            "org.gnome.SettingsDaemon.Power.Screen",
            "org.gnome.SettingsDaemon.Power",
            "org.gnome.SettingsDaemon.Power.Backlight",
        ];

        for path in [GSD_POWER_PATH, GSD_POWER_PATH_SCREEN] {
            let mut iface_candidates: Vec<String> = Self::introspect_interfaces(&conn, path).await;
            iface_candidates.extend(known_ifaces.iter().map(|s| s.to_string()));
            iface_candidates.sort();
            iface_candidates.dedup();

            for iface in iface_candidates {
                let proxy = match Proxy::new(&conn, GSD_POWER_DESTINATION, path, iface.as_str()).await {
                    Ok(p) => p,
                    Err(e) => {
                        remember_err(anyhow!("{e}"));
                        continue;
                    }
                };

                // Try a few common methods.
                for method in ["GetPercentage", "GetBrightness"] {
                    match proxy.call::<_, _, u32>(method, &()).await {
                        Ok(pct) => return Ok((pct as i32).clamp(0, 100)),
                        Err(e) => remember_err(anyhow!("{e}")),
                    }
                    match proxy.call::<_, _, i32>(method, &()).await {
                        Ok(pct) => return Ok(pct.clamp(0, 100)),
                        Err(e) => remember_err(anyhow!("{e}")),
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow!("gsd-power GetPercentage failed")))
    }

    #[cfg(feature = "backend-gnome")]
    async fn gsd_power_set_percentage(pct: i32) -> Result<()> {
        let conn = Connection::session().await?;
        let pct_i32 = pct.clamp(0, 100);
        let pct_u32 = pct_i32 as u32;
        let pct_u8 = pct_i32 as u8;
        let ratio = (pct_i32 as f64 / 100.0).clamp(0.0, 1.0);

        let mut attempts: Vec<String> = Vec::new();
        let mut push_attempt = |s: String| {
            // Keep errors readable in TUI: cap the list.
            if attempts.len() < 40 {
                attempts.push(s);
            }
        };

        // Capture current state to detect no-op "success" implementations.
        let before_backlight = Self::backlight_snapshot_values();
        let before_pct = Self::gsd_power_get_percentage().await.ok();

        // Helper to decide whether a call actually did anything.
        async fn verify_effect(
            desired_pct: i32,
            before_backlight: &[(String, u32)],
        ) -> bool {
            // gsd-power may apply asynchronously; retry a few times.
            // Prefer gsd reported value (matches GNOME slider semantics),
            // and also observe kernel backlight when available.
            for delay_ms in [60u64, 120, 240, 480] {
                sleep(Duration::from_millis(delay_ms)).await;

                // Check gsd reported percentage first.
                if let Ok(cur) = GnomeBackend::gsd_power_get_percentage().await {
                    if (cur - desired_pct).abs() <= 1 {
                        return true;
                    }
                }

                // Also check kernel backlight state (read-only) for any change.
                let after_backlight = GnomeBackend::backlight_snapshot_values();
                if !before_backlight.is_empty() && !after_backlight.is_empty() {
                    for (name, before) in before_backlight {
                        if let Some((_, after)) = after_backlight.iter().find(|(n, _)| n == name) {
                            if (*after as i64 - *before as i64).abs() >= 1 {
                                return true;
                            }
                        }
                    }
                }
            }

            false
        }

        // Discover & try real setters under the service tree.
        let mut setters =
            Self::discover_brightness_setters(&conn, GSD_POWER_DESTINATION, GSD_POWER_PATH).await;

        // Prefer screen/backlight-looking candidates.
        setters.sort_by_key(|s| {
            let t = format!("{} {} {}", s.path, s.iface, s.name).to_ascii_lowercase();
            let mut score: i32 = 0;
            if t.contains("screen") { score += 40; }
            if t.contains("backlight") { score += 30; }
            if t.contains("percentage") { score += 20; }
            if t.contains("brightness") { score += 10; }
            if t.contains("application") { score -= 100; }
            if t.contains("kbd") || t.contains("keyboard") { score -= 100; }
            -score // sort ascending -> highest score first
        });

        push_attempt(format!("discovered {} brightness setters in gsd-power", setters.len()));

        for s in setters {
            match s.kind {
                DbusSetterKind::Method => {
                    let proxy = match Proxy::new(&conn, s.destination.as_str(), s.path.as_str(), s.iface.as_str()).await {
                        Ok(p) => p,
                        Err(e) => {
                            push_attempt(format!("proxy {} @ {}: {e}", s.iface, s.path));
                            continue;
                        }
                    };

                    // For doubles, try both ratio and percent depending on name.
                    let mut tried = false;
                    match s.arg_ty.as_str() {
                        "y" => {
                            tried = true;
                            match proxy.call::<_, _, ()>(s.name.as_str(), &(pct_u8)).await {
                                Ok(()) => {
                                    if verify_effect(pct_i32, &before_backlight).await {
                                        return Ok(());
                                    }
                                    push_attempt(format!("{}::{}(y) @ {}: no effect", s.iface, s.name, s.path));
                                }
                                Err(e) => push_attempt(format!("{}::{}(y) @ {}: {e}", s.iface, s.name, s.path)),
                            }
                        }
                        "n" => {
                            tried = true;
                            match proxy.call::<_, _, ()>(s.name.as_str(), &((pct_i32 as i16))).await {
                                Ok(()) => {
                                    if verify_effect(pct_i32, &before_backlight).await {
                                        return Ok(());
                                    }
                                    push_attempt(format!("{}::{}(n) @ {}: no effect", s.iface, s.name, s.path));
                                }
                                Err(e) => push_attempt(format!("{}::{}(n) @ {}: {e}", s.iface, s.name, s.path)),
                            }
                        }
                        "q" => {
                            tried = true;
                            match proxy.call::<_, _, ()>(s.name.as_str(), &((pct_i32 as u16))).await {
                                Ok(()) => {
                                    if verify_effect(pct_i32, &before_backlight).await {
                                        return Ok(());
                                    }
                                    push_attempt(format!("{}::{}(q) @ {}: no effect", s.iface, s.name, s.path));
                                }
                                Err(e) => push_attempt(format!("{}::{}(q) @ {}: {e}", s.iface, s.name, s.path)),
                            }
                        }
                        "i" => {
                            tried = true;
                            match proxy.call::<_, _, ()>(s.name.as_str(), &(pct_i32)).await {
                                Ok(()) => {
                                    if verify_effect(pct_i32, &before_backlight).await {
                                        return Ok(());
                                    }
                                    push_attempt(format!("{}::{}(i) @ {}: no effect", s.iface, s.name, s.path));
                                }
                                Err(e) => push_attempt(format!("{}::{}(i) @ {}: {e}", s.iface, s.name, s.path)),
                            }
                        }
                        "u" => {
                            tried = true;
                            match proxy.call::<_, _, ()>(s.name.as_str(), &(pct_u32)).await {
                                Ok(()) => {
                                    if verify_effect(pct_i32, &before_backlight).await {
                                        return Ok(());
                                    }
                                    push_attempt(format!("{}::{}(u) @ {}: no effect", s.iface, s.name, s.path));
                                }
                                Err(e) => push_attempt(format!("{}::{}(u) @ {}: {e}", s.iface, s.name, s.path)),
                            }
                        }
                        "d" => {
                            tried = true;
                            let n = s.name.to_ascii_lowercase();
                            let vals: Vec<f64> = if n.contains("percent") || n.contains("percentage") {
                                vec![pct_i32 as f64, ratio]
                            } else {
                                vec![ratio, pct_i32 as f64]
                            };
                            for v in vals {
                                match proxy.call::<_, _, ()>(s.name.as_str(), &(v)).await {
                                    Ok(()) => {
                                        if verify_effect(pct_i32, &before_backlight).await {
                                            return Ok(());
                                        }
                                        push_attempt(format!("{}::{}(d={v}) @ {}: no effect", s.iface, s.name, s.path));
                                    }
                                    Err(e) => push_attempt(format!("{}::{}(d={v}) @ {}: {e}", s.iface, s.name, s.path)),
                                }
                            }
                        }
                        other => {
                            push_attempt(format!("{}::{}({other}) @ {}: unsupported", s.iface, s.name, s.path));
                        }
                    }
                    if !tried {
                        continue;
                    }
                }
                DbusSetterKind::Property => {
                    let props_proxy = match PropertiesProxy::builder(&conn)
                        .destination(s.destination.as_str())
                        .and_then(|b| b.path(s.path.as_str()))
                    {
                        Ok(b) => match b.build().await {
                            Ok(p) => p,
                            Err(e) => {
                                push_attempt(format!("PropertiesProxy build @ {}: {e}", s.path));
                                continue;
                            }
                        },
                        Err(e) => {
                            push_attempt(format!("PropertiesProxy builder @ {}: {e}", s.path));
                            continue;
                        }
                    };

                    let iface_name = match InterfaceName::try_from(s.iface.as_str()) {
                        Ok(v) => v,
                        Err(_) => {
                            push_attempt(format!("Properties.Set {} @ {}: invalid iface {}", s.name, s.path, s.iface));
                            continue;
                        }
                    };

                    let val = match s.arg_ty.as_str() {
                        "y" => Value::from(pct_u8),
                        "n" => Value::from(pct_i32 as i16),
                        "q" => Value::from(pct_i32 as u16),
                        "i" => Value::from(pct_i32),
                        "u" => Value::from(pct_u32),
                        "d" => Value::from(ratio),
                        other => {
                            push_attempt(format!("Properties.Set {} @ {}: unsupported type {other}", s.name, s.path));
                            continue;
                        }
                    };

                    match props_proxy.set(iface_name, s.name.as_str(), &val).await {
                        Ok(()) => {
                            if verify_effect(pct_i32, &before_backlight).await {
                                return Ok(());
                            }
                            push_attempt(format!("Properties.Set {} via {} @ {}: no effect", s.name, s.iface, s.path));
                        }
                        Err(e) => push_attempt(format!("Properties.Set {} via {} @ {}: {e}", s.name, s.iface, s.path)),
                    }
                }
            }
        }

        if let (Some(before), Ok(after)) = (before_pct, Self::gsd_power_get_percentage().await) {
            if (after - before).abs() <= 0 && (after - pct_i32).abs() > 1 {
                push_attempt(format!(
                    "gsd-power reported {before}% before and {after}% after; desired {pct_i32}% (no change)"
                ));
            }
        }

        bail!(
            "failed to call gsd-power SetPercentage; attempts: {}",
            attempts.join(" | ")
        )
    }

    fn backlight_snapshot_values() -> Vec<(String, u32)> {
        let base = Path::new("/sys/class/backlight");
        let Ok(entries) = fs::read_dir(base) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name() {
                Some(v) => v.to_string_lossy().to_string(),
                None => continue,
            };
            if let Some(cur) = Self::read_backlight_current(&path) {
                out.push((name, cur));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn approx_contains(list: &[f64], value: f64) -> bool {
        list.iter().any(|v| (*v - value).abs() < 0.01)
    }

    fn approx_intersection(a: &[f64], b: &[f64]) -> Vec<f64> {
        a.iter()
            .copied()
            .filter(|x| b.iter().any(|y| (*y - *x).abs() < 0.01))
            .collect()
    }

    fn fmt_scales(scales: &[f64]) -> String {
        let mut s = scales.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        s.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        s.iter()
            .map(|v| format!("{:.2}", v))
            .collect::<Vec<_>>()
            .join(", ")
    }

    #[cfg(feature = "backend-gnome")]
    fn supported_scales_for_logical_monitor(
        monitors: &[MonitorInfo],
        lm: &LogicalMonitorInfo,
    ) -> Option<Vec<f64>> {
        let mut supported_intersection: Option<Vec<f64>> = None;

        for (spec, mode_id, _p) in &lm.monitors {
            let mon = monitors.iter().find(|m| m.spec == *spec)?;
            let mm = mon.modes.iter().find(|m| m.id == *mode_id)?;

            let mut scales = if mm.supported_scales.is_empty() {
                vec![mm.preferred_scale]
            } else {
                mm.supported_scales.clone()
            };
            scales.retain(|v| *v > 0.0);
            if scales.is_empty() {
                scales.push(1.0);
            }

            supported_intersection = Some(match supported_intersection {
                None => scales,
                Some(prev) => Self::approx_intersection(&prev, &scales),
            });
        }

        let mut out = supported_intersection?;
        out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        if out.is_empty() { None } else { Some(out) }
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
    ) -> Result<(
        u32,
        Vec<MutterMonitorState>,
        Vec<MutterLogicalMonitorState>,
        HashMap<String, OwnedValue>,
    )> {
        let proxy = Self::mutter_proxy(conn).await?;
        let reply: (
            u32,
            Vec<MutterMonitorState>,
            Vec<MutterLogicalMonitorState>,
            HashMap<String, OwnedValue>,
        ) = proxy
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
            // This legacy parser is only used for fallback decoding paths; scales are not exposed
            // in this variant, so keep safe defaults.
            preferred_scale: 1.0,
            supported_scales: Vec::new(),
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

        Ok(LogicalMonitorInfo {
            x,
            y,
            scale,
            transform,
            primary,
            props: HashMap::new(),
            monitors,
        })
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
    fn build_apply_args_v1(
        serial: u32,
        method: u32,
        logicals: Vec<LogicalMonitorInfo>,
        global_props: HashMap<String, OwnedValue>,
    ) -> Result<(u32, u32, Vec<LogicalMonitorConfigV1>, HashMap<String, OwnedValue>)> {

        // Build strongly-typed GVariant args to match Mutter's expected signature.
        let mut lm_values: Vec<LogicalMonitorConfigV1> = Vec::with_capacity(logicals.len());

        for lm in logicals {
            let mut monitors_arr: Vec<MonitorConfigV1> = Vec::with_capacity(lm.monitors.len());
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

        Ok((serial, method, lm_values, global_props))
    }

    #[cfg(feature = "backend-gnome")]
    fn build_apply_args_v2(
        serial: u32,
        method: u32,
        logicals: Vec<LogicalMonitorInfo>,
        global_props: HashMap<String, OwnedValue>,
    ) -> Result<(u32, u32, Vec<LogicalMonitorConfigV2>, HashMap<String, OwnedValue>)> {
        let mut lm_values: Vec<LogicalMonitorConfigV2> = Vec::with_capacity(logicals.len());
        for lm in logicals {
            let mut specs: Vec<MonitorSpecTuple> = Vec::with_capacity(lm.monitors.len());
            for (spec, _mode_id, _props) in lm.monitors {
                specs.push((spec.connector, spec.vendor, spec.product, spec.serial));
            }
            // In this signature variant, per-logical-monitor properties are a{sv}.
            // Preserve what Mutter returned, since some versions require specific keys.
            lm_values.push((lm.x, lm.y, lm.scale, lm.transform, lm.primary, specs, lm.props));
        }

        Ok((serial, method, lm_values, global_props))
    }

    #[cfg(feature = "backend-gnome")]
    fn build_apply_args_v0(
        serial: u32,
        method: u32,
        logicals: Vec<LogicalMonitorInfo>,
        global_props: HashMap<String, OwnedValue>,
    ) -> Result<(u32, u32, Vec<LogicalMonitorConfigV0>, HashMap<String, OwnedValue>)> {
        let mut lm_values: Vec<LogicalMonitorConfigV0> = Vec::with_capacity(logicals.len());
        for lm in logicals {
            let mut monitors_arr: Vec<MonitorConfigV0> = Vec::with_capacity(lm.monitors.len());
            for (spec, mode_id, props) in lm.monitors {
                // connector + mode id + properties
                monitors_arr.push((spec.connector, mode_id, props));
            }
            lm_values.push((lm.x, lm.y, lm.scale, lm.transform, lm.primary, monitors_arr));
        }
        Ok((serial, method, lm_values, global_props))
    }

    #[cfg(feature = "backend-gnome")]
    async fn apply_logical_monitors(
        serial: u32,
        logicals: Vec<LogicalMonitorInfo>,
        global_props: HashMap<String, OwnedValue>,
    ) -> Result<()> {
        let conn = Connection::session().await?;
        let proxy = Self::mutter_proxy(&conn).await?;

        fn props_variants(mut props: HashMap<String, OwnedValue>) -> Vec<HashMap<String, OwnedValue>> {
            let mut out = Vec::new();
            out.push(props.clone());
            if props.remove("layout-mode").is_some() {
                out.push(props);
            }
            out.push(HashMap::new());
            out
        }

        // Mutter's ApplyMonitorsConfig signature differs across versions.
        // We try multiple known signatures in a best-effort order.
        let serial0 = serial;
        let logicals0 = logicals.clone();
        let props0 = global_props.clone();
        let props_candidates = props_variants(props0);

        {
            let mut last_err: Option<zbus::Error> = None;
            for method in [2u32, 1u32] {
                for props in &props_candidates {
                    let (serial, method, logical_values, props) =
                        Self::build_apply_args_v2(serial0, method, logicals0.clone(), props.clone())?;
                    debug!(serial, method, logical_count = logical_values.len(), "Applying Mutter monitor config (v2)");
                    match proxy
                        .call::<_, _, ()>("ApplyMonitorsConfig", &(serial, method, logical_values, props))
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(err) => {
                            debug!(?err, method, "ApplyMonitorsConfig(v2) failed");
                            last_err = Some(err);
                        }
                    }
                }
            }
            if let Some(err) = last_err {
                debug!(?err, "ApplyMonitorsConfig(v2) failed for all methods; trying v1");
            }
        }

        {
            let mut last_err: Option<zbus::Error> = None;
            for method in [2u32, 1u32] {
                for props in &props_candidates {
                    let (serial, method, logical_values, props) =
                        Self::build_apply_args_v1(serial0, method, logicals.clone(), props.clone())?;
                    debug!(serial, method, logical_count = logical_values.len(), "Applying Mutter monitor config (v1)");
                    match proxy
                        .call::<_, _, ()>("ApplyMonitorsConfig", &(serial, method, logical_values, props))
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(err) => {
                            debug!(?err, method, "ApplyMonitorsConfig(v1) failed");
                            last_err = Some(err);
                        }
                    }
                }
            }
            if let Some(err) = last_err {
                debug!(?err, "ApplyMonitorsConfig(v1) failed for all methods; trying v0");
            }
        }

        // v0: a(iiduba(ssa{sv}))
        {
            let mut last_err: Option<zbus::Error> = None;
            for method in [2u32, 1u32] {
                for props in &props_candidates {
                    let (serial, method, logical_values, props) =
                        Self::build_apply_args_v0(serial0, method, logicals.clone(), props.clone())?;
                    debug!(serial, method, logical_count = logical_values.len(), "Applying Mutter monitor config (v0)");
                    match proxy
                        .call::<_, _, ()>("ApplyMonitorsConfig", &(serial, method, logical_values, props))
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(err) => {
                            debug!(?err, method, "ApplyMonitorsConfig(v0) failed");
                            last_err = Some(err);
                        }
                    }
                }
            }
            if let Some(err) = last_err {
                return Err(anyhow::anyhow!(
                    "failed to call Mutter ApplyMonitorsConfig (v0/v1/v2 all failed): {err}"
                ));
            }
        }

        Ok(())
    }

    #[cfg(feature = "backend-gnome")]
    async fn load_state() -> Result<(
        u32,
        Vec<MonitorInfo>,
        Vec<LogicalMonitorInfo>,
        HashMap<String, OwnedValue>,
    )> {
        let conn = Connection::session().await?;
        let (serial, monitors_raw, logicals_raw, _props) = Self::get_current_state(&conn).await?;

        let mut monitors: Vec<MonitorInfo> = Vec::with_capacity(monitors_raw.len());
        for (spec_tuple, mode_states, props) in monitors_raw {
            let spec = MonitorSpec {
                connector: spec_tuple.0,
                vendor: spec_tuple.1,
                product: spec_tuple.2,
                serial: spec_tuple.3,
            };

            let mut modes: Vec<MonitorMode> = Vec::with_capacity(mode_states.len());
            for (id, width, height, refresh, _preferred_scale, _supported_scales, mode_props) in
                mode_states
            {
                let current = match mode_props.get("is-current") {
                    Some(v) => Self::ov_to_bool(v).unwrap_or(false),
                    None => false,
                };
                let preferred = match mode_props.get("is-preferred") {
                    Some(v) => Self::ov_to_bool(v).unwrap_or(false),
                    None => false,
                };
                modes.push(MonitorMode {
                    id,
                    mode: DisplayMode::new(width.max(0) as u32, height.max(0) as u32, {
                        if refresh > 0.0 {
                            Some(refresh as f32)
                        } else {
                            None
                        }
                    }),
                    preferred,
                    current,
                    preferred_scale: _preferred_scale,
                    supported_scales: _supported_scales,
                });
            }

            monitors.push(MonitorInfo {
                spec,
                modes,
                _props: props,
            });
        }

        // Helper: choose a mode id for a monitor (used when Mutter doesn't provide it
        // in logical monitor entries for this version).
        fn pick_mode_id(mi: &MonitorInfo) -> Option<String> {
            mi.modes
                .iter()
                .find(|m| m.current)
                .or_else(|| mi.modes.iter().find(|m| m.preferred))
                .or_else(|| mi.modes.first())
                .map(|m| m.id.clone())
        }

        let mut logicals: Vec<LogicalMonitorInfo> = Vec::with_capacity(logicals_raw.len());
        for (x, y, scale, transform, primary, monitor_specs, lm_props) in logicals_raw {
            let mut monitors_cfg: Vec<(MonitorSpec, String, HashMap<String, OwnedValue>)> =
                Vec::with_capacity(monitor_specs.len());

            for spec_tuple in monitor_specs {
                let spec = MonitorSpec {
                    connector: spec_tuple.0,
                    vendor: spec_tuple.1,
                    product: spec_tuple.2,
                    serial: spec_tuple.3,
                };
                let mode_id = monitors
                    .iter()
                    .find(|m| m.spec == spec)
                    .and_then(pick_mode_id)
                    .unwrap_or_default();
                monitors_cfg.push((spec, mode_id, HashMap::new()));
            }

            logicals.push(LogicalMonitorInfo {
                x,
                y,
                scale,
                transform,
                primary,
                props: lm_props,
                monitors: monitors_cfg,
            });
        }

        Ok((serial, monitors, logicals, _props))
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
            let (_serial, monitors, logicals, _props) = Self::load_state().await?;
            let persisted = Self::load_persisted_state();
            let gamma_supported = Self::mutter_gamma_available().await;
            let ddc_available = Self::ddcutil_is_available();
            let backlight_ratio = Self::backlight_get_ratio();
            let gsd_brightness = Self::gsd_power_get_percentage().await.ok().map(|p| (p as f32 / 100.0).clamp(0.0, 1.0));

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
                    out.available_scales = Self::supported_scales_for_logical_monitor(&monitors, lm);
                    out.position = Some((lm.x, lm.y));
                    // Map Mutter transform (0-7) back to string
                    out.transform = Some(match lm.transform {
                        0 => "normal".to_string(),
                        1 => "90".to_string(),
                        2 => "180".to_string(),
                        3 => "270".to_string(),
                        4 => "flipped".to_string(),
                        5 => "flipped-90".to_string(),
                        6 => "flipped-180".to_string(),
                        7 => "flipped-270".to_string(),
                        _ => "normal".to_string(),
                    });

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
                    out.available_scales = None;
                    out.current_mode = None;
                }

                out.available_modes = mon.modes.iter().map(|m| m.mode.clone()).collect();

                let internal = Self::connector_looks_internal(&name);
                let ddc_bus = if !internal && ddc_available {
                    Self::find_ddc_i2c_bus_for_connector(&name)
                } else {
                    None
                };

                let brightness_value = if internal {
                    gsd_brightness.or(backlight_ratio)
                } else if let Some(bus) = ddc_bus {
                    // Best-effort: cache to avoid calling ddcutil every refresh.
                    let ttl = StdDuration::from_secs(5);
                    if let Some(v) = Self::cached_ddc_brightness(&name, ttl) {
                        Some(v)
                    } else {
                        match Self::ddc_get_brightness_ratio(bus) {
                            Ok(v) => {
                                Self::set_cached_ddc_brightness(&name, v);
                                Some(v)
                            }
                            Err(err) => {
                                debug!(?err, bus, output = %name, "ddcutil getvcp failed");
                                persisted
                                    .outputs
                                    .get(&name)
                                    .and_then(|e| e.brightness)
                            }
                        }
                    }
                } else {
                    None
                };

                let (gamma_value, temp_value) = if gamma_supported {
                    let entry = persisted.outputs.get(&name);
                    (entry.and_then(|e| e.gamma), entry.and_then(|e| e.temperature))
                } else {
                    (None, None)
                };
                out.color = DisplayColorSettings {
                    brightness: brightness_value,
                    gamma: gamma_value,
                    temperature: temp_value,
                };
                out.color_caps = DisplayColorCapabilities {
                    brightness: if internal {
                        brightness_value.is_some()
                    } else {
                        ddc_available && ddc_bus.is_some()
                    },
                    gamma: gamma_supported,
                    temperature: gamma_supported,
                };

                outputs.push(out);
            }

            Ok(outputs)
        }
    }

    async fn set_brightness(&self, output: &str, value: f32) -> Result<()> {
        #[cfg(feature = "backend-gnome")]
        {
            let clamped = value.clamp(0.0, 1.0);

            if Self::connector_looks_internal(output) {
                // Internal panel: use GNOME power slider/backlight (global semantics).
                let pct = (clamped * 100.0).round() as i32;
                if let Err(gsd_err) = Self::gsd_power_set_percentage(pct).await {
                    debug!(?gsd_err, "gsd-power SetPercentage failed or had no effect; falling back to backlight");
                    if let Err(backlight_err) = Self::set_backlight_ratio(clamped).await {
                        return Err(anyhow!(
                            "failed to set brightness: gsd-power failed ({gsd_err}); backlight failed ({backlight_err})"
                        ));
                    }
                }
                return Ok(());
            }

            // External monitor: try hardware DDC/CI.
            if !Self::ddcutil_is_available() {
                bail!("ddcutil is not available; install it to control external monitor brightness on GNOME");
            }
            let bus = Self::find_ddc_i2c_bus_for_connector(output)
                .ok_or_else(|| anyhow!("failed to locate DDC/CI i2c bus for output {output} under /sys/class/drm"))?;

            Self::ddc_set_brightness_ratio(bus, clamped)
                .with_context(|| format!("failed to set DDC/CI brightness for {output} (i2c bus {bus})"))?;

            // Verify it actually took effect (some displays accept but ignore VCP writes).
            sleep(Duration::from_millis(80)).await;
            if let Ok(after) = Self::ddc_get_brightness_ratio(bus) {
                if (after - clamped).abs() > 0.08 {
                    bail!(
                        "DDC/CI brightness write did not take effect for {output}: requested {:.0}%, observed {:.0}%",
                        clamped * 100.0,
                        after * 100.0
                    );
                }
                Self::set_cached_ddc_brightness(output, after);
            } else {
                // If we can't read back, keep the requested value as best-effort.
                Self::set_cached_ddc_brightness(output, clamped);
            }

            let mut state = Self::load_persisted_state();
            state
                .outputs
                .entry(output.to_string())
                .or_default()
                .brightness = Some(clamped);
            let _ = Self::save_persisted_state(&state);

            Ok(())
        }
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = output;
            Self::set_backlight_ratio(value).await
        }
    }

    async fn set_gamma(&self, output: &str, value: f32) -> Result<()> {
        #[cfg(feature = "backend-gnome")]
        {
            if !Self::mutter_gamma_available().await {
                bail!("Gamma control is not supported on this GNOME/Mutter version (SetCrtcGamma unavailable)");
            }
            let gamma = value.max(0.1);

            let mut state = Self::load_persisted_state();
            state
                .outputs
                .entry(output.to_string())
                .or_default()
                .gamma = Some(gamma);
            let _ = Self::save_persisted_state(&state);

            Self::apply_persisted_color_for_output(output).await
        }
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, value);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }
    }

    async fn set_temperature(&self, output: &str, value: u16) -> Result<()> {
        #[cfg(feature = "backend-gnome")]
        {
            if !Self::mutter_gamma_available().await {
                bail!("Per-output color temperature is not supported on this GNOME/Mutter version (SetCrtcGamma unavailable)");
            }
            let temp = value.clamp(1000, 10000);

            let mut state = Self::load_persisted_state();
            state
                .outputs
                .entry(output.to_string())
                .or_default()
                .temperature = Some(temp);
            let _ = Self::save_persisted_state(&state);

            Self::apply_persisted_color_for_output(output).await
        }
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, value);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }
    }

    async fn set_mode(&self, output: &str, mode: DisplayMode) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, mode);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, monitors, mut logicals, props) = Self::load_state().await?;

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

            Self::apply_logical_monitors(serial, logicals, props).await
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
            let (serial, monitors, mut logicals, props) = Self::load_state().await?;

            let Some((idx, _lm)) = Self::find_output_logical(&logicals, output) else {
                bail!("Output {output} is disabled; enable it before setting scale");
            };

            // Validate scale against supported scales for the active mode(s) in this logical monitor.
            let lm = &logicals[idx];

            let mut supported_intersection: Option<Vec<f64>> = None;
            let mut any_mode: Option<(u32, u32)> = None;

            for (spec, mode_id, _p) in &lm.monitors {
                let Some(mon) = monitors.iter().find(|m| m.spec == *spec) else { continue };
                let Some(mm) = mon.modes.iter().find(|m| m.id == *mode_id) else { continue };
                any_mode = Some((mm.mode.width, mm.mode.height));
                let scales = if mm.supported_scales.is_empty() {
                    vec![mm.preferred_scale].into_iter().filter(|v| *v > 0.0).collect::<Vec<_>>()
                } else {
                    mm.supported_scales.clone()
                };
                supported_intersection = Some(match supported_intersection {
                    None => scales,
                    Some(prev) => Self::approx_intersection(&prev, &scales),
                });
            }

            if let Some(scales) = supported_intersection {
                if !scales.is_empty() && !Self::approx_contains(&scales, scale) {
                    let (w, h) = any_mode.unwrap_or((0, 0));
                    bail!(
                        "Scale {scale:.2} is not valid for resolution {w}x{h}. Supported scales: {} (GNOME may require enabling Fractional Scaling for non-integer values)",
                        Self::fmt_scales(&scales)
                    );
                }
            }

            logicals[idx].scale = scale;
            Self::apply_logical_monitors(serial, logicals, props).await
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
            let (serial, monitors, mut logicals, props) = Self::load_state().await?;

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
                return Self::apply_logical_monitors(serial, logicals, props).await;
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
                props: HashMap::new(),
                monitors: vec![(mon.spec.clone(), mode.id.clone(), HashMap::new())],
            };

            logicals.push(new_lm);
            Self::apply_logical_monitors(serial, logicals, props).await
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
            let (serial, _monitors, mut logicals, props) = Self::load_state().await?;

            let Some((idx, _lm)) = Self::find_output_logical(&logicals, output) else {
                bail!("Output {output} is disabled; enable it before setting position");
            };

            logicals[idx].x = x;
            logicals[idx].y = y;
            Self::apply_logical_monitors(serial, logicals, props).await
        }
    }

    async fn set_position_relative(&self, output: &str, direction: RelativePosition) -> Result<()> {
        #[cfg(not(feature = "backend-gnome"))]
        {
            let _ = (output, direction);
            bail!("GNOME backend is disabled at compile time (enable feature backend-gnome)");
        }

        #[cfg(feature = "backend-gnome")]
        {
            let (serial, monitors, mut logicals, props) = Self::load_state().await?;

            let (out_idx, lm) = Self::find_output_logical(&logicals, output)
                .ok_or_else(|| anyhow!("Output {output} is disabled"))?;

            let (relative_to, dir_str) = match &direction {
                RelativePosition::LeftOf(s) => (s, "left"),
                RelativePosition::RightOf(s) => (s, "right"),
                RelativePosition::Above(s) => (s, "above"),
                RelativePosition::Below(s) => (s, "below"),
            };

            let (_rel_idx, rel_lm) = Self::find_output_logical(&logicals, relative_to)
                .ok_or_else(|| anyhow!("Output {relative_to} is disabled"))?;

            // Estimate widths/heights by the first monitor in each logical monitor.
            let out_size = lm
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

            let (new_x, new_y) = match dir_str {
                "left" => (rel_lm.x.saturating_sub(out_size.0), rel_lm.y),
                "right" => (rel_lm.x.saturating_add(rel_size.0), rel_lm.y),
                "above" => (rel_lm.x, rel_lm.y.saturating_sub(out_size.1)),
                "below" => (rel_lm.x, rel_lm.y.saturating_add(rel_size.1)),
                _ => unreachable!(),
            };

            logicals[out_idx].x = new_x;
            logicals[out_idx].y = new_y;
            Self::apply_logical_monitors(serial, logicals, props).await
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
            let (serial, _monitors, mut logicals, props) = Self::load_state().await?;

            let Some((idx, _lm)) = Self::find_output_logical(&logicals, output) else {
                bail!("Output {output} is disabled; enable it before setting transform");
            };

            logicals[idx].transform = GnomeBackend::transform_to_mutter(transform)?;
            Self::apply_logical_monitors(serial, logicals, props).await
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
            let (serial, monitors, mut logicals, props) = Self::load_state().await?;

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
                props: HashMap::new(),
                monitors: vec![
                    (src.spec.clone(), src_mode_id.clone(), HashMap::new()),
                    (dst.spec.clone(), dst_mode_id.clone(), HashMap::new()),
                ],
            };
            logicals.push(mirror_lm);

            GnomeBackend::apply_logical_monitors(serial, logicals, props).await
        }
    }

    async fn set_adaptive_sync(&self, _output: &str, _enabled: bool) -> Result<()> {
        // Mutter's public DisplayConfig API does not expose VRR toggles.
        Err(anyhow!("Adaptive sync control is not supported on GNOME/Mutter"))
    }
}
