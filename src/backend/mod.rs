use std::env;

use anyhow::{bail, Result};
use async_trait::async_trait;
use tokio::runtime::Runtime;
use tokio::time::{timeout, Duration};
use tracing::info;

use crate::cli::BackendSelector;
use crate::models::{DisplayMode, DisplayOutput};

#[cfg(feature = "backend-gnome")]
use zbus::fdo::DBusProxy;
#[cfg(feature = "backend-gnome")]
use zbus::names::BusName;
#[cfg(feature = "backend-gnome")]
use zbus::Connection;

#[cfg(feature = "backend-gnome")]
const MUTTER_DISPLAYCONFIG_BUS: &str = "org.gnome.Mutter.DisplayConfig";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    X11,
    Wlroots,
    Gnome,
    Kde,
}

impl BackendKind {
    #[cfg(feature = "backend-gnome")]
    async fn mutter_displayconfig_available() -> bool {
        let Ok(conn) = Connection::session().await else {
            return false;
        };
        let Ok(proxy) = DBusProxy::new(&conn).await else {
            return false;
        };
        let Ok(bus) = BusName::try_from(MUTTER_DISPLAYCONFIG_BUS) else {
            return false;
        };
        proxy.name_has_owner(bus).await.unwrap_or(false)
    }

    #[cfg(feature = "backend-gnome")]
    fn mutter_displayconfig_available_blocking() -> bool {
        // Autodetection runs before we necessarily have a runtime; create a tiny one.
        // Keep this fast and bounded to avoid hanging on broken D-Bus sessions.
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return false;
        };

        rt.block_on(async {
            timeout(Duration::from_millis(250), Self::mutter_displayconfig_available())
                .await
                .unwrap_or(false)
        })
    }

    pub fn auto_detect() -> Self {
        let wayland_display = env::var("WAYLAND_DISPLAY").ok();

        if wayland_display.is_some() {
            // Prefer Mutter D-Bus backend whenever it is actually available.
            // This is more robust than trusting XDG_* env vars (which are often missing in
            // non-interactive shells / systemd user services).
            #[cfg(feature = "backend-gnome")]
            {
                if Self::mutter_displayconfig_available_blocking() {
                    return BackendKind::Gnome;
                }
            }

            // Fallback heuristic: if env vars clearly indicate GNOME, pick it even if the D-Bus
            // check failed (e.g. transient bus startup).
            let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
            let session_desktop = env::var("XDG_SESSION_DESKTOP").unwrap_or_default();
            let desktop_session = env::var("DESKTOP_SESSION").unwrap_or_default();
            let gnomeish = [
                desktop.as_str(),
                session_desktop.as_str(),
                desktop_session.as_str(),
            ]
            .iter()
            .flat_map(|v| v.split(':'))
            .any(|part| part.eq_ignore_ascii_case("GNOME") || part.eq_ignore_ascii_case("ubuntu"));

            if cfg!(feature = "backend-gnome") && gnomeish {
                return BackendKind::Gnome;
            }

            let kdeish = [
                desktop.as_str(),
                session_desktop.as_str(),
                desktop_session.as_str(),
            ]
            .iter()
            .flat_map(|v| v.split(':'))
            .any(|part| part.eq_ignore_ascii_case("KDE"));

            if cfg!(feature = "backend-kde") && kdeish {
                return BackendKind::Kde;
            }

            BackendKind::Wlroots
        } else {
            BackendKind::X11
        }
    }
}

impl From<BackendSelector> for BackendKind {
    fn from(value: BackendSelector) -> Self {
        match value {
            BackendSelector::X11 => BackendKind::X11,
            BackendSelector::Wlroots => BackendKind::Wlroots,
            BackendSelector::Gnome => BackendKind::Gnome,
            BackendSelector::Kde => BackendKind::Kde,
        }
    }
}

#[async_trait]
pub trait DisplayBackend: Send + Sync {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>>;
    async fn set_mode(&self, _output: &str, _mode: DisplayMode) -> Result<()> {
        bail!("set_mode not implemented for this backend")
    }
    async fn set_scale(&self, _output: &str, _scale: f64) -> Result<()> {
        bail!("set_scale not implemented for this backend")
    }
    async fn set_brightness(&self, _output: &str, _value: f32) -> Result<()> {
        bail!("set_brightness not implemented for this backend")
    }
    async fn set_gamma(&self, _output: &str, _value: f32) -> Result<()> {
        bail!("set_gamma not implemented for this backend")
    }
    async fn set_temperature(&self, _output: &str, _value: u16) -> Result<()> {
        bail!("set_temperature not implemented for this backend")
    }
    async fn set_enabled(&self, _output: &str, _enabled: bool) -> Result<()> {
        bail!("set_enabled not implemented for this backend")
    }
    async fn set_position(&self, _output: &str, _x: i32, _y: i32) -> Result<()> {
        bail!("set_position not implemented for this backend")
    }
    async fn set_position_relative(&self, _output: &str, _relative_to: &str, _direction: &str) -> Result<()> {
        bail!("set_position_relative not implemented for this backend")
    }
    async fn set_transform(&self, _output: &str, _transform: &str) -> Result<()> {
        bail!("set_transform not implemented for this backend")
    }
    async fn set_mirror(&self, _output: &str, _target: &str) -> Result<()> {
        bail!("set_mirror not implemented for this backend")
    }
    async fn set_adaptive_sync(&self, _output: &str, _enabled: bool) -> Result<()> {
        bail!("set_adaptive_sync not implemented for this backend")
    }
}

