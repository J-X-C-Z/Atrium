//! The app's live engine projector.
//!
//! Engine-backed sessions already send the same [`AgentEvent`]s the legacy
//! path folds into a `Transcript`. This module hosts the projection half of
//! the engine's [`EventStore`] in the app, so the chat list can read
//! `ThreadProjection` / `TurnProjection` / pending approvals instead of
//! guessing live state from the transcript. Non-engine sessions keep the
//! legacy transcript path.
//!
//! Everything here is GPUI-free: an event plus an injected `now` decides what
//! projection rows change, and tests drive it without a window.

use std::sync::Mutex;

use atrium_core::{AgentEvent, RequestId, ThreadMeta, TurnStatus};
use atrium_engine::store::{
    EventStore, PendingApprovalProjection, ProjectionStore, StoreError, ThreadProjection,
    TurnProjection,
};
use jiff::Timestamp;

use crate::engine_view::{ProjectedChat, chat_from_projection};

/// A live projection store fed by the engine's events.
pub struct EngineProjector {
    store: Mutex<EventStore>,
}

impl EngineProjector {
    /// An in-memory store; projections are rebuilt as events arrive.
    ///
    /// This is live UI state, not a second conversation store: the engine's
    /// own persistence remains the source of truth. Restarts re-learn state
    /// from the sessions the app opens.
    pub fn new() -> Result<Self, StoreError> {
        Ok(Self {
            store: Mutex::new(EventStore::in_memory()?),
        })
    }

    /// Folds one event, using the wall clock for its timestamps.
    pub fn apply(&self, event: &AgentEvent) {
        self.apply_at(event, Timestamp::now());
    }

