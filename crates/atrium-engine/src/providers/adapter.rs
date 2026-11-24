// Ported from packages/provider-core/src/server/ProviderAdapter.ts and
// packages/provider-core/src/server/selectionTransition.ts
// (T3 Code v0.1.11, MIT). The Effect service and stream types are replaced
// by an ordinary synchronous Rust trait; the data records, their JSON field
// names and T3's error messages are kept.
//! The provider-adapter interface and the records that cross it.
//!
//! A provider adapter owns one configured provider instance (an instance id
//! plus a driver kind). Opening a session yields a
//! [`ProviderSessionRuntime`] for one provider session; threads, turns,
//! runtime requests and snapshots hang off that runtime. The engine (and,
//! through [`crate::runtime`], the app) only sees these records, never a
//! Codex or ACP protocol type.
//!
//! Everything here is GPUI-free and synchronous. The provider process runs on
//! its own threads inside the concrete adapter; events arrive through an
//! [`AdapterEventSink`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contracts::{
    ChatAttachment, CreationSource, MessageId, ModelSelection, ProviderApprovalDecision,
    ProviderCapabilities, ProviderInstanceId, ProviderInteractionMode, ProviderSession,
    ProviderSessionId, ProviderSessionStatus, ProviderThread, ProviderTurn, ProviderTurnId,
    RunAttemptId, RunId, RuntimeMode, RuntimeRequest, ScheduledTaskId, ThreadId,
};

/// T3's `ProviderDriverKind` for the Codex app-server.
pub const CODEX_DRIVER_KIND: &str = "codex";

/// T3's `ProviderAdapterV2RuntimePolicy` (`approvalPolicy` and
/// `sandboxPolicy` are provider-defined JSON, as in T3).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicy {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<Value>,
    /// The administrator's ceiling, when a managed policy sets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_mode: Option<RuntimeMode>,
}

/// T3's `ProviderAdapterV2TurnMessage`: the user's words plus who and what
/// produced them. `attachments` use the contract's chat-attachment union.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnMessage {
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub created_by: crate::contracts::Actor,
    pub creation_source: CreationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
}

impl TurnMessage {
    pub fn user(message_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            message_id: MessageId::new(message_id),
            text: text.into(),
            attachments: Vec::new(),
            created_by: crate::contracts::Actor::User,
            creation_source: CreationSource::Server,
            scheduled_task_id: None,
            sender_thread_id: None,
        }
    }
}

/// T3's `ProviderSelectionTransitionPlan`, with the same `type` tags.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SelectionTransitionPlan {
    /// The selection is applied by the next turn, which is what threading
    /// providers such as Codex do.
    ApplyOnNextTurn,
    RestartSession,
    CreateWithHandoff,
    Reject {
        reason: String,
    },
}

/// T3's `turnScopedSelectionTransition`.
pub fn turn_scoped_selection_transition() -> SelectionTransitionPlan {
    SelectionTransitionPlan::ApplyOnNextTurn
}

/// T3's `acpSelectionTransition`: a model change is refused when the active
/// ACP session did not advertise model switching.
pub fn acp_selection_transition(
    current: &ModelSelection,
    target: &ModelSelection,
    supports_model_switch_in_session: bool,
) -> SelectionTransitionPlan {
    if current.model != target.model && !supports_model_switch_in_session {
        return SelectionTransitionPlan::Reject {
            reason: "The active ACP session does not expose a model-switch capability.".into(),
        };
    }
    SelectionTransitionPlan::ApplyOnNextTurn
}

