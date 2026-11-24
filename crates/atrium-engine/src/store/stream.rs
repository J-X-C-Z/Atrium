// Ported from apps/server/src/orchestration-v2/ThreadStream.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/LiveStreamBudget.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/threadHistoryPaging.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/WireProjection.ts (T3 Code v0.1.11, MIT).
// Ported from packages/shared/src/orchestrationV2BoundedSnapshot.ts (T3 Code v0.1.11, MIT).
//!
//! Per-thread event streams over the SQLite store.
//!
//! A stream resumes from a client cursor (`after=<sequence>`). T3 replays the
//! stored tail while the log covers the gap and the replay stays inside its
//! count and byte caps; otherwise it sends one bounded snapshot: the control
//! state plus the newest slice of the timeline that fits
//! [`THREAD_HISTORY_PAGE_POLICY`]. This module is that decision, the bounded
//! paging behind it, and the live-tail budget, without a transport: callers
//! (the app, `atrium-node`) read the batch and forward it themselves.
//!
//! Time enters as a `now` argument on recovery. Nothing here touches GPUI.
//!
//! Not ported with item 21: `deferWorkHistoryRows` (it needs the enriched
//! run/attempt statuses the deferred-work page transport builds) and the
//! `dedupeBodies`/`compactIds` wire optimizations (transport-only; the
//! `compactTurnItems` opt-out is ported because it changes the projection).

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue, json};

use crate::contracts::{
    Actor, AppThread, ContextHandoffId, ContextTransferId, ConversationMessage, ExecutionNode,
    MessageId, PlanArtifact, PlanArtifactStatus, PlanId, ProjectedItemVisibility,
    ProjectedTurnItem, ProviderInstanceId, ProviderSession, ProviderThread, ProviderThreadId,
    ProviderTurn, Run, RunAttempt, RunId, RuntimeRequest, RuntimeRequestId, RuntimeRequestStatus,
    StoredEvent, ThreadId, TurnItem, UserMessageInputIntent,
};
use crate::store::events::{EventStore, ReadOptions, Result, StoreError, latest_sequence, read};

/// T3's `THREAD_RESUME_MAX_REPLAY_EVENTS`.
pub const THREAD_RESUME_MAX_REPLAY_EVENTS: usize = 128;
/// T3's `THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES`.
pub const THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES: usize = 1_048_576;
/// T3's `THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES`.
pub const THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES: usize = 1_048_576;

/// T3's `THREAD_HISTORY_INLINE_WORK_ITEM_LIMIT`.
pub const THREAD_HISTORY_INLINE_WORK_ITEM_LIMIT: usize = 150;
/// T3's `OLDER_THREAD_USER_TURN_LIMIT`.
pub const OLDER_THREAD_USER_TURN_LIMIT: usize = 20;
/// T3's `THREAD_HISTORY_MAX_RAW_TURNS`.
pub const THREAD_HISTORY_MAX_RAW_TURNS: usize = 150;

/// T3's `THREAD_HISTORY_SNAPSHOT_ROW_LIMIT`.
pub const THREAD_HISTORY_SNAPSHOT_ROW_LIMIT: usize = THREAD_HISTORY_PAGE_POLICY.max_items + 2;
/// T3's `THREAD_HISTORY_TURN_SNAPSHOT_ROW_LIMIT`.
pub const THREAD_HISTORY_TURN_SNAPSHOT_ROW_LIMIT: usize =
    match THREAD_HISTORY_PAGE_POLICY.max_turn_items {
        Some(limit) => limit + 2,
        None => THREAD_HISTORY_PAGE_POLICY.max_items + 2,
    };

/// T3's `LIVE_STREAM_MAX_ITEMS`.
pub const LIVE_STREAM_MAX_ITEMS: usize = 1_000;
/// T3's `LIVE_STREAM_MAX_SERIALIZED_BYTES`.
pub const LIVE_STREAM_MAX_SERIALIZED_BYTES: usize = 8 * 1024 * 1024;

const THREAD_HISTORY_CURSOR_MAX_LENGTH: usize = 4_096;
const EVENT_LOG_PRUNED_THROUGH: &str = "event-log-pruned-through";
const THREAD_STREAM_PROJECTOR_PREFIX: &str = "thread-stream:";

// ---------------------------------------------------------------------------
// Resume planning (ThreadStream.ts)
// ---------------------------------------------------------------------------

/// A raw payload this large expands past its encoded size once decoded.
pub fn is_thread_replay_raw_payload_safe(raw_payload_bytes: usize) -> bool {
    raw_payload_bytes <= THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES
}

/// Sum the encoded bytes of the stream items, stopping once `limit` is passed.
pub fn thread_replay_encoded_bytes(items: &[ThreadStreamItem]) -> usize {
    thread_replay_encoded_bytes_with_limit(items, THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES)
}

pub fn thread_replay_encoded_bytes_with_limit(items: &[ThreadStreamItem], limit: usize) -> usize {
    let mut total = 0;
    for item in items {
        total += serde_json::to_vec(item)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        if total > limit {
            break;
        }
    }
    total
}

/// What a resume sends: a bounded event replay, or one current snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadResumePlan {
    Replay {
        after_sequence: i64,
        through_sequence: i64,
    },
    Snapshot,
}

/// Bound event count and encoded bytes; an excessive replay becomes a snapshot.
pub fn decide_thread_resume(
    after_sequence: i64,
    high_water: i64,
    replay_event_count: usize,
    replay_encoded_bytes: usize,
) -> ThreadResumePlan {
    if after_sequence > high_water
        || replay_event_count > THREAD_RESUME_MAX_REPLAY_EVENTS
        || replay_encoded_bytes > THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES
    {
        return ThreadResumePlan::Snapshot;
    }
    ThreadResumePlan::Replay {
        after_sequence,
        through_sequence: high_water,
    }
}

/// T3's `OrchestrationV2ThreadStreamItem` core subset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ThreadStreamItem {
    Synchronized,
    Snapshot {
        #[serde(flatten)]
        snapshot: Box<BoundedThreadSnapshot>,
    },
    Event {
        sequence: i64,
        event: ThreadStreamEvent,
    },
    #[serde(rename = "unknown-event")]
    UnknownEvent {
        sequence: i64,
        event_type: String,
    },
}

impl ThreadStreamItem {
    pub fn from_stored(event: &StoredEvent) -> Self {
        Self::Event {
            sequence: event.sequence,
            event: ThreadStreamEvent {
                event_type: event.event_type.clone(),
                payload: event.payload.clone(),
            },
        }
    }
}

/// The part of a stored event that reaches the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: JsonValue,
}

// ---------------------------------------------------------------------------
// History cursors and paging (threadHistoryPaging.ts)
// ---------------------------------------------------------------------------

/// T3's `THREAD_HISTORY_PAGE_POLICY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadHistoryPagePolicy {
    pub max_user_turns: Option<usize>,
    pub max_items: usize,
    /// Defaults to `max_items`.
    pub max_turn_items: Option<usize>,
    pub max_encoded_bytes: usize,
}

pub const THREAD_HISTORY_PAGE_POLICY: ThreadHistoryPagePolicy = ThreadHistoryPagePolicy {
    max_user_turns: Some(10),
    max_items: 75,
    max_turn_items: Some(500),
    max_encoded_bytes: 1_048_576,
};

/// T3's `ThreadHistoryCursorPayload`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadHistoryCursorPayload {
    pub snapshot_sequence: u32,
    pub source_thread_id: String,
    pub source_item_id: String,
    pub position: u32,
    pub inclusive: bool,
}

/// What a caller that passes only the newest part of a history knows about
/// the rows it left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadHistoryPrefix {
    pub has_user_turn: bool,
    /// Runs with a user message or notification among the rows left out.
    pub started_runs: HashSet<String>,
}

