// Ported from apps/server/src/orchestration-v2/{Orchestrator,EventSink,EffectWorker,EffectOutbox,RuntimePolicy,runtimeLayer,QueuedRunOrder,CommandPolicy}.ts (T3 Code v0.1.11, MIT).

//! Commands decided as events, then applied to projections.
//!
//! This is the pure, no-I/O core of T3's orchestration-v2. Time enters as a
//! `now` argument; provider processes are represented only as outbox effects;
//! persistence is the [`event_sink::Store`] seam.
//!
//! # Ported slice
//!
//! - `thread.create`, including the first run and initial message committing
//!   together with the thread ([`orchestrator::decide`]);
//! - `message.dispatch` intake: start vs steer vs queue, maintenance-command
//!   queuing, the queued tail's model selection, title seeding;
//! - `thread.stop` / `run.interrupt`: interrupt card, effect, immediate settle
//!   before a provider start, queue hold;
//! - provider turn start and run completion/failure, with the next queued run
//!   starting in the same decision ([`orchestrator::plan_run_finished`]);
//! - approval requested and answered (`runtime-request.respond`);
//! - `thread.model-selection.set`, `thread.runtime-mode.set`;
//! - `queue.resume`, `queued-run.start`;
//! - the `Store` transaction (events + projections + receipt + outbox) and a
//!   memory reference implementation;
//! - outbox claim/retry/terminal-fail/cancel/process-loss reconciliation;
//! - runtime policy: provider mode narrowing, managed ceiling, overrides;
//! - queued-run delivery order.
//!
//! # Unported commands
//!
//! These remain in the TypeScript orchestrator and must not be assumed here:
//!
//! `thread.archive`, `thread.unarchive`, `thread.delete`, `thread.settle`,
//! `thread.auto-settle`, `thread.unsettle`, `thread.snooze`,
//! `thread.unsnooze`, `thread.auto-settle.set`, `thread.pin`, `thread.unpin`,
//! `thread.pin.reorder`, `thread.active.reorder`, `thread.tag.set`,
//! `thread.visit`, `thread.mark-unread`, `thread.metadata.update`,
//! `thread.pull-request.link`, `thread.pull-request.unlink`,
//! `thread.pull-request-link.sync`, `thread.issue.link`, `thread.issue.unlink`,
//! `thread.pull-request.watch`, `thread.pull-request-watch.sync`,
//! `thread.pull-request-watch.checks-reopen`, `thread.pull-request.sync`,
//! `thread.title.regeneration.complete`, `thread.project.adopt-folder`,
//! `thread.interaction-mode.set`, `thread.guest-share.open`,
//! `thread.guest-share.redeem`, `thread.guest-message.submit`,
//! `thread.guest-message.dismiss`, `thread.fork`, `thread.merge_back`,
//! `thread.created.record`, `thread.user-input.dismiss`,
//! `thread.background-work.settle`, `thread.interrupt.unresponsive`,
//! `provider.switch`, `provider-session.detach`, `message.dispatch` extras
//! (goal commands, idle compaction, source plans, fork/merge-back transfers,
//! delegated-completion cohorts, guest attribution, restart/manual/usage-limit
//! continuations, steer-follow-up ordinal interleaving),
//! `prepared-run.release`, `prepared-run.progress`, `prepared-run.fail`,
//! `prepared-run.retry`, `notification.delivery.accept`, `subagent.stop`,
//! `queued-message.promote-to-steer`, `queued-run.reorder`,
//! `queued-run.cancel`, `queued-run.edit`, `checkpoint.rollback`,
//! `delegated_task.*`, and `runtime-request.delivery.fail`.
//!
//! # Unported effects
//!
//! `provider-runtime.continue`, `provider-session.detach`,
//! `provider-session.apply-thread-title`, `provider-thread.rollback`,
//! `checkpoint.capture`, `scheduled-tasks.cleanup`, `terminal.cleanup`,
//! `preview.cleanup`, `attachment.cleanup`, and the full restart/session
//! transition and steer-follow-up fallback logic. The executor for every
//! effect this module emits is also out of scope: this module only decides and
//! queues them.

pub mod effect_worker;
pub mod event_sink;
pub mod orchestrator;
pub mod queued_run_order;
pub mod runtime_policy;
pub mod types;

#[cfg(test)]
mod tests;

pub use effect_worker::{EffectExecutionError, EffectStatus, OutboxQueue, QueuedEffect};
pub use event_sink::{
    CancelUnsettled, CommandReceipt, CommitCommandInput, CommitCommandOutput, MemoryStore,
    ReceiptStatus, RejectedCommandInput, Store, StoreError, StoredEvent,
};
pub use orchestrator::{
    ApprovalRequested, Decision, DispatchResult, decide, dispatch, is_native_maintenance_command,
    plan_approval_requested, plan_provider_turn_started, plan_run_finished,
    plan_start_next_queued_run, provider_switch_waits_for_turn, resolve_dispatch_intent,
};
pub use queued_run_order::{is_automatic_completion_run, queued_runs_in_delivery_order};
pub use runtime_policy::{
    ManagedPolicy, PermissionDisposition, ResolvedRuntimePolicy, RuntimePolicy, RuntimePolicyError,
    RuntimePolicyOverride, acp_permission_disposition, managed_runtime_mode_ceiling_message,
    provider_runtime_mode,
};
pub use types::*;
