// Ported from apps/server/src/orchestration-v2/EffectOutbox.ts and apps/server/src/orchestration-v2/EffectWorker.ts (T3 Code v0.1.11, MIT).

//! The effect outbox and the worker that drains it.
//!
//! Pure over plain data plus a `now` argument: claiming, retry backoff,
//! terminal failure, cancellation and process-loss reconciliation are all
//! deterministic. Executing an effect is a caller-supplied closure, so no
//! provider process lives here.
//!
//! Claim rules ported from `EffectOutbox.claimableCandidatePredicate`:
//! - an effect waits for every earlier pending effect on its thread and for
//!   any running effect on its thread;
//! - title generation is its own lane, so a long title job never blocks turn
//!   work and turn work never blocks title generation;
//! - retries keep their original row order (a retry's backoff blocks later
//!   work, so a turn cannot race a pending rollback).

use std::collections::BTreeSet;

use super::types::{EffectRequest, PendingEffect, ThreadId, Timestamp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedEffect {
    pub id: String,
    pub command_id: String,
    pub thread_id: ThreadId,
    pub request: EffectRequest,
    pub status: EffectStatus,
    pub attempt_count: u32,
    pub available_at: Timestamp,
    pub lease_owner: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub last_error: Option<String>,
    /// Insertion order, the Rust stand-in for SQLite `rowid`.
    pub sequence: u64,
}

impl QueuedEffect {
    pub fn type_name(&self) -> &'static str {
        self.request.type_name()
    }

    fn is_title_generation(&self) -> bool {
        matches!(self.request, EffectRequest::ThreadTitleGenerate { .. })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconcileSummary {
    pub requeued: usize,
    pub cancelled: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutboxQueue {
    effects: Vec<QueuedEffect>,
    next_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectExecutionError {
    pub message: String,
    /// The failure proves the starting run is gone, so the last attempt settles it.
    pub cannot_settle_run: bool,
}

impl EffectExecutionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cannot_settle_run: false,
        }
    }

    pub fn run_state_missing(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cannot_settle_run: true,
        }
    }
}

