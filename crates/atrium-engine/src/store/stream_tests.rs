// Ported from apps/server/src/orchestration-v2/ThreadStream.test.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/LiveStreamBudget.test.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/threadHistoryPaging.test.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/boundedSnapshotTransport.test.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/ProjectionRecovery.test.ts (T3 Code v0.1.11, MIT).
//!
//! Store-level cases for thread resume, bounded snapshots, the live budget and
//! restart recovery.

use std::cell::RefCell;

use rusqlite::params;
use serde_json::{Value, json};

use super::*;
use crate::contracts::{
    Actor, AppThread, ProjectedItemVisibility, ProjectedTurnItem, RunAttempt, ThreadId, TurnItemId,
};
use crate::store::events::{ApplyCommand, EventStore, NewEvent, NewProjectEvent, StoreError};
use crate::store::projections::{ProjectionWrite, ThreadProjection};

const NOW: &str = "2026-09-08T00:00:00.000Z";
const T1: &str = "2026-09-08T00:00:01.000Z";
const THREAD: &str = "thread-stream-test";

fn thread_id() -> ThreadId {
    ThreadId::new(THREAD)
}

// ---------------------------------------------------------------------------
// Fixtures (the T3 test helpers)
// ---------------------------------------------------------------------------

fn item_value(index: usize, output_bytes: Option<usize>) -> Value {
    let id = format!("item-{index}");
    let output = output_bytes
        .map(|bytes| "x".repeat(bytes))
        .unwrap_or_else(|| format!("output-{index}"));
    json!({
        "id": id,
        "type": "command_execution",
        "threadId": THREAD,
        "runId": null,
        "nodeId": null,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": index + 1,
        "status": "completed",
        "title": format!("Command {index}"),
        "input": format!("cmd-{index}"),
        "output": output,
        "exitCode": 0,
        "startedAt": NOW,
        "completedAt": NOW,
        "updatedAt": NOW,
    })
}

fn make_row(index: usize, output_bytes: Option<usize>) -> ProjectedTurnItem {
    let id = format!("item-{index}");
    ProjectedTurnItem {
        position: index as u32,
        visibility: ProjectedItemVisibility::Local,
        source_thread_id: thread_id(),
        source_item_id: TurnItemId::new(&id),
        item: serde_json::from_value(item_value(index, output_bytes)).expect("command item"),
    }
}

fn make_turn_start(index: usize, created_by: Actor) -> ProjectedTurnItem {
    let mut row = make_row(index, None);
    let message_id = format!("prompt-{index}");
    let item = json!({
        "id": format!("item-{index}"),
        "type": "user_message",
        "threadId": THREAD,
        "runId": null,
        "nodeId": null,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": index + 1,
        "status": "completed",
        "title": null,
        "createdBy": match created_by { Actor::User => "user", Actor::Agent => "agent", Actor::System => "system" },
        "creationSource": "provider",
        "inputIntent": "turn_start",
        "messageId": message_id,
        "text": format!("Prompt {index}"),
        "attachments": [],
        "startedAt": NOW,
        "completedAt": NOW,
        "updatedAt": NOW,
    });
    row.item = serde_json::from_value(item).expect("turn start");
    row
}

fn interrupt_row(index: usize, type_: &str) -> ProjectedTurnItem {
    let id = if type_ == "run_interrupt_request" {
        "interrupt-req"
    } else {
        "interrupt-res"
    };
    let item = json!({
        "id": id,
        "type": type_,
        "threadId": THREAD,
        "runId": "run-interrupt-1",
        "nodeId": "node-interrupt-1",
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": index + 1,
        "status": "completed",
        "title": type_,
        "message": if type_ == "run_interrupt_request" { "Stopping" } else { "Stopped" },
        "startedAt": NOW,
        "completedAt": NOW,
        "updatedAt": NOW,
    });
    ProjectedTurnItem {
        position: index as u32,
        visibility: ProjectedItemVisibility::Local,
        source_thread_id: thread_id(),
        source_item_id: TurnItemId::new(id),
        item: serde_json::from_value(item).expect("interrupt item"),
    }
}

fn thread_value(thread: &str) -> Value {
    json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": thread,
        "projectId": "project-1",
        "title": "Thread",
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": {
            "parentThreadId": null,
            "relationshipToParent": null,
            "rootThreadId": thread,
        },
        "forkedFrom": null,
        "createdAt": NOW,
        "updatedAt": NOW,
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "deletedAt": null,
    })
}

fn app_thread(thread: &str) -> AppThread {
    serde_json::from_value(thread_value(thread)).expect("app thread")
}

