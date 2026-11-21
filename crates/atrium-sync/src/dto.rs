use std::collections::BTreeMap;

use atrium_core::{ApprovalRequest, Item, ThreadMeta, UserInputRequest};
use serde::{Deserialize, Serialize};

/// `GET /v1/atrium`: what a node knows about the Atrium that embeds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDirectory {
    pub install_id: String,
    pub device_name: String,
    pub os: String,
    pub app_version: String,
    pub sync_version: u32,
    /// The Tailscale login that owns the device, when known.
    #[serde(default)]
    pub owner: String,
}

/// One conversation as another device lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadAnnouncement {
    pub id: String,
    pub backend: String,
    pub name: Option<String>,
    pub preview: String,
    pub cwd: String,
    pub model: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
    /// The install id whose agent holds the session.
    pub owner: String,
    /// `idle`, `running`, `waiting_for_approval`, `waiting_for_answer`.
    pub state: String,
}

impl ThreadAnnouncement {
    pub fn from_meta(meta: ThreadMeta, owner: impl Into<String>, state: impl Into<String>) -> Self {
        Self {
            id: meta.id,
            backend: meta.backend,
            name: meta.name,
            preview: meta.preview,
            cwd: meta.cwd,
            model: meta.model,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            owner: owner.into(),
            state: state.into(),
        }
    }

    pub fn into_meta(self) -> ThreadMeta {
        ThreadMeta {
            id: self.id,
            backend: self.backend,
            name: self.name,
            preview: self.preview,
            cwd: self.cwd,
            model: self.model,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// The owner's transcript at one cursor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadSnapshot {
    pub thread: ThreadAnnouncement,
    pub items: Vec<Item>,
    #[serde(default)]
    pub approvals: Vec<ApprovalRequest>,
    #[serde(default)]
    pub questions: Vec<UserInputRequest>,
    /// Bumped by the owner whenever its transcript changes.
    pub cursor: u64,
}

/// Everything needed to continue one conversation on another computer.
///
/// The session and git bundle are base64 so the package can cross the node
/// as one JSON answer. The owner keeps the originals until the target
/// confirms it applied them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MovePackage {
    pub thread: ThreadAnnouncement,
    pub items: Vec<Item>,
    #[serde(default)]
    pub approvals: Vec<ApprovalRequest>,
    #[serde(default)]
    pub questions: Vec<UserInputRequest>,
    /// Relative to the agent's session directory, such as
    /// `2026/11/21/rollout-….jsonl.zst`.
    #[serde(default)]
    pub session_relative: Option<String>,
    #[serde(default)]
    pub session_base64: Option<String>,
    /// The clone-stable project identity, when the folder is a git repo.
    #[serde(default)]
    pub git_identity: Option<String>,
    #[serde(default)]
    pub git_bundle_base64: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub git_commit: Option<String>,
}

/// One change after a cursor. `rebuild` means the follower must take a new
/// snapshot: its cursor is older than what the owner still keeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadEvent {
    pub cursor: u64,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub rebuild: bool,
}

/// A follower's turn, sent to the owner to execute.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnInput {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
}

/// A follower's approval decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalAnswer {
    /// `accept`, `accept_for_session` or `decline`.
    pub decision: String,
}

/// A follower's answers to an agent's questions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnswerInput {
    pub answers: BTreeMap<String, Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_announcement_round_trips_its_meta() {
        let meta = ThreadMeta {
            id: "t".into(),
            backend: "codex".into(),
            name: Some("Title".into()),
            preview: "hello".into(),
            cwd: "/code".into(),
            model: None,
            created_at: 1,
            updated_at: 2,
        };
        let announcement = ThreadAnnouncement::from_meta(meta.clone(), "me", "running");
        assert_eq!(announcement.clone().into_meta(), meta);
        assert_eq!(announcement.owner, "me");
    }
}