/// One page of the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedTimelinePage {
    pub items: Vec<ProjectedTurnItem>,
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundedProjectionResult {
    pub projection: ThreadProjectionSnapshot,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub latest_local_turn_ordinal: Option<u32>,
}

fn base64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        }
    }
    out
}

fn base64url_decode(value: &str) -> Option<Vec<u8>> {
    fn sextet(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => (byte - b'A') as u32,
            b'a'..=b'z' => (byte - b'a') as u32 + 26,
            b'0'..=b'9' => (byte - b'0') as u32 + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        })
    }
    let bytes = value.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let a = sextet(chunk[0])?;
        let b = sextet(*chunk.get(1)?)?;
        let c = match chunk.get(2).copied() {
            Some(byte) => sextet(byte)?,
            None => 0,
        };
        let d = match chunk.get(3).copied() {
            Some(byte) => sextet(byte)?,
            None => 0,
        };
        let triple = (a << 18) | (b << 12) | (c << 6) | d;
        out.push((triple >> 16) as u8);
        if chunk.len() > 2 {
            out.push((triple >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(triple as u8);
        }
    }
    Some(out)
}

pub fn encode_thread_history_cursor(
    snapshot_sequence: u32,
    source_thread_id: &str,
    source_item_id: &str,
    position: u32,
    inclusive: bool,
) -> String {
    let payload = json!({
        "v": 1,
        "seq": snapshot_sequence,
        "st": source_thread_id,
        "si": source_item_id,
        "p": position,
        "inclusive": inclusive,
    });
    base64url_encode(payload.to_string().as_bytes())
}

/// Decode a cursor, rejecting anything T3's schema would reject.
pub fn decode_thread_history_cursor(cursor: &str) -> Result<ThreadHistoryCursorPayload> {
    if cursor.is_empty() || cursor.len() > THREAD_HISTORY_CURSOR_MAX_LENGTH {
        return Err(StoreError::InvalidHistoryCursor);
    }
    let bytes = base64url_decode(cursor).ok_or(StoreError::InvalidHistoryCursor)?;
    let parsed: JsonValue =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::InvalidHistoryCursor)?;
    let object = parsed.as_object().ok_or(StoreError::InvalidHistoryCursor)?;
    let number = |key: &str| {
        object
            .get(key)
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    };
    let string = |key: &str| {
        object
            .get(key)
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    if number("v") != Some(1) {
        return Err(StoreError::InvalidHistoryCursor);
    }
    let inclusive = match object.get("inclusive") {
        None => false,
        Some(JsonValue::Bool(value)) => *value,
        Some(_) => return Err(StoreError::InvalidHistoryCursor),
    };
    Ok(ThreadHistoryCursorPayload {
        snapshot_sequence: number("seq").ok_or(StoreError::InvalidHistoryCursor)?,
        source_thread_id: string("st").ok_or(StoreError::InvalidHistoryCursor)?,
        source_item_id: string("si").ok_or(StoreError::InvalidHistoryCursor)?,
        position: number("p").ok_or(StoreError::InvalidHistoryCursor)?,
        inclusive,
    })
}

fn bytes_of_json(value: &impl Serialize) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

pub fn projected_row_encoded_bytes(row: &ProjectedTurnItem) -> usize {
    bytes_of_json(row)
}

/// Charge local items twice because bounded snapshots also carry
/// `projection.turnItems`; inherited rows appear only once.
pub fn projected_row_bounded_snapshot_encoded_bytes(
    row: &ProjectedTurnItem,
    thread_id: &ThreadId,
) -> usize {
    let row_bytes = projected_row_encoded_bytes(row);
    if row.visibility == ProjectedItemVisibility::Local || row.source_thread_id == *thread_id {
        row_bytes + bytes_of_json(&row.item)
    } else {
        row_bytes
    }
}

fn renumber_positions(items: Vec<ProjectedTurnItem>) -> Vec<ProjectedTurnItem> {
    items
        .into_iter()
        .enumerate()
        .map(|(position, mut row)| {
            row.position = position as u32;
            row
        })
        .collect()
}

pub fn is_conversation_history_item(item: &TurnItem) -> bool {
    !matches!(
        item.type_name(),
        "command_execution" | "reasoning" | "file_change" | "dynamic_tool"
    )
}

fn item_run_id(item: &TurnItem) -> Option<&str> {
    item.base()
        .and_then(|base| base.run_id.as_ref())
        .map(|id| id.as_str())
}

fn item_ordinal(item: &TurnItem) -> Option<u32> {
    item.base().map(|base| base.ordinal)
}

fn item_created_by(item: &TurnItem) -> Option<Actor> {
    match item {
        TurnItem::UserMessage(item) => Some(item.created_by),
        _ => None,
    }
}

fn item_input_intent(item: &TurnItem) -> Option<UserMessageInputIntent> {
    match item {
        TurnItem::UserMessage(item) => Some(item.input_intent),
        _ => None,
    }
}

fn item_message_id(item: &TurnItem) -> Option<&MessageId> {
    match item {
        TurnItem::UserMessage(item) => Some(&item.message_id),
        TurnItem::AssistantMessage(item) => Some(&item.message_id),
        _ => None,
    }
}

fn item_request_id(item: &TurnItem) -> Option<&RuntimeRequestId> {
    match item {
        TurnItem::UserInputRequest(item) => Some(&item.request_id),
        TurnItem::ApprovalRequest(item) => Some(&item.request_id),
        _ => None,
    }
}

fn item_plan_id(item: &TurnItem) -> Option<&PlanId> {
    match item {
        TurnItem::ProposedPlan(item) => Some(&item.plan_id),
        TurnItem::TodoList(item) => Some(&item.plan_id),
        _ => None,
    }
}

fn item_context_handoff_id(item: &TurnItem) -> Option<&ContextHandoffId> {
    match item {
        TurnItem::Handoff(item) => Some(&item.context_handoff_id),
        _ => None,
    }
}

fn is_thread_history_turn_start(item: &TurnItem) -> bool {
    matches!(
        item_input_intent(item),
        Some(UserMessageInputIntent::TurnStart | UserMessageInputIntent::QueuedTurn)
    )
}

pub fn is_thread_history_user_turn(item: &TurnItem) -> bool {
    is_thread_history_turn_start(item)
        && matches!(item, TurnItem::UserMessage(_))
        && item_created_by(item) == Some(Actor::User)
}

fn thread_history_turn_starts(
    items: &[TurnItem],
    earlier_runs: Option<&HashSet<String>>,
) -> Vec<bool> {
    let mut started_runs: HashSet<String> = earlier_runs.cloned().unwrap_or_default();
    items
        .iter()
        .map(|item| {
            if !matches!(item.type_name(), "user_message" | "notification") {
                return false;
            }
            let first_input = item_run_id(item).is_some_and(|run| !started_runs.contains(run));
            if let Some(run) = item_run_id(item) {
                started_runs.insert(run.to_owned());
            }
            if item.type_name() == "notification" {
                first_input
            } else {
                is_thread_history_turn_start(item)
            }
        })
        .collect()
}

