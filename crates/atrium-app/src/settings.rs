//! What Atrium remembers between launches. Kept small and human-editable:
//! `~/Library/Application Support/Atrium/settings.json` on macOS.

use std::path::PathBuf;

use codex_client::AccessMode;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where new chats run.
    pub cwd: Option<PathBuf>,
    /// Folders chats ran in lately, newest first.
    pub recent_folders: Vec<PathBuf>,
    /// The model for new turns; Codex's default when unset.
    pub model: Option<String>,
    /// Reasoning effort for new turns; the model's default when unset.
    pub effort: Option<String>,
    /// How much Codex may do without asking.
    pub mode: AccessMode,
    /// The `codex` binary; found on PATH when unset.
    pub codex_binary: Option<PathBuf>,
    pub theme: ThemePref,
    /// Chats pinned to the top of the sidebar.
    pub pinned: Vec<String>,
    /// Whether the sidebar is folded to its icon rail.
    pub sidebar_collapsed: bool,
    /// Ask for the Fast service tier (about twice the speed, more usage).
    pub fast: bool,
    /// Plan mode: Codex proposes a plan and waits before it changes anything.
    pub plan_mode: bool,
    /// Do not guess the next message after each answer.
    pub no_predictions: bool,
    /// The agent new chats use: `codex`, `claude`, `deepseek`, `antigravity`.
    pub default_agent: Option<String>,
    /// Commands that start ACP agents, by agent id, overriding detection,
    /// such as `{"deepseek": "/opt/dsh --profile acp"}`.
    pub agent_commands: std::collections::HashMap<String, String>,
    /// Agent sessions the user archived (ACP has no archive of its own).
    pub hidden_sessions: Vec<String>,
    /// Project folders, in the order they were added.
    pub projects: Vec<PathBuf>,
    /// The project the user was in, to come back to it.
    pub active_project: Option<PathBuf>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("Atrium").join("settings.json"))
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

    /// Makes `dir` the working folder and the first of the recent ones.
    pub fn use_folder(&mut self, dir: PathBuf) {
        self.recent_folders.retain(|recent| *recent != dir);
        self.recent_folders.insert(0, dir.clone());
        self.recent_folders.truncate(6);
        self.cwd = Some(dir);
    }

    pub fn is_pinned(&self, thread_id: &str) -> bool {
        self.pinned.iter().any(|id| id == thread_id)
    }

    pub fn toggle_pin(&mut self, thread_id: &str) {
        if self.is_pinned(thread_id) {
            self.pinned.retain(|id| id != thread_id);
        } else {
            self.pinned.insert(0, thread_id.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_folders_stay_unique_and_short() {
        let mut settings = Settings::default();
        for ix in 0..8 {
            settings.use_folder(PathBuf::from(format!("/p{ix}")));
        }
        settings.use_folder(PathBuf::from("/p3"));
        assert_eq!(settings.recent_folders.len(), 6);
        assert_eq!(settings.recent_folders[0], PathBuf::from("/p3"));
        assert_eq!(
            settings
                .recent_folders
                .iter()
                .filter(|p| p.as_path() == std::path::Path::new("/p3"))
                .count(),
            1
        );
    }

    #[test]
    fn old_settings_files_still_load() {
        let settings: Settings = serde_json::from_str(r#"{"cwd": "/x", "model": "m"}"#).unwrap();
        assert_eq!(settings.mode, AccessMode::Auto);
        assert_eq!(settings.theme, ThemePref::System);
    }
}
