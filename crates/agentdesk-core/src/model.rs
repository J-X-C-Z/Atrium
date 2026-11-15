use serde::{Deserialize, Serialize};

/// The id of a request the backend made to us (such as an approval), echoed
/// back verbatim in the answer. JSON-RPC allows numbers or strings.
pub type RequestId = serde_json::Value;

/// A conversation as listed in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadMeta {
    pub id: String,
    /// User-facing title; falls back to `preview` when the backend has none.
    pub name: Option<String>,
    /// Usually the first user message.
    pub preview: String,
    pub cwd: String,
    pub model: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

impl ThreadMeta {
    pub fn title(&self) -> String {
        let pick = self
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(self.preview.trim());
        let line = pick.lines().next().unwrap_or("").trim();
        if line.is_empty() {
            "New chat".to_string()
        } else if line.chars().count() > 60 {
            let cut: String = line.chars().take(60).collect();
            format!("{cut}…")
        } else {
            line.to_string()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

/// Where one item of a turn stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Add,
    Delete,
    Update { move_to: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    pub diff: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStep {
    pub step: String,
    pub done: bool,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub total_tokens: u64,
    pub last_turn_tokens: u64,
    pub context_window: Option<u64>,
}

/// One thing that happened in a turn, normalized across backends.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    User {
        id: String,
        text: String,
    },
    Agent {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        text: String,
    },
    Command {
        id: String,
        command: String,
        cwd: String,
        status: ItemStatus,
        output: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    FileChange {
        id: String,
        changes: Vec<FileChange>,
        status: ItemStatus,
    },
    Tool {
        id: String,
        /// Such as `server.tool` for MCP.
        name: String,
        status: ItemStatus,
        arguments: String,
        result: String,
        duration_ms: Option<u64>,
    },
    WebSearch {
        id: String,
        query: String,
    },
    /// Something the backend reported that we do not draw specially yet.
    Other {
        id: String,
        label: String,
    },
}

impl Item {
    pub fn id(&self) -> &str {
        match self {
            Item::User { id, .. }
            | Item::Agent { id, .. }
            | Item::Reasoning { id, .. }
            | Item::Command { id, .. }
            | Item::FileChange { id, .. }
            | Item::Tool { id, .. }
            | Item::WebSearch { id, .. }
            | Item::Other { id, .. } => id,
        }
    }
}