/// Always admit at least one row so oversized items cannot deadlock paging.
#[allow(clippy::too_many_arguments)]
fn select_older_timeline_page(
    items: &[ProjectedTurnItem],
    exclusive_end_index: usize,
    snapshot_sequence: u32,
    policy: ThreadHistoryPagePolicy,
    row_encoded_bytes: Option<&dyn Fn(&ProjectedTurnItem) -> usize>,
    prefix: Option<&ThreadHistoryPrefix>,
) -> SelectedTimelinePage {
    let end = exclusive_end_index.min(items.len());
    if end == 0 {
        return SelectedTimelinePage {
            items: Vec::new(),
            next_cursor: None,
            has_more_history: false,
        };
    }

    let candidates = &items[..end];
    let item_refs: Vec<TurnItem> = candidates.iter().map(|row| row.item.clone()).collect();
    let turn_starts =
        thread_history_turn_starts(&item_refs, prefix.map(|value| &value.started_runs));
    let turn_limit = if prefix.is_some_and(|value| value.has_user_turn)
        || candidates
            .iter()
            .any(|row| is_thread_history_user_turn(&row.item))
    {
        policy.max_user_turns
    } else {
        None
    };
    let max_items = if turn_limit.is_some() {
        policy.max_turn_items.unwrap_or(policy.max_items)
    } else {
        policy.max_items
    };

    let mut selected: Vec<ProjectedTurnItem> = Vec::new();
    let mut encoded_bytes = 0usize;
    let mut user_turns = 0usize;
    let mut raw_turns = 0usize;
    for index in (0..end).rev() {
        let row = &items[index];
        if !selected.is_empty()
            && (selected.len() >= max_items
                || (turn_limit.is_some()
                    && (user_turns >= turn_limit.unwrap_or(0)
                        || raw_turns >= THREAD_HISTORY_MAX_RAW_TURNS)))
        {
            break;
        }
        let row_bytes =
            row_encoded_bytes.map_or_else(|| projected_row_encoded_bytes(row), |cost| cost(row));
        if !selected.is_empty() && encoded_bytes + row_bytes > policy.max_encoded_bytes {
            break;
        }
        selected.push(row.clone());
        encoded_bytes += row_bytes;
        if is_thread_history_user_turn(&row.item) {
            user_turns += 1;
        }
        if turn_starts[index] {
            raw_turns += 1;
        }
    }
    // Inside a turn longer than the window no selected row starts a user turn.
    if user_turns == 0 {
        selected.truncate(policy.max_items);
    }
    selected.reverse();

    let oldest = selected.first().cloned();
    let oldest_index = oldest
        .as_ref()
        .and_then(|oldest| {
            items.iter().position(|row| {
                row.source_thread_id == oldest.source_thread_id
                    && row.source_item_id == oldest.source_item_id
            })
        })
        .map_or(-1, |index| index as i64);
    let has_more_history = oldest_index > 0;
    let next_cursor = if has_more_history {
        oldest.map(|oldest| {
            encode_thread_history_cursor(
                snapshot_sequence,
                oldest.source_thread_id.as_str(),
                oldest.source_item_id.as_str(),
                oldest.position,
                false,
            )
        })
    } else {
        None
    };

    SelectedTimelinePage {
        items: renumber_positions(selected),
        next_cursor,
        has_more_history,
    }
}

pub fn select_recent_timeline_window(
    items: &[ProjectedTurnItem],
    snapshot_sequence: u32,
    policy: ThreadHistoryPagePolicy,
    row_encoded_bytes: Option<&dyn Fn(&ProjectedTurnItem) -> usize>,
) -> SelectedTimelinePage {
    select_older_timeline_page(
        items,
        items.len(),
        snapshot_sequence,
        policy,
        row_encoded_bytes,
        None,
    )
}

pub fn find_cursor_end_index(
    items: &[ProjectedTurnItem],
    cursor: &ThreadHistoryCursorPayload,
) -> usize {
    if let Some(index) = items.iter().position(|row| {
        row.source_thread_id.as_str() == cursor.source_thread_id
            && row.source_item_id.as_str() == cursor.source_item_id
    }) {
        return index + usize::from(cursor.inclusive);
    }
    // Inclusive deferred cursors carry source ordinals; ordinary cursors carry
    // visible positions.
    let by_position = items.iter().position(|row| {
        if cursor.inclusive {
            row.source_thread_id.as_str() == cursor.source_thread_id
                && item_ordinal(&row.item).unwrap_or(row.position) > cursor.position
        } else {
            row.position >= cursor.position
        }
    });
    by_position.unwrap_or(items.len())
}

/// Older pages allow twice the turns; a targeted page widens further so it
/// reaches `through_entry_id`.
pub fn older_history_page_policy(
    items: &[ProjectedTurnItem],
    cursor: &str,
    policy: Option<ThreadHistoryPagePolicy>,
    through_entry_id: Option<&str>,
) -> Result<ThreadHistoryPagePolicy> {
    let policy = policy.unwrap_or(ThreadHistoryPagePolicy {
        max_user_turns: Some(OLDER_THREAD_USER_TURN_LIMIT),
        ..THREAD_HISTORY_PAGE_POLICY
    });
    let Some(through_entry_id) = through_entry_id else {
        return Ok(policy);
    };
    let decoded = decode_thread_history_cursor(cursor)?;
    let anchor_index = find_cursor_end_index(items, &decoded);
    let Some(target_index) = items.iter().position(|row| {
        item_message_id(&row.item)
            .map(|id| id.as_str())
            .or_else(|| row.item.id().map(|id| id.as_str()))
            == Some(through_entry_id)
    }) else {
        return Ok(policy);
    };
    if target_index >= anchor_index {
        return Ok(policy);
    }
    let turns = items[target_index..anchor_index]
        .iter()
        .filter(|row| is_thread_history_user_turn(&row.item))
        .count();
    let bump = usize::from(!is_thread_history_user_turn(&items[target_index].item));
    Ok(ThreadHistoryPagePolicy {
        max_user_turns: Some(
            (OLDER_THREAD_USER_TURN_LIMIT * 2)
                .min((OLDER_THREAD_USER_TURN_LIMIT).max(turns + bump)),
        ),
        max_items: policy.max_items * 2,
        max_turn_items: Some(policy.max_turn_items.unwrap_or(policy.max_items) * 2),
        max_encoded_bytes: policy.max_encoded_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn select_history_page_from_cursor(
    items: &[ProjectedTurnItem],
    cursor: &str,
    snapshot_sequence: u32,
    policy: Option<ThreadHistoryPagePolicy>,
    row_encoded_bytes: Option<&dyn Fn(&ProjectedTurnItem) -> usize>,
    through_entry_id: Option<&str>,
    exclusive_end_index: Option<usize>,
    prefix: Option<&ThreadHistoryPrefix>,
) -> Result<SelectedTimelinePage> {
    let decoded = decode_thread_history_cursor(cursor)?;
    let effective_policy = older_history_page_policy(items, cursor, policy, through_entry_id)?;
    Ok(select_older_timeline_page(
        items,
        exclusive_end_index.unwrap_or_else(|| find_cursor_end_index(items, &decoded)),
        snapshot_sequence,
        effective_policy,
        row_encoded_bytes,
        prefix,
    ))
}

pub fn compute_latest_local_turn_ordinal(turn_items: &[TurnItem]) -> Option<u32> {
    turn_items.iter().filter_map(item_ordinal).max()
}

pub fn bounded_timeline_encoded_bytes(
    visible_turn_items: &[ProjectedTurnItem],
    turn_items: &[TurnItem],
) -> usize {
    visible_turn_items
        .iter()
        .map(projected_row_encoded_bytes)
        .sum::<usize>()
        + turn_items.iter().map(bytes_of_json).sum::<usize>()
}

// ---------------------------------------------------------------------------
// The thread projection snapshot
// ---------------------------------------------------------------------------

/// T3's `OrchestrationV2ContextHandoff`, reduced to the fields paging reads;
/// everything else round-trips through `extra`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextHandoffRecord {
    pub id: ContextHandoffId,
    pub thread_id: ThreadId,
    pub target_run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_id: Option<ContextTransferId>,
    #[serde(default)]
    pub from_provider_thread_ids: Vec<ProviderThreadId>,
    pub to_provider_thread_id: ProviderThreadId,
    #[serde(default)]
    pub covered_run_ordinals: JsonValue,
    pub strategy: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_message_id: Option<MessageId>,
    pub summary_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_provider_instance_id: Option<ProviderInstanceId>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_in_turn_item: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, JsonValue>,
}

/// T3's `OrchestrationV2ThreadProjection`, rebuilt from the projection rows.
///
/// Subagent records, checkpoint scope/record records and context transfers are
/// not ported yet (contracts item 20 defers them); they travel as raw JSON so
/// the wire shape survives. `visibleTurnItems` is derived from this thread's
/// turn items until the orchestration layer owns lineage projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjectionSnapshot {
    pub thread: AppThread,
    pub runs: Vec<Run>,
    pub attempts: Vec<RunAttempt>,
    pub nodes: Vec<ExecutionNode>,
    #[serde(default)]
    pub subagents: Vec<JsonValue>,
    #[serde(default)]
    pub provider_sessions: Vec<ProviderSession>,
    #[serde(default)]
    pub provider_threads: Vec<ProviderThread>,
    #[serde(default)]
    pub provider_turns: Vec<ProviderTurn>,
    #[serde(default)]
    pub runtime_requests: Vec<RuntimeRequest>,
    #[serde(default)]
    pub messages: Vec<ConversationMessage>,
    #[serde(default)]
    pub plans: Vec<PlanArtifact>,
    #[serde(default)]
    pub turn_items: Vec<TurnItem>,
    #[serde(default)]
    pub checkpoint_scopes: Vec<JsonValue>,
    #[serde(default)]
    pub checkpoints: Vec<JsonValue>,
    #[serde(default)]
    pub context_handoffs: Vec<ContextHandoffRecord>,
    #[serde(default)]
    pub context_transfers: Vec<JsonValue>,
    #[serde(default)]
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    pub updated_at: String,
}

impl ThreadProjectionSnapshot {
    /// A projection with only the thread and control arrays filled in.
    pub fn empty(thread: AppThread, updated_at: String) -> Self {
        Self {
            thread,
            runs: Vec::new(),
            attempts: Vec::new(),
            nodes: Vec::new(),
            subagents: Vec::new(),
            provider_sessions: Vec::new(),
            provider_threads: Vec::new(),
            provider_turns: Vec::new(),
            runtime_requests: Vec::new(),
            messages: Vec::new(),
            plans: Vec::new(),
            turn_items: Vec::new(),
            checkpoint_scopes: Vec::new(),
            checkpoints: Vec::new(),
            context_handoffs: Vec::new(),
            context_transfers: Vec::new(),
            visible_turn_items: Vec::new(),
            updated_at,
        }
    }
}

/// T3's `BoundedThreadSnapshot` stream item payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundedThreadSnapshot {
    pub snapshot_sequence: u32,
    pub projection: ThreadProjectionSnapshot,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub latest_local_turn_ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_items_omit_local_visible: Option<bool>,
}