impl std::fmt::Display for EffectExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl OutboxQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn effects(&self) -> &[QueuedEffect] {
        &self.effects
    }

    pub fn get(&self, effect_id: &str) -> Option<&QueuedEffect> {
        self.effects.iter().find(|effect| effect.id == effect_id)
    }

    pub fn get_mut(&mut self, effect_id: &str) -> Option<&mut QueuedEffect> {
        self.effects
            .iter_mut()
            .find(|effect| effect.id == effect_id)
    }

    /// Persist rows only; notifying workers is the caller's post-commit step.
    pub fn enqueue(&mut self, effects: impl IntoIterator<Item = PendingEffect>, now: Timestamp) {
        for effect in effects {
            if self.effects.iter().any(|existing| existing.id == effect.id) {
                continue;
            }
            let sequence = self.next_sequence;
            self.next_sequence += 1;
            self.effects.push(QueuedEffect {
                id: effect.id,
                command_id: effect.command_id,
                thread_id: effect.thread_id,
                request: effect.request,
                status: EffectStatus::Pending,
                attempt_count: 0,
                available_at: effect.available_at.unwrap_or(now),
                lease_owner: None,
                created_at: now,
                updated_at: now,
                completed_at: None,
                last_error: None,
                sequence,
            });
        }
    }

    /// Claim the next effect that may run, marking it running for `worker_id`.
    pub fn claim_next(
        &mut self,
        worker_id: &str,
        now: Timestamp,
        exclude_restart_continuations: bool,
        max_running_title_generations: Option<usize>,
    ) -> Option<QueuedEffect> {
        let index = self
            .effects
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                self.is_claimable(
                    candidate,
                    now,
                    exclude_restart_continuations,
                    max_running_title_generations,
                )
            })
            .min_by_key(|(_, candidate)| {
                (
                    candidate.available_at,
                    candidate.created_at,
                    candidate.sequence,
                )
            })
            .map(|(index, _)| index)?;

        let effect = &mut self.effects[index];
        effect.status = EffectStatus::Running;
        effect.attempt_count += 1;
        effect.lease_owner = Some(worker_id.to_owned());
        effect.updated_at = now;
        effect.last_error = None;
        Some(effect.clone())
    }

    fn is_claimable(
        &self,
        candidate: &QueuedEffect,
        now: Timestamp,
        exclude_restart_continuations: bool,
        max_running_title_generations: Option<usize>,
    ) -> bool {
        if candidate.status != EffectStatus::Pending || candidate.available_at > now {
            return false;
        }
        if candidate.is_title_generation()
            && max_running_title_generations.is_some_and(|limit| {
                self.effects
                    .iter()
                    .filter(|effect| {
                        effect.status == EffectStatus::Running && effect.is_title_generation()
                    })
                    .count()
                    >= limit
            })
        {
            return false;
        }
        for active in &self.effects {
            if active.thread_id != candidate.thread_id {
                continue;
            }
            let blocks = active.status == EffectStatus::Running
                || (active.status == EffectStatus::Pending
                    && active.sequence < candidate.sequence
                    && (!exclude_restart_continuations
                        || active.type_name() != "provider-runtime.continue"));
            if !blocks {
                continue;
            }
            let same_lane = (candidate.is_title_generation() && active.is_title_generation())
                || (!candidate.is_title_generation() && !active.is_title_generation());
            if same_lane {
                return false;
            }
        }
        true
    }

    /// The earliest time at which some effect can be claimed; idle workers poll it.
    pub fn next_claimable_at(
        &self,
        exclude_restart_continuations: bool,
        max_running_title_generations: Option<usize>,
    ) -> Option<Timestamp> {
        self.effects
            .iter()
            .filter(|candidate| {
                self.is_claimable(
                    candidate,
                    Timestamp::MAX,
                    exclude_restart_continuations,
                    max_running_title_generations,
                )
            })
            .map(|candidate| candidate.available_at)
            .min()
    }

    pub fn succeed(&mut self, effect_id: &str, worker_id: &str, now: Timestamp) -> bool {
        self.settle_owned(effect_id, worker_id, now, EffectStatus::Succeeded, None)
    }

    pub fn fail_effect(
        &mut self,
        effect_id: &str,
        worker_id: &str,
        error: impl Into<String>,
        now: Timestamp,
    ) -> bool {
        self.settle_owned(
            effect_id,
            worker_id,
            now,
            EffectStatus::Failed,
            Some(error.into()),
        )
    }

    fn settle_owned(
        &mut self,
        effect_id: &str,
        worker_id: &str,
        now: Timestamp,
        status: EffectStatus,
        error: Option<String>,
    ) -> bool {
        let Some(effect) = self
            .effects
            .iter_mut()
            .find(|effect| effect.id == effect_id)
        else {
            return false;
        };
        if effect.status != EffectStatus::Running
            || effect.lease_owner.as_deref() != Some(worker_id)
        {
            return false;
        }
        effect.status = status;
        effect.lease_owner = None;
        effect.completed_at = Some(now);
        effect.updated_at = now;
        effect.last_error = error;
        true
    }

    /// Return a claimed effect to the queue with a backoff delay.
    pub fn retry(
        &mut self,
        effect_id: &str,
        worker_id: &str,
        error: impl Into<String>,
        delay_ms: i64,
        now: Timestamp,
    ) -> bool {
        let Some(effect) = self
            .effects
            .iter_mut()
            .find(|effect| effect.id == effect_id)
        else {
            return false;
        };
        if effect.status != EffectStatus::Running
            || effect.lease_owner.as_deref() != Some(worker_id)
        {
            return false;
        }
        effect.status = EffectStatus::Pending;
        effect.available_at = now.saturating_add(delay_ms.max(0));
        effect.lease_owner = None;
        effect.updated_at = now;
        effect.last_error = Some(error.into());
        true
    }

    /// Cancel every unsettled effect of the named types on one thread.
    pub fn cancel_unsettled(
        &mut self,
        thread_id: &str,
        effect_types: &[&str],
        reason: &str,
        now: Timestamp,
    ) -> Vec<String> {
        if effect_types.is_empty() {
            return Vec::new();
        }
        self.effects
            .iter_mut()
            .filter(|effect| {
                effect.thread_id == thread_id
                    && matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
                    && effect_types.contains(&effect.type_name())
            })
            .map(|effect| {
                effect.status = EffectStatus::Cancelled;
                effect.lease_owner = None;
                effect.completed_at = Some(now);
                effect.updated_at = now;
                effect.last_error = Some(reason.to_owned());
                effect.id.clone()
            })
            .collect()
    }

    /// A new server process: replay-safe running effects go back to `pending`;
    /// process-bound ones are cancelled because their process is gone.
    pub fn reconcile_after_process_loss(&mut self, now: Timestamp) -> ReconcileSummary {
        let mut summary = ReconcileSummary::default();
        for effect in &mut self.effects {
            if effect.status == EffectStatus::Running
                && effect.request.is_replay_safe_after_process_loss()
            {
                effect.status = EffectStatus::Pending;
                effect.lease_owner = None;
                effect.available_at = now;
                effect.updated_at = now;
                effect.last_error =
                    Some("Requeued after the previous server process ended.".into());
                summary.requeued += 1;
            } else if matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
                && effect.request.is_process_bound()
            {
                effect.status = EffectStatus::Cancelled;
                effect.lease_owner = None;
                effect.completed_at = Some(now);
                effect.updated_at = now;
                effect.last_error = Some(
                    "Cancelled because the server process ended before the effect completed."
                        .into(),
                );
                summary.cancelled += 1;
            }
        }
        summary
    }

    /// Claim and execute one effect, then settle it exactly as the T3 worker does.
    ///
    /// Returns whether an effect was attempted.
    pub fn run_once<F>(
        &mut self,
        worker_id: &str,
        now: Timestamp,
        max_attempts: u32,
        max_running_title_generations: Option<usize>,
        mut execute: F,
    ) -> bool
    where
        F: FnMut(&QueuedEffect) -> Result<(), EffectExecutionError>,
    {
        let Some(effect) = self.claim_next(worker_id, now, false, max_running_title_generations)
        else {
            return false;
        };
        match execute(&effect) {
            Ok(()) => {
                self.succeed(&effect.id, worker_id, now);
            }
            Err(error) => {
                let non_retryable = is_non_retryable_provider_turn_control_failure(
                    effect.type_name(),
                    &error.message,
                );
                if non_retryable {
                    self.succeed(&effect.id, worker_id, now);
                } else if effect.attempt_count >= max_attempts
                    && (!effect.request.owns_starting_run() || error.cannot_settle_run)
                {
                    self.fail_effect(&effect.id, worker_id, error.message, now);
                } else {
                    let delay = retry_delay_ms(effect.attempt_count);
                    self.retry(&effect.id, worker_id, error.message, delay, now);
                }
            }
        }
        true
    }

    /// Drain until idle, at most `max_effects` effects.
    pub fn drain<F>(
        &mut self,
        worker_id: &str,
        now: Timestamp,
        max_attempts: u32,
        max_effects: usize,
        mut execute: F,
    ) -> usize
    where
        F: FnMut(&QueuedEffect) -> Result<(), EffectExecutionError>,
    {
        let mut completed = 0;
        while completed < max_effects
            && self.run_once(worker_id, now, max_attempts, None, &mut execute)
        {
            completed += 1;
        }
        completed
    }

    /// Cancelled effect ids still tracked, for tests and diagnostics.
    pub fn cancelled_ids(&self) -> BTreeSet<&str> {
        self.effects
            .iter()
            .filter(|effect| effect.status == EffectStatus::Cancelled)
            .map(|effect| effect.id.as_str())
            .collect()
    }
}