/// One adapter failure. Variants follow T3's tagged errors; the messages are
/// the ones T3 renders, because the app shows them as they are.
#[derive(Debug, thiserror::Error)]
pub enum ProviderAdapterError {
    #[error("Failed to read {driver} provider capabilities.")]
    Capabilities { driver: String },
    #[error("Failed to open {driver} provider session {provider_session_id}.")]
    OpenSession {
        driver: String,
        provider_session_id: String,
    },
    #[error("Failed to close {driver} provider session {provider_session_id}.")]
    CloseSession {
        driver: String,
        provider_session_id: String,
    },
    #[error("Failed to resume {driver} provider thread {provider_thread_id}.")]
    ResumeThread {
        driver: String,
        provider_thread_id: String,
    },
    #[error("{detail}")]
    ThreadInUse {
        driver: String,
        provider_thread_id: String,
        detail: String,
    },
    #[error("Failed to ensure {driver} provider thread for app thread {thread_id}.")]
    EnsureThread { driver: String, thread_id: String },
    #[error("Failed to read {driver} provider thread snapshot {provider_thread_id}.")]
    ReadThreadSnapshot {
        driver: String,
        provider_thread_id: String,
    },
    #[error("Failed to start run {run_id} on {driver} provider thread {provider_thread_id}.")]
    TurnStart {
        driver: String,
        thread_id: String,
        provider_thread_id: String,
        run_id: String,
    },
    #[error("{driver} provider thread {provider_thread_id} does not support active-run steering.")]
    SteerRunUnsupported {
        driver: String,
        provider_thread_id: String,
    },
    #[error(
        "Failed to steer active run {provider_turn_id} on {driver} provider thread {provider_thread_id}."
    )]
    SteerRun {
        driver: String,
        provider_thread_id: String,
        provider_turn_id: String,
    },
    #[error("Failed to interrupt {driver} provider turn {provider_turn_id}.")]
    Interrupt {
        driver: String,
        provider_thread_id: String,
        provider_turn_id: String,
    },
    #[error("Failed to respond to {driver} runtime request {request_id}.")]
    RuntimeRequestResponse { driver: String, request_id: String },
    #[error("{driver} provider protocol error: {detail}.")]
    Protocol { driver: String, detail: String },
    #[error("No running {driver} session owns this thread.")]
    SessionStopped { driver: String },
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
}

impl ProviderAdapterError {
    pub fn protocol(driver: &str, detail: impl Into<String>) -> Self {
        Self::Protocol {
            driver: driver.to_string(),
            detail: detail.into(),
        }
    }

    pub fn failed(error: impl std::fmt::Display) -> Self {
        Self::Failed(error.to_string())
    }
}

/// What opening a provider session needs, from T3's
/// `ProviderAdapterV2OpenSessionInput`. `now` is an ISO timestamp supplied by
/// the caller so adapters never read a clock while deciding.
#[derive(Clone, Debug)]
pub struct OpenSessionInput {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub model_selection: ModelSelection,
    pub runtime_policy: RuntimePolicy,
    pub resume_from_session: Option<ProviderSession>,
    pub initial_native_thread_id: Option<String>,
    pub now: String,
}

/// One event an adapter reports, mirroring the subset of T3's
/// `ProviderAdapterV2Event` this port uses. Field names match the wire.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum AdapterEvent {
    ProviderSessionUpdated {
        driver: String,
        provider_session: ProviderSession,
    },
    ProviderThreadUpdated {
        driver: String,
        provider_thread: ProviderThread,
        pending_background_work: Option<bool>,
    },
    ProviderTurnUpdated {
        driver: String,
        thread_id: Option<ThreadId>,
        provider_turn: ProviderTurn,
    },
    RuntimeRequestUpdated {
        driver: String,
        thread_id: Option<ThreadId>,
        runtime_request: RuntimeRequest,
    },
    /// A protocol notification or request already translated to the app's
    /// own event model. Kept beside the provider-adapter records so
    /// `atrium-runtime` can drive the app without holding a protocol type.
    Agent(atrium_core::AgentEvent),
    /// A turn reached a terminal state. `status` is one of T3's terminal tags.
    TurnTerminal {
        driver: String,
        provider_thread_id: String,
        provider_turn_id: String,
        run_ordinal: u32,
        status: String,
    },
}

/// Where an adapter sends its events. Cheap to clone and safe to call from
/// the provider's pump thread.
pub type AdapterEventSink = Arc<dyn Fn(AdapterEvent) + Send + Sync>;

/// T3's `ProviderAdapterV2EnsureThreadInput`.
#[derive(Clone, Debug)]
pub struct EnsureThreadInput {
    pub thread_id: ThreadId,
    pub title: Option<String>,
    pub model_selection: ModelSelection,
    pub runtime_policy: RuntimePolicy,
    pub provider_session_id: Option<ProviderSessionId>,
    pub existing_provider_thread: Option<ProviderThread>,
    pub force_new_native_thread: bool,
    pub now: String,
}