fn make_projection(visible_turn_items: Vec<ProjectedTurnItem>) -> ThreadProjectionSnapshot {
    let runs = vec![
        serde_json::from_value(json!({
            "id": "run-1",
            "threadId": THREAD,
            "ordinal": 1,
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
            "providerThreadId": null,
            "userMessageId": "message-run-1",
            "rootNodeId": null,
            "activeAttemptId": null,
            "status": "completed",
            "requestedAt": NOW,
            "startedAt": NOW,
            "completedAt": NOW,
            "checkpointId": null,
            "contextHandoffId": null,
        }))
        .expect("run"),
    ];
    let attempts = vec![
        serde_json::from_value::<RunAttempt>(json!({
            "id": "attempt-1",
            "runId": "run-1",
            "attemptOrdinal": 1,
            "rootNodeId": "node-1",
            "providerInstanceId": "codex",
            "providerThreadId": "provider-thread-1",
            "providerTurnId": null,
            "reason": "initial",
            "status": "completed",
            "startedAt": NOW,
            "completedAt": NOW,
        }))
        .expect("attempt"),
    ];
    let runtime_requests = vec![
        serde_json::from_value(json!({
            "id": "req-1",
            "nodeId": "node-1",
            "providerTurnId": null,
            "nativeRequestRef": null,
            "kind": "command",
            "status": "resolved",
            "responseCapability": { "type": "not_resumable", "reason": "done" },
            "createdAt": NOW,
            "resolvedAt": NOW,
        }))
        .expect("runtime request"),
    ];
    ThreadProjectionSnapshot {
        runs,
        attempts,
        runtime_requests,
        turn_items: visible_turn_items
            .iter()
            .map(|row| row.item.clone())
            .collect(),
        visible_turn_items,
        ..ThreadProjectionSnapshot::empty(app_thread(THREAD), NOW.to_owned())
    }
}

fn timeline_projection(item_count: usize) -> ThreadProjectionSnapshot {
    make_projection((0..item_count).map(|index| make_row(index, None)).collect())
}

fn handoff_value(status: &str, summary_bytes: usize) -> Value {
    json!({
        "id": "side-chat-handoff",
        "transferId": "side-chat-transfer",
        "threadId": THREAD,
        "targetRunId": "side-chat-run",
        "fromProviderThreadIds": [],
        "toProviderThreadId": "side-chat-provider-thread",
        "coveredRunOrdinals": { "from": 1, "to": 1 },
        "strategy": "manual_context",
        "status": status,
        "summaryMessageId": null,
        "summaryText": "x".repeat(summary_bytes),
        "delivery": { "nativeThreadId": "native", "status": "inline", "itemIds": [] },
        "createdByProviderInstanceId": null,
        "createdAt": NOW,
        "updatedAt": NOW,
    })
}

// ---------------------------------------------------------------------------
// ThreadStream.test.ts
// ---------------------------------------------------------------------------

#[test]
fn replays_when_the_event_count_is_within_the_bound() {
    let plan = decide_thread_resume(
        10,
        20_000,
        THREAD_RESUME_MAX_REPLAY_EVENTS,
        THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES,
    );
    assert_eq!(
        plan,
        ThreadResumePlan::Replay {
            after_sequence: 10,
            through_sequence: 20_000,
        }
    );
}

#[test]
fn falls_back_to_a_snapshot_when_encoded_replay_bytes_exceed_the_bound() {
    assert_eq!(
        decide_thread_resume(10, 11, 1, THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES + 1,),
        ThreadResumePlan::Snapshot
    );
}

#[test]
fn falls_back_to_a_snapshot_when_the_client_cursor_is_ahead_of_the_store() {
    assert_eq!(
        decide_thread_resume(50, 40, 0, 0),
        ThreadResumePlan::Snapshot
    );
}

#[test]
fn builds_fallback_snapshots_with_a_bounded_timeline_and_history_cursor() {
    let mut projection = timeline_projection(80);
    projection.context_handoffs =
        vec![serde_json::from_value(handoff_value("ready", 8_000)).expect("context handoff")];
    let snapshot = build_bounded_thread_stream_snapshot(44, projection, false);

    assert_eq!(
        snapshot
            .projection
            .context_handoffs
            .iter()
            .map(|handoff| handoff.summary_text.as_str())
            .collect::<Vec<_>>(),
        vec![""]
    );
    assert_eq!(snapshot.snapshot_sequence, 44);
    assert_eq!(snapshot.projection.visible_turn_items.len(), 75);
    assert_eq!(snapshot.projection.turn_items.len(), 75);
    assert!(snapshot.history_cursor.is_some());
    assert!(snapshot.has_more_history);
    assert_eq!(snapshot.latest_local_turn_ordinal, Some(80));

    // The item keeps T3's `kind: snapshot` wire shape.
    let item = ThreadStreamItem::Snapshot {
        snapshot: Box::new(snapshot),
    };
    let value = serde_json::to_value(&item).unwrap();
    assert_eq!(value["kind"], json!("snapshot"));
    assert_eq!(value["snapshotSequence"], json!(44));
    assert_eq!(value["hasMoreHistory"], json!(true));
}

#[test]
fn rejects_a_ten_mib_raw_replay_even_when_its_projected_form_fits() {
    const LARGE_OUTPUT: usize = 10 * 1_048_576;
    let payload = json!({
        "id": "large-dynamic-tool",
        "type": "dynamic_tool",
        "threadId": THREAD,
        "runId": null,
        "nodeId": null,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": 1,
        "status": "completed",
        "title": "Large result",
        "toolName": "mcp__test__large_result",
        "input": { "query": "small" },
        "output": { "text": "x".repeat(LARGE_OUTPUT) },
        "startedAt": NOW,
        "completedAt": NOW,
        "updatedAt": NOW,
    });
    let raw_replay = vec![ThreadStreamItem::Event {
        sequence: 10,
        event: ThreadStreamEvent {
            event_type: "turn-item.updated".to_owned(),
            payload: payload.clone(),
        },
    }];
    let projected_replay = vec![ThreadStreamItem::Event {
        sequence: 10,
        event: ThreadStreamEvent {
            event_type: "turn-item.updated".to_owned(),
            payload: json!({
                "id": "large-dynamic-tool",
                "type": "dynamic_tool",
                "threadId": THREAD,
                "ordinal": 1,
                "status": "completed",
                "toolName": "mcp__test__large_result",
                "input": { "query": "small" },
                "outputOmitted": true,
                "updatedAt": NOW,
            }),
        },
    }];

    let raw_payload_bytes = serde_json::to_vec(&payload).unwrap().len();
    assert_eq!(raw_payload_bytes, 10_486_214);
    assert!(raw_payload_bytes > THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES);
    assert!(!is_thread_replay_raw_payload_safe(raw_payload_bytes));
    assert!(thread_replay_encoded_bytes(&raw_replay) > THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES);
    let projected_bytes = thread_replay_encoded_bytes(&projected_replay);
    assert!(projected_bytes <= 2_048);
    assert_eq!(
        decide_thread_resume(9, 10, projected_replay.len(), projected_bytes),
        ThreadResumePlan::Replay {
            after_sequence: 9,
            through_sequence: 10,
        }
    );
}

