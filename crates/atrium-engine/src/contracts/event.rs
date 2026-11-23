// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! The orchestration domain-event union.
//!
//! Every event carries the same base (id, thread, run, node, driver,
//! occurrence time) and a tagged payload. The core subset is ported member by
//! member; an event type from a newer server decodes as
//! [`DomainEvent::Unknown`] with its raw JSON and never encodes again, which is
//! what lets the engine skip it instead of dropping the whole stream.
//!
//! Deliberately unported members: `subagent.updated`, `checkpoint-scope.*`,
//! `checkpoint.*`, `context-handoff.updated` and `context-transfer.*` (those
//! records belong to items 20 and 30).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::base::{
    Actor, CommandId, EventId, MessageId, NodeId, ProviderDriverKind, ProviderInstanceId,
    ProviderSessionId, ProviderTurnId, RawEventId, RunId, RuntimeRequestId, ThreadId,
    UnknownUnionMember, trim_non_empty,
};
use super::capabilities::LimitRecoveryUpdate;
use super::capabilities::NativeRefStrength;
use super::chat::{ChatAttachment, OrchestrationMessageContext};
use super::item::{IdleCompaction, PlanArtifact, TurnItem};
use super::provider::{
    ProviderApprovalDecision, ProviderRef, ProviderSession, ProviderSessionDetached,
    ProviderThread, ProviderTurn, ProviderUserInputAnswers,
};
use super::run::{ExecutionNode, Run, RunAttempt, RunBackgroundWorkCancelled};
use super::thread::{
    AppThread, CreationSource, LinkedSender, ThreadVisit, TitleRegenerationStatus,
};

/// Fields every domain event carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainEventBase {
    pub id: EventId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<ProviderDriverKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_event_id: Option<RawEventId>,
    pub occurred_at: String,
}

/// A base plus one payload, the shape of every single-tag event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPayload<T> {
    #[serde(flatten)]
    pub base: DomainEventBase,
    pub payload: T,
}

/// The `thread.*` events whose payload is the whole thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadMetadataEventType {
    Archived,
    Unarchived,
    Deleted,
    Settled,
    Unsettled,
    Snoozed,
    Unsnoozed,
    Pinned,
    AutoSettleSet,
    Unpinned,
    PinReordered,
    ActiveReordered,
    TagsUpdated,
    Visited,
    MarkedUnread,
    MetadataUpdated,
    PullRequestSynced,
    IssueSynced,
    RuntimeModeUpdated,
    InteractionModeUpdated,
    ModelSelectionUpdated,
    ProviderSwitched,
    GuestShareUpdated,
}

impl ThreadMetadataEventType {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Archived => "thread.archived",
            Self::Unarchived => "thread.unarchived",
            Self::Deleted => "thread.deleted",
            Self::Settled => "thread.settled",
            Self::Unsettled => "thread.unsettled",
            Self::Snoozed => "thread.snoozed",
            Self::Unsnoozed => "thread.unsnoozed",
            Self::Pinned => "thread.pinned",
            Self::AutoSettleSet => "thread.auto-settle-set",
            Self::Unpinned => "thread.unpinned",
            Self::PinReordered => "thread.pin-reordered",
            Self::ActiveReordered => "thread.active-reordered",
            Self::TagsUpdated => "thread.tags-updated",
            Self::Visited => "thread.visited",
            Self::MarkedUnread => "thread.marked-unread",
            Self::MetadataUpdated => "thread.metadata-updated",
            Self::PullRequestSynced => "thread.pull-request-synced",
            Self::IssueSynced => "thread.issue-synced",
            Self::RuntimeModeUpdated => "thread.runtime-mode-updated",
            Self::InteractionModeUpdated => "thread.interaction-mode-updated",
            Self::ModelSelectionUpdated => "thread.model-selection-updated",
            Self::ProviderSwitched => "thread.provider-switched",
            Self::GuestShareUpdated => "thread.guest-share-updated",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "thread.archived" => Self::Archived,
            "thread.unarchived" => Self::Unarchived,
            "thread.deleted" => Self::Deleted,
            "thread.settled" => Self::Settled,
            "thread.unsettled" => Self::Unsettled,
            "thread.snoozed" => Self::Snoozed,
            "thread.unsnoozed" => Self::Unsnoozed,
            "thread.pinned" => Self::Pinned,
            "thread.auto-settle-set" => Self::AutoSettleSet,
            "thread.unpinned" => Self::Unpinned,
            "thread.pin-reordered" => Self::PinReordered,
            "thread.active-reordered" => Self::ActiveReordered,
            "thread.tags-updated" => Self::TagsUpdated,
            "thread.visited" => Self::Visited,
            "thread.marked-unread" => Self::MarkedUnread,
            "thread.metadata-updated" => Self::MetadataUpdated,
            "thread.pull-request-synced" => Self::PullRequestSynced,
            "thread.issue-synced" => Self::IssueSynced,
            "thread.runtime-mode-updated" => Self::RuntimeModeUpdated,
            "thread.interaction-mode-updated" => Self::InteractionModeUpdated,
            "thread.model-selection-updated" => Self::ModelSelectionUpdated,
            "thread.provider-switched" => Self::ProviderSwitched,
            "thread.guest-share-updated" => Self::GuestShareUpdated,
            _ => return None,
        })
    }
}

