use std::env;

use anyhow::{bail, Result};
use async_trait::async_trait;
use tokio::runtime::Runtime;
use tracing::info;

use crate::cli::BackendSelector;
use crate::models::{DisplayMode, DisplayOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    X11,
    Wlroots,
}

impl BackendKind {
    pub fn auto_detect() -> Self {
        let wayland_display = env::var("WAYLAND_DISPLAY").ok();

        if wayland_display.is_some() {
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
        }
    }

    pub fn fetch_outputs(&self, kind: BackendKind) -> Result<Vec<DisplayOutput>> {
        let backend = self.get_backend(kind)?;
        self.runtime
            .block_on(async move { backend.list_outputs().await })
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

pub mod x11;
pub mod wlroots;
