// Ported from apps/server/src/orchestration-v2/EventStore.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/orchestration-v2/ProjectionStore.ts (T3 Code v0.1.11, MIT).
// Ported from apps/server/src/persistence/Migrations/055_OrchestrationV2.ts (T3 Code v0.1.11, MIT).
//!
//! The append-only application event log and its projections.
//!
//! T3 keeps two stores over one SQLite database: `OrchestrationEventStore`
//! writes the shared `orchestration_events` log (project events and V2 thread
//! events), and `ProjectionStore` folds the same domain events into indexed
//! projection rows that carry the original payload JSON. This port keeps the
//! same tables, columns and keys; it is synchronous (`rusqlite`) and has no
//! async or effect runtime.
//!
//! The orchestration layer writes a command through one path,
//! [`EventStore::apply`]: events, the command receipt and every projection row
//! commit in a single `BEGIN IMMEDIATE` transaction, so a failure leaves no
//! half-applied command behind.

mod events;
mod migrations;
mod orchestration;
mod projections;
mod stream;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stream_tests;

pub use events::{
    ApplyCommand, ApplyResult, CommandReceipt, EventStore, NewEvent, NewProjectEvent, ReadOptions,
    StoreError, append_events, append_project_event, latest_application_sequence, latest_sequence,
    read, read_by_command_id, read_receipt, upsert_receipt,
};
pub use migrations::migrate;
pub use projections::{
    ActivityProjection, MessageProjection, NodeProjection, PendingApprovalProjection,
    ProjectionStore, ProjectionWrite, ProjectorState, ProviderSessionProjection,
    RunAttemptProjection, RunProjection, ThreadProjection, TurnProjection,
};
pub use stream::{
    BoundedProjectionResult, BoundedThreadSnapshot, ContextHandoffRecord, LIVE_STREAM_MAX_ITEMS,
    LIVE_STREAM_MAX_SERIALIZED_BYTES, LatestLiveStreamBuffer, LiveStreamBudget,
    LiveStreamBufferError, LiveStreamLimits, LiveStreamUsage, RetainedLiveItem,
    SelectedTimelinePage, THREAD_HISTORY_INLINE_WORK_ITEM_LIMIT, THREAD_HISTORY_MAX_RAW_TURNS,
    THREAD_HISTORY_PAGE_POLICY, THREAD_HISTORY_SNAPSHOT_ROW_LIMIT,
    THREAD_HISTORY_TURN_SNAPSHOT_ROW_LIMIT, THREAD_RESUME_MAX_RAW_PAYLOAD_BYTES,
    THREAD_RESUME_MAX_REPLAY_ENCODED_BYTES, THREAD_RESUME_MAX_REPLAY_EVENTS,
    ThreadHistoryCursorPayload, ThreadHistoryPagePolicy, ThreadHistoryPrefix,
    ThreadProjectionSnapshot, ThreadResumePlan, ThreadStream, ThreadStreamBatch, ThreadStreamEvent,
    ThreadStreamItem, ThreadStreamRecovery, ThreadStreamResume, bounded_snapshot_projection,
    bounded_timeline_encoded_bytes, buffer_latest_live_stream, build_bounded_thread_projection,
    build_bounded_thread_stream_snapshot, compute_latest_local_turn_ordinal, decide_thread_resume,
    decode_thread_history_cursor, encode_thread_history_cursor, find_cursor_end_index,
    is_conversation_history_item, is_thread_history_user_turn, is_thread_replay_raw_payload_safe,
    load_thread_projection, older_history_page_policy, omit_local_visible_turn_items,
    project_thread_projection_for_wire, projected_row_bounded_snapshot_encoded_bytes,
    projected_row_encoded_bytes, pruned_through_sequence, restore_local_visible_turn_items,
    select_history_page_from_cursor, select_recent_timeline_window, thread_replay_encoded_bytes,
    thread_replay_encoded_bytes_with_limit,
};