    /// Folds one event with an injected clock, so tests are deterministic.
    pub fn apply_at(&self, event: &AgentEvent, now: Timestamp) {
        // Streaming deltas are the overwhelming majority; they carry nothing
        // the live projections read, so do not touch SQLite for them.
        if !matches!(
            event,
            AgentEvent::ThreadStarted(_)
                | AgentEvent::ThreadNameUpdated { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::ApprovalRequested(_)
                | AgentEvent::UserInputRequested(_)
                | AgentEvent::ApprovalResolved { .. }
        ) {
            return;
        }
        let store = self.store.lock().expect("engine projector");
        let projections = store.projections();
        let now_iso = now.to_string();
        match event {
            AgentEvent::ThreadStarted(meta) => {
                upsert_thread(&projections, thread_from_meta(meta, &now_iso));
            }
            AgentEvent::ThreadNameUpdated { thread_id, name } => {
                let mut thread = existing_thread(&projections, thread_id, &now_iso);
                thread.title = name.clone().unwrap_or_default();
                thread.updated_at = now_iso;
                upsert_thread(&projections, thread);
            }
            AgentEvent::TurnStarted { thread_id, turn_id } => {
                touch_thread(&projections, thread_id, &now_iso);
                let existing = projections.get_turn(thread_id, turn_id).ok().flatten();
                let (requested_at, pending_message_id, assistant_message_id) = existing
                    .map(|turn| {
                        (
                            turn.requested_at,
                            turn.pending_message_id,
                            turn.assistant_message_id,
                        )
                    })
                    .unwrap_or_else(|| (now_iso.clone(), None, None));
                let _ = projections.upsert_turn(&TurnProjection {
                    thread_id: thread_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    pending_message_id,
                    assistant_message_id,
                    state: "running".into(),
                    requested_at,
                    started_at: Some(now_iso.clone()),
                    completed_at: None,
                    checkpoint_turn_count: None,
                    checkpoint_ref: None,
                    checkpoint_status: None,
                    checkpoint_files_json: "[]".into(),
                });
            }
            AgentEvent::TurnCompleted {
                thread_id,
                turn_id,
                status,
                ..
            } => {
                touch_thread(&projections, thread_id, &now_iso);
                let existing = projections.get_turn(thread_id, turn_id).ok().flatten();
                let (requested_at, started_at, pending_message_id, assistant_message_id) = existing
                    .map(|turn| {
                        (
                            turn.requested_at,
                            turn.started_at,
                            turn.pending_message_id,
                            turn.assistant_message_id,
                        )
                    })
                    .unwrap_or_else(|| (now_iso.clone(), Some(now_iso.clone()), None, None));
                let _ = projections.upsert_turn(&TurnProjection {
                    thread_id: thread_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    pending_message_id,
                    assistant_message_id,
                    state: turn_state(*status).into(),
                    requested_at,
                    started_at,
                    completed_at: Some(now_iso.clone()),
                    checkpoint_turn_count: None,
                    checkpoint_ref: None,
                    checkpoint_status: None,
                    checkpoint_files_json: "[]".into(),
                });
                // Nothing can still be waiting once the turn is over.
                let pending = projections
                    .list_pending_approvals(thread_id, "pending")
                    .unwrap_or_default();
                for row in pending
                    .into_iter()
                    .filter(|row| row.turn_id.as_deref() == Some(turn_id.as_str()))
                {
                    resolve(&projections, row, &now_iso, None);
                }
            }
            AgentEvent::ApprovalRequested(request) => {
                touch_thread(&projections, &request.thread_id, &now_iso);
                upsert_request(
                    &projections,
                    &request.thread_id,
                    request_id_text(&request.request_id),
                    Some(request.turn_id.clone()).filter(|turn| !turn.is_empty()),
                    &now_iso,
                );
            }
            AgentEvent::UserInputRequested(request) => {
                touch_thread(&projections, &request.thread_id, &now_iso);
                upsert_request(
                    &projections,
                    &request.thread_id,
                    request_id_text(&request.request_id),
                    Some(request.turn_id.clone()).filter(|turn| !turn.is_empty()),
                    &now_iso,
                );
            }
            AgentEvent::ApprovalResolved {
                thread_id,
                request_id,
            } => {
                let wanted = request_id_text(request_id);
                let pending = projections
                    .list_pending_approvals(thread_id, "pending")
                    .unwrap_or_default();
                if let Some(row) = pending.into_iter().find(|row| row.request_id == wanted) {
                    resolve(&projections, row, &now_iso, None);
                }
            }
            _ => {}
        }
    }

    /// The thread row, when the projector has seen this thread.
    #[cfg(test)]
    pub fn thread(&self, thread_id: &str) -> Option<ThreadProjection> {
        let store = self.store.lock().expect("engine projector");
        store.projections().get_thread(thread_id).ok().flatten()
    }

    /// The thread's turns, oldest first.
    #[cfg(test)]
    pub fn turns(&self, thread_id: &str) -> Vec<TurnProjection> {
        let store = self.store.lock().expect("engine projector");
        store
            .projections()
            .list_turns(thread_id)
            .unwrap_or_default()
    }

    /// Requests still waiting on the user (approvals and questions).
    #[cfg(test)]
    pub fn pending_approvals(&self, thread_id: &str) -> usize {
        let store = self.store.lock().expect("engine projector");
        store
            .projections()
            .list_pending_approvals(thread_id, "pending")
            .map(|rows| rows.len())
            .unwrap_or(0)
    }

    /// What the chat list should show for this thread, if it is known.
    pub fn chat(&self, thread_id: &str) -> Option<ProjectedChat> {
        let store = self.store.lock().expect("engine projector");
        let projections = store.projections();
        let thread = projections.get_thread(thread_id).ok().flatten()?;
        let turns = projections.list_turns(thread_id).unwrap_or_default();
        let pending = projections
            .list_pending_approvals(thread_id, "pending")
            .map(|rows| rows.len())
            .unwrap_or(0);
        Some(chat_from_projection(&thread, &turns, pending))
    }
}

impl Default for EngineProjector {
    fn default() -> Self {
        Self::new().expect("an in-memory projection store")
    }
}

fn upsert_thread(projections: &ProjectionStore<'_>, row: ThreadProjection) {
    let _ = projections.upsert_thread(&row);
}

fn existing_thread(
    projections: &ProjectionStore<'_>,
    thread_id: &str,
    now_iso: &str,
) -> ThreadProjection {
    projections
        .get_thread(thread_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| skeleton_thread(thread_id, now_iso))
}

fn skeleton_thread(thread_id: &str, now_iso: &str) -> ThreadProjection {
    ThreadProjection {
        thread_id: thread_id.to_string(),
        project_id: String::new(),
        title: String::new(),
        default_provider: String::new(),
        provider_instance_id: String::new(),
        runtime_mode: "default".into(),
        interaction_mode: "default".into(),
        active_provider_thread_id: None,
        created_at: now_iso.to_string(),
        updated_at: now_iso.to_string(),
        archived_at: None,
        deleted_at: None,
        payload_json: "{}".into(),
    }
}

fn touch_thread(projections: &ProjectionStore<'_>, thread_id: &str, now_iso: &str) {
    let mut thread = existing_thread(projections, thread_id, now_iso);
    thread.updated_at = now_iso.to_string();
    upsert_thread(projections, thread);
}

fn thread_from_meta(meta: &ThreadMeta, now_iso: &str) -> ThreadProjection {
    ThreadProjection {
        thread_id: meta.id.clone(),
        project_id: String::new(),
        title: meta.title(),
        default_provider: meta.backend.clone(),
        provider_instance_id: meta.backend.clone(),
        runtime_mode: "default".into(),
        interaction_mode: "default".into(),
        active_provider_thread_id: None,
        created_at: iso_second(meta.created_at, now_iso),
        updated_at: iso_second(meta.updated_at, now_iso),
        archived_at: None,
        deleted_at: None,
        payload_json: "{}".into(),
    }
}

fn upsert_request(
    projections: &ProjectionStore<'_>,
    thread_id: &str,
    request_id: String,
    turn_id: Option<String>,
    now_iso: &str,
) {
    let _ = projections.upsert_pending_approval(&PendingApprovalProjection {
        request_id,
        thread_id: thread_id.to_string(),
        turn_id,
        status: "pending".into(),
        decision: None,
        created_at: now_iso.to_string(),
        resolved_at: None,
    });
}

fn resolve(
    projections: &ProjectionStore<'_>,
    row: PendingApprovalProjection,
    now_iso: &str,
    decision: Option<String>,
) {
    let _ = projections.upsert_pending_approval(&PendingApprovalProjection {
        status: "resolved".into(),
        decision,
        resolved_at: Some(now_iso.to_string()),
        ..row
    });
}

fn iso_second(seconds: i64, fallback: &str) -> String {
    Timestamp::from_second(seconds)
        .map(|at| at.to_string())
        .unwrap_or_else(|_| fallback.to_string())
}

fn turn_state(status: TurnStatus) -> &'static str {
    match status {
        TurnStatus::InProgress => "running",
        TurnStatus::Completed => "completed",
        TurnStatus::Interrupted => "interrupted",
        TurnStatus::Failed => "failed",
    }
}