fn is_local_projected_row(projection: &ThreadProjectionSnapshot, row: &ProjectedTurnItem) -> bool {
    row.visibility == ProjectedItemVisibility::Local || row.source_thread_id == projection.thread.id
}

fn pending_request_ids(projection: &ThreadProjectionSnapshot) -> HashSet<String> {
    projection
        .runtime_requests
        .iter()
        .filter(|request| request.status == RuntimeRequestStatus::Pending)
        .map(|request| request.id.as_str().to_owned())
        .collect()
}

// Clients build the answer panel of an open question or approval from its row,
// and a run that kept working pushes that row out of the newest turns.
fn is_retained_dependency(item: &TurnItem, pending: &HashSet<String>) -> bool {
    match item {
        TurnItem::RunInterruptRequest(_) => true,
        TurnItem::UserInputRequest(_) | TurnItem::ApprovalRequest(_) => {
            item_request_id(item).is_some_and(|id| pending.contains(id.as_str()))
        }
        _ => false,
    }
}

/// Retain small requests outside the visible window; later result pages still
/// need them for attempt/run reducers.
fn retained_interrupt_request_turn_items(
    projection: &ThreadProjectionSnapshot,
    visible: &[ProjectedTurnItem],
) -> Vec<TurnItem> {
    let visible_local_ids: HashSet<String> = visible
        .iter()
        .filter(|row| is_local_projected_row(projection, row))
        .map(|row| row.source_item_id.as_str().to_owned())
        .collect();
    let pending = pending_request_ids(projection);
    projection
        .turn_items
        .iter()
        .filter(|item| is_retained_dependency(item, &pending))
        .filter(|item| {
            item.id()
                .is_none_or(|id| !visible_local_ids.contains(id.as_str()))
        })
        .cloned()
        .collect()
}

fn local_turn_items_for_visible_window(
    projection: &ThreadProjectionSnapshot,
    visible: &[ProjectedTurnItem],
) -> Vec<TurnItem> {
    visible
        .iter()
        .filter(|row| is_local_projected_row(projection, row))
        .map(|row| row.item.clone())
        .collect()
}

fn messages_for_bounded_projection(
    projection: &ThreadProjectionSnapshot,
    turn_items: &[TurnItem],
) -> Vec<ConversationMessage> {
    let retained_message_ids: HashSet<String> = turn_items
        .iter()
        .filter_map(item_message_id)
        .map(|id| id.as_str().to_owned())
        .collect();
    let latest_run = projection.runs.iter().max_by_key(|run| run.ordinal);
    let mut retained_run_ids: HashSet<String> = HashSet::new();
    let mut retained_message_ids = retained_message_ids;
    for run in &projection.runs {
        let keep = latest_run.is_some_and(|latest| latest.id == run.id)
            || matches!(
                run.status,
                crate::contracts::RunStatus::Preparing
                    | crate::contracts::RunStatus::Starting
                    | crate::contracts::RunStatus::Running
                    | crate::contracts::RunStatus::Waiting
                    | crate::contracts::RunStatus::Queued
            );
        if keep {
            retained_run_ids.insert(run.id.as_str().to_owned());
            retained_message_ids.insert(run.user_message_id.as_str().to_owned());
        }
    }
    projection
        .messages
        .iter()
        .filter(|message| {
            retained_message_ids.contains(message.id.as_str())
                || message
                    .run_id
                    .as_ref()
                    .is_some_and(|run| retained_run_ids.contains(run.as_str()))
                || message.delegated_completion.is_some()
                // Keep the workflow script outside the history window; clients
                // use it to open the coordinator thread.
                || message.workflow_script == Some(true)
        })
        .cloned()
        .collect()
}

fn plan_kind(plan: &PlanArtifact) -> &'static str {
    match plan {
        PlanArtifact::ProposedPlan(_) => "proposed_plan",
        PlanArtifact::TodoList(_) => "todo_list",
    }
}

fn plan_status(plan: &PlanArtifact) -> PlanArtifactStatus {
    match plan {
        PlanArtifact::ProposedPlan(plan) => plan.base.status,
        PlanArtifact::TodoList(plan) => plan.base.status,
    }
}

fn plan_id(plan: &PlanArtifact) -> &PlanId {
    match plan {
        PlanArtifact::ProposedPlan(plan) => &plan.base.id,
        PlanArtifact::TodoList(plan) => &plan.base.id,
    }
}

fn plan_run_id(plan: &PlanArtifact) -> Option<&crate::contracts::RunId> {
    match plan {
        PlanArtifact::ProposedPlan(plan) => plan.base.run_id.as_ref(),
        PlanArtifact::TodoList(plan) => plan.base.run_id.as_ref(),
    }
}

fn strip_plan_detail(plan: &PlanArtifact) -> PlanArtifact {
    match plan {
        PlanArtifact::ProposedPlan(plan) => {
            let mut plan = plan.clone();
            plan.markdown.clear();
            plan.base.detail_in_turn_item = Some(true);
            PlanArtifact::ProposedPlan(plan)
        }
        PlanArtifact::TodoList(plan) => {
            let mut plan = plan.clone();
            plan.explanation = None;
            plan.base.detail_in_turn_item = Some(true);
            PlanArtifact::TodoList(plan)
        }
    }
}