/// T3's `OrchestrationV2DomainEvent` core subset plus an unknown member.
// The thread event carries the whole projection; boxing it would change the
// shape every consumer matches on.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum DomainEvent {
    ThreadCreated(EventPayload<AppThread>),
    ThreadMetadata {
        type_: ThreadMetadataEventType,
        event: EventPayload<AppThread>,
    },
    ThreadVisitRecorded(EventPayload<ThreadVisit>),
    RunCreated(EventPayload<Run>),
    RunUpdated(EventPayload<Run>),
    RunBackgroundWorkCancelled(EventPayload<RunBackgroundWorkCancelled>),
    RunAttemptCreated(EventPayload<RunAttempt>),
    RunAttemptUpdated(EventPayload<RunAttempt>),
    NodeUpdated(EventPayload<ExecutionNode>),
    ProviderSessionAttached(EventPayload<ProviderSession>),
    ProviderSessionUpdated(EventPayload<ProviderSession>),
    ProviderSessionDetached(EventPayload<ProviderSessionDetached>),
    ProviderThreadUpdated(EventPayload<ProviderThread>),
    ProviderTurnUpdated(EventPayload<ProviderTurn>),
    RuntimeRequestUpdated(EventPayload<RuntimeRequest>),
    MessageUpdated(EventPayload<ConversationMessage>),
    TurnItemUpdated(EventPayload<TurnItem>),
    PlanUpdated(EventPayload<PlanArtifact>),
    Unknown(UnknownUnionMember),
}

impl<'de> Deserialize<'de> for DomainEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (tag, value) = super::base::tagged_union_tag(deserializer)?;
        let parse = |value: serde_json::Value| {
            serde_json::from_value::<EventPayload<AppThread>>(value).map_err(D::Error::custom)
        };
        Ok(match tag.as_str() {
            "thread.created" => serde_json::from_value(value)
                .map(Self::ThreadCreated)
                .map_err(D::Error::custom)?,
            "thread.visit-recorded" => serde_json::from_value(value)
                .map(Self::ThreadVisitRecorded)
                .map_err(D::Error::custom)?,
            "run.created" => serde_json::from_value(value)
                .map(Self::RunCreated)
                .map_err(D::Error::custom)?,
            "run.updated" => serde_json::from_value(value)
                .map(Self::RunUpdated)
                .map_err(D::Error::custom)?,
            "run.background-work-cancelled" => serde_json::from_value(value)
                .map(Self::RunBackgroundWorkCancelled)
                .map_err(D::Error::custom)?,
            "run-attempt.created" => serde_json::from_value(value)
                .map(Self::RunAttemptCreated)
                .map_err(D::Error::custom)?,
            "run-attempt.updated" => serde_json::from_value(value)
                .map(Self::RunAttemptUpdated)
                .map_err(D::Error::custom)?,
            "node.updated" => serde_json::from_value(value)
                .map(Self::NodeUpdated)
                .map_err(D::Error::custom)?,
            "provider-session.attached" => serde_json::from_value(value)
                .map(Self::ProviderSessionAttached)
                .map_err(D::Error::custom)?,
            "provider-session.updated" => serde_json::from_value(value)
                .map(Self::ProviderSessionUpdated)
                .map_err(D::Error::custom)?,
            "provider-session.detached" => serde_json::from_value(value)
                .map(Self::ProviderSessionDetached)
                .map_err(D::Error::custom)?,
            "provider-thread.updated" => serde_json::from_value(value)
                .map(Self::ProviderThreadUpdated)
                .map_err(D::Error::custom)?,
            "provider-turn.updated" => serde_json::from_value(value)
                .map(Self::ProviderTurnUpdated)
                .map_err(D::Error::custom)?,
            "runtime-request.updated" => serde_json::from_value(value)
                .map(Self::RuntimeRequestUpdated)
                .map_err(D::Error::custom)?,
            "message.updated" => serde_json::from_value(value)
                .map(Self::MessageUpdated)
                .map_err(D::Error::custom)?,
            "turn-item.updated" => serde_json::from_value(value)
                .map(Self::TurnItemUpdated)
                .map_err(D::Error::custom)?,
            "plan.updated" => serde_json::from_value(value)
                .map(Self::PlanUpdated)
                .map_err(D::Error::custom)?,
            other => match ThreadMetadataEventType::from_wire(other) {
                Some(type_) => Self::ThreadMetadata {
                    type_,
                    event: parse(value)?,
                },
                None => Self::Unknown(UnknownUnionMember::new(tag, value)),
            },
        })
    }
}