/// T3's `ProviderAdapterV2TurnInput`.
#[derive(Clone, Debug)]
pub struct AdapterTurnInput {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub run_ordinal: u32,
    pub provider_turn_ordinal: u32,
    pub attempt_id: RunAttemptId,
    pub root_node_id: crate::contracts::NodeId,
    pub provider_thread: ProviderThread,
    pub message: TurnMessage,
    pub model_selection: ModelSelection,
    pub runtime_policy: RuntimePolicy,
    /// The thread's MCP app notes, as `(key, text)`.
    pub app_context: Vec<(String, String)>,
    pub now: String,
}

/// T3's `ProviderAdapterV2SteerInput`.
#[derive(Clone, Debug)]
pub struct SteerInput {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub provider_thread: ProviderThread,
    pub provider_turn_id: ProviderTurnId,
    pub message: TurnMessage,
    pub model_selection: Option<ModelSelection>,
    pub now: String,
}

/// T3's `ProviderAdapterV2RuntimeRequestResponseInput`.
#[derive(Clone, Debug, Default)]
pub struct RuntimeRequestResponse {
    pub request_id: String,
    pub decision: Option<ProviderApprovalDecision>,
    pub feedback: Option<String>,
    pub answers: Option<crate::contracts::ProviderUserInputAnswers>,
}

/// T3's `ProviderAdapterV2ThreadSnapshot`.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadSnapshot {
    pub provider_thread: ProviderThread,
    pub provider_turns: Vec<ProviderTurn>,
    pub messages: Vec<crate::contracts::ConversationMessage>,
    pub runtime_requests: Vec<RuntimeRequest>,
    pub provider_payload: Option<Value>,
}

/// Skills found under one folder, in Codex's `skills/list` shape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillGroup {
    pub cwd: String,
    #[serde(default)]
    pub skills: Vec<Value>,
    #[serde(default)]
    pub errors: Vec<Value>,
}

/// What [`ProviderSessionRuntime::config`] returns: Codex's config plus the
/// layer origins it reported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderConfig {
    pub config: Value,
    pub origins: Value,
}

/// The session-scoped adapter interface. It is T3's
/// `ProviderAdapterV2SessionRuntime` reduced to the operations this port
/// implements; the rest of T3's interface (rollback, fork, subagents, MCP
/// apps) is listed as a gap in the item report.
pub trait ProviderSessionRuntime: Send + Sync {
    fn provider_session(&self) -> ProviderSession;
    fn driver(&self) -> &str;

    /// Start a native thread, or resume the existing one.
    fn ensure_thread(
        &self,
        input: EnsureThreadInput,
    ) -> Result<ProviderThread, ProviderAdapterError>;

    /// Load a thread for writing (`thread/resume`). This is the path that
    /// takes the provider's writer lock.
    fn resume_thread(
        &self,
        provider_thread: &ProviderThread,
        model_selection: Option<&ModelSelection>,
        runtime_policy: Option<&RuntimePolicy>,
        now: &str,
    ) -> Result<ProviderThread, ProviderAdapterError>;

    fn start_turn(&self, input: AdapterTurnInput) -> Result<ProviderTurn, ProviderAdapterError>;

    fn steer_turn(&self, input: SteerInput) -> Result<(), ProviderAdapterError>;

    fn interrupt_turn(
        &self,
        provider_thread: &ProviderThread,
        provider_turn_id: &ProviderTurnId,
    ) -> Result<(), ProviderAdapterError>;

    /// Apply a new model, effort, service tier and approval policy to a
    /// thread that is already loaded, instead of only to the next turn.
    fn apply_model_selection(
        &self,
        provider_thread: &ProviderThread,
        model_selection: &ModelSelection,
        runtime_policy: Option<&RuntimePolicy>,
    ) -> Result<(), ProviderAdapterError>;

    /// Rename the native thread while its process is running.
    fn apply_thread_title(
        &self,
        provider_thread: &ProviderThread,
        title: &str,
    ) -> Result<(), ProviderAdapterError>;

    fn respond_to_runtime_request(
        &self,
        input: RuntimeRequestResponse,
    ) -> Result<(), ProviderAdapterError>;

    /// Read a thread for views and sync. This uses `thread/read`, which does
    /// not take the writer lock, so a Codex desktop app keeps it.
    fn read_thread_snapshot(
        &self,
        provider_thread: &ProviderThread,
    ) -> Result<ThreadSnapshot, ProviderAdapterError>;