pub fn build_bounded_thread_projection(
    projection: ThreadProjectionSnapshot,
    snapshot_sequence: u32,
    policy: Option<ThreadHistoryPagePolicy>,
) -> BoundedProjectionResult {
    let policy = policy.unwrap_or(THREAD_HISTORY_PAGE_POLICY);
    let thread_id = projection.thread.id.clone();
    let run_ordinals: HashMap<String, u32> = projection
        .runs
        .iter()
        .map(|run| (run.id.as_str().to_owned(), run.ordinal))
        .collect();
    let mut latest_todo_list: Option<PlanArtifact> = None;
    let mut latest_todo_list_ordinal: Option<u32> = None;
    for plan in &projection.plans {
        let ordinal = plan_run_id(plan).and_then(|run| run_ordinals.get(run.as_str()).copied());
        if plan_kind(plan) == "todo_list"
            && plan_status(plan) != PlanArtifactStatus::Superseded
            && ordinal.is_some_and(|ordinal| {
                latest_todo_list_ordinal.is_none_or(|latest| ordinal >= latest)
            })
        {
            latest_todo_list = Some(plan.clone());
            latest_todo_list_ordinal = ordinal;
        }
    }
    let latest_todo_list_id = latest_todo_list.as_ref().map(|plan| plan_id(plan).clone());

    let mut control_projection = projection.clone();
    control_projection.plans = projection
        .plans
        .iter()
        .filter(|plan| {
            plan_status(plan) == PlanArtifactStatus::Active
                || latest_todo_list_id
                    .as_ref()
                    .is_some_and(|id| plan_id(plan) == id)
        })
        .cloned()
        .collect();
    control_projection.context_handoffs = projection
        .context_handoffs
        .iter()
        .filter(|handoff| handoff.status == "pending" || handoff.status == "ready")
        .cloned()
        .collect();

    let latest_local_turn_ordinal = compute_latest_local_turn_ordinal(&projection.turn_items);
    let pending = pending_request_ids(&control_projection);
    let dependency_reserve: usize = control_projection
        .turn_items
        .iter()
        .filter(|item| is_retained_dependency(item, &pending))
        .map(bytes_of_json)
        .sum();
    let control_bytes = {
        let mut control = control_projection.clone();
        control.messages.clear();
        control.turn_items.clear();
        control.visible_turn_items.clear();
        bytes_of_json(&control)
    };
    // Control state cannot be paged, so the window keeps half the budget;
    // dependency cards take their reserve from that half.
    let half = policy.max_encoded_bytes / 2;
    let window_budget = half.saturating_sub(dependency_reserve).max(
        policy
            .max_encoded_bytes
            .saturating_sub(dependency_reserve)
            .saturating_sub(control_bytes)
            .saturating_sub(1_024),
    );
    let window_policy = ThreadHistoryPagePolicy {
        max_encoded_bytes: window_budget,
        ..policy
    };
    let cost =
        |row: &ProjectedTurnItem| projected_row_bounded_snapshot_encoded_bytes(row, &thread_id);
    let window = select_recent_timeline_window(
        &control_projection.visible_turn_items,
        snapshot_sequence,
        window_policy,
        Some(&cost),
    );
    let visible_turn_items = renumber_positions(window.items);
    let window_turn_items =
        local_turn_items_for_visible_window(&control_projection, &visible_turn_items);
    let dependency_turn_items =
        retained_interrupt_request_turn_items(&control_projection, &visible_turn_items);
    let mut turn_item_by_id: HashMap<String, TurnItem> = HashMap::new();
    let mut turn_item_order: Vec<String> = Vec::new();
    for item in window_turn_items.into_iter().chain(dependency_turn_items) {
        let Some(id) = item.id().map(|id| id.as_str().to_owned()) else {
            continue;
        };
        if !turn_item_by_id.contains_key(&id) {
            turn_item_order.push(id.clone());
        }
        turn_item_by_id.insert(id, item);
    }
    let turn_items: Vec<TurnItem> = turn_item_order
        .into_iter()
        .filter_map(|id| turn_item_by_id.remove(&id))
        .collect();

    let visible_plan_ids: HashSet<String> = turn_items
        .iter()
        .filter_map(item_plan_id)
        .map(|id| id.as_str().to_owned())
        .collect();
    let visible_handoff_ids: HashSet<String> = turn_items
        .iter()
        .filter_map(item_context_handoff_id)
        .map(|id| id.as_str().to_owned())
        .collect();
    let plans: Vec<PlanArtifact> = projection
        .plans
        .iter()
        .filter(|plan| {
            plan_status(plan) == PlanArtifactStatus::Active
                || latest_todo_list_id
                    .as_ref()
                    .is_some_and(|id| plan_id(plan) == id)
                || visible_plan_ids.contains(plan_id(plan).as_str())
        })
        .map(|plan| {
            if plan_status(plan) == PlanArtifactStatus::Active
                || latest_todo_list_id
                    .as_ref()
                    .is_some_and(|id| plan_id(plan) == id)
            {
                plan.clone()
            } else {
                strip_plan_detail(plan)
            }
        })
        .collect();
    let context_handoffs: Vec<ContextHandoffRecord> = projection
        .context_handoffs
        .iter()
        .filter(|handoff| {
            handoff.status == "pending"
                || handoff.status == "ready"
                || visible_handoff_ids.contains(handoff.id.as_str())
        })
        .map(|handoff| {
            if handoff.status == "pending" || handoff.status == "ready" {
                handoff.clone()
            } else {
                let mut handoff = handoff.clone();
                handoff.summary_text.clear();
                handoff.detail_in_turn_item = Some(true);
                handoff
            }
        })
        .collect();
    let messages = messages_for_bounded_projection(&control_projection, &turn_items);

    BoundedProjectionResult {
        projection: ThreadProjectionSnapshot {
            plans,
            context_handoffs,
            messages,
            turn_items,
            visible_turn_items,
            ..control_projection
        },
        history_cursor: window.next_cursor,
        has_more_history: window.has_more_history,
        latest_local_turn_ordinal,
    }
}

fn command_omits_output(item: &TurnItem) -> bool {
    matches!(item, TurnItem::CommandExecution(execution) if execution.output.as_deref().is_some_and(|output| !output.trim().is_empty()))
}

fn project_turn_item_for_wire(item: &TurnItem) -> TurnItem {
    match item {
        TurnItem::CommandExecution(execution) => {
            let omitted = command_omits_output(item);
            let mut execution = execution.clone();
            execution.output = None;
            if omitted {
                execution.output_omitted = Some(true);
            }
            TurnItem::CommandExecution(execution)
        }
        TurnItem::FileChange(change) => {
            let mut change = change.clone();
            change.diff_str = None;
            change.old_str = None;
            change.new_str = None;
            TurnItem::FileChange(change)
        }
        TurnItem::Handoff(handoff) => {
            let mut handoff = handoff.clone();
            handoff.summary = None;
            TurnItem::Handoff(handoff)
        }
        other => other.clone(),
    }
}

