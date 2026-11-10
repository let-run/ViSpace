//! Per-inventory local API launch preference; independent of the HTTP runtime.
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ApiSettings {
    pub enabled: bool,
    pub port: u16,
}
impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8080,
        }
    }
}
impl ApiSettings {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = match crate::platform::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let settings: Self = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            settings.port >= 1024,
            "Saved API port must be between 1024 and 65535"
        );
        Ok(settings)
    }
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(self.port >= 1024, "API port must be between 1024 and 65535");
        crate::platform::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
