// Ported from packages/contracts/src/orchestrationV2.ts (T3 Code v0.1.11, MIT).
//! The app thread projection and the small records around it.
//!
//! Pull-request and issue links, guest shares and open stats are kept as raw
//! JSON where their own modules are UI-facing or belong to a later item; their
//! field names and positions in the thread stay exactly T3's.

use serde::{Deserialize, Serialize};

use super::base::{
    Actor, CommandId, EnvironmentId, EventId, IsoDateTime, ProjectId, ProviderInstanceId,
    ProviderThreadId, ScheduledTaskId, ThreadId,
};
use super::capabilities::{LimitRecovery, ThreadOpens};
use super::model::ModelSelection;
use super::provider::{AuthMcpClientAccess, ProviderInteractionMode, RuntimeMode};

/// T3's `OrchestrationV2CreationSource`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreationSource {
    Web,
    Mobile,
    Mcp,
    Provider,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadHistoryOrigin {
    Native,
    V1Import,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettledOverride {
    Settled,
    Active,
}

/// The linked environment whose agent started this work here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkOrigin {
    pub session_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AuthMcpClientAccess>,
}

/// A work from a link paired below full access may change only work from the
/// same link. An origin stamped before access was recorded stays fenced.
pub fn link_origin_is_fenced(origin: &LinkOrigin) -> bool {
    origin.access != Some(AuthMcpClientAccess::FullAccess)
}

/// The thread in a linked environment that delegated this one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedFrom {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub title: String,
}

/// The thread in a linked environment whose agent sent a message here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedSender {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationshipToParent {
    Fork,
    Subagent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThreadLineage {
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<RelationshipToParent>,
    pub root_thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_chat: Option<bool>,
}

/// Subagents and side chats live inside their parent's view, never in thread
/// lists.
pub fn is_listed_thread_lineage(lineage: &AppThreadLineage) -> bool {
    lineage.relationship_to_parent != Some(RelationshipToParent::Subagent)
        && lineage.side_chat != Some(true)
}

/// What a thread was forked from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ForkSource {
    Run {
        thread_id: ThreadId,
        run_id: super::base::RunId,
    },
    Node {
        node_id: super::base::NodeId,
    },
    ProviderThread {
        provider_thread_id: ProviderThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_turn_id: Option<super::base::ProviderTurnId>,
    },
}