pub fn project_thread_projection_for_wire(
    projection: ThreadProjectionSnapshot,
) -> ThreadProjectionSnapshot {
    let context_handoffs = projection
        .context_handoffs
        .into_iter()
        .map(|mut handoff| {
            handoff.summary_text.clear();
            handoff.delivery = None;
            handoff
        })
        .collect::<Vec<_>>();
    let turn_items = projection
        .turn_items
        .iter()
        .map(project_turn_item_for_wire)
        .collect::<Vec<_>>();
    let mut by_key: HashMap<(String, String), TurnItem> = turn_items
        .iter()
        .filter_map(|item| {
            let base = item.base()?;
            Some((
                (
                    base.thread_id.as_str().to_owned(),
                    base.id.as_str().to_owned(),
                ),
                item.clone(),
            ))
        })
        .collect();
    let visible_turn_items = projection
        .visible_turn_items
        .iter()
        .map(|row| {
            let mut row = row.clone();
            let key = (
                row.item
                    .base()
                    .map(|base| base.thread_id.as_str().to_owned())
                    .unwrap_or_default(),
                row.source_item_id.as_str().to_owned(),
            );
            let projected = match by_key.get(&key) {
                Some(projected) => projected.clone(),
                None => {
                    let projected = project_turn_item_for_wire(&row.item);
                    by_key.insert(key, projected.clone());
                    projected
                }
            };
            row.item = projected;
            row
        })
        .collect();
    ThreadProjectionSnapshot {
        context_handoffs,
        turn_items,
        visible_turn_items,
        ..projection
    }
}

/// Build the snapshot T3 sends when the log cannot cover a resume gap.
pub fn build_bounded_thread_stream_snapshot(
    snapshot_sequence: u32,
    projection: ThreadProjectionSnapshot,
    compact_turn_items: bool,
) -> BoundedThreadSnapshot {
    let projection = project_thread_projection_for_wire(projection);
    let bounded = build_bounded_thread_projection(projection, snapshot_sequence, None);
    let compact = if compact_turn_items {
        omit_local_visible_turn_items(&bounded.projection)
    } else {
        None
    };
    let turn_items_omit_local_visible = compact.as_ref().map(|_| true);
    BoundedThreadSnapshot {
        snapshot_sequence,
        projection: compact.unwrap_or(bounded.projection),
        history_cursor: bounded.history_cursor,
        has_more_history: bounded.has_more_history,
        latest_local_turn_ordinal: bounded.latest_local_turn_ordinal,
        turn_items_omit_local_visible,
    }
}

/// T3's `omitLocalVisibleTurnItems`: requires object identity so restoring
/// reproduces the exact entry. Rust clones stand in for identity.
pub fn omit_local_visible_turn_items(
    projection: &ThreadProjectionSnapshot,
) -> Option<ThreadProjectionSnapshot> {
    let mut omitted = 0usize;
    for row in &projection.visible_turn_items {
        if !is_local_projected_row(projection, row) {
            continue;
        }
        if projection.turn_items.get(omitted) != Some(&row.item) {
            return None;
        }
        omitted += 1;
    }
    if omitted == 0 {
        return None;
    }
    let mut projection = projection.clone();
    projection.turn_items = projection.turn_items[omitted..].to_vec();
    Some(projection)
}

/// T3's `restoreLocalVisibleTurnItems`.
pub fn restore_local_visible_turn_items(
    mut projection: ThreadProjectionSnapshot,
) -> ThreadProjectionSnapshot {
    let mut local: Vec<TurnItem> = projection
        .visible_turn_items
        .iter()
        .filter(|row| is_local_projected_row(&projection, row))
        .map(|row| row.item.clone())
        .collect();
    if local.is_empty() {
        return projection;
    }
    local.extend(projection.turn_items);
    projection.turn_items = local;
    projection
}

/// T3's `boundedSnapshotProjection`: undo both opt-in compactions.
pub fn bounded_snapshot_projection(
    mut snapshot: BoundedThreadSnapshot,
) -> ThreadProjectionSnapshot {
    if snapshot.turn_items_omit_local_visible == Some(true) {
        snapshot.projection = restore_local_visible_turn_items(snapshot.projection);
    }
    snapshot.projection
}

// ---------------------------------------------------------------------------
// Live stream budget (LiveStreamBudget.ts)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("The live event buffer is full. Resume from the last received sequence.")]
pub struct LiveStreamBufferError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveStreamLimits {
    pub max_items: usize,
    pub max_serialized_bytes: usize,
}