fn fork_projection() -> ThreadProjectionSnapshot {
    let base = timeline_projection(6);
    let local = base
        .visible_turn_items
        .iter()
        .skip(3)
        .enumerate()
        .map(|(index, row)| {
            let mut row = row.clone();
            row.position = index as u32;
            row
        })
        .collect::<Vec<_>>();
    let inherited = base
        .visible_turn_items
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, row)| {
            let id = TurnItemId::new(format!("parent-{index}"));
            let mut item_value = serde_json::to_value(&row.item).unwrap();
            item_value["id"] = json!(id.as_str());
            item_value["threadId"] = json!("thread-stream-parent");
            ProjectedTurnItem {
                position: index as u32,
                visibility: ProjectedItemVisibility::Inherited,
                source_thread_id: ThreadId::new("thread-stream-parent"),
                source_item_id: id,
                item: serde_json::from_value(item_value).unwrap(),
            }
        })
        .collect::<Vec<_>>();
    let request = serde_json::from_value(json!({
        "id": "interrupt-request",
        "type": "run_interrupt_request",
        "threadId": THREAD,
        "runId": null,
        "nodeId": null,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": 0,
        "status": "completed",
        "title": null,
        "message": "Stop",
        "startedAt": NOW,
        "completedAt": NOW,
        "updatedAt": NOW,
    }))
    .unwrap();
    let mut visible = inherited;
    visible.extend(local.iter().cloned());
    visible
        .iter_mut()
        .enumerate()
        .for_each(|(position, row)| row.position = position as u32);
    ThreadProjectionSnapshot {
        turn_items: std::iter::once(request)
            .chain(local.into_iter().map(|row| row.item))
            .collect(),
        visible_turn_items: visible,
        ..base
    }
}

fn assert_compact_restores_exact_snapshot(projection: ThreadProjectionSnapshot) {
    let full = build_bounded_thread_stream_snapshot(44, projection.clone(), false);
    let compact = build_bounded_thread_stream_snapshot(44, projection, true);

    assert!(full.turn_items_omit_local_visible.is_none());
    assert_eq!(compact.turn_items_omit_local_visible, Some(true));
    assert!(compact.projection.turn_items.len() < full.projection.turn_items.len());
    let mut compact_fields = compact.clone();
    compact_fields.projection = full.projection.clone();
    compact_fields.turn_items_omit_local_visible = None;
    assert_eq!(compact_fields, full);
    assert_eq!(bounded_snapshot_projection(compact), full.projection);
}

#[test]
fn restores_the_exact_opted_out_snapshot_for_linear_threads() {
    assert_compact_restores_exact_snapshot(timeline_projection(80));
}

#[test]
fn restores_the_exact_opted_out_snapshot_for_fork_threads_with_a_retained_interrupt() {
    assert_compact_restores_exact_snapshot(fork_projection());
}

// ---------------------------------------------------------------------------
// threadHistoryPaging.test.ts
// ---------------------------------------------------------------------------

