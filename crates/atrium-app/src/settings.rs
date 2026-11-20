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
    /// Launch configurations explicitly approved on this device, keyed by project.
    pub preview_commands:
        std::collections::HashMap<PathBuf, atrium_project::preview::PreviewCommands>,
    /// The project the user was in, to come back to it.
    pub active_project: Option<PathBuf>,
    /// Nodes added by address, besides those found on the tailnet.
    pub nodes: Vec<String>,
    /// The bearer token nodes ask for, when they are set to.
    pub node_token: Option<String>,
    /// Names this install to nodes, so it finds its own processes.
    pub install_id: String,
    /// Named sets of environment variables, such as "staging".
    pub environments: Vec<SavedEnvironment>,
    /// The environment new sessions get, by name.
    pub environment: Option<String>,
    /// MCP servers every agent gets, Codex and ACP alike.
    pub mcp_servers: Vec<SharedMcp>,
    /// How much of each turn the chat shows.
    pub transcript_mode: TranscriptMode,
    /// The access mode last used in each folder, to come back to it.
    pub folder_modes: std::collections::HashMap<PathBuf, AccessMode>,
    /// No system notifications when a conversation you are not looking at
    /// finishes, fails or needs you.
    pub quiet_notifications: bool,
    /// Serve project snapshots to this user on loopback and the tailnet.
    pub remote_view: bool,
    /// Optional ntfy topic name or self-hosted topic URL.
    pub push_topic: Option<String>,
    /// The editor files open in, by its launcher (`code`, `cursor`, `zed`…);
    /// the first installed one when unset.
    pub editor: Option<String>,
    /// New chats in a git repository start in a worktree of their own.
    pub chat_worktrees: bool,
    /// The command the Run view starts, by folder.
    pub run_commands: std::collections::HashMap<String, String>,
    /// Prompts Atrium sends by itself at set times.
    pub schedules: Vec<crate::schedule::Schedule>,
    /// Do not look for a newer Atrium.
    pub no_update_check: bool,
    /// The interface's language.
    pub language: crate::i18n::Language,
    /// Where new chats run on each device, by device id.
    pub device_folders: std::collections::HashMap<String, String>,
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
        // Each folder keeps the access mode last chosen in it.
        if let Some(mode) = self.folder_modes.get(&dir) {
            self.mode = *mode;
        }
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
        assert!(settings.preview_commands.is_empty());
        assert!(!settings.remote_view);
        assert!(settings.push_topic.is_none());
    }
}

/// How much of each turn the chat shows: answers with folded details, the
/// reasoning opened, or everything opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptMode {
    #[default]
    Normal,
    Thinking,
    Verbose,
}

impl TranscriptMode {
    pub fn next(self) -> Self {
        match self {
            Self::Normal => Self::Thinking,
            Self::Thinking => Self::Verbose,
            Self::Verbose => Self::Normal,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal view",
            Self::Thinking => "Thinking view",
            Self::Verbose => "Verbose view",
        }
    }
}

/// A named set of environment variables new sessions can run with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedEnvironment {
    pub name: String,
    pub vars: Vec<(String, String)>,
}

/// An MCP server configured once in Atrium for every agent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedMcp {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Kept but not handed to agents.
    pub off: bool,
}

impl SharedMcp {
    /// Reads `name` and a command line (`npx -y @scope/server --flag`),
    /// split on spaces with quotes kept together.
    pub fn parse(name: &str, line: &str) -> Option<Self> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut quote: Option<char> = None;
        for c in line.trim().chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (None, '"' | '\'') => quote = Some(c),
                (None, c) if c.is_whitespace() => {
                    if !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                }
                (_, c) => word.push(c),
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
        let name: String = name
            .trim()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if name.is_empty() || words.is_empty() {
            return None;
        }
        Some(Self {
            name,
            command: words.remove(0),
            args: words,
            ..Self::default()
        })
    }
}

/// `KEY=value` lines as variables; blank lines and `#` comments skipped.
pub fn parse_vars(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let v = v.trim().trim_matches('"').trim_matches('\'');
            (!k.trim().is_empty()).then(|| (k.trim().to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn mcp_lines_and_variables_parse() {
        let mcp = SharedMcp::parse("my docs", "npx -y \"@acme/docs server\" --port 3").unwrap();
        assert_eq!(mcp.name, "my-docs");
        assert_eq!(mcp.command, "npx");
        assert_eq!(mcp.args, vec!["-y", "@acme/docs server", "--port", "3"]);
        assert!(SharedMcp::parse("x", "  ").is_none());
        assert_eq!(
            parse_vars(
                "# staging\nAPI_URL=https://s.example.com\nexport TOKEN_NAME='ci'\nnot a var\n"
            ),
            vec![
                ("API_URL".to_string(), "https://s.example.com".to_string()),
                ("TOKEN_NAME".to_string(), "ci".to_string())
            ]
        );
    }
}
