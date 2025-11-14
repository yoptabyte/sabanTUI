use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::models::DisplayOutput;

pub struct X11Backend;

impl X11Backend {
    pub fn new() -> Result<Self> {
        bail!("backend-x11 feature not implemented")
    }
}

#[async_trait]
impl crate::backend::DisplayBackend for X11Backend {
    async fn list_outputs(&self) -> Result<Vec<DisplayOutput>> {
        bail!("backend-x11 feature not implemented")
    }
}