#[test]
fn builds_a_bounded_projection_that_preserves_non_timeline_control_state() {
    let full = make_projection((0..40).map(|index| make_row(index, None)).collect());
    let bounded = build_bounded_thread_projection(
        full.clone(),
        7,
        Some(ThreadHistoryPagePolicy {
            max_user_turns: Some(10),
            max_items: 5,
            max_turn_items: Some(500),
            max_encoded_bytes: 10_000_000,
        }),
    );
    assert_eq!(bounded.projection.runs, full.runs);
    assert_eq!(bounded.projection.runtime_requests, full.runtime_requests);
    assert_eq!(bounded.projection.attempts, full.attempts);
    assert_eq!(bounded.projection.visible_turn_items.len(), 5);
    assert_eq!(bounded.projection.turn_items.len(), 5);
    assert!(bounded.has_more_history);
    assert!(bounded.history_cursor.is_some());
    assert_eq!(
        bounded
            .projection
            .visible_turn_items
            .iter()
            .map(|row| row.position)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    assert_eq!(bounded.latest_local_turn_ordinal, Some(40));
    assert_eq!(
        compute_latest_local_turn_ordinal(&full.turn_items),
        Some(40)
    );
}

#[test]
fn carries_a_full_projection_watermark_when_the_bounded_window_is_inherited_only() {
    let local_older = make_row(0, None);
    let local_mid = make_row(1, None);
    let mut inherited = make_row(2, None);
    inherited.visibility = ProjectedItemVisibility::Inherited;
    inherited.source_thread_id = ThreadId::new("parent-thread");
    let mut item_value = serde_json::to_value(&inherited.item).unwrap();
    item_value["threadId"] = json!("parent-thread");
    inherited.item = serde_json::from_value(item_value).unwrap();
    let mut full = make_projection(vec![
        local_older.clone(),
        local_mid.clone(),
        inherited.clone(),
    ]);
    full.visible_turn_items = vec![inherited.clone()];
    full.turn_items = vec![local_older.item, local_mid.item];
    let bounded = build_bounded_thread_projection(
        full,
        2,
        Some(ThreadHistoryPagePolicy {
            max_user_turns: Some(10),
            max_items: 1,
            max_turn_items: Some(500),
            max_encoded_bytes: 10_000_000,
        }),
    );
    assert_eq!(
        bounded
            .projection
            .visible_turn_items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec![inherited.source_item_id.as_str()]
    );
    assert_eq!(bounded.latest_local_turn_ordinal, Some(2));
    assert_eq!(compute_latest_local_turn_ordinal(&[]), None);
}

#[test]
fn retains_an_out_of_window_interrupt_request_needed_by_a_visible_result() {
    let request = interrupt_row(0, "run_interrupt_request");
    let filler = make_row(1, None);
    let result = interrupt_row(2, "run_interrupt_result");
    let full = make_projection(vec![request, filler, result]);
    let bounded = build_bounded_thread_projection(
        full,
        3,
        Some(ThreadHistoryPagePolicy {
            max_user_turns: Some(10),
            max_items: 1,
            max_turn_items: Some(500),
            max_encoded_bytes: 10_000_000,
        }),
    );
    assert_eq!(
        bounded
            .projection
            .visible_turn_items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["interrupt-res"]
    );
    let mut ids = bounded
        .projection
        .turn_items
        .iter()
        .filter_map(|item| item.id().map(|id| id.as_str().to_owned()))
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec!["interrupt-req", "interrupt-res"]);
    assert!(bounded.has_more_history);
}

#[test]
fn charges_local_row_item_duplication_when_measuring_bounded_timeline_bytes() {
    let rows = (0..8)
        .map(|index| make_row(index, Some(2_000)))
        .collect::<Vec<_>>();
    let full = make_projection(rows.clone());
    let max_encoded_bytes = 12_000;
    let bounded = build_bounded_thread_projection(
        full,
        1,
        Some(ThreadHistoryPagePolicy {
            max_user_turns: Some(10),
            max_items: 50,
            max_turn_items: Some(500),
            max_encoded_bytes,
        }),
    );
    let contribution = bounded_timeline_encoded_bytes(
        &bounded.projection.visible_turn_items,
        &bounded.projection.turn_items,
    );
    assert!(contribution <= max_encoded_bytes);
    assert!(!bounded.projection.visible_turn_items.is_empty());
    assert!(bounded.projection.visible_turn_items.len() < rows.len());

    let history_page = select_recent_timeline_window(
        &rows,
        1,
        ThreadHistoryPagePolicy {
            max_user_turns: Some(10),
            max_items: 50,
            max_turn_items: Some(500),
            max_encoded_bytes,
        },
        None,
    );
    let history_bytes: usize = history_page
        .items
        .iter()
        .map(projected_row_encoded_bytes)
        .sum();
    assert!(history_bytes <= max_encoded_bytes);
    assert!(history_page.items.len() > bounded.projection.visible_turn_items.len());
    assert!(
        projected_row_bounded_snapshot_encoded_bytes(&rows[0], &thread_id())
            > projected_row_encoded_bytes(&rows[0])
    );
}

fn message_value(index: usize, workflow_script: Option<bool>) -> Value {
    let mut value = json!({
        "id": format!("message-{index}"),
        "threadId": THREAD,
        "runId": format!("old-run-{index}"),
        "nodeId": null,
        "role": "assistant",
        "text": "x".repeat(1_000),
        "attachments": [],
        "streaming": false,
        "createdBy": "agent",
        "creationSource": "provider",
        "createdAt": NOW,
        "updatedAt": NOW,
    });
    if let Some(flag) = workflow_script {
        value["workflowScript"] = json!(flag);
    }
    value
}

#[test]
fn does_not_carry_the_full_duplicated_message_table_into_a_bounded_snapshot() {
    let mut projection = make_projection((0..120).map(|index| make_row(index, None)).collect());
    projection.messages = (0..2_000)
        .map(|index| serde_json::from_value(message_value(index, None)).unwrap())
        .collect();
    let bounded = build_bounded_thread_projection(projection, 9, None);
    assert!(bounded.projection.messages.is_empty());
    assert!(
        serde_json::to_vec(&bounded.projection).unwrap().len()
            < THREAD_HISTORY_PAGE_POLICY.max_encoded_bytes + 100_000
    );
}

#[test]
fn keeps_a_workflow_coordinators_script_outside_the_bounded_window() {
    let mut projection = make_projection((0..120).map(|index| make_row(index, None)).collect());
    projection.messages = vec![
        serde_json::from_value(message_value(0, Some(true))).unwrap(),
        serde_json::from_value(message_value(1, Some(false))).unwrap(),
        serde_json::from_value(message_value(2, None)).unwrap(),
    ];
    let bounded = build_bounded_thread_projection(projection, 9, None);
    assert_eq!(
        bounded
            .projection
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        vec!["message-0"]
    );
}