impl Serialize for DomainEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::ThreadCreated(event) => {
                super::base::serialize_tagged("thread.created", event, serializer)
            }
            Self::ThreadMetadata { type_, event } => {
                super::base::serialize_tagged(type_.as_wire(), event, serializer)
            }
            Self::ThreadVisitRecorded(event) => {
                super::base::serialize_tagged("thread.visit-recorded", event, serializer)
            }
            Self::RunCreated(event) => {
                super::base::serialize_tagged("run.created", event, serializer)
            }
            Self::RunUpdated(event) => {
                super::base::serialize_tagged("run.updated", event, serializer)
            }
            Self::RunBackgroundWorkCancelled(event) => {
                super::base::serialize_tagged("run.background-work-cancelled", event, serializer)
            }
            Self::RunAttemptCreated(event) => {
                super::base::serialize_tagged("run-attempt.created", event, serializer)
            }
            Self::RunAttemptUpdated(event) => {
                super::base::serialize_tagged("run-attempt.updated", event, serializer)
            }
            Self::NodeUpdated(event) => {
                super::base::serialize_tagged("node.updated", event, serializer)
            }
            Self::ProviderSessionAttached(event) => {
                super::base::serialize_tagged("provider-session.attached", event, serializer)
            }
            Self::ProviderSessionUpdated(event) => {
                super::base::serialize_tagged("provider-session.updated", event, serializer)
            }
            Self::ProviderSessionDetached(event) => {
                super::base::serialize_tagged("provider-session.detached", event, serializer)
            }
            Self::ProviderThreadUpdated(event) => {
                super::base::serialize_tagged("provider-thread.updated", event, serializer)
            }
            Self::ProviderTurnUpdated(event) => {
                super::base::serialize_tagged("provider-turn.updated", event, serializer)
            }
            Self::RuntimeRequestUpdated(event) => {
                super::base::serialize_tagged("runtime-request.updated", event, serializer)
            }
            Self::MessageUpdated(event) => {
                super::base::serialize_tagged("message.updated", event, serializer)
            }
            Self::TurnItemUpdated(event) => {
                super::base::serialize_tagged("turn-item.updated", event, serializer)
            }
            Self::PlanUpdated(event) => {
                super::base::serialize_tagged("plan.updated", event, serializer)
            }
            Self::Unknown(member) => Err(serde::ser::Error::custom(format!(
                "unknown event type `{}` is never sent",
                member.tag
            ))),
        }
    }
}