    fn skills(&self, cwds: &[String]) -> Result<Vec<SkillGroup>, ProviderAdapterError>;

    fn config(&self, cwd: Option<&str>) -> Result<ProviderConfig, ProviderAdapterError>;

    /// The provider's usage windows in item 26's shared report shape.
    ///
    /// Codex's app-server exposes `account/rateLimits/read` and Claude's CLI
    /// `get_usage`; the app's usage store consumes [`crate::usage`] records
    /// through this seam. The Codex adapter (item 22) and the Claude adapter
    /// (item 23) do not implement it yet, so the default reports no usage
    /// rather than failing a session.
    fn usage_limits(
        &self,
        _now: &str,
    ) -> Result<Option<crate::usage::ProviderUsageLimits>, ProviderAdapterError> {
        Ok(None)
    }

    /// Drop the provider's subscription to a thread; the core can then unload
    /// it once idle.
    fn unload_thread(&self, provider_thread: &ProviderThread) -> Result<(), ProviderAdapterError>;

    fn close(&self);
}

/// T3's `ProviderAdapterV2`: one configured provider instance.
pub trait ProviderAdapter: Send + Sync {
    fn instance_id(&self) -> &ProviderInstanceId;
    fn driver(&self) -> &str;
    /// Set when opening a session rates the version the runtime reports at
    /// its handshake, so the host can skip its probe-based check.
    fn checks_version_at_handshake(&self) -> bool {
        false
    }
    fn capabilities(&self) -> ProviderCapabilities;
    fn plan_selection_transition(
        &self,
        current: &ModelSelection,
        target: &ModelSelection,
    ) -> SelectionTransitionPlan;
    fn open_session(
        &self,
        input: OpenSessionInput,
        events: AdapterEventSink,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterError>;
}

/// T3's `providerSession` helper: a ready session carrying the adapter's
/// capabilities.
pub fn provider_session(
    id: ProviderSessionId,
    instance_id: ProviderInstanceId,
    cwd: Option<&str>,
    model: Option<&str>,
    capabilities: ProviderCapabilities,
    now: &str,
) -> ProviderSession {
    ProviderSession {
        id,
        driver: crate::contracts::ProviderDriverKind::new(CODEX_DRIVER_KIND),
        provider_instance_id: instance_id,
        status: ProviderSessionStatus::Ready,
        cwd: cwd.map(str::to_owned).unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .display()
                .to_string()
        }),
        model: model.map(str::to_owned),
        capabilities,
        extension_statuses: Vec::new(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
        last_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(model: &str) -> ModelSelection {
        ModelSelection::new("codex", model)
    }

    // Ported from packages/provider-core/src/server/selectionTransition.ts
    // (T3 Code v0.1.11, MIT): its cases are stated here because T3 keeps the
    // function in adapter tests rather than a dedicated file.
    #[test]
    fn turn_scoped_transitions_apply_on_the_next_turn() {
        assert_eq!(
            turn_scoped_selection_transition(),
            SelectionTransitionPlan::ApplyOnNextTurn
        );
        assert_eq!(
            serde_json::to_value(SelectionTransitionPlan::ApplyOnNextTurn).unwrap(),
            serde_json::json!({"type": "apply_on_next_turn"})
        );
    }

    #[test]
    fn acp_refuses_a_model_switch_when_the_session_cannot_switch() {
        let plan = acp_selection_transition(&selection("claude-a"), &selection("claude-b"), false);
        assert_eq!(
            plan,
            SelectionTransitionPlan::Reject {
                reason: "The active ACP session does not expose a model-switch capability.".into()
            }
        );
        assert_eq!(
            serde_json::to_value(&plan).unwrap(),
            serde_json::json!({
                "type": "reject",
                "reason": "The active ACP session does not expose a model-switch capability."
            })
        );
    }

    #[test]
    fn acp_applies_same_model_and_option_changes_next_turn() {
        assert_eq!(
            acp_selection_transition(&selection("claude-a"), &selection("claude-a"), false),
            SelectionTransitionPlan::ApplyOnNextTurn
        );
        assert_eq!(
            acp_selection_transition(&selection("claude-a"), &selection("claude-b"), true),
            SelectionTransitionPlan::ApplyOnNextTurn
        );
    }
}