#[test]
fn omits_historical_control_details_that_remain_available_from_history_items() {
    let mut projection = make_projection((0..200).map(|index| make_row(index, None)).collect());
    projection.plans = (0..120)
        .map(|index| {
            serde_json::from_value(json!({
                "id": format!("plan-{index}"),
                "threadId": THREAD,
                "runId": null,
                "nodeId": format!("node-{index}"),
                "status": "completed",
                "kind": "proposed_plan",
                "markdown": "p".repeat(20_000),
            }))
            .unwrap()
        })
        .collect();
    projection.context_handoffs = (0..120)
        .map(|index| {
            let mut value = handoff_value("superseded", 20_000);
            value["id"] = json!(format!("handoff-{index}"));
            serde_json::from_value(value).unwrap()
        })
        .collect();
    let bounded = project_thread_projection_for_wire(projection.clone());
    let bounded = build_bounded_thread_projection(bounded, 9, None);
    assert!(
        serde_json::to_vec(&bounded.projection).unwrap().len()
            <= THREAD_HISTORY_PAGE_POLICY.max_encoded_bytes
    );
    assert!(bounded.projection.plans.is_empty());
    assert!(bounded.projection.context_handoffs.is_empty());
    assert_eq!(projection.plans.len(), 120);
    assert_eq!(
        match &projection.plans[0] {
            crate::contracts::PlanArtifact::ProposedPlan(plan) => plan.markdown.len(),
            crate::contracts::PlanArtifact::TodoList(_) => 0,
        },
        20_000
    );
}

#[test]
fn preserves_oversized_actionable_state() {
    let projection = make_projection(vec![make_row(0, None)]);
    let mut actionable = projection.clone();
    actionable.plans = vec![
        serde_json::from_value(json!({
            "id": "plan-actionable",
            "threadId": THREAD,
            "runId": null,
            "nodeId": "node-actionable",
            "status": "active",
            "kind": "proposed_plan",
            "markdown": "a".repeat(2_097_152),
        }))
        .unwrap(),
    ];
    let bounded = build_bounded_thread_projection(actionable.clone(), 9, None);
    assert_eq!(bounded.projection.plans, actionable.plans);
    assert_eq!(
        bounded.projection.visible_turn_items,
        actionable.visible_turn_items
    );
}

#[test]
fn measures_only_the_rows_needed_to_stop_at_the_ceiling() {
    struct Case {
        name: &'static str,
        items: Vec<ProjectedTurnItem>,
        policy: ThreadHistoryPagePolicy,
        admitted: usize,
        measured: usize,
    }
    let cases = vec![
        Case {
            name: "item ceiling",
            items: (0..5).map(|index| make_row(index, None)).collect(),
            policy: ThreadHistoryPagePolicy {
                max_user_turns: Some(10),
                max_items: 3,
                max_turn_items: Some(500),
                max_encoded_bytes: 1_000,
            },
            admitted: 3,
            measured: 3,
        },
        Case {
            name: "user turn ceiling",
            items: vec![
                make_row(0, None),
                make_turn_start(1, Actor::User),
                make_row(2, None),
            ],
            policy: ThreadHistoryPagePolicy {
                max_user_turns: Some(1),
                max_items: 100,
                max_turn_items: Some(500),
                max_encoded_bytes: 1_000,
            },
            admitted: 2,
            measured: 2,
        },
        Case {
            name: "raw turn ceiling",
            items: std::iter::once(make_turn_start(0, Actor::User))
                .chain((1..152).map(|index| make_turn_start(index, Actor::Agent)))
                .collect(),
            policy: ThreadHistoryPagePolicy {
                max_user_turns: Some(10),
                max_items: 1_000,
                max_turn_items: None,
                max_encoded_bytes: 1_000_000,
            },
            admitted: 150,
            measured: 150,
        },
        Case {
            name: "byte budget, which still admits an exact fit",
            items: (0..5).map(|index| make_row(index, None)).collect(),
            policy: ThreadHistoryPagePolicy {
                max_user_turns: Some(10),
                max_items: 100,
                max_turn_items: Some(500),
                max_encoded_bytes: 20,
            },
            admitted: 2,
            measured: 3,
        },
        Case {
            name: "byte budget, which still admits an oversized first row",
            items: (0..5).map(|index| make_row(index, None)).collect(),
            policy: ThreadHistoryPagePolicy {
                max_user_turns: Some(10),
                max_items: 100,
                max_turn_items: Some(500),
                max_encoded_bytes: 5,
            },
            admitted: 1,
            measured: 2,
        },
    ];
    for case in cases {
        let measured_ids: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let page = select_recent_timeline_window(
            &case.items,
            1,
            case.policy,
            Some(&|row| {
                measured_ids
                    .borrow_mut()
                    .push(row.source_item_id.as_str().to_owned());
                10
            }),
        );
        assert_eq!(
            page.items
                .iter()
                .map(|row| row.source_item_id.as_str())
                .collect::<Vec<_>>(),
            case.items[case.items.len() - case.admitted..]
                .iter()
                .map(|row| row.source_item_id.as_str())
                .collect::<Vec<_>>(),
            "{}",
            case.name
        );
        assert_eq!(measured_ids.borrow().len(), case.measured, "{}", case.name);
        assert!(page.has_more_history, "{}", case.name);
        assert_eq!(
            decode_thread_history_cursor(page.next_cursor.as_deref().unwrap())
                .unwrap()
                .source_item_id,
            page.items[0].source_item_id.as_str(),
            "{}",
            case.name
        );
    }
}

