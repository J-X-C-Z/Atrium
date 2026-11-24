// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! The command subset the orchestrator accepts.
//!
//! Ported: `thread.create` (with `initialMessage`), `thread.stop`,
//! `thread.archive` / `thread.unarchive` / `thread.delete`,
//! `message.dispatch` (intake, send and steer), `run.interrupt`,
//! `queued-message.promote-to-steer`, `runtime-request.respond`,
//! `thread.model-selection.set`, `thread.runtime-mode.set` and
//! `thread.interaction-mode.set`.
//!
//! Deliberately unported commands (guest shares, PR/issue links, settling,
//! pinning, forking, delegation, checkpoint rollback and queue editing) belong
//! to later items or to the server-only internal union. A command tag this
//! build does not know is an error: unlike events, a command must never be
//! half-applied.

use serde::{Deserialize, Serialize};

use super::base::Actor;
use super::base::{
    CommandId, MessageId, NodeId, ProjectId, RunId, RuntimeRequestId, ScheduledTaskId, ThreadId,
    closed_union,
};
use super::chat::{ChatAttachment, OrchestrationMessageContext};
use super::item::IdleCompaction;
use super::model::ModelSelection;
use super::provider::{
    ProviderApprovalDecision, ProviderInteractionMode, ProviderThreadNativeMetadata,
    ProviderUserInputAnswers, RuntimeMode, UserInputAttachments,
};
use super::run::{SourcePlanRef, ThreadLaunchWorkspaceStrategy};
use super::thread::{CreationSource, DelegatedFrom, LinkOrigin, LinkedSender};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStopCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// False when the caller stops delegated tasks itself; otherwise Stop also
    /// stops every task under the thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_delegated_tasks: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedNativeThreadRef {
    pub driver: super::base::ProviderDriverKind,
    pub native_id: String,
    pub strength: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedNativeThread {
    #[serde(rename = "ref")]
    pub thread_ref: ImportedNativeThreadRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ProviderThreadNativeMetadata>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialMessage {
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<OrchestrationMessageContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_strategy: Option<ThreadLaunchWorkspaceStrategy>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCreateCommand {
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    /// Only the server sets this; a client's `thread.create` never carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_origin: Option<LinkOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<DelegatedFrom>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_native_thread: Option<ImportedNativeThread>,
    /// Only the server sets this; the thread and its first run commit together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_message: Option<InitialMessage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadArchiveCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUnarchiveCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDeleteCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    /// Delete only a thread with no runs and no messages; otherwise succeed
    /// without a change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_if_unused: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageDeliveryIntent {
    Auto,
    Steer,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MessageDispatchMode {
    DeferStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_strategy: Option<ThreadLaunchWorkspaceStrategy>,
    },
    SteerActive {
        target_run_id: RunId,
    },
    RestartActive {
        target_run_id: RunId,
    },
    QueueAfterActive {},
    StartImmediately {},
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionDispatch {
    pub parent_run_id: RunId,
    pub generation: u32,
    pub task_ids: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDispatchCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<super::item::Notification>,
    /// Sends this pending guest message; the server uses the guest's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_message_id: Option<super::base::GuestMessageId>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_sender: Option<LinkedSender>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_compaction: Option<IdleCompaction>,
    /// Start a new provider session without the thread's earlier context.
    /// Never steers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_context: Option<bool>,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<OrchestrationMessageContext>,
    pub attachments: Vec<ChatAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_seed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan_ref: Option<SourcePlanRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_follow_up_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_follow_up_after_run_ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_recovery_request_id: Option<CommandId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_intent: Option<MessageDeliveryIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<DelegatedCompletionDispatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_mode: Option<MessageDispatchMode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInterruptCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_queue: Option<bool>,
}

/// Promote a queued run to a steer of the active one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessagePromoteToSteerCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub queued_run_id: RunId,
    pub target_run_id: RunId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequestRespondCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request_id: RuntimeRequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ProviderApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<ProviderUserInputAnswers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_by_question_id: Option<UserInputAttachments>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadModelSelectionSetCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub model_selection: ModelSelection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRuntimeModeSetCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub runtime_mode: RuntimeMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInteractionModeSetCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub interaction_mode: ProviderInteractionMode,
}

// The create and dispatch commands carry whole payloads; T3's union is the
// public contract and callers match on every variant in place.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum OrchestrationV2Command {
    ThreadStop(ThreadStopCommand),
    ThreadCreate(ThreadCreateCommand),
    ThreadArchive(ThreadArchiveCommand),
    ThreadUnarchive(ThreadUnarchiveCommand),
    ThreadDelete(ThreadDeleteCommand),
    MessageDispatch(MessageDispatchCommand),
    RunInterrupt(RunInterruptCommand),
    QueuedMessagePromoteToSteer(QueuedMessagePromoteToSteerCommand),
    RuntimeRequestRespond(RuntimeRequestRespondCommand),
    ThreadModelSelectionSet(ThreadModelSelectionSetCommand),
    ThreadRuntimeModeSet(ThreadRuntimeModeSetCommand),
    ThreadInteractionModeSet(ThreadInteractionModeSetCommand),
}

closed_union!(
    OrchestrationV2Command,
    "thread.stop" => ThreadStop(ThreadStopCommand),
    "thread.create" => ThreadCreate(ThreadCreateCommand),
    "thread.archive" => ThreadArchive(ThreadArchiveCommand),
    "thread.unarchive" => ThreadUnarchive(ThreadUnarchiveCommand),
    "thread.delete" => ThreadDelete(ThreadDeleteCommand),
    "message.dispatch" => MessageDispatch(MessageDispatchCommand),
    "run.interrupt" => RunInterrupt(RunInterruptCommand),
    "queued-message.promote-to-steer" => QueuedMessagePromoteToSteer(QueuedMessagePromoteToSteerCommand),
    "runtime-request.respond" => RuntimeRequestRespond(RuntimeRequestRespondCommand),
    "thread.model-selection.set" => ThreadModelSelectionSet(ThreadModelSelectionSetCommand),
    "thread.runtime-mode.set" => ThreadRuntimeModeSet(ThreadRuntimeModeSetCommand),
    "thread.interaction-mode.set" => ThreadInteractionModeSet(ThreadInteractionModeSetCommand),
);

impl OrchestrationV2Command {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::ThreadStop(_) => "thread.stop",
            Self::ThreadCreate(_) => "thread.create",
            Self::ThreadArchive(_) => "thread.archive",
            Self::ThreadUnarchive(_) => "thread.unarchive",
            Self::ThreadDelete(_) => "thread.delete",
            Self::MessageDispatch(_) => "message.dispatch",
            Self::RunInterrupt(_) => "run.interrupt",
            Self::QueuedMessagePromoteToSteer(_) => "queued-message.promote-to-steer",
            Self::RuntimeRequestRespond(_) => "runtime-request.respond",
            Self::ThreadModelSelectionSet(_) => "thread.model-selection.set",
            Self::ThreadRuntimeModeSet(_) => "thread.runtime-mode.set",
            Self::ThreadInteractionModeSet(_) => "thread.interaction-mode.set",
        }
    }

    pub fn command_id(&self) -> &CommandId {
        match self {
            Self::ThreadStop(command) => &command.command_id,
            Self::ThreadCreate(command) => &command.command_id,
            Self::ThreadArchive(command) => &command.command_id,
            Self::ThreadUnarchive(command) => &command.command_id,
            Self::ThreadDelete(command) => &command.command_id,
            Self::MessageDispatch(command) => &command.command_id,
            Self::RunInterrupt(command) => &command.command_id,
            Self::QueuedMessagePromoteToSteer(command) => &command.command_id,
            Self::RuntimeRequestRespond(command) => &command.command_id,
            Self::ThreadModelSelectionSet(command) => &command.command_id,
            Self::ThreadRuntimeModeSet(command) => &command.command_id,
            Self::ThreadInteractionModeSet(command) => &command.command_id,
        }
    }

    pub fn thread_id(&self) -> &ThreadId {
        match self {
            Self::ThreadStop(command) => &command.thread_id,
            Self::ThreadCreate(command) => &command.thread_id,
            Self::ThreadArchive(command) => &command.thread_id,
            Self::ThreadUnarchive(command) => &command.thread_id,
            Self::ThreadDelete(command) => &command.thread_id,
            Self::MessageDispatch(command) => &command.thread_id,
            Self::RunInterrupt(command) => &command.thread_id,
            Self::QueuedMessagePromoteToSteer(command) => &command.thread_id,
            Self::RuntimeRequestRespond(command) => &command.thread_id,
            Self::ThreadModelSelectionSet(command) => &command.thread_id,
            Self::ThreadRuntimeModeSet(command) => &command.thread_id,
            Self::ThreadInteractionModeSet(command) => &command.thread_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_thread_create_with_an_initial_message() {
        let value = json!({
            "type": "thread.create",
            "createdBy": "agent",
            "creationSource": "mcp",
            "commandId": "command-1",
            "threadId": "thread-1",
            "projectId": "project-1",
            "title": "Fix the build",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
            "runtimeMode": "auto",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "initialMessage": {
                "messageId": "message-1",
                "text": "go",
                "attachments": [],
                "titleSeed": "Fix the build"
            }
        });
        let command: OrchestrationV2Command = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(command.type_name(), "thread.create");
        assert_eq!(command.command_id().as_str(), "command-1");
        assert_eq!(serde_json::to_value(&command).unwrap(), value);
    }

    #[test]
    fn round_trips_message_dispatch_as_a_steer() {
        let value = json!({
            "type": "message.dispatch",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": "command-2",
            "threadId": "thread-1",
            "messageId": "message-2",
            "text": "actually, do this",
            "attachments": [],
            "deliveryIntent": "steer",
            "dispatchMode": { "type": "steer_active", "targetRunId": "run-1" }
        });
        let command: OrchestrationV2Command = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&command).unwrap(), value);
    }

    #[test]
    fn round_trips_approval_and_model_commands() {
        let respond = json!({
            "type": "runtime-request.respond",
            "commandId": "command-3",
            "threadId": "thread-1",
            "requestId": "request-1",
            "decision": "acceptForSession",
            "answers": { "q1": ["a"] }
        });
        let command: OrchestrationV2Command = serde_json::from_value(respond.clone()).unwrap();
        assert_eq!(serde_json::to_value(&command).unwrap(), respond);

        let model = json!({
            "type": "thread.model-selection.set",
            "commandId": "command-4",
            "threadId": "thread-1",
            "modelSelection": {
                "instanceId": "codex",
                "model": "gpt-5-codex",
                "options": [{ "id": "reasoningEffort", "value": "high" }]
            }
        });
        let command: OrchestrationV2Command = serde_json::from_value(model.clone()).unwrap();
        assert_eq!(serde_json::to_value(&command).unwrap(), model);
    }

    #[test]
    fn an_unknown_command_tag_is_an_error() {
        let unknown = json!({
            "type": "thread.teleport",
            "commandId": "command-5",
            "threadId": "thread-1"
        });
        assert!(serde_json::from_value::<OrchestrationV2Command>(unknown).is_err());
    }

    #[test]
    fn tolerates_unknown_command_fields() {
        let value = json!({
            "type": "thread.stop",
            "commandId": "command-6",
            "threadId": "thread-1",
            "futureField": true
        });
        let command: OrchestrationV2Command = serde_json::from_value(value).unwrap();
        assert_eq!(command.type_name(), "thread.stop");
    }
}