impl DomainEvent {
    pub fn type_name(&self) -> &str {
        match self {
            Self::ThreadCreated(_) => "thread.created",
            Self::ThreadMetadata { type_, .. } => type_.as_wire(),
            Self::ThreadVisitRecorded(_) => "thread.visit-recorded",
            Self::RunCreated(_) => "run.created",
            Self::RunUpdated(_) => "run.updated",
            Self::RunBackgroundWorkCancelled(_) => "run.background-work-cancelled",
            Self::RunAttemptCreated(_) => "run-attempt.created",
            Self::RunAttemptUpdated(_) => "run-attempt.updated",
            Self::NodeUpdated(_) => "node.updated",
            Self::ProviderSessionAttached(_) => "provider-session.attached",
            Self::ProviderSessionUpdated(_) => "provider-session.updated",
            Self::ProviderSessionDetached(_) => "provider-session.detached",
            Self::ProviderThreadUpdated(_) => "provider-thread.updated",
            Self::ProviderTurnUpdated(_) => "provider-turn.updated",
            Self::RuntimeRequestUpdated(_) => "runtime-request.updated",
            Self::MessageUpdated(_) => "message.updated",
            Self::TurnItemUpdated(_) => "turn-item.updated",
            Self::PlanUpdated(_) => "plan.updated",
            Self::Unknown(member) => &member.tag,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionMessage {
    pub parent_run_id: RunId,
    pub generation: u32,
    pub task_ids: Vec<NodeId>,
}

/// T3's `OrchestrationV2ConversationMessage` (the message projection).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<super::item::Notification>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<super::base::ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows_thread_selection: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_sender: Option<LinkedSender>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_compaction: Option<IdleCompaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_script: Option<bool>,
    pub id: MessageId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub role: MessageRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<OrchestrationMessageContext>,
    pub attachments: Vec<ChatAttachment>,
    pub streaming: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<DelegatedCompletionMessage>,
}

/// A provider request's kind: an approval, or one of the three
/// orchestration-only requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeRequestKind {
    Command,
    FileRead,
    FileChange,
    McpElicitation,
    Permission,
    DynamicToolCall,
    UserInput,
    AuthRefresh,
}

impl RuntimeRequestKind {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::FileRead => "file-read",
            Self::FileChange => "file-change",
            Self::McpElicitation => "mcp-elicitation",
            Self::Permission => "permission",
            Self::DynamicToolCall => "dynamic_tool_call",
            Self::UserInput => "user_input",
            Self::AuthRefresh => "auth_refresh",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "command" => Self::Command,
            "file-read" => Self::FileRead,
            "file-change" => Self::FileChange,
            "mcp-elicitation" => Self::McpElicitation,
            "permission" => Self::Permission,
            "dynamic_tool_call" => Self::DynamicToolCall,
            "user_input" => Self::UserInput,
            "auth_refresh" => Self::AuthRefresh,
            _ => return None,
        })
    }
}

impl Serialize for RuntimeRequestKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for RuntimeRequestKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_wire(&raw)
            .ok_or_else(|| D::Error::custom(format!("unknown runtime request kind `{raw}`")))
    }
}

/// How a client can still answer a request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ResponseCapability {
    Live {
        provider_session_id: ProviderSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supports_feedback: Option<bool>,
    },
    Message {},
    NotResumable {
        reason: String,
    },
}

/// T3's `OrchestrationV2RuntimeRequest` (approvals and questions).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: RuntimeRequestId,
    pub node_id: NodeId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_request_ref: Option<ProviderRef>,
    pub kind: RuntimeRequestKind,
    pub status: super::provider::RuntimeRequestStatus,
    pub response_capability: ResponseCapability,
    pub created_at: String,
    pub resolved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ProviderApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<ProviderUserInputAnswers>,
}

/// A limit recovery update that the store applies to a thread metadata
/// command. Kept beside the events because `thread.limitRecovery` is what
/// replays it.
pub fn limit_recovery_update_is_valid(update: &LimitRecoveryUpdate) -> bool {
    update.is_valid()
}

pub fn is_trimmed_non_empty(value: &str) -> bool {
    trim_non_empty(value).is_some()
}

/// The `NativeRefStrength` values a request's native ref may use.
pub fn is_known_ref_strength(value: NativeRefStrength) -> bool {
    matches!(
        value,
        NativeRefStrength::Strong | NativeRefStrength::Weak | NativeRefStrength::None
    )
}

/// Commands are keyed by the command that produced an event; this helper
/// reads that key off a stored event row.
pub fn command_id_of(row: &super::StoredEvent) -> Option<&CommandId> {
    row.command_id.as_ref()
}

/// The title regeneration outcome carried by `thread.title.regeneration.complete`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleRegenerationOutcomeStatus {
    Renamed,
    Unchanged,
    Failed,
}