#[test]
fn recovers_identity_miss_cursors_when_position_is_past_a_shrunken_timeline() {
    let items = (0..5)
        .map(|index| make_row(index, None))
        .collect::<Vec<_>>();
    let cursor = encode_thread_history_cursor(3, THREAD, "item-deleted-long-ago", 40, false);
    let page = select_history_page_from_cursor(
        &items,
        &cursor,
        3,
        Some(ThreadHistoryPagePolicy {
            max_user_turns: Some(10),
            max_items: 2,
            max_turn_items: Some(500),
            max_encoded_bytes: 10_000_000,
        }),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["item-3", "item-4"]
    );
    assert!(page.has_more_history);
    assert!(page.next_cursor.is_some());

    let sparse = vec![make_row(0, None), make_row(10, None), make_row(20, None)];
    let cursor = encode_thread_history_cursor(3, THREAD, "item-hidden", 15, false);
    let middle =
        select_history_page_from_cursor(&sparse, &cursor, 3, None, None, None, None, None).unwrap();
    assert_eq!(
        middle
            .items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["item-0", "item-10"]
    );
}

#[test]
fn keeps_cursor_resolution_stable_when_newer_items_append() {
    let initial = (0..20)
        .map(|index| make_row(index, None))
        .collect::<Vec<_>>();
    let recent = select_recent_timeline_window(
        &initial,
        2,
        ThreadHistoryPagePolicy {
            max_user_turns: Some(10),
            max_items: 5,
            max_turn_items: Some(500),
            max_encoded_bytes: 10_000_000,
        },
        None,
    );
    let cursor = recent.next_cursor.unwrap();
    let mut grown = initial;
    grown.push(make_row(20, None));
    grown.push(make_row(21, None));
    let older = select_history_page_from_cursor(
        &grown,
        &cursor,
        5,
        Some(ThreadHistoryPagePolicy {
            max_user_turns: Some(20),
            max_items: 5,
            max_turn_items: Some(500),
            max_encoded_bytes: 10_000_000,
        }),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        older
            .items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["item-10", "item-11", "item-12", "item-13", "item-14"]
    );
    assert!(older.has_more_history);
}

// ---------------------------------------------------------------------------
// LiveStreamBudget.test.ts
// ---------------------------------------------------------------------------

#[test]
fn retains_until_ack_and_refuses_an_overflowing_item() {
    let mut budget = LiveStreamBudget::new(LiveStreamLimits {
        max_items: 3,
        max_serialized_bytes: LIVE_STREAM_MAX_SERIALIZED_BYTES,
    });
    let first = budget.retain(json!({ "text": "first" })).unwrap();
    let second = budget.retain(json!({ "text": "second" })).unwrap();
    let third = budget.retain(json!({ "text": "third" })).unwrap();
    assert_eq!(budget.usage().retained_items, 3);
    assert_eq!(
        budget.retain(json!({ "text": "fourth" })).unwrap_err(),
        LiveStreamBufferError
    );
    assert!(budget.failed());
    budget.release([first, second]);
    assert_eq!(budget.usage().retained_items, 1);
    budget.release([third]);
    assert_eq!(
        budget.usage(),
        LiveStreamUsage {
            retained_items: 0,
            retained_serialized_bytes: 0,
        }
    );
}

#[test]
fn counts_an_unacknowledged_state_against_the_items_limit_when_its_replacement_arrives() {
    let first = json!({ "sequence": 1, "text": "é".repeat(20) });
    let next = json!({ "sequence": 2, "text": "é".repeat(20) });
    let mut buffer = LatestLiveStreamBuffer::new(LiveStreamLimits {
        max_items: 1,
        max_serialized_bytes: LIVE_STREAM_MAX_SERIALIZED_BYTES,
    });
    buffer.push("same-thread", first.clone()).unwrap();
    let delivered = buffer.drain().unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        buffer.push("same-thread", next).unwrap_err(),
        LiveStreamBufferError
    );
    assert!(buffer.failed());
    buffer.ack(delivered);
    assert_eq!(buffer.usage().retained_items, 0);
}

#[test]
fn counts_an_unacknowledged_state_against_the_bytes_limit_when_its_replacement_arrives() {
    let first = json!({ "sequence": 1, "text": "é".repeat(20) });
    let next = json!({ "sequence": 2, "text": "é".repeat(20) });
    let mut buffer = LatestLiveStreamBuffer::new(LiveStreamLimits {
        max_items: LIVE_STREAM_MAX_ITEMS,
        max_serialized_bytes: serde_json::to_vec(&first).unwrap().len(),
    });
    buffer.push("same-thread", first).unwrap();
    let delivered = buffer.drain().unwrap();
    assert_eq!(
        buffer.push("same-thread", next).unwrap_err(),
        LiveStreamBufferError
    );
    buffer.ack(delivered);
}

