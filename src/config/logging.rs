use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::Config;

fn def_enabled() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Logging {
    // when true, every error is appended (in clear text) to the log file
    // in addition to the popup shown in the UI
    #[serde(default = "def_enabled")]
    pub enabled: bool,

    // path to the log file. if left empty it defaults to
    // <data_dir>/error.log (~/.local/share/mal-tui/error.log).
    // a leading `~` is expanded to the user's home directory.
    #[serde(default)]
    pub path: String,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            enabled: def_enabled(),
            path: String::new(),
        }
    }
}

impl Logging {
    // resolve the configured path into an absolute file path, falling back
    // to <data_dir>/error.log when no path is set
    pub fn resolve_path(&self) -> PathBuf {
        let trimmed = self.path.trim();
        if trimmed.is_empty() {
            return Config::data_dir().join("error.log");
        }

        if let Some(rest) = trimmed.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }

        PathBuf::from(trimmed)
    }
}