/// T3's `ThreadLinkedPullRequest`; the other PR records are UI-facing and
/// stay raw JSON on the thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLinkedPullRequest {
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u32,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleRegenerationMarker {
    pub request_id: CommandId,
    pub started_at: IsoDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleRegenerationStatus {
    Renamed,
    Unchanged,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastTitleRegeneration {
    pub status: TitleRegenerationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub request_id: CommandId,
    pub completed_at: IsoDateTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackFailure {
    pub request_id: CommandId,
    pub message: String,
}

/// A visit changes read state only, so its event leaves the rest of the
/// thread out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadVisit {
    pub last_visited_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens: Option<serde_json::Value>,
}

/// Apply a visit to a thread without touching the rest of it.
pub fn apply_thread_visit(mut thread: AppThread, visit: ThreadVisit) -> AppThread {
    thread.last_visited_at = visit.last_visited_at;
    if visit.opens.is_some() {
        thread.opens = visit.opens;
    }
    thread
}

/// T3's `OrchestrationV2AppThread`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThread {
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_pull_request: Option<ThreadLinkedPullRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_pull_request: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues: Option<Vec<serde_json::Value>>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<ThreadHistoryOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_origin: Option<LinkOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<DelegatedFrom>,
    pub lineage: AppThreadLineage,
    pub forked_from: Option<ForkSource>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    pub archived_at: Option<IsoDateTime>,
    #[serde(default)]
    pub settled_override: Option<SettledOverride>,
    #[serde(default)]
    pub settled_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsettled_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_snooze_wake_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_recovery: Option<LimitRecovery>,
    /// `ThreadGuestShare`; the guest module is not part of this port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_share: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_settle_disabled_at: Option<IsoDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_order_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_order_key: Option<String>,
    /// Absent on threads written before tags existed; read as empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub last_visited_at: Option<IsoDateTime>,
    /// `OrchestrationV2ThreadOpens`, read leniently as raw JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_regeneration: Option<TitleRegenerationMarker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_title_regeneration: Option<LastTitleRegeneration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_renamed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_request_id: Option<CommandId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_failure: Option<RollbackFailure>,
    pub deleted_at: Option<IsoDateTime>,
}

impl Default for AppThreadLineage {
    fn default() -> Self {
        Self {
            parent_thread_id: None,
            relationship_to_parent: None,
            root_thread_id: ThreadId::new(""),
            side_chat: None,
        }
    }
}

impl Default for AppThread {
    fn default() -> Self {
        Self {
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            id: ThreadId::new(""),
            project_id: ProjectId::new(""),
            title: String::new(),
            provider_instance_id: super::base::ProviderInstanceId::new(""),
            model_selection: ModelSelection::new("", ""),
            runtime_mode: super::provider::RuntimeMode::FullAccess,
            interaction_mode: super::provider::ProviderInteractionMode::Default,
            branch: None,
            worktree_path: None,
            linked_pull_request: None,
            pull_requests: None,
            branch_pull_request: None,
            issues: None,
            active_provider_thread_id: None,
            history_origin: None,
            link_origin: None,
            delegated_from: None,
            lineage: AppThreadLineage::default(),
            forked_from: None,
            created_at: String::new(),
            updated_at: String::new(),
            archived_at: None,
            settled_override: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            last_snooze_wake_at: None,
            limit_recovery: None,
            guest_share: None,
            pinned_at: None,
            auto_settle_disabled_at: None,
            pin_order_key: None,
            active_order_key: None,
            tags: None,
            last_visited_at: None,
            opens: None,
            title_regeneration: None,
            last_title_regeneration: None,
            title_renamed: None,
            rollback_request_id: None,
            rollback_failure: None,
            deleted_at: None,
        }
    }
}

/// A thread another agent drives: a local subagent or a delegated task.
pub fn is_delegated_app_thread(thread: &AppThread) -> bool {
    thread.lineage.relationship_to_parent == Some(RelationshipToParent::Subagent)
        || thread.delegated_from.is_some()
}

/// Provider-native children cannot receive messages; app-owned delegated
/// children can.
pub fn is_provider_native_subagent_thread(thread: &AppThread) -> bool {
    thread.lineage.relationship_to_parent == Some(RelationshipToParent::Subagent)
        && thread.creation_source == CreationSource::Provider
}

/// A status event carries the whole thread; the store still keys it by id.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadEventEnvelope {
    pub event_id: EventId,
    pub thread: AppThread,
}

/// The wire shape of `OrchestrationV2ThreadOpens` when it does decode.
pub fn parse_thread_opens(value: &serde_json::Value) -> Option<ThreadOpens> {
    serde_json::from_value(value.clone()).ok()
}

/// Who sent a prompt, for [`with_message_sender_note`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MessageSender<'a> {
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub scheduled_task_id: Option<&'a ScheduledTaskId>,
    pub sender_thread_id: Option<&'a ThreadId>,
    pub linked_sender: Option<&'a LinkedSender>,
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Whether a prompt starts with a slash command or a `$skill` mention, which
/// providers parse from the first characters. T3's expression is
/// `/^\s*(?:\/|\p{Sc}[a-zA-Z0-9:_-]*[a-zA-Z])/u`, so a bare slash counts and
/// a currency symbol must be followed by a letter before a non-symbol.
fn starts_with_leading_command(text: &str) -> bool {
    let mut chars = text.trim_start().chars();
    match chars.next() {
        Some('/') => true,
        Some(symbol) if is_currency_symbol(symbol) => chars
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-'))
            .any(|c| c.is_ascii_alphabetic()),
        _ => false,
    }
}

/// The Unicode `Sc` currency symbols common enough to start a prompt. T3
/// matches the whole category; this covers the symbols its users type.
fn is_currency_symbol(value: char) -> bool {
    matches!(
        value,
        '$' | '€'
            | '£'
            | '¥'
            | '₹'
            | '₽'
            | '₩'
            | '₺'
            | '₴'
            | '₦'
            | '₪'
            | '₫'
            | '฿'
            | '₱'
            | '₡'
            | '₲'
            | '₵'
            | '₸'
            | '₼'
            | '₾'
            | '₿'
    )
}

