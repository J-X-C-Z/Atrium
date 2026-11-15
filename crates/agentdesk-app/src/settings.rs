//! What AgentDesk remembers between launches. Kept small and human-editable:
//! `~/Library/Application Support/AgentDesk/settings.json` on macOS.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where new chats run.
    pub cwd: Option<PathBuf>,
    /// The model for new turns; Codex's default when unset.
    pub model: Option<String>,
    /// The `codex` binary; found on PATH when unset.
    pub codex_binary: Option<PathBuf>,
    /// Codex approval policy for new chats: `untrusted`, `on-request` or `never`.
    pub approval_policy: Option<String>,
    /// Codex sandbox for new chats: `read-only`, `workspace-write` or `danger-full-access`.
    pub sandbox: Option<String>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("AgentDesk").join("settings.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                log::warn!(
                    "settings: {} is not valid ({error}); using defaults",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| {
                let text = serde_json::to_string_pretty(self).expect("settings serialize");
                std::fs::write(&path, text)
            });
        if let Err(error) = result {
            log::warn!("settings: could not save {}: {error}", path.display());
        }
    }

    /// The folder new chats start in: the saved one if it still exists, else home.
    pub fn working_dir(&self) -> PathBuf {
        self.cwd
            .clone()
            .filter(|dir| dir.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"))
    }
}