pub struct BackendRegistry {
    pub(crate) runtime: Runtime,
}

impl Default for BackendRegistry {
    fn default() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        Self { runtime }
    }
}

impl BackendRegistry {
    pub(crate) fn get_backend(&self, kind: BackendKind) -> Result<Box<dyn DisplayBackend>> {
        info!(?kind, "Selecting backend");
        match kind {
            BackendKind::X11 => Ok(Box::new(crate::backend::x11::X11Backend::new()?)),
            BackendKind::Wlroots => Ok(Box::new(crate::backend::wlroots::WlrootsBackend::new()?)),
            BackendKind::Gnome => Ok(Box::new(crate::backend::gnome::GnomeBackend::new()?)),
            BackendKind::Kde => Ok(Box::new(crate::backend::kde::KdeBackend::new()?)),
        }
    }

    pub fn fetch_outputs(&self, kind: BackendKind) -> Result<Vec<DisplayOutput>> {
        let backend = self.get_backend(kind)?;
        let result = self.runtime.block_on(async move { backend.list_outputs().await });

        // GNOME Wayland (Mutter) does NOT support wlr-output-management. If we guessed wlroots
        // anyway, fall back to the Mutter D-Bus backend when it is available.
        if kind == BackendKind::Wlroots {
            if let Err(err) = &result {
                let msg = err.to_string();
                if msg.contains("wlr-output-management-unstable-v1")
                {
                    #[cfg(feature = "backend-gnome")]
                    {
                        let available = self.runtime.block_on(async {
                            timeout(Duration::from_millis(250), BackendKind::mutter_displayconfig_available())
                                .await
                                .unwrap_or(false)
                        });
                        if available {
                            info!("wlroots backend unsupported on this compositor; falling back to GNOME backend");
                            let gnome = self.get_backend(BackendKind::Gnome)?;
                            return self.runtime.block_on(async move { gnome.list_outputs().await });
                        }
                    }
                }
            }
        }

        result
    }

    pub fn dispatch_list(&self, kind: BackendKind) -> Result<()> {
        let outputs = self.fetch_outputs(kind)?;
        for output in outputs {
            println!("{}", output);
        }
        Ok(())
    }

    pub fn execute_apply(
        &self,
        kind: BackendKind,
        output: &str,
        mode: Option<DisplayMode>,
        scale: Option<f64>,
        brightness: Option<f32>,
        gamma: Option<f32>,
        temperature: Option<u16>,
        enabled: Option<bool>,
    ) -> Result<()> {
        let backend = self.get_backend(kind)?;
        self.runtime.block_on(async move {
            if let Some(enabled) = enabled {
                backend.set_enabled(output, enabled).await?;
            }
            if let Some(mode) = mode {
                backend.set_mode(output, mode).await?;
            }
            if let Some(scale) = scale {
                backend.set_scale(output, scale).await?;
            }
            if let Some(brightness) = brightness {
                backend.set_brightness(output, brightness).await?;
            }
            if let Some(gamma) = gamma {
                backend.set_gamma(output, gamma).await?;
            }
            if let Some(temp) = temperature {
                backend.set_temperature(output, temp).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }

    pub fn execute_position_relative(&self, kind: BackendKind, output: &str, relative_to: &str, direction: &str) -> Result<()> {
        let backend = self.get_backend(kind)?;
        self.runtime.block_on(async move {
            backend.set_position_relative(output, relative_to, direction).await
        })
    }

    pub fn execute_position(&self, kind: BackendKind, output: &str, x: i32, y: i32) -> Result<()> {
        let backend = self.get_backend(kind)?;
        self.runtime.block_on(async move { backend.set_position(output, x, y).await })
    }

    pub fn execute_transform(&self, kind: BackendKind, output: &str, transform: &str) -> Result<()> {
        let backend = self.get_backend(kind)?;
        self.runtime.block_on(async move {
            backend.set_transform(output, transform).await
        })
    }

    pub fn execute_mirror(&self, kind: BackendKind, output: &str, target: &str) -> Result<()> {
        let backend = self.get_backend(kind)?;
        self.runtime.block_on(async move {
            backend.set_mirror(output, target).await
        })
    }

    pub fn execute_adaptive_sync(&self, kind: BackendKind, output: &str, enabled: bool) -> Result<()> {
        let backend = self.get_backend(kind)?;
        self.runtime.block_on(async move {
            backend.set_adaptive_sync(output, enabled).await
        })
    }
}

pub mod gnome;
pub mod x11;
pub mod wlroots;
pub mod kde;