/// Exponential backoff capped at 30 s, as `EffectWorker.runOnce` computes it.
pub fn retry_delay_ms(attempt_count: u32) -> i64 {
    (100 * 2_i64.pow(attempt_count.saturating_sub(1).min(16))).min(30_000)
}

/// An interrupt the provider has already ended is not worth retrying.
pub fn is_non_retryable_provider_turn_control_failure(effect_type: &str, message: &str) -> bool {
    if effect_type != "provider-turn.interrupt" {
        return false;
    }
    let message = message.to_ascii_lowercase();
    [
        "is not active",
        "hard teardown is already in progress",
        "treating as already interrupted",
        "treating as already stopped",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{
        CommandId, EffectRequest, ModelSelection, PendingEffect, ProviderSessionId,
        ProviderThreadId, ProviderTurnId, RunId,
    };

    fn effect(
        id: &str,
        thread: &str,
        request: EffectRequest,
        available_at: Option<i64>,
    ) -> PendingEffect {
        PendingEffect {
            id: id.into(),
            command_id: CommandId::from(format!("command:{id}")),
            thread_id: thread.into(),
            request,
            available_at,
        }
    }

    fn start(run_id: &str) -> EffectRequest {
        EffectRequest::ProviderTurnStart {
            run_id: RunId::from(run_id),
        }
    }

    fn interrupt() -> EffectRequest {
        EffectRequest::ProviderTurnInterrupt {
            provider_session_id: ProviderSessionId::from("session"),
            provider_thread_id: ProviderThreadId::from("provider-thread"),
            provider_turn_id: ProviderTurnId::from("provider-turn"),
        }
    }

    fn title() -> EffectRequest {
        EffectRequest::ThreadTitleGenerate {
            message_id: "message:title".into(),
        }
    }

    #[test]
    fn claims_one_effect_per_thread_in_order() {
        let mut outbox = OutboxQueue::new();
        outbox.enqueue(
            [
                effect("first", "thread:a", start("run:first"), Some(10)),
                effect("second", "thread:a", start("run:second"), Some(10)),
                effect("other", "thread:b", start("run:other"), Some(10)),
            ],
            0,
        );

        let first = outbox.claim_next("worker", 10, false, None).expect("claim");
        assert_eq!(first.id, "first");
        // A running effect blocks its own thread but not another one.
        let other = outbox.claim_next("worker", 10, false, None).expect("claim");
        assert_eq!(other.id, "other");
        assert!(outbox.claim_next("worker", 10, false, None).is_none());
        assert_eq!(
            outbox.get("second").expect("second").status,
            EffectStatus::Pending
        );
    }

    #[test]
    fn retry_backoff_blocks_later_work_until_it_is_due() {
        let mut outbox = OutboxQueue::new();
        outbox.enqueue(
            [
                effect("first", "thread:a", start("run:first"), Some(0)),
                effect("second", "thread:a", start("run:second"), Some(0)),
            ],
            0,
        );
        let claimed = outbox.claim_next("worker", 0, false, None).expect("claim");
        assert_eq!(claimed.attempt_count, 1);
        assert!(outbox.retry("first", "worker", "boom", 200, 0));
        assert_eq!(outbox.get("first").expect("first").available_at, 200);
        assert!(outbox.claim_next("worker", 199, false, None).is_none());
        let second = outbox
            .claim_next("worker", 200, false, None)
            .expect("claim");
        assert_eq!(second.id, "first");
        assert_eq!(second.attempt_count, 2);
        assert_eq!(retry_delay_ms(1), 100);
        assert_eq!(retry_delay_ms(2), 200);
        assert_eq!(retry_delay_ms(20), 30_000);
    }

    #[test]
    fn title_generation_does_not_block_turn_work() {
        let mut outbox = OutboxQueue::new();
        outbox.enqueue(
            [
                effect("title", "thread:a", title(), Some(0)),
                effect("start", "thread:a", start("run:start"), Some(0)),
            ],
            0,
        );
        let first = outbox.claim_next("worker", 0, false, None).expect("claim");
        let second = outbox.claim_next("worker", 0, false, None).expect("claim");
        assert_eq!(
            BTreeSet::from([first.id.as_str(), second.id.as_str()]),
            BTreeSet::from(["title", "start"])
        );
    }

    #[test]
    fn max_running_title_generations_caps_the_lane() {
        let mut outbox = OutboxQueue::new();
        outbox.enqueue(
            [
                effect("title:a", "thread:a", title(), Some(0)),
                effect("title:b", "thread:b", title(), Some(0)),
            ],
            0,
        );
        assert!(outbox.claim_next("worker", 0, false, Some(1)).is_some());
        assert!(outbox.claim_next("worker", 0, false, Some(1)).is_none());
    }

    #[test]
    fn last_attempt_fails_a_run_effect_and_settles_a_missing_run() {
        // An ordinary start failure keeps retry ownership even at the last
        // attempt; only a verdict that the run is gone settles it.
        let mut outbox = OutboxQueue::new();
        outbox.enqueue(
            [effect("start", "thread:a", start("run:start"), Some(0))],
            0,
        );
        outbox.run_once("worker", 0, 1, None, |_| {
            Err(EffectExecutionError::new("provider refused"))
        });
        assert_eq!(
            outbox.get("start").expect("start").status,
            EffectStatus::Pending
        );

        let mut missing = OutboxQueue::new();
        missing.enqueue(
            [effect("start", "thread:a", start("run:start"), Some(0))],
            0,
        );
        missing.run_once("worker", 0, 1, None, |_| {
            Err(EffectExecutionError::run_state_missing("run is gone"))
        });
        assert_eq!(
            missing.get("start").expect("start").status,
            EffectStatus::Failed
        );

        let mut retrying = OutboxQueue::new();
        retrying.enqueue(
            [effect("start", "thread:a", start("run:start"), Some(0))],
            0,
        );
        retrying.run_once("worker", 0, 5, None, |_| {
            Err(EffectExecutionError::new("provider refused"))
        });
        let effect = retrying.get("start").expect("start");
        assert_eq!(effect.status, EffectStatus::Pending);
        assert_eq!(effect.available_at, 100);
    }

    #[test]
    fn a_settled_interrupt_is_not_retried() {
        let mut outbox = OutboxQueue::new();
        outbox.enqueue([effect("interrupt", "thread:a", interrupt(), Some(0))], 0);
        outbox.run_once("worker", 0, 5, None, |_| {
            Err(EffectExecutionError::new("turn is not active"))
        });
        assert_eq!(
            outbox.get("interrupt").expect("interrupt").status,
            EffectStatus::Succeeded
        );
    }

    #[test]
    fn process_loss_requeues_replay_safe_and_cancels_process_bound() {
        let mut outbox = OutboxQueue::new();
        outbox.enqueue(
            [
                effect("title", "thread:a", title(), Some(0)),
                effect("start", "thread:b", start("run:start"), Some(0)),
            ],
            0,
        );
        assert!(outbox.claim_next("worker", 0, false, None).is_some());
        assert!(outbox.claim_next("worker", 0, false, None).is_some());

        let summary = outbox.reconcile_after_process_loss(50);
        assert_eq!(summary.requeued, 1);
        assert_eq!(summary.cancelled, 1);
        assert_eq!(
            outbox.get("title").expect("title").status,
            EffectStatus::Pending
        );
        assert_eq!(outbox.get("title").expect("title").available_at, 50);
        assert_eq!(
            outbox.get("start").expect("start").status,
            EffectStatus::Cancelled
        );
    }

    #[test]
    fn cancellation_settles_pending_and_running_effects() {
        let mut outbox = OutboxQueue::new();
        outbox.enqueue(
            [
                effect("start", "thread:a", start("run:start"), Some(0)),
                effect("title", "thread:a", title(), Some(0)),
            ],
            0,
        );
        assert!(outbox.claim_next("worker", 0, false, None).is_some());
        let cancelled = outbox.cancel_unsettled("thread:a", &["provider-turn.start"], "stop", 10);
        assert_eq!(cancelled, vec!["start".to_owned()]);
        assert_eq!(
            outbox.get("start").expect("start").status,
            EffectStatus::Cancelled
        );
        assert_eq!(
            outbox.get("title").expect("title").status,
            EffectStatus::Pending
        );
    }

    #[test]
    fn a_queued_steer_carries_its_message_and_optional_selection() {
        let selection = ModelSelection::new("codex", "gpt");
        let request = EffectRequest::ProviderTurnSteer {
            provider_session_id: "session".into(),
            provider_thread_id: "provider-thread".into(),
            provider_turn_id: "provider-turn".into(),
            message_id: "message:steer".into(),
            model_selection: Some(selection.clone()),
        };
        let pending = effect("steer", "thread:a", request, None);
        let mut outbox = OutboxQueue::new();
        outbox.enqueue([pending], 7);
        let claimed = outbox.claim_next("worker", 7, false, None).expect("claim");
        assert_eq!(claimed.available_at, 7);
        assert!(matches!(
            claimed.request,
            EffectRequest::ProviderTurnSteer { ref model_selection, .. }
                if model_selection.as_ref() == Some(&selection)
        ));
    }
}
