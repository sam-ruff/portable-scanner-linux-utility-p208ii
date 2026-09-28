//! Optional receipt naming. AI failures never stop the scan worker.

mod files;
mod provider;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use files::{Job, process};
pub use provider::{CliReader, Provider, ProviderKind, discover};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Analysis {
    pub merchant: String,
    pub date: Option<String>,
    pub total: Option<String>,
    pub currency: Option<String>,
    pub confidence: f64,
    pub blank_sides: Vec<bool>,
}

#[cfg_attr(test, mockall::automock)]
pub trait ReceiptReader {
    fn analyse(&self, images: Vec<PathBuf>) -> Result<Analysis, String>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub enabled: bool,
    pub provider: Option<ProviderKind>,
}

impl Preferences {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("p208ii/preferences.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let parent = path.parent().ok_or("invalid preferences path")?;
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|err| err.to_string())?;
        serde_json::to_writer_pretty(&mut temporary, self).map_err(|err| err.to_string())?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|err| err.to_string())?;
        temporary.persist(path).map_err(|err| err.to_string())?;
        Ok(())
    }
}