/// Re-export so event consumers do not need the thread module for this one.
pub type TitleStatus = TitleRegenerationStatus;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn detached_event(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "type": "provider-session.detached",
            "threadId": "thread-1",
            "occurredAt": "2026-04-20T00:00:00.000Z",
            "payload": {
                "providerSessionId": "provider-session-1",
                "detachedAt": "2026-04-20T00:00:00.000Z"
            }
        })
    }

    // Ported from packages/contracts/src/orchestrationV2.test.ts.
    #[test]
    fn decodes_thread_event_types_from_a_newer_server_as_skippable() {
        let known: DomainEvent = serde_json::from_value(detached_event("event-1")).unwrap();
        assert_eq!(known.type_name(), "provider-session.detached");
        assert!(!known.is_unknown());

        let future: DomainEvent = serde_json::from_value(json!({
            "id": "event-2",
            "type": "run.from-a-future-server",
            "threadId": "thread-1",
            "occurredAt": "2026-04-20T00:00:00.000Z",
            "payload": { "runId": "run-1" }
        }))
        .unwrap();
        assert!(future.is_unknown());
        assert_eq!(future.type_name(), "run.from-a-future-server");
        // Unknown events are decode-only.
        assert!(serde_json::to_value(&future).is_err());

        // A known type whose payload is broken still fails.
        let mut broken = detached_event("event-4");
        broken["payload"] = json!({});
        assert!(serde_json::from_value::<DomainEvent>(broken).is_err());
    }

    #[test]
    fn round_trips_a_known_event_with_its_extra_base_fields() {
        let value = json!({
            "id": "event-1",
            "type": "thread.archived",
            "threadId": "thread-1",
            "runId": "run-1",
            "driver": "codex",
            "occurredAt": "2026-04-20T00:00:00.000Z",
            "payload": {
                "createdBy": "user",
                "creationSource": "web",
                "id": "thread-1",
                "projectId": "project-1",
                "title": "Thread",
                "providerInstanceId": "codex",
                "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "branch": null,
                "worktreePath": null,
                "activeProviderThreadId": null,
                "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1" },
                "forkedFrom": null,
                "createdAt": "2026-04-20T00:00:00.000Z",
                "updatedAt": "2026-04-20T00:00:00.000Z",
                "archivedAt": "2026-04-20T00:00:00.000Z",
                "settledOverride": null,
                "settledAt": null,
                "lastVisitedAt": null,
                "deletedAt": null
            }
        });
        let event: DomainEvent = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(event.type_name(), "thread.archived");
        assert_eq!(serde_json::to_value(&event).unwrap(), value);
    }

    #[test]
    fn round_trips_run_and_runtime_request_events() {
        let run = json!({
            "id": "event-run",
            "type": "run.created",
            "threadId": "thread-1",
            "occurredAt": "2026-04-20T00:00:00.000Z",
            "payload": {
                "id": "run-1",
                "threadId": "thread-1",
                "ordinal": 1,
                "providerInstanceId": "codex",
                "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
                "providerThreadId": null,
                "userMessageId": "message-1",
                "rootNodeId": null,
                "activeAttemptId": null,
                "status": "queued",
                "requestedAt": "2026-04-20T00:00:00.000Z",
                "startedAt": null,
                "completedAt": null,
                "checkpointId": null,
                "contextHandoffId": null
            }
        });
        let event: DomainEvent = serde_json::from_value(run.clone()).unwrap();
        assert_eq!(event.type_name(), "run.created");
        assert_eq!(serde_json::to_value(&event).unwrap(), run);

        let request = json!({
            "id": "event-request",
            "type": "runtime-request.updated",
            "threadId": "thread-1",
            "occurredAt": "2026-04-20T00:00:00.000Z",
            "payload": {
                "id": "request-1",
                "nodeId": "node-1",
                "providerTurnId": null,
                "nativeRequestRef": null,
                "kind": "command",
                "status": "pending",
                "responseCapability": {
                    "type": "live",
                    "providerSessionId": "provider-session-1"
                },
                "createdAt": "2026-04-20T00:00:00.000Z",
                "resolvedAt": null
            }
        });
        let event: DomainEvent = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(event.type_name(), "runtime-request.updated");
        assert_eq!(serde_json::to_value(&event).unwrap(), request);
    }

    #[test]
    fn tolerates_unknown_fields_on_known_events() {
        let mut value = detached_event("event-1");
        value["futureField"] = json!(1);
        value["payload"]["more"] = json!("x");
        let event: DomainEvent = serde_json::from_value(value).unwrap();
        assert_eq!(event.type_name(), "provider-session.detached");
    }

    #[test]
    fn runtime_request_kinds_keep_their_mixed_wire_names() {
        assert_eq!(RuntimeRequestKind::FileRead.as_wire(), "file-read");
        assert_eq!(
            RuntimeRequestKind::DynamicToolCall.as_wire(),
            "dynamic_tool_call"
        );
        assert_eq!(
            serde_json::from_str::<RuntimeRequestKind>("\"auth_refresh\"").unwrap(),
            RuntimeRequestKind::AuthRefresh
        );
    }
}