#[test]
fn coalesces_the_latest_state_per_key() {
    let values = buffer_latest_live_stream(
        vec![
            (
                "thread-a".to_owned(),
                json!({ "sequence": 1, "text": "one" }),
            ),
            (
                "thread-a".to_owned(),
                json!({ "sequence": 2, "text": "two" }),
            ),
            (
                "thread-b".to_owned(),
                json!({ "sequence": 3, "text": "three" }),
            ),
        ],
        LiveStreamLimits::default(),
    )
    .unwrap();
    assert_eq!(
        values
            .iter()
            .map(|value| value["sequence"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

// ---------------------------------------------------------------------------
// Store-level streams
// ---------------------------------------------------------------------------

fn open_store() -> (tempfile::TempDir, EventStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = EventStore::open(dir.path().join("store.sqlite")).expect("open store");
    (dir, store)
}

fn seed_thread_projection(store: &mut EventStore, thread: &str) {
    let thread_projection = ThreadProjection {
        thread_id: thread.to_owned(),
        project_id: "project-1".to_owned(),
        title: "Thread".to_owned(),
        default_provider: "codex".to_owned(),
        provider_instance_id: "codex".to_owned(),
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        active_provider_thread_id: None,
        created_at: NOW.to_owned(),
        updated_at: NOW.to_owned(),
        archived_at: None,
        deleted_at: None,
        payload_json: thread_value(thread).to_string(),
    };
    store
        .apply(
            ApplyCommand {
                command_id: "command:seed-thread",
                thread_id: thread,
                command_type: "thread.create",
                accepted_at: NOW,
            },
            &[],
            &[ProjectionWrite::Thread(thread_projection)],
        )
        .expect("seed projection");
}

fn append_events(
    store: &EventStore,
    thread: &str,
    count: usize,
) -> Vec<crate::contracts::StoredEvent> {
    let events = (0..count)
        .map(|index| {
            NewEvent::new(
                format!("event:{index}"),
                thread,
                "run.updated",
                NOW,
                json!({ "index": index }),
            )
        })
        .collect::<Vec<_>>();
    store.append(None, &events).expect("append events")
}

fn set_pruned_through(store: &EventStore, sequence: i64) {
    store
        .connection()
        .execute(
            "INSERT INTO orchestration_v2_projection_metadata (
                projection_name, schema_version, last_sequence, updated_at
             ) VALUES ('event-log-pruned-through', 2, ?1, ?2)
             ON CONFLICT(projection_name) DO UPDATE SET last_sequence = excluded.last_sequence",
            params![sequence, NOW],
        )
        .expect("metadata");
}

fn insert_turn_item(store: &EventStore, row: &ProjectedTurnItem) {
    store
        .connection()
        .execute(
            "INSERT INTO orchestration_v2_projection_turn_items (
                turn_item_id, thread_id, run_id, node_id, provider_thread_id,
                provider_turn_id, parent_item_id, ordinal, type, status, updated_at, payload_json
             ) VALUES (?1, ?2, NULL, NULL, NULL, NULL, NULL, ?3, ?4, 'completed', ?5, ?6)",
            params![
                row.source_item_id.as_str(),
                row.source_thread_id.as_str(),
                row.item.base().map(|base| base.ordinal).unwrap_or(0),
                row.item.type_name(),
                NOW,
                serde_json::to_string(&row.item).unwrap(),
            ],
        )
        .expect("insert turn item");
}

#[test]
fn resume_replays_the_stored_tail_after_a_client_cursor() {
    let (_dir, store) = open_store();
    append_events(&store, THREAD, 3);
    let resume = store.thread_stream(THREAD).resume(1).unwrap();
    assert_eq!(resume.high_water, 3);
    match resume.batch {
        ThreadStreamBatch::Replay {
            after_sequence,
            through_sequence,
            events,
        } => {
            assert_eq!(after_sequence, 1);
            assert_eq!(through_sequence, 3);
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                vec![2, 3]
            );
        }
        other => panic!("expected replay, got {other:?}"),
    }
}

#[test]
fn resume_falls_back_to_a_bounded_snapshot_when_the_cursor_is_ahead_of_the_store() {
    let (_dir, mut store) = open_store();
    seed_thread_projection(&mut store, THREAD);
    insert_turn_item(&store, &make_row(0, None));
    insert_turn_item(&store, &make_row(1, None));
    append_events(&store, THREAD, 2);
    let resume = store.thread_stream(THREAD).resume(99).unwrap();
    match resume.batch {
        ThreadStreamBatch::Snapshot { snapshot } => {
            assert_eq!(snapshot.snapshot_sequence, 2);
            assert_eq!(snapshot.projection.thread.id.as_str(), THREAD);
            assert_eq!(snapshot.projection.visible_turn_items.len(), 2);
            assert_eq!(snapshot.latest_local_turn_ordinal, Some(2));
        }
        other => panic!("expected snapshot, got {other:?}"),
    }
}

#[test]
fn resume_falls_back_to_a_snapshot_when_pruning_moved_past_the_cursor() {
    let (_dir, mut store) = open_store();
    seed_thread_projection(&mut store, THREAD);
    let events = append_events(&store, THREAD, 3);
    set_pruned_through(&store, events[1].sequence);
    let resume = store
        .thread_stream(THREAD)
        .resume(events[0].sequence)
        .unwrap();
    assert!(matches!(resume.batch, ThreadStreamBatch::Snapshot { .. }));
}

#[test]
fn resume_at_its_own_high_water_below_the_pruned_boundary_replays_nothing() {
    let (_dir, store) = open_store();
    let events = append_events(&store, THREAD, 1);
    set_pruned_through(&store, events[0].sequence + 10);
    let resume = store
        .thread_stream(THREAD)
        .resume(events[0].sequence)
        .unwrap();
    match resume.batch {
        ThreadStreamBatch::Replay { events, .. } => assert!(events.is_empty()),
        other => panic!("expected an empty replay, got {other:?}"),
    }
}

#[test]
fn resume_falls_back_to_a_snapshot_when_the_replay_exceeds_the_event_cap() {
    let (_dir, mut store) = open_store();
    seed_thread_projection(&mut store, THREAD);
    append_events(&store, THREAD, THREAD_RESUME_MAX_REPLAY_EVENTS + 1);
    let resume = store.thread_stream(THREAD).resume(0).unwrap();
    assert!(matches!(resume.batch, ThreadStreamBatch::Snapshot { .. }));
}

#[test]
fn restart_recovery_loads_the_projection_and_resumes_from_the_saved_sequence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    {
        let mut store = EventStore::open(&path).expect("open store");
        seed_thread_projection(&mut store, THREAD);
        insert_turn_item(&store, &make_row(0, None));
        append_events(&store, THREAD, 3);
        store
            .thread_stream(THREAD)
            .save_sequence(1, NOW)
            .expect("save cursor");
    }

    // Restart: a fresh connection over the same database.
    let store = EventStore::open(&path).expect("reopen store");
    let stream = store.thread_stream(THREAD);
    let recovery = stream.recover(T1).expect("recover");
    assert_eq!(recovery.saved_sequence, 1);
    assert_eq!(recovery.high_water, 3);
    let projection = recovery.projection.expect("projection loaded on open");
    assert_eq!(projection.thread.id.as_str(), THREAD);
    assert_eq!(projection.visible_turn_items.len(), 1);
    match recovery.batch {
        ThreadStreamBatch::Replay { events, .. } => assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3]
        ),
        other => panic!("expected replay, got {other:?}"),
    }
    // The cursor advanced, so a second restart has nothing to send.
    assert_eq!(stream.saved_sequence().unwrap(), Some(3));
    let again = stream.recover(T1).expect("recover again");
    match again.batch {
        ThreadStreamBatch::Replay { events, .. } => assert!(events.is_empty()),
        other => panic!("expected an empty replay, got {other:?}"),
    }
}

