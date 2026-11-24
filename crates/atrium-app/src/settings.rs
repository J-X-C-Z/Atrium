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

/// A model and reasoning effort; each the default when unset.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelChoice {
    pub model: Option<String>,
    pub effort: Option<String>,
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
    /// The model and effort for Codex on each other device, by device id:
    /// each device's Codex lists its own models.
    pub device_models: std::collections::HashMap<String, ModelChoice>,
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
    /// Prompts Atrium sends by itself at set times, after an interval, or once.
    pub schedules: Vec<crate::schedule::Schedule>,
    /// Do not look for a newer Atrium.
    pub no_update_check: bool,
    /// The interface's language.
    pub language: crate::i18n::Language,
    /// Workflow run folders, newest first.
    pub workflow_runs: Vec<PathBuf>,
    /// Template versions whose folder commands the user has approved.
    pub workflow_confirmed: std::collections::HashMap<String, u32>,
    /// Where new chats run on each device, by device id.
    pub device_folders: std::collections::HashMap<String, String>,
    /// Whether this install shares its overlay and conversations with the
    /// other devices of the same Tailscale owner.
    pub sync_enabled: Option<bool>,
    /// What other devices call this one.
    pub device_name: String,
    /// Tokens from pairing codes, by the address of the device that gave them.
    pub paired: std::collections::HashMap<String, String>,
    /// This computer's Atrium Workspace folder; `~/Atrium Workspace` when unset.
    pub workspace_dir: Option<PathBuf>,
    /// Other computers' Atrium Workspace folders, by device id, when their
    /// node's roots do not name one.
    pub device_workspaces: std::collections::HashMap<String, String>,
    /// What the user calls each other device, by device id (a 备注名).
    pub device_names: std::collections::HashMap<String, String>,
    /// The team member each project stream runs as when it stands in for
    /// another, by project root and stream id. App state, so the badge
    /// survives a restart without changing `.atrium/`.
    pub team_stand_ins:
        std::collections::HashMap<PathBuf, std::collections::HashMap<String, String>>,
    /// Where this state is saved; the usual place when unset. Kept out of
    /// the file itself; tests point it at a temporary folder.
    #[serde(skip)]
    pub(crate) save_path: Option<PathBuf>,
}

impl Settings {
    /// What to call `device`: the name given to it, else its id.
    pub fn device_label(&self, device: &str) -> String {
        match self.device_names.get(device).map(|n| n.trim()) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => device.to_string(),
        }
    }

    /// The member `stream` in `root` stands in for, if any.
    pub fn stand_in(&self, root: &std::path::Path, stream: &str) -> Option<&str> {
        self.team_stand_ins
            .get(root)
            .and_then(|streams| streams.get(stream))
            .map(String::as_str)
    }

    /// Records (or with `None`, forgets) the member `stream` stands in for.
    pub fn set_stand_in(&mut self, root: &std::path::Path, stream: &str, member: Option<&str>) {
        match member {
            Some(member) => {
                self.team_stand_ins
                    .entry(root.to_path_buf())
                    .or_default()
                    .insert(stream.to_string(), member.to_string());
            }
            None => {
                if let Some(streams) = self.team_stand_ins.get_mut(root) {
                    streams.remove(stream);
                    if streams.is_empty() {
                        self.team_stand_ins.remove(root);
                    }
                }
            }
        }
    }

    /// Names `device`; an empty name clears it.
    pub fn set_device_alias(&mut self, device: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.device_names.remove(device);
        } else {
            self.device_names
                .insert(device.to_string(), name.to_string());
        }
    }

    fn path(&self) -> Option<PathBuf> {
        self.save_path
            .clone()
            .or_else(|| Some(dirs::config_dir()?.join("Atrium").join("settings.json")))
    }

    pub fn load() -> Self {
        let Some(path) = dirs::config_dir().map(|dir| dir.join("Atrium").join("settings.json"))
        else {
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
        let Some(path) = self.path() else { return };
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
    /// This computer's Atrium Workspace folder, where spread projects live.
    pub fn atrium_workspace(&self) -> PathBuf {
        self.workspace_dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(atrium_project::spread::WORKSPACE_DIR)
        })
    }

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

    pub fn sync_enabled(&self) -> bool {
        self.sync_enabled.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn devices_show_their_alias_or_id() {
        let mut settings = super::Settings::default();
        assert_eq!(settings.device_label("jxcz-pc"), "jxcz-pc");
        settings.set_device_alias("jxcz-pc", "  Office PC ");
        assert_eq!(settings.device_label("jxcz-pc"), "Office PC");
        settings.set_device_alias("jxcz-pc", "  ");
        assert_eq!(settings.device_label("jxcz-pc"), "jxcz-pc");
    }

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
    fn stand_in_marks_survive_saving_and_reloading() {
        let root = std::path::Path::new("/projects/api");
        let mut settings = Settings::default();
        settings.set_stand_in(root, "web-1", Some("gemini"));
        settings.set_stand_in(root, "web-2", Some("claude"));
        // The app state file is JSON; the badge reads this back after a restart.
        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.stand_in(root, "web-1"), Some("gemini"));
        assert_eq!(back.stand_in(root, "web-2"), Some("claude"));
        assert_eq!(back.stand_in(root, "web-3"), None);
        settings.set_stand_in(root, "web-1", None);
        assert_eq!(settings.stand_in(root, "web-1"), None);
        assert_eq!(settings.stand_in(root, "web-2"), Some("claude"));
        // Forgetting the last stream also drops the empty project entry.
        settings.set_stand_in(root, "web-2", None);
        assert!(!settings.team_stand_ins.contains_key(root));
    }

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