impl Default for LiveStreamLimits {
    fn default() -> Self {
        Self {
            max_items: LIVE_STREAM_MAX_ITEMS,
            max_serialized_bytes: LIVE_STREAM_MAX_SERIALIZED_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RetainedLiveItem {
    value: JsonValue,
    serialized_bytes: usize,
    id: u64,
}

impl RetainedLiveItem {
    pub fn value(&self) -> &JsonValue {
        &self.value
    }

    pub fn sequence(&self) -> Option<i64> {
        self.value.get("sequence").and_then(JsonValue::as_i64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveStreamUsage {
    pub retained_items: usize,
    pub retained_serialized_bytes: usize,
}

/// T3's `makeLiveStreamBudget`: a retained item stays charged until the
/// caller ACKs it, so a slow consumer cannot outrun the buffer.
#[derive(Clone, Debug)]
pub struct LiveStreamBudget {
    limits: LiveStreamLimits,
    retained: HashMap<u64, usize>,
    retained_serialized_bytes: usize,
    next_id: u64,
    failed: bool,
}

impl LiveStreamBudget {
    pub fn new(limits: LiveStreamLimits) -> Self {
        Self {
            limits,
            retained: HashMap::new(),
            retained_serialized_bytes: 0,
            next_id: 0,
            failed: false,
        }
    }

    pub fn check(&self) -> std::result::Result<(), LiveStreamBufferError> {
        if self.failed {
            Err(LiveStreamBufferError)
        } else {
            Ok(())
        }
    }

    pub fn retain(
        &mut self,
        value: JsonValue,
    ) -> std::result::Result<RetainedLiveItem, LiveStreamBufferError> {
        self.check()?;
        let serialized_bytes = bytes_of_json(&value);
        let next_items = self.retained.len() + 1;
        let next_bytes = self.retained_serialized_bytes + serialized_bytes;
        if next_items > self.limits.max_items || next_bytes > self.limits.max_serialized_bytes {
            self.failed = true;
            return Err(LiveStreamBufferError);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.retained.insert(id, serialized_bytes);
        self.retained_serialized_bytes = next_bytes;
        Ok(RetainedLiveItem {
            value,
            serialized_bytes,
            id,
        })
    }

    /// Replace retained states in one step, as `bufferLatestLiveStream` does.
    pub fn replace(
        &mut self,
        previous: &[RetainedLiveItem],
        values: Vec<JsonValue>,
    ) -> std::result::Result<Vec<RetainedLiveItem>, LiveStreamBufferError> {
        self.check()?;
        let next: Vec<(JsonValue, usize)> = values
            .into_iter()
            .map(|value| {
                let bytes = bytes_of_json(&value);
                (value, bytes)
            })
            .collect();
        let mut next_items = self.retained.len() + next.len();
        let mut next_bytes =
            self.retained_serialized_bytes + next.iter().map(|(_, bytes)| *bytes).sum::<usize>();
        for item in previous {
            if self.retained.contains_key(&item.id) {
                next_items -= 1;
                next_bytes -= item.serialized_bytes;
            }
        }
        if next_items > self.limits.max_items || next_bytes > self.limits.max_serialized_bytes {
            self.failed = true;
            return Err(LiveStreamBufferError);
        }
        self.release(previous.iter().cloned());
        let mut created = Vec::with_capacity(next.len());
        for (value, serialized_bytes) in next {
            let id = self.next_id;
            self.next_id += 1;
            self.retained.insert(id, serialized_bytes);
            created.push(RetainedLiveItem {
                value,
                serialized_bytes,
                id,
            });
        }
        self.retained_serialized_bytes = next_bytes;
        Ok(created)
    }

    pub fn release(&mut self, items: impl IntoIterator<Item = RetainedLiveItem>) {
        for item in items {
            if let Some(bytes) = self.retained.remove(&item.id) {
                self.retained_serialized_bytes =
                    self.retained_serialized_bytes.saturating_sub(bytes);
            }
        }
    }

    pub fn usage(&self) -> LiveStreamUsage {
        LiveStreamUsage {
            retained_items: self.retained.len(),
            retained_serialized_bytes: self.retained_serialized_bytes,
        }
    }

    pub fn failed(&self) -> bool {
        self.failed
    }
}

/// T3's `bufferLatestLiveStream`, synchronous: queued full states coalesce by
/// key, and a delivered state stays charged until the next pull ACKs it.
#[derive(Clone, Debug)]
pub struct LatestLiveStreamBuffer {
    budget: LiveStreamBudget,
    pending: HashMap<String, RetainedLiveItem>,
    in_flight: Vec<RetainedLiveItem>,
}

impl LatestLiveStreamBuffer {
    pub fn new(limits: LiveStreamLimits) -> Self {
        Self {
            budget: LiveStreamBudget::new(limits),
            pending: HashMap::new(),
            in_flight: Vec::new(),
        }
    }

    pub fn push(
        &mut self,
        key: impl Into<String>,
        value: JsonValue,
    ) -> std::result::Result<(), LiveStreamBufferError> {
        let key = key.into();
        if self.budget.failed() {
            return Err(LiveStreamBufferError);
        }
        let sequence = value.get("sequence").and_then(JsonValue::as_i64);
        if let Some(previous) = self.pending.get(&key)
            && sequence
                .is_some_and(|sequence| previous.sequence().is_some_and(|prev| prev >= sequence))
        {
            return Ok(());
        }
        let previous = self.pending.get(&key).cloned();
        let replaced = self.budget.replace(previous.as_slice(), vec![value])?;
        let Some(item) = replaced.into_iter().next() else {
            return Ok(());
        };
        self.pending.insert(key, item);
        Ok(())
    }

    /// Pull one batch. The items stay charged until [`Self::ack`].
    pub fn drain(&mut self) -> std::result::Result<Vec<RetainedLiveItem>, LiveStreamBufferError> {
        self.budget.check()?;
        let mut items: Vec<RetainedLiveItem> = self.pending.drain().map(|(_, item)| item).collect();
        items.sort_by_key(|item| item.sequence());
        self.in_flight.extend(items.iter().cloned());
        Ok(items)
    }

    pub fn ack(&mut self, items: Vec<RetainedLiveItem>) {
        for item in &items {
            self.in_flight.retain(|in_flight| in_flight.id != item.id);
        }
        self.budget.release(items);
    }

    pub fn ack_all(&mut self) {
        let in_flight = std::mem::take(&mut self.in_flight);
        self.budget.release(in_flight);
    }

    pub fn usage(&self) -> LiveStreamUsage {
        self.budget.usage()
    }

    pub fn failed(&self) -> bool {
        self.budget.failed()
    }
}

/// Keep the latest value per key in a synchronous batch of live updates.
pub fn buffer_latest_live_stream(
    values: impl IntoIterator<Item = (String, JsonValue)>,
    limits: LiveStreamLimits,
) -> std::result::Result<Vec<JsonValue>, LiveStreamBufferError> {
    let mut buffer = LatestLiveStreamBuffer::new(limits);
    for (key, value) in values {
        buffer.push(key, value)?;
    }
    let items = buffer.drain()?;
    let values = items
        .iter()
        .map(|item| item.value().clone())
        .collect::<Vec<_>>();
    buffer.ack(items);
    Ok(values)
}

// ---------------------------------------------------------------------------
// The stream over the SQLite store
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum ThreadStreamBatch {
    Replay {
        after_sequence: i64,
        through_sequence: i64,
        events: Vec<StoredEvent>,
    },
    Snapshot {
        snapshot: Box<BoundedThreadSnapshot>,
    },
}

impl ThreadStreamBatch {
    pub fn as_items(&self) -> Vec<ThreadStreamItem> {
        match self {
            Self::Replay { events, .. } => {
                events.iter().map(ThreadStreamItem::from_stored).collect()
            }
            Self::Snapshot { snapshot } => vec![ThreadStreamItem::Snapshot {
                snapshot: snapshot.clone(),
            }],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadStreamResume {
    pub high_water: i64,
    pub batch: ThreadStreamBatch,
}

/// Restart recovery: the saved cursor, the projection loaded on open, and the
/// batch that catches the stream up.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadStreamRecovery {
    pub saved_sequence: i64,
    pub high_water: i64,
    pub projection: Option<ThreadProjectionSnapshot>,
    pub batch: ThreadStreamBatch,
}

/// A per-thread stream over the `EventStore` connection.
#[derive(Clone, Debug)]
pub struct ThreadStream<'a> {
    conn: &'a Connection,
    thread_id: String,
}

impl<'a> ThreadStream<'a> {
    pub fn new(conn: &'a Connection, thread_id: impl Into<String>) -> Self {
        Self {
            conn,
            thread_id: thread_id.into(),
        }
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn high_water(&self) -> Result<i64> {
        latest_sequence(self.conn, Some(&self.thread_id))
    }

    /// The sequence at or below which the event log may have been pruned.
    pub fn pruned_through(&self) -> Result<i64> {
        pruned_through_sequence(self.conn)
    }

    /// The saved per-thread stream cursor, if the app has consumed anything.
    pub fn saved_sequence(&self) -> Result<Option<i64>> {
        saved_stream_sequence(self.conn, &self.thread_id)
    }

    /// Persist the per-thread stream cursor for the next restart.
    pub fn save_sequence(&self, sequence: i64, now: &str) -> Result<()> {
        crate::store::projections::upsert_projector_state(
            self.conn,
            &crate::store::projections::ProjectorState {
                projector: format!("{THREAD_STREAM_PROJECTOR_PREFIX}{}", self.thread_id),
                last_applied_sequence: sequence,
                updated_at: now.to_owned(),
            },
        )
    }

    /// Load the projection rows this thread already has.
    pub fn load_projection(&self) -> Result<Option<ThreadProjectionSnapshot>> {
        load_thread_projection(self.conn, &self.thread_id)
    }

    /// Resume after a client cursor. Replays when the log covers the gap and
    /// the replay stays bounded; otherwise sends one bounded snapshot.
    pub fn resume(&self, after_sequence: i64) -> Result<ThreadStreamResume> {
        // Projections load lazily; a normal replay never pays for them.
        self.resume_with(after_sequence, None)
    }

    /// Restart recovery: load the projection and the latest sequence, resume
    /// from the saved cursor, then advance the cursor to the high water.
    pub fn recover(&self, now: &str) -> Result<ThreadStreamRecovery> {
        let projection = self.load_projection()?;
        let saved_sequence = self.saved_sequence()?.unwrap_or(0);
        let high_water = self.high_water()?;
        let resume = self.resume_with(saved_sequence, projection.clone())?;
        self.save_sequence(high_water, now)?;
        Ok(ThreadStreamRecovery {
            saved_sequence,
            high_water,
            projection,
            batch: resume.batch,
        })
    }

    fn resume_with(
        &self,
        after_sequence: i64,
        mut projection: Option<ThreadProjectionSnapshot>,
    ) -> Result<ThreadStreamResume> {
        let high_water = self.high_water()?;
        let pruned_through = self.pruned_through()?;
        // A cursor ahead of the log, or below the pruned boundary with events
        // still missing, cannot be replayed.
        if after_sequence > high_water
            || (after_sequence < high_water && after_sequence < pruned_through)
        {
            return self.snapshot_resume(high_water, projection);
        }

        let events = read(
            self.conn,
            &ReadOptions {
                after_sequence: Some(after_sequence),
                through_sequence: Some(high_water),
                thread_id: Some(self.thread_id.to_owned()),
                limit: Some(THREAD_RESUME_MAX_REPLAY_EVENTS + 1),
                event_type: None,
            },
        )?;
        // Pruning publishes its boundary before deleting; a pass during the
        // read shows up here.
        if after_sequence < high_water && after_sequence < self.pruned_through()? {
            return self.snapshot_resume(high_water, projection);
        }
        // Retained creation events may describe deleted threads; replace the
        // replay with a snapshot when one exists.
        if events
            .iter()
            .any(|event| event.event_type == "thread.created")
        {
            if projection.is_none() {
                projection = self.load_projection()?;
            }
            if projection.is_some() {
                return self.snapshot_resume(high_water, projection);
            }
        }
        let raw_payload_bytes = events
            .iter()
            .map(|event| bytes_of_json(&event.payload))
            .max()
            .unwrap_or(0);
        if events.len() > THREAD_RESUME_MAX_REPLAY_EVENTS
            || !is_thread_replay_raw_payload_safe(raw_payload_bytes)
        {
            return self.snapshot_resume(high_water, projection);
        }
        let items: Vec<ThreadStreamItem> =
            events.iter().map(ThreadStreamItem::from_stored).collect();
        let plan = decide_thread_resume(
            after_sequence,
            high_water,
            items.len(),
            thread_replay_encoded_bytes(&items),
        );
        match plan {
            ThreadResumePlan::Snapshot => self.snapshot_resume(high_water, projection),
            ThreadResumePlan::Replay {
                after_sequence,
                through_sequence,
            } => Ok(ThreadStreamResume {
                high_water,
                batch: ThreadStreamBatch::Replay {
                    after_sequence,
                    through_sequence,
                    events,
                },
            }),
        }
    }

    fn snapshot_resume(
        &self,
        high_water: i64,
        projection: Option<ThreadProjectionSnapshot>,
    ) -> Result<ThreadStreamResume> {
        let projection = match projection {
            Some(projection) => projection,
            None => self
                .load_projection()?
                .ok_or_else(|| StoreError::ThreadNotProjected(self.thread_id.to_owned()))?,
        };
        let snapshot_sequence = u32::try_from(high_water).unwrap_or(u32::MAX);
        let snapshot = build_bounded_thread_stream_snapshot(snapshot_sequence, projection, false);
        Ok(ThreadStreamResume {
            high_water,
            batch: ThreadStreamBatch::Snapshot {
                snapshot: Box::new(snapshot),
            },
        })
    }
}

impl EventStore {
    /// A per-thread stream over this store's connection.
    pub fn thread_stream(&self, thread_id: &str) -> ThreadStream<'_> {
        ThreadStream::new(self.connection(), thread_id)
    }
}

/// T3 stores the prune boundary in the projection metadata rows.
pub fn pruned_through_sequence(conn: &Connection) -> Result<i64> {
    let sequence: Option<i64> = conn
        .query_row(
            "SELECT last_sequence FROM orchestration_v2_projection_metadata
             WHERE projection_name = ?1",
            params![EVENT_LOG_PRUNED_THROUGH],
            |row| row.get(0),
        )
        .optional()?;
    Ok(sequence.unwrap_or(0))
}

fn saved_stream_sequence(conn: &Connection, thread_id: &str) -> Result<Option<i64>> {
    let sequence: Option<i64> = conn
        .query_row(
            "SELECT last_applied_sequence FROM projection_state WHERE projector = ?1",
            params![format!("{THREAD_STREAM_PROJECTOR_PREFIX}{thread_id}")],
            |row| row.get(0),
        )
        .optional()?;
    Ok(sequence)
}

fn read_payloads(conn: &Connection, sql: &str, thread_id: &str) -> Result<Vec<JsonValue>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params![thread_id], |row| row.get::<_, String>(0))?;
    let mut values = Vec::new();
    for row in rows {
        values.push(serde_json::from_str(&row?)?);
    }
    Ok(values)
}

fn decode_rows<T: serde::de::DeserializeOwned>(values: Vec<JsonValue>) -> Result<Vec<T>> {
    values
        .into_iter()
        .map(|value| serde_json::from_value(value).map_err(StoreError::from))
        .collect()
}

/// Rebuild one thread's projection from the projection tables.
pub fn load_thread_projection(
    conn: &Connection,
    thread_id: &str,
) -> Result<Option<ThreadProjectionSnapshot>> {
    let thread_payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM orchestration_v2_projection_threads
             WHERE thread_id = ?1",
            params![thread_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(thread_payload) = thread_payload else {
        return Ok(None);
    };
    let thread: AppThread = serde_json::from_str(&thread_payload)?;
    let updated_at = thread.updated_at.clone();
    let runs = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_runs
         WHERE thread_id = ?1 ORDER BY ordinal, run_id",
        thread_id,
    )?)?;
    let attempts = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_run_attempts
         WHERE thread_id = ?1 ORDER BY attempt_ordinal, attempt_id",
        thread_id,
    )?)?;
    let nodes = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_nodes
         WHERE thread_id = ?1 ORDER BY node_id",
        thread_id,
    )?)?;
    let provider_sessions = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_sessions
         WHERE thread_id = ?1 ORDER BY provider_session_id",
        thread_id,
    )?)?;
    let provider_threads = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_threads
         WHERE thread_id = ?1 ORDER BY provider_thread_id",
        thread_id,
    )?)?;
    let provider_turns = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_turns
         WHERE thread_id = ?1 ORDER BY provider_thread_id, ordinal",
        thread_id,
    )?)?;
    let runtime_requests = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_runtime_requests
         WHERE thread_id = ?1 ORDER BY created_at, runtime_request_id",
        thread_id,
    )?)?;
    let messages = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_messages
         WHERE thread_id = ?1 ORDER BY created_at, message_id",
        thread_id,
    )?)?;
    let plans = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_plans
         WHERE thread_id = ?1 ORDER BY plan_id",
        thread_id,
    )?)?;
    let mut turn_items: Vec<TurnItem> = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_turn_items
         WHERE thread_id = ?1 ORDER BY ordinal, turn_item_id",
        thread_id,
    )?)?;
    turn_items.sort_by_key(|item| {
        (
            item_ordinal(item).unwrap_or(0),
            item.id()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_default(),
        )
    });
    let context_handoffs = decode_rows(read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_context_handoffs
         WHERE thread_id = ?1 ORDER BY updated_at, context_handoff_id",
        thread_id,
    )?)?;
    let checkpoint_scopes = read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_checkpoint_scopes
         WHERE thread_id = ?1 ORDER BY created_at, scope_id",
        thread_id,
    )?;
    let checkpoints = read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_checkpoints
         WHERE thread_id = ?1 ORDER BY captured_at, checkpoint_id",
        thread_id,
    )?;
    let context_transfers = read_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_context_transfers
         WHERE source_thread_id = ?1 OR target_thread_id = ?1
         ORDER BY updated_at, context_transfer_id",
        thread_id,
    )?;

    // T3's projection layer owns lineage; until it lands, this thread's own
    // turn items are its local visible timeline.
    let visible_turn_items = turn_items
        .iter()
        .enumerate()
        .filter_map(|(position, item)| {
            let base = item.base()?;
            Some(ProjectedTurnItem {
                position: position as u32,
                visibility: ProjectedItemVisibility::Local,
                source_thread_id: base.thread_id.clone(),
                source_item_id: base.id.clone(),
                item: item.clone(),
            })
        })
        .collect();

    Ok(Some(ThreadProjectionSnapshot {
        thread,
        runs,
        attempts,
        nodes,
        subagents: Vec::new(),
        provider_sessions,
        provider_threads,
        provider_turns,
        runtime_requests,
        messages,
        plans,
        turn_items,
        checkpoint_scopes,
        checkpoints,
        context_handoffs,
        context_transfers,
        visible_turn_items,
        updated_at,
    }))
}