/// Prefix the line the model reads before a prompt that a person did not
/// type. A leading slash command or `$skill` mention stays exactly as sent.
pub fn with_message_sender_note(message: &MessageSender<'_>, text: &str) -> String {
    let scheduled_note = || {
        message.scheduled_task_id.map(|task_id| {
            format!(
                "[T3 Code: scheduled task {} sent this prompt, not the user typing in this chat.]",
                json_string(task_id.as_str()),
            )
        })
    };
    let note = if message.created_by == Actor::Agent {
        if let Some(sender) = message.linked_sender {
            Some(format!(
                "[T3 Code: the agent in T3 thread {} on linked environment {} sent this message, not the user.]",
                json_string(sender.thread_id.as_str()),
                json_string(sender.environment_id.as_str()),
            ))
        } else if let Some(sender_thread_id) = message.sender_thread_id {
            Some(format!(
                "[T3 Code: the agent in T3 thread {} sent this message, not the user.]",
                json_string(sender_thread_id.as_str()),
            ))
        } else if message.scheduled_task_id.is_some() {
            scheduled_note()
        } else if message.creation_source == CreationSource::Mcp {
            Some(
                "[T3 Code: this message came through T3 Code's tools, not from the user typing in this chat.]"
                    .to_owned(),
            )
        } else {
            None
        }
    } else {
        scheduled_note()
    };

    match note {
        None => text.to_owned(),
        Some(_) if starts_with_leading_command(text) => text.to_owned(),
        Some(note) if text.is_empty() => note,
        Some(note) => format!("{note}\n\n{text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_thread() -> serde_json::Value {
        json!({
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
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "lastVisitedAt": null,
            "deletedAt": null
        })
    }

    // Ported from packages/contracts/src/orchestrationV2.test.ts.
    #[test]
    fn decodes_the_projection_thread_and_defaults_the_nullable_fields() {
        let thread: AppThread = serde_json::from_value(minimal_thread()).unwrap();
        assert_eq!(thread.settled_override, None);
        assert_eq!(thread.settled_at, None);
        assert_eq!(thread.last_visited_at, None);
        let encoded = serde_json::to_value(&thread).unwrap();
        assert_eq!(encoded["settledOverride"], json!(null));
        assert_eq!(encoded["settledAt"], json!(null));
        assert_eq!(encoded["lastVisitedAt"], json!(null));
    }

    #[test]
    fn round_trips_a_thread_with_every_core_field() {
        let mut value = minimal_thread();
        value["historyOrigin"] = json!("native");
        value["tags"] = json!(["one", "two"]);
        value["pinnedAt"] = json!("2026-04-20T00:00:00.000Z");
        value["titleRenamed"] = json!(true);
        value["linkOrigin"] = json!({
            "sessionId": "session-1",
            "label": "phone",
            "access": "full-access"
        });
        let thread: AppThread = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&thread).unwrap(), value);
    }

    #[test]
    fn tolerates_unknown_thread_fields() {
        let mut value = minimal_thread();
        value["aFutureField"] = json!({ "anything": 1 });
        let thread: AppThread = serde_json::from_value(value).unwrap();
        assert_eq!(thread.id.as_str(), "thread-1");
    }

    #[test]
    fn forks_and_lineage() {
        let forked: ForkSource = serde_json::from_value(json!({
            "type": "provider_thread",
            "providerThreadId": "pt-1"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&forked).unwrap(),
            json!({ "type": "provider_thread", "providerThreadId": "pt-1" })
        );

        let lineage = AppThreadLineage {
            parent_thread_id: Some(ThreadId::new("p")),
            relationship_to_parent: Some(RelationshipToParent::Subagent),
            root_thread_id: ThreadId::new("p"),
            side_chat: None,
        };
        assert!(!is_listed_thread_lineage(&lineage));
    }

    #[test]
    fn link_origins_below_full_access_are_fenced() {
        let origin: LinkOrigin = serde_json::from_value(json!({
            "sessionId": "s", "label": "l", "access": "auto"
        }))
        .unwrap();
        assert!(link_origin_is_fenced(&origin));
        let full: LinkOrigin = serde_json::from_value(json!({
            "sessionId": "s", "label": "l", "access": "full-access"
        }))
        .unwrap();
        assert!(!link_origin_is_fenced(&full));
    }

    // Ported from packages/contracts/src/orchestrationV2.test.ts.
    #[test]
    fn labels_agent_and_scheduled_messages_for_the_provider() {
        let sender_thread = ThreadId::new("t");
        let linked = LinkedSender {
            environment_id: EnvironmentId::new("e"),
            thread_id: ThreadId::new("r"),
            title: "x".to_owned(),
        };
        let scheduled = ScheduledTaskId::new("s");

        let cases: Vec<(MessageSender<'_>, &str, String)> = vec![
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Mcp,
                    scheduled_task_id: None,
                    sender_thread_id: Some(&sender_thread),
                    linked_sender: None,
                },
                "Hi",
                "[T3 Code: the agent in T3 thread \"t\" sent this message, not the user.]\n\nHi"
                    .to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Mcp,
                    scheduled_task_id: None,
                    sender_thread_id: Some(&sender_thread),
                    linked_sender: None,
                },
                "",
                "[T3 Code: the agent in T3 thread \"t\" sent this message, not the user.]"
                    .to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Mcp,
                    scheduled_task_id: None,
                    sender_thread_id: Some(&sender_thread),
                    linked_sender: None,
                },
                " /git/review staged",
                " /git/review staged".to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Mcp,
                    scheduled_task_id: None,
                    sender_thread_id: Some(&sender_thread),
                    linked_sender: None,
                },
                "$deploy ship it",
                "$deploy ship it".to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Mcp,
                    scheduled_task_id: None,
                    sender_thread_id: Some(&sender_thread),
                    linked_sender: None,
                },
                "$25 left",
                "[T3 Code: the agent in T3 thread \"t\" sent this message, not the user.]\n\n$25 left"
                    .to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Mcp,
                    scheduled_task_id: None,
                    sender_thread_id: None,
                    linked_sender: Some(&linked),
                },
                "Hi",
                "[T3 Code: the agent in T3 thread \"r\" on linked environment \"e\" sent this message, not the user.]\n\nHi"
                    .to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::User,
                    creation_source: CreationSource::Web,
                    scheduled_task_id: Some(&scheduled),
                    sender_thread_id: None,
                    linked_sender: None,
                },
                "Hi",
                "[T3 Code: scheduled task \"s\" sent this prompt, not the user typing in this chat.]\n\nHi"
                    .to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Mcp,
                    scheduled_task_id: None,
                    sender_thread_id: None,
                    linked_sender: None,
                },
                "Hi",
                "[T3 Code: this message came through T3 Code's tools, not from the user typing in this chat.]\n\nHi"
                    .to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::User,
                    creation_source: CreationSource::Web,
                    scheduled_task_id: None,
                    sender_thread_id: None,
                    linked_sender: None,
                },
                "Hi",
                "Hi".to_owned(),
            ),
            (
                MessageSender {
                    created_by: Actor::Agent,
                    creation_source: CreationSource::Server,
                    scheduled_task_id: None,
                    sender_thread_id: None,
                    linked_sender: None,
                },
                "Hi",
                "Hi".to_owned(),
            ),
        ];

        for (message, text, expected) in cases {
            assert_eq!(with_message_sender_note(&message, text), expected);
        }
    }

    #[test]
    fn history_origin_keeps_the_underscored_wire_value() {
        assert_eq!(
            serde_json::to_value(ThreadHistoryOrigin::V1Import).unwrap(),
            json!("v1_import")
        );
        assert_eq!(
            serde_json::to_value(ThreadHistoryOrigin::Native).unwrap(),
            json!("native")
        );
    }

    #[test]
    fn a_visit_changes_only_read_state() {
        let thread: AppThread =
            serde_json::from_value(minimal_thread()).expect("the minimal thread decodes");
        let visited = apply_thread_visit(
            thread.clone(),
            ThreadVisit {
                last_visited_at: Some("2026-04-21T00:00:00.000Z".to_owned()),
                opens: None,
            },
        );
        assert_eq!(
            visited.last_visited_at.as_deref(),
            Some("2026-04-21T00:00:00.000Z")
        );
        assert_eq!(visited.id, thread.id);
    }
}
