//! Atrium's shared memory, as files on disk that every agent and any
//! memory service can use.
//!
//! ```text
//! <root>/                      default: <config dir>/Atrium/memory
//!   config.json                Config: budget, Hindsight, what Atrium sends
//!   global/entries.json        what holds everywhere: about the user, lessons
//!   projects/<key>/entries.json  what holds in one project (key: project_key)
//!   projects/<key>/path        the project's folder
//!   projects/<key>/skills/     skills for that project only
//!   skills/<name>/SKILL.md     skills for every project (agentskills format)
//!   sessions/<thread>.jsonl    conversations, as Atrium exports them
//!   inbox/<ms>-<thread>.json   finished conversations waiting for review
//!   journal.jsonl              every change to the notes, for undo
//! ```
//!
//! Atrium writes `sessions/` and `inbox/`, and puts the notes and the skill
//! index at the start of every agent session. A memory service (such as a
//! Hermes-style curator) reads the inbox and changes notes only through
//! [`Store::apply`], so limits, duplicates, the safety scan and the journal
//! hold for every writer. `docs/memory-contract.md` describes it for
//! service authors.

pub mod hindsight;
pub mod inbox;
pub mod inject;
pub mod mcp;
pub mod scan;
pub mod session;
pub mod skills;
pub mod store;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use store::{Change, Entry, Landing, Op, OpError, Scope, State, Store, Target};

/// Format version of everything under the root.
pub const VERSION: u32 = 1;

/// Where the memory lives unless told otherwise.
pub fn default_root() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Atrium").join("memory"))
}

/// How the memory is used; Atrium writes it, services read it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Put the entries in use and the skill index into new agent sessions.
    pub inject: bool,
    /// At most this many tokens go into a session, all parts together.
    pub budget_tokens: usize,
    /// Learn from conversations: export them to `sessions/` and queue
    /// them in `inbox/` for a memory service (and Hindsight's `retain`).
    pub learn: bool,
    /// What a service learns waits for the user before agents see it.
    pub confirm_learned: bool,
    /// Ask the service to tidy every night: merge duplicates, archive what
    /// went stale. The user's own and pinned entries stay as they are.
    pub nightly_tidy: bool,
    /// A conversation is queued once it has been quiet this long.
    pub review_after_idle_min: u32,
    /// Agents (backend ids such as `codex`, `claude`) that get no memory.
    pub off_agents: Vec<String>,
    /// Search by meaning, through Hindsight.
    pub hindsight: Option<HindsightConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            inject: true,
            budget_tokens: 2000,
            learn: true,
            confirm_learned: false,
            nightly_tidy: false,
            review_after_idle_min: 10,
            off_agents: Vec::new(),
            hindsight: None,
        }
    }
}

/// A Hindsight server: conversations are retained there, and what it
/// recalls for a new conversation goes into the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HindsightConfig {
    pub enabled: bool,
    /// Such as `http://localhost:8888`.
    pub url: String,
    /// Sent as `Authorization: Bearer`, when the server asks for one.
    pub api_key: Option<String>,
    /// The global bank; a project's bank is `<bank>-<project key>`.
    pub bank: String,
    /// Give each project its own bank.
    pub project_banks: bool,
    /// At most this many tokens of recalled memories per session.
    pub recall_tokens: usize,
    /// Send finished conversations to `retain`.
    pub retain: bool,
}

impl Default for HindsightConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            url: "http://localhost:8888".into(),
            api_key: None,
            bank: "atrium".into(),
            project_banks: true,
            recall_tokens: 800,
            retain: true,
        }
    }
}

impl HindsightConfig {
    /// The banks a conversation in `project` reads and writes: the
    /// project's (when kept apart) and the global one.
    pub fn banks(&self, project: Option<&str>) -> Vec<String> {
        let mut banks = Vec::new();
        if self.project_banks
            && let Some(key) = project
        {
            banks.push(format!("{}-{key}", self.bank));
        }
        banks.push(self.bank.clone());
        banks
    }
}

impl Config {
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(root.join("config.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(root)?;
        store::write_atomic(
            &root.join("config.json"),
            &serde_json::to_string_pretty(self).unwrap_or_default(),
        )
    }

    /// Where entries a service or an agent adds go.
    pub fn landing(&self) -> Landing {
        if self.confirm_learned {
            Landing::Pending
        } else {
            Landing::Active
        }
    }

    /// The Hindsight server, when it is on.
    pub fn hindsight(&self) -> Option<&HindsightConfig> {
        self.hindsight
            .as_ref()
            .filter(|h| h.enabled && !h.url.trim().is_empty())
    }

    pub fn on_for(&self, agent: &str) -> bool {
        let base = agent.split('@').next().unwrap_or(agent);
        self.inject && !self.off_agents.iter().any(|a| a == base)
    }
}

/// The folder a working directory's project memory belongs to: the git
/// root above it, or the folder itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    let start = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut dir = start.as_path();
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return start,
        }
    }
}

/// A stable, readable key for a project folder: its name, lower-case,
/// and a short hash of its full path, such as `atrium-3f2a9c1d`.
pub fn project_key(root: &Path) -> String {
    let name: String = root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name = name.trim_matches('-');
    let name = if name.is_empty() { "project" } else { name };
    let hash = Sha256::digest(root.to_string_lossy().as_bytes());
    let short: String = hash.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{}-{short}", &name[..name.len().min(24)])
}

/// Tokens a text will take, roughly: a CJK character is about one, other
/// text about four characters to a token.
pub fn estimate_tokens(text: &str) -> usize {
    let mut wide = 0usize;
    let mut narrow = 0usize;
    for c in text.chars() {
        if is_wide(c) {
            wide += 1;
        } else {
            narrow += 1;
        }
    }
    wide + narrow.div_ceil(4)
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_keys_are_readable_and_stable() {
        let a = project_key(Path::new("/home/me/code/Atrium App"));
        assert!(a.starts_with("atrium-app-"), "{a}");
        assert_eq!(a, project_key(Path::new("/home/me/code/Atrium App")));
        assert_ne!(a, project_key(Path::new("/srv/Atrium App")));
    }

    #[test]
    fn the_git_root_names_the_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let deep = dir.path().join("src/ui");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(project_root(&deep), dir.path().canonicalize().unwrap());
    }

    #[test]
    fn chinese_counts_about_one_token_a_character() {
        assert_eq!(estimate_tokens("用中文写"), 4);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn hindsight_banks_follow_the_project() {
        let h = HindsightConfig::default();
        assert_eq!(h.banks(Some("app-1234")), ["atrium-app-1234", "atrium"]);
        assert_eq!(h.banks(None), ["atrium"]);
    }

    #[test]
    fn an_agent_can_be_left_out() {
        let config = Config {
            off_agents: vec!["claude".into()],
            ..Default::default()
        };
        assert!(config.on_for("codex"));
        assert!(!config.on_for("claude"));
        assert!(!config.on_for("claude@studio"));
    }
}
