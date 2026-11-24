// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! Checkpoint scopes and captured checkpoint records.
//!
//! These are the records the checkpoint services read and write. The wire field
//! names and union tags match T3's `OrchestrationV2CheckpointScope` and
//! `OrchestrationV2Checkpoint`.

use serde::{Deserialize, Serialize};

use super::base::{
    CheckpointId, CheckpointRef, CheckpointScopeId, NodeId, NonNegativeInt, ProviderThreadId,
    RunId, ThreadId,
};
use super::item::{CheckpointFileSummary, CheckpointFileTotals};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointScopeKind {
    RootRun,
    Subagent,
    Tool,
    ProviderThread,
    Manual,
}

/// T3's `OrchestrationV2CheckpointScope`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointScope {
    pub id: CheckpointScopeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_scope_id: Option<CheckpointScopeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub kind: CheckpointScopeKind,
    pub ordinal_within_parent: NonNegativeInt,
    pub advances_app_run_count: bool,
    pub cwd: String,
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckpointStatus {
    Ready,
    Missing,
    Error,
    Stale,
}

/// T3's `OrchestrationV2Checkpoint`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub thread_id: ThreadId,
    pub scope_id: CheckpointScopeId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_checkpoint_id: Option<CheckpointId>,
    pub ordinal_within_scope: NonNegativeInt,
    pub app_run_ordinal: Option<u32>,
    #[serde(rename = "ref")]
    pub checkpoint_ref: CheckpointRef,
    pub status: CheckpointStatus,
    pub files: Vec<CheckpointFileSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_total: Option<CheckpointFileTotals>,
    pub captured_at: String,
}