fn request_id_text(request: &RequestId) -> String {
    match request {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::{ApprovalKind, ApprovalRequest, RequestId, ThreadMeta, UserInputRequest};
    use serde_json::json;

    fn now() -> Timestamp {
        Timestamp::from_second(1_000).unwrap()
    }

    fn meta() -> ThreadMeta {
        ThreadMeta {
            id: "t1".into(),
            backend: "codex".into(),
            name: Some("Fix the build".into()),
            preview: "fix it".into(),
            cwd: "/work".into(),
            model: Some("gpt-5".into()),
            created_at: 900,
            updated_at: 950,
        }
    }

    fn turn_started() -> AgentEvent {
        AgentEvent::TurnStarted {
            thread_id: "t1".into(),
            turn_id: "turn-1".into(),
        }
    }

    fn turn_completed(status: TurnStatus) -> AgentEvent {
        AgentEvent::TurnCompleted {
            thread_id: "t1".into(),
            turn_id: "turn-1".into(),
            status,
            error: None,
            duration_ms: Some(10),
        }
    }

    fn approval(id: &str) -> AgentEvent {
        AgentEvent::ApprovalRequested(ApprovalRequest {
            request_id: json!(id),
            thread_id: "t1".into(),
            turn_id: "turn-1".into(),
            item_id: "item-1".into(),
            kind: ApprovalKind::Command {
                command: "cargo test".into(),
                cwd: "/work".into(),
                reason: None,
            },
        })
    }

    #[test]
    fn a_live_turn_reads_working_then_waiting_then_idle() {
        let projector = EngineProjector::new().expect("projector");
        projector.apply_at(&AgentEvent::ThreadStarted(meta()), now());
        assert_eq!(
            projector.chat("t1").expect("chat").activity,
            crate::engine_view::ChatActivity::Idle
        );

        projector.apply_at(&turn_started(), now());
        assert_eq!(
            projector.chat("t1").expect("chat").activity,
            crate::engine_view::ChatActivity::Working
        );

        projector.apply_at(&approval("req-1"), now());
        assert_eq!(projector.pending_approvals("t1"), 1);
        assert_eq!(
            projector.chat("t1").expect("chat").activity,
            crate::engine_view::ChatActivity::Waiting
        );

        projector.apply_at(
            &AgentEvent::ApprovalResolved {
                thread_id: "t1".into(),
                request_id: RequestId::from(json!("req-1")),
            },
            now(),
        );
        assert_eq!(projector.pending_approvals("t1"), 0);
        assert_eq!(
            projector.chat("t1").expect("chat").activity,
            crate::engine_view::ChatActivity::Working
        );

        projector.apply_at(&turn_completed(TurnStatus::Completed), now());
        assert_eq!(
            projector.chat("t1").expect("chat").activity,
            crate::engine_view::ChatActivity::Idle
        );
        let turns = projector.turns("t1");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].state, "completed");
    }

    #[test]
    fn a_finished_turn_resolves_what_was_waiting_on_it() {
        let projector = EngineProjector::new().expect("projector");
        projector.apply_at(&AgentEvent::ThreadStarted(meta()), now());
        projector.apply_at(&turn_started(), now());
        projector.apply_at(&approval("req-1"), now());
        projector.apply_at(
            &AgentEvent::UserInputRequested(UserInputRequest {
                request_id: json!("q-1"),
                thread_id: "t1".into(),
                turn_id: "turn-1".into(),
                item_id: "item-1".into(),
                questions: Vec::new(),
            }),
            now(),
        );
        assert_eq!(projector.pending_approvals("t1"), 2);

        projector.apply_at(&turn_completed(TurnStatus::Failed), now());
        assert_eq!(projector.pending_approvals("t1"), 0);
        let turns = projector.turns("t1");
        assert_eq!(turns[0].state, "failed");
        assert!(turns[0].completed_at.is_some());
    }

    #[test]
    fn a_title_update_keeps_the_thread_row_and_moves_it_up() {
        let projector = EngineProjector::new().expect("projector");
        projector.apply_at(&AgentEvent::ThreadStarted(meta()), now());
        let later = Timestamp::from_second(1_200).unwrap();
        projector.apply_at(
            &AgentEvent::ThreadNameUpdated {
                thread_id: "t1".into(),
                name: Some("Green build".into()),
            },
            later,
        );
        let thread = projector.thread("t1").expect("thread");
        assert_eq!(thread.title, "Green build");
        assert_eq!(thread.default_provider, "codex");
        assert_eq!(
            thread.created_at,
            Timestamp::from_second(900).unwrap().to_string()
        );
        assert_eq!(thread.updated_at, later.to_string());
    }

    #[test]
    fn a_turn_arriving_before_any_thread_metadata_still_projects() {
        let projector = EngineProjector::new().expect("projector");
        projector.apply_at(&turn_started(), now());
        // Events out of order fill a skeleton rather than dropping the turn.
        assert_eq!(
            projector.chat("t1").expect("chat").activity,
            crate::engine_view::ChatActivity::Working
        );
        projector.apply_at(&AgentEvent::ThreadStarted(meta()), now());
        let thread = projector.thread("t1").expect("thread");
        assert_eq!(thread.title, "Fix the build");
    }
}