#[test]
fn recovery_snapshots_when_the_saved_cursor_is_ahead_of_the_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.sqlite");
    {
        let mut store = EventStore::open(&path).expect("open store");
        seed_thread_projection(&mut store, THREAD);
        append_events(&store, THREAD, 1);
        store
            .thread_stream(THREAD)
            .save_sequence(99, NOW)
            .expect("save cursor");
    }
    let store = EventStore::open(&path).expect("reopen store");
    let recovery = store.thread_stream(THREAD).recover(T1).expect("recover");
    assert!(matches!(recovery.batch, ThreadStreamBatch::Snapshot { .. }));
    assert_eq!(
        store.thread_stream(THREAD).saved_sequence().unwrap(),
        Some(1)
    );
}

#[test]
fn an_overflowed_live_tail_resumes_without_losing_or_duplicating_events() {
    let (_dir, store) = open_store();
    append_events(&store, THREAD, 3);
    let mut buffer = LatestLiveStreamBuffer::new(LiveStreamLimits {
        max_items: 1,
        max_serialized_bytes: LIVE_STREAM_MAX_SERIALIZED_BYTES,
    });
    buffer
        .push("thread", json!({ "sequence": 1, "text": "one" }))
        .unwrap();
    let received = buffer.drain().unwrap();
    assert_eq!(received[0].sequence(), Some(1));
    // The next live event arrives while the first is unacknowledged.
    assert_eq!(
        buffer
            .push("thread", json!({ "sequence": 2, "text": "two" }))
            .unwrap_err(),
        LiveStreamBufferError
    );
    let last_received = received[0].sequence().unwrap();
    buffer.ack(received);
    let resume = store.thread_stream(THREAD).resume(last_received).unwrap();
    match resume.batch {
        ThreadStreamBatch::Replay { events, .. } => assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3]
        ),
        other => panic!("expected replay, got {other:?}"),
    }
}

#[test]
fn projection_loading_keeps_event_store_rows_separate_from_project_events() {
    let (_dir, store) = open_store();
    store
        .append_project_event(&NewProjectEvent {
            event_id: "project-event".to_owned(),
            project_id: "project-1".to_owned(),
            event_type: "project.created".to_owned(),
            occurred_at: NOW.to_owned(),
            command_id: None,
            causation_event_id: None,
            correlation_id: None,
            payload: json!({ "id": "project-1" }),
            metadata: json!({}),
        })
        .unwrap();
    append_events(&store, THREAD, 1);
    let resume = store.thread_stream(THREAD).resume(0).unwrap();
    match resume.batch {
        ThreadStreamBatch::Replay { events, .. } => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].thread_id.as_str(), THREAD);
        }
        other => panic!("expected replay, got {other:?}"),
    }
}

#[test]
fn refuses_to_snapshot_a_thread_without_a_projection() {
    let (_dir, store) = open_store();
    let error = store
        .thread_stream(THREAD)
        .resume(10)
        .expect_err("no projection");
    assert!(matches!(error, StoreError::ThreadNotProjected(thread) if thread == THREAD));
}

#[test]
fn rejects_a_cursor_that_is_not_a_valid_payload() {
    assert!(matches!(
        decode_thread_history_cursor("not-a-cursor"),
        Err(StoreError::InvalidHistoryCursor)
    ));
    let cursor = encode_thread_history_cursor(1, THREAD, "item-1", 1, false);
    let decoded = decode_thread_history_cursor(&cursor).unwrap();
    assert_eq!(decoded.snapshot_sequence, 1);
    assert_eq!(decoded.source_thread_id, THREAD);
    assert_eq!(decoded.source_item_id, "item-1");
    assert_eq!(decoded.position, 1);
    assert!(!decoded.inclusive);
}
