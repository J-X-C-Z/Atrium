// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! Context transfers: how a fork or handoff carries a conversation forward.
//!
//! The JSON names and tags match T3's `OrchestrationV2ContextTransfer`. A fork
//! plans one transfer; the target run consumes it later.

use serde::{Deserialize, Serialize};

use super::base::{
    Actor, CheckpointId, ContextHandoffId, ContextTransferId, NonNegativeInt, PositiveInt,
    ProviderInstanceId, RunId, ThreadId, TurnItemId,
};
use super::provider::ProviderRef;

/// T3's `OrchestrationV2ContextSourcePoint`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourcePoint {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_item_id: Option<TurnItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thread_ref: Option<ProviderRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_ref: Option<ProviderRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextTransferType {
    Fork,
    ProviderHandoff,
    MergeBack,
    SubagentSpawn,
    SubagentResult,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextTransferStatus {
    Pending,
    ResolvedNative,
    ResolvedPortable,
    Failed,
    Consumed,
    Superseded,
}

/// T3's `OrchestrationV2ContextTransferResolution`; each arm is tagged by
/// `strategy`, and the strategy names use T3's snake_case values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "strategy",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ContextTransferResolution {
    NativeFork {
        provider_thread_ref: ProviderRef,
    },
    PortableContext {
        context_handoff_id: ContextHandoffId,
    },
    DeltaContext {
        context_handoff_id: ContextHandoffId,
    },
    ForkDeltaContext {
        context_handoff_id: ContextHandoffId,
    },
    CheckpointContext {
        context_handoff_id: ContextHandoffId,
    },
}

/// T3's `OrchestrationV2ProviderTurnTokenUsage`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurnTokenUsage {
    pub used_tokens: NonNegativeInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<Option<NonNegativeInt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_ttl_ms: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_expires_at: Option<String>,
    pub updated_at: String,
}

/// T3's `OrchestrationV2ContextTransfer`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransfer {
    pub id: ContextTransferId,
    #[serde(rename = "type")]
    pub transfer_type: ContextTransferType,
    pub source_thread_id: ThreadId,
    pub target_thread_id: ThreadId,
    pub source_point: ContextSourcePoint,
    pub base_point: Option<ContextSourcePoint>,
    pub source_provider_instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_token_usage: Option<ProviderTurnTokenUsage>,
    pub target_provider_instance_id: Option<ProviderInstanceId>,
    pub target_run_id: Option<RunId>,
    pub status: ContextTransferStatus,
    pub resolution: Option<ContextTransferResolution>,
    pub created_by: Actor,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub consumed_at: Option<String>,
}
