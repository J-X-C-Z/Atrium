// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! The capability records a provider session records when it is attached.
//!
//! The UI reads these to show only supported controls. `runtimePolicy` is
//! defaulted on decode: events stored before it existed replay with the
//! weaker client-boundary guarantee, exactly as T3 does.

use serde::{Deserialize, Serialize};

use super::base::{CommandId, NonNegativeInt, RunId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimePolicyEnforcement {
    Native,
    /// The default for sessions recorded before `runtimePolicy` existed.
    #[default]
    ClientBoundary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicyCapabilities {
    pub enforcement: RuntimePolicyEnforcement,
}

impl Default for RuntimePolicyCapabilities {
    fn default() -> Self {
        Self {
            enforcement: RuntimePolicyEnforcement::ClientBoundary,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    pub supports_multiple_provider_threads_per_session: bool,
    pub supports_model_switch_in_session: bool,
    pub supports_provider_switching_via_handoff: bool,
    pub supports_runtime_mode_switch_in_session: bool,
    pub pending_requests_survive_restart: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCapabilities {
    pub can_create_empty_thread: bool,
    pub can_read_thread_snapshot: bool,
    pub can_rollback_thread: bool,
    pub can_fork_thread: bool,
    pub can_fork_from_turn: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_fork_while_source_running: Option<bool>,
    pub can_fork_from_subagent_thread: bool,
    pub exposes_native_thread_id: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStatusQuality {
    Strong,
    Weak,
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_native_goals: Option<bool>,
    pub exposes_native_turn_id: bool,
    pub emits_turn_started: bool,
    pub emits_turn_completed: bool,
    pub supports_interrupt: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_native_resume: Option<bool>,
    pub supports_active_steering: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_interrupts_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_accepts_attachments: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_during_compaction: Option<bool>,
    pub supports_steering_by_interrupt_restart: bool,
    pub supports_queued_messages: bool,
    pub terminal_status_quality: TerminalStatusQuality,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamingCapabilities {
    pub streams_assistant_text: bool,
    pub streams_reasoning: bool,
    pub streams_tool_output: bool,
    pub streams_plan_text: bool,
    pub emits_message_completed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapabilities {
    pub exposes_tool_item_ids: bool,
    pub emits_tool_started: bool,
    pub emits_tool_completed: bool,
    pub emits_tool_output: bool,
    pub supports_mcp_tools: bool,
    pub supports_dynamic_tool_callbacks: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCapabilities {
    pub supports_command_approval: bool,
    pub supports_file_read_approval: bool,
    pub supports_file_change_approval: bool,
    pub supports_apply_patch_approval: bool,
    pub approvals_have_native_request_ids: bool,
    pub approval_callbacks_are_live_only: bool,
    pub approvals_can_originate_from_subagents: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningCapabilities {
    pub emits_plan_updated: bool,
    pub emits_todo_list: bool,
    pub emits_proposed_plan: bool,
    pub supports_structured_questions: bool,
    pub plan_deltas_have_item_ids: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCapabilities {
    pub supports_subagents: bool,
    pub exposes_subagent_thread_ids: bool,
    pub emits_subagent_lifecycle: bool,
    pub can_wait_for_subagents: bool,
    pub can_close_subagents: bool,
    pub can_fork_subagent_thread: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCapabilities {
    pub accepts_system_context: bool,
    pub accepts_developer_context: bool,
    pub accepts_synthetic_user_context: bool,
    pub can_generate_summaries: bool,
    pub can_consume_handoff_summaries: bool,
    pub supports_delta_handoff: bool,
    pub supports_full_thread_handoff: bool,
    pub max_recommended_handoff_chars: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointCapabilities {
    pub app_can_checkpoint_filesystem: bool,
    pub supports_nested_checkpoint_scopes: bool,
    pub provider_can_rollback_conversation: bool,
    pub provider_rollback_returns_snapshot: bool,
    pub provider_can_read_conversation_snapshot: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeRefStrength {
    Strong,
    Weak,
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCapabilities {
    pub native_thread_ids: NativeRefStrength,
    pub native_turn_ids: NativeRefStrength,
    pub native_item_ids: NativeRefStrength,
    pub native_request_ids: NativeRefStrength,
}

/// T3's `OrchestrationV2ProviderCapabilities`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub sessions: SessionCapabilities,
    pub threads: ThreadCapabilities,
    pub turns: TurnCapabilities,
    pub streaming: StreamingCapabilities,
    pub tools: ToolCapabilities,
    pub approvals: ApprovalCapabilities,
    pub planning: PlanningCapabilities,
    pub subagents: SubagentCapabilities,
    pub context: ContextCapabilities,
    pub checkpointing: CheckpointCapabilities,
    pub identity: IdentityCapabilities,
    /// Replay defaults old events to the weaker client-boundary guarantee.
    #[serde(default)]
    pub runtime_policy: RuntimePolicyCapabilities,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitRecovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<CommandId>,
    pub run_id: RunId,
    pub reset_at: String,
    pub auto_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<bool>,
}

/// A recovery update must change `autoResume` or `snooze`; a no-op is
/// rejected before the command reaches the store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitRecoveryUpdate {
    pub run_id: RunId,
    pub reset_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<bool>,
}

impl LimitRecoveryUpdate {
    pub fn is_valid(&self) -> bool {
        self.auto_resume.is_some() || self.snooze.is_some()
    }
}

/// T3's `OrchestrationV2ThreadOpens` (epoch-millisecond ranking arithmetic).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadOpens {
    pub opened_at: f64,
    pub score: f64,
    pub picks: Vec<ThreadOpenPick>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadOpenPick {
    pub query: String,
    pub at: f64,
}

/// T3's `OrchestrationV2WorkflowAgent` state, reused by the run-attempt
/// background-work notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowAgentState {
    Queued,
    Running,
    Completed,
    Failed,
}

/// Non-negative counters are `NonNegativeInt` on the wire.
pub type TokenCount = NonNegativeInt;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Ported from packages/contracts/src/orchestrationV2.test.ts.
    fn legacy_capabilities() -> serde_json::Value {
        json!({
            "sessions": {
                "supportsMultipleProviderThreadsPerSession": false,
                "supportsModelSwitchInSession": false,
                "supportsProviderSwitchingViaHandoff": true,
                "supportsRuntimeModeSwitchInSession": false,
                "pendingRequestsSurviveRestart": false
            },
            "threads": {
                "canCreateEmptyThread": true,
                "canReadThreadSnapshot": false,
                "canRollbackThread": true,
                "canForkThread": false,
                "canForkFromTurn": false,
                "canForkFromSubagentThread": false,
                "exposesNativeThreadId": true
            },
            "turns": {
                "exposesNativeTurnId": false,
                "emitsTurnStarted": true,
                "emitsTurnCompleted": true,
                "supportsInterrupt": true,
                "supportsActiveSteering": false,
                "supportsSteeringByInterruptRestart": true,
                "supportsQueuedMessages": true,
                "terminalStatusQuality": "strong"
            },
            "streaming": {
                "streamsAssistantText": true,
                "streamsReasoning": true,
                "streamsToolOutput": true,
                "streamsPlanText": false,
                "emitsMessageCompleted": true
            },
            "tools": {
                "exposesToolItemIds": true,
                "emitsToolStarted": true,
                "emitsToolCompleted": true,
                "emitsToolOutput": true,
                "supportsMcpTools": false,
                "supportsDynamicToolCallbacks": false
            },
            "approvals": {
                "supportsCommandApproval": true,
                "supportsFileReadApproval": true,
                "supportsFileChangeApproval": true,
                "supportsApplyPatchApproval": false,
                "approvalsHaveNativeRequestIds": false,
                "approvalCallbacksAreLiveOnly": true,
                "approvalsCanOriginateFromSubagents": false
            },
            "planning": {
                "emitsPlanUpdated": true,
                "emitsTodoList": true,
                "emitsProposedPlan": false,
                "supportsStructuredQuestions": true,
                "planDeltasHaveItemIds": false
            },
            "subagents": {
                "supportsSubagents": false,
                "exposesSubagentThreadIds": false,
                "emitsSubagentLifecycle": false,
                "canWaitForSubagents": false,
                "canCloseSubagents": false,
                "canForkSubagentThread": false
            },
            "context": {
                "acceptsSystemContext": false,
                "acceptsDeveloperContext": false,
                "acceptsSyntheticUserContext": true,
                "canGenerateSummaries": true,
                "canConsumeHandoffSummaries": true,
                "supportsDeltaHandoff": true,
                "supportsFullThreadHandoff": true,
                "maxRecommendedHandoffChars": null
            },
            "checkpointing": {
                "appCanCheckpointFilesystem": true,
                "supportsNestedCheckpointScopes": true,
                "providerCanRollbackConversation": true,
                "providerRollbackReturnsSnapshot": true,
                "providerCanReadConversationSnapshot": false
            },
            "identity": {
                "nativeThreadIds": "strong",
                "nativeTurnIds": "weak",
                "nativeItemIds": "weak",
                "nativeRequestIds": "weak"
            }
        })
    }

    #[test]
    fn decodes_persisted_capability_snapshots_that_predate_runtime_policy() {
        let decoded: ProviderCapabilities = serde_json::from_value(legacy_capabilities()).unwrap();
        assert_eq!(
            decoded.runtime_policy,
            RuntimePolicyCapabilities {
                enforcement: RuntimePolicyEnforcement::ClientBoundary
            }
        );

        let mut explicit = legacy_capabilities();
        explicit["runtimePolicy"] = json!({ "enforcement": "native" });
        let decoded: ProviderCapabilities = serde_json::from_value(explicit).unwrap();
        assert_eq!(
            decoded.runtime_policy,
            RuntimePolicyCapabilities {
                enforcement: RuntimePolicyEnforcement::Native
            }
        );
    }
}
