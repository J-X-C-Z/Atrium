// Ported from apps/server/src/orchestration-v2/Orchestrator.ts and apps/server/src/orchestration-v2/CommandPolicy.ts (T3 Code v0.1.11, MIT).

//! The thread lifecycle commands, decided as events.
//!
//! Every function here is pure: it reads a [`Projection`] snapshot, takes
//! `now`, and returns [`Decision`] (events + outbox effects). Persistence
//! belongs to [`dispatch`], which commits through the [`Store`] seam.
//!
//! Ported commands:
//! - `thread.create` (thread, first run and initial message commit together)
//! - `message.dispatch` (start vs steer vs queue, plus maintenance titles)
//! - `thread.stop`, `run.interrupt`
//! - provider turn started / run completed / failed
//! - approval requested / answered (`runtime-request.respond`)
//! - `thread.model-selection.set`, `thread.runtime-mode.set`
//! - `queue.resume`, `queued-run.start`
//!
//! Unported commands are listed in the module documentation of
//! [`crate::orchestration`].
//!
//! Simplifications (kept explicit so the store module can add them):
//! - goal commands, idle compaction, source plans, fork/merge-back transfers,
//!   delegated-completion cohorts and guest shares are not decided here;
//! - provider/model capability lookups for steering come from the projection,
//!   never from an adapter process;
//! - worktree preparation is represented as `defer_start`; the launch service
//!   owns the actual preparation.

use super::event_sink::{
    CancelUnsettled, CommandReceipt, CommitCommandInput, ReceiptStatus, RejectedCommandInput,
    Store, StoredEvent,
};
use super::types::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub events: Vec<DomainEvent>,
    pub effects: Vec<PendingEffect>,
    pub cancel_unsettled: Option<CancelUnsettled>,
}

impl Decision {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.effects.is_empty() && self.cancel_unsettled.is_none()
    }

    fn add_effect(
        &mut self,
        command_id: &str,
        thread_id: &str,
        suffix: &str,
        request: EffectRequest,
    ) {
        self.effects.push(PendingEffect {
            id: format!("effect:{command_id}:{suffix}"),
            command_id: command_id.to_owned(),
            thread_id: thread_id.to_owned(),
            request,
            available_at: None,
        });
    }
}

/// A deterministic event id factory, the pure analogue of T3's `emit`.
struct Emitter {
    command_id: CommandId,
    thread_id: ThreadId,
    now: Timestamp,
    next: u32,
}

impl Emitter {
    fn new(command_id: &str, thread_id: &str, now: Timestamp) -> Self {
        Self {
            command_id: command_id.to_owned(),
            thread_id: thread_id.to_owned(),
            now,
            next: 0,
        }
    }

    fn event(&mut self, kind: EventKind) -> DomainEvent {
        self.next += 1;
        DomainEvent::new(
            format!("event:{}:{}", self.command_id, self.next),
            self.thread_id.clone(),
            self.now,
            kind,
        )
    }
}

fn push_event(projection: &mut Projection, decision: &mut Decision, event: DomainEvent) {
    projection.apply(&event);
    decision.events.push(event);
}

/// Decide one command against an optional projection snapshot.
pub fn decide(
    existing: Option<&Projection>,
    command: &Command,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    match command {
        Command::ThreadCreate(command) => decide_thread_create(existing, command, now),
        _ => {
            let Some(existing) = existing else {
                return Err(OrchestratorError::ThreadNotFound(
                    command.thread_id().clone(),
                ));
            };
            let mut projection = existing.clone();
            let mut emitter = Emitter::new(command.command_id(), command.thread_id(), now);
            let mut decision = Decision::new();
            match command {
                Command::MessageDispatch(command) => {
                    dispatch_message(&mut projection, command, now, &mut emitter, &mut decision)?;
                }
                Command::ThreadStop(command) => {
                    dispatch_thread_stop(
                        &mut projection,
                        command,
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::RunInterrupt(command) => {
                    plan_run_interrupt(&mut projection, command, now, &mut emitter, &mut decision)?;
                }
                Command::RuntimeRequestRespond(command) => {
                    dispatch_runtime_request_respond(
                        &mut projection,
                        command,
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::ThreadModelSelectionSet(command) => {
                    dispatch_model_selection(
                        &mut projection,
                        command,
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::ThreadRuntimeModeSet(command) => {
                    dispatch_runtime_mode(
                        &mut projection,
                        command,
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::QueueResume { .. } => {
                    plan_start_next_queued_run_with(
                        &mut projection,
                        None,
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::QueuedRunStart { run_id, .. } => {
                    plan_start_next_queued_run_with(
                        &mut projection,
                        Some(run_id.as_str()),
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::ThreadGuestShareClose { .. } => {
                    // Closing a share that never existed is a successful no-op.
                }
                Command::ThreadCreate(_) => unreachable!("handled above"),
            }
            Ok(decision)
        }
    }
}

fn decide_thread_create(
    existing: Option<&Projection>,
    command: &ThreadCreate,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    if existing.is_some() {
        return Err(OrchestratorError::ThreadAlreadyExists {
            command_id: command.command_id.clone(),
            thread_id: command.thread_id.clone(),
        });
    }
    let thread = Thread {
        id: command.thread_id.clone(),
        project_id: command.project_id.clone(),
        title: command.title.clone(),
        provider_instance_id: command.model_selection.instance_id.clone(),
        model_selection: command.model_selection.clone(),
        runtime_mode: command.runtime_mode,
        interaction_mode: command.interaction_mode,
        branch: command.branch.clone(),
        worktree_path: command.worktree_path.clone(),
        active_provider_thread_id: None,
        created_by: command.created_by,
        creation_source: command.creation_source,
        created_at: now,
        updated_at: now,
        archived_at: None,
        settled_override: None,
        settled_at: None,
        last_visited_at: None,
        title_regeneration: None,
        deleted_at: None,
    };
    let mut projection = Projection::new(thread.clone());
    let mut emitter = Emitter::new(&command.command_id, &command.thread_id, now);
    let mut decision = Decision::new();
    let created = emitter.event(EventKind::ThreadCreated(thread));
    push_event(&mut projection, &mut decision, created);

    // A first message commits with the thread and its run; no client ever sees
    // the new thread idle. A failure here rejects the whole command.
    if let Some(initial) = &command.initial_message {
        let dispatch = MessageDispatch {
            command_id: command.command_id.clone(),
            thread_id: command.thread_id.clone(),
            message_id: initial.message_id.clone(),
            text: initial.text.clone(),
            attachments: initial.attachments.clone(),
            created_by: command.created_by,
            creation_source: command.creation_source,
            model_selection: Some(command.model_selection.clone()),
            dispatch_mode: match &initial.workspace_strategy {
                Some(workspace_strategy) => DispatchMode::DeferStart {
                    workspace_strategy: Some(workspace_strategy.clone()),
                },
                None => DispatchMode::StartImmediately,
            },
            delivery_intent: None,
            scheduled_task_id: initial.scheduled_task_id.clone(),
            sender_thread_id: initial.sender_thread_id.clone(),
            title_seed: initial.title_seed.clone(),
            fresh_context: false,
            notification: None,
            delegated_completion: None,
            steer_follow_up_of_run_id: None,
            steer_follow_up_after_run_ordinal: None,
            mode_limit: command.mode_limit.clone(),
        };
        dispatch_message(&mut projection, &dispatch, now, &mut emitter, &mut decision)?;
    }
    Ok(decision)
}

/// Decide how a message enters the thread: start, steer, or queue.
///
/// Port of `CommandPolicy.resolveMessageDispatchIntent`: an explicit delivery
/// intent (auto/steer/restart) resolves against the provider's authoritative
/// capabilities; maintenance commands never steer.
pub fn resolve_dispatch_intent(
    projection: &Projection,
    requested: DispatchMode,
    delivery_intent: Option<DeliveryIntent>,
) -> DispatchMode {
    let mode = resolve_delivery_intent(projection, requested, delivery_intent);
    let DispatchMode::SteerActive { target_run_id } = &mode else {
        return mode;
    };
    let Some(target) = projection.run(target_run_id) else {
        return mode;
    };
    // Compaction and sign-out cannot accept steers; queue follow-ups.
    if projection
        .message(&target.user_message_id)
        .is_some_and(|message| is_native_maintenance_command(&message.text, &message.attachments))
    {
        return DispatchMode::QueueAfterActive;
    }
    // A provider that can neither steer nor restart its turn takes the message
    // after the turn.
    let turns = target
        .status
        .is_active()
        .then(|| projection.provider_turn_capabilities(target))
        .flatten();
    match turns {
        Some(turns) if !turns.supports_active_steering && provider_switch_waits_for_turn(turns) => {
            DispatchMode::QueueAfterActive
        }
        _ => mode,
    }
}

fn resolve_delivery_intent(
    projection: &Projection,
    requested: DispatchMode,
    delivery_intent: Option<DeliveryIntent>,
) -> DispatchMode {
    let Some(delivery_intent) = delivery_intent else {
        return requested;
    };
    let Some(active_run) = projection.active_run() else {
        return DispatchMode::StartImmediately;
    };
    match delivery_intent {
        DeliveryIntent::Steer => DispatchMode::SteerActive {
            target_run_id: active_run.id.clone(),
        },
        DeliveryIntent::Restart => DispatchMode::RestartActive {
            target_run_id: active_run.id.clone(),
        },
        DeliveryIntent::Auto => {
            // A run can be active before its provider turn starts or after it
            // finishes. Queue automatic input in those gaps so steering cannot
            // reject the message.
            if active_run.status == RunStatus::Preparing
                || active_run.status == RunStatus::Starting
                || projection.running_provider_turn(active_run).is_none()
            {
                return DispatchMode::QueueAfterActive;
            }
            match projection.provider_turn_capabilities(active_run) {
                Some(capabilities) if capabilities.supports_active_steering => {
                    DispatchMode::SteerActive {
                        target_run_id: active_run.id.clone(),
                    }
                }
                Some(capabilities) if capabilities.supports_queued_messages => {
                    DispatchMode::QueueAfterActive
                }
                Some(capabilities) if capabilities.supports_steering_by_interrupt_restart => {
                    DispatchMode::RestartActive {
                        target_run_id: active_run.id.clone(),
                    }
                }
                _ => DispatchMode::QueueAfterActive,
            }
        }
    }
}

/// A provider that cannot restart its turn for new input runs a switch after the turn.
pub fn provider_switch_waits_for_turn(turns: TurnCapabilities) -> bool {
    turns.supports_queued_messages
        && !(turns.supports_interrupt && turns.supports_steering_by_interrupt_restart)
}

/// `/compact` and `/logout` never steer or restart a turn.
pub fn is_native_maintenance_command(text: &str, attachments: &[Attachment]) -> bool {
    attachments.is_empty()
        && matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "/compact" | "/logout"
        )
}

fn dispatch_message(
    projection: &mut Projection,
    command: &MessageDispatch,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if projection.thread.deleted_at.is_some() {
        return Err(OrchestratorError::ThreadDeleted(
            projection.thread.id.clone(),
        ));
    }
    refuse_above_dispatch_mode_limit(projection, command)?;

    // A steer follow-up that lost its source records an accepted no-op so the
    // sender's retry does not repeat.
    if let Some(source_run_id) = &command.steer_follow_up_of_run_id {
        let valid = projection.run(source_run_id).is_some()
            && command.dispatch_mode == DispatchMode::QueueAfterActive
            && projection.thread.archived_at.is_none()
            && !projection.stop_reached(source_run_id);
        if !valid {
            emit_thread_metadata(projection, decision, emitter);
            return Ok(());
        }
    }

    let mut model_selection = command
        .model_selection
        .clone()
        .unwrap_or_else(|| projection.thread.model_selection.clone());
    let follows_thread_selection =
        command.scheduled_task_id.is_some() && command.model_selection.is_none();

    maybe_seed_title(projection, command, now, emitter, decision);

    let mut dispatch_mode = resolve_dispatch_intent(
        projection,
        command.dispatch_mode.clone(),
        command.delivery_intent,
    );
    dispatch_mode = normalize_live_dispatch_mode(projection, command, dispatch_mode);

    if matches!(
        dispatch_mode,
        DispatchMode::SteerActive { .. } | DispatchMode::RestartActive { .. }
    ) {
        dispatch_steer_into_run(
            projection,
            command,
            &model_selection,
            &dispatch_mode,
            follows_thread_selection,
            now,
            emitter,
            decision,
        )?;
        return Ok(());
    }

    let active_run = projection.active_run().cloned();
    let steer_follow_up_source = command
        .steer_follow_up_of_run_id
        .as_deref()
        .and_then(|run_id| projection.run(run_id))
        .cloned();
    let queue_behind = active_run.or(steer_follow_up_source);
    let starts_or_queues = matches!(
        dispatch_mode,
        DispatchMode::DeferStart { .. }
            | DispatchMode::StartImmediately
            | DispatchMode::QueueAfterActive
    );
    let should_queue = queue_behind.is_some() && starts_or_queues;

    if should_queue {
        // An explicit pick queued ahead reaches the thread only when its run
        // starts; otherwise a queued send takes the queue tail's selection.
        if command.model_selection.is_none()
            && command.steer_follow_up_of_run_id.is_none()
            && !command_follows_current_conversation(command)
        {
            model_selection = queue_tail_model_selection(projection);
        }
        let queue_behind = queue_behind.expect("checked above");
        plan_queued_run(
            projection,
            command,
            &model_selection,
            follows_thread_selection,
            &queue_behind,
            now,
            emitter,
            decision,
        )?;
        return Ok(());
    }

    plan_new_run(
        projection,
        command,
        &model_selection,
        follows_thread_selection,
        dispatch_mode,
        now,
        emitter,
        decision,
    )
}

fn refuse_above_dispatch_mode_limit(
    projection: &Projection,
    command: &MessageDispatch,
) -> Result<(), OrchestratorError> {
    let Some(limit) = &command.mode_limit else {
        return Ok(());
    };
    if projection
        .thread
        .runtime_mode
        .is_above(Some(limit.runtime_mode))
        || projection.thread.interaction_mode != limit.interaction_mode
    {
        return Err(OrchestratorError::ModeLimitExceeded {
            command_id: command.command_id.clone(),
            thread_id: command.thread_id.clone(),
            runtime_mode: limit.runtime_mode,
            interaction_mode: limit.interaction_mode,
        });
    }
    Ok(())
}

fn normalize_live_dispatch_mode(
    projection: &Projection,
    command: &MessageDispatch,
    mut mode: DispatchMode,
) -> DispatchMode {
    if let DispatchMode::SteerActive { target_run_id }
    | DispatchMode::RestartActive { target_run_id } = &mode
    {
        let target = projection.run(target_run_id);
        let too_late = target.is_none_or(|target| {
            target.status == RunStatus::Completed
                || (matches!(mode, DispatchMode::RestartActive { .. })
                    && !target.status.is_active())
                || ((target.status == RunStatus::Running || target.status == RunStatus::Waiting)
                    && projection.running_provider_turn(target).is_none())
        });
        if too_late {
            mode = DispatchMode::StartImmediately;
        }
    }
    // Stop leaves the run reading as running until its provider stops. A plain
    // send in that window queues behind the stopped queue instead of steering
    // text the agent never receives.
    if command.delivery_intent == Some(DeliveryIntent::Auto)
        && let DispatchMode::SteerActive { target_run_id }
        | DispatchMode::RestartActive { target_run_id } = &mode
        && projection.stop_reached(target_run_id)
    {
        mode = DispatchMode::QueueAfterActive;
    }
    if command.fresh_context
        && matches!(
            mode,
            DispatchMode::SteerActive { .. } | DispatchMode::RestartActive { .. }
        )
    {
        mode = DispatchMode::QueueAfterActive;
    }
    mode
}

fn maybe_seed_title(
    projection: &mut Projection,
    command: &MessageDispatch,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) {
    let is_maintenance = is_native_maintenance_command(&command.text, &command.attachments);
    let has_user_message = projection
        .messages
        .iter()
        .any(|message| message.role == MessageRole::User);
    let seeds_untitled_thread = command.title_seed.is_some() && !has_user_message;
    let only_maintenance_history = !has_user_message
        || projection
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .all(|message| is_native_maintenance_command(&message.text, &message.attachments));
    if is_maintenance
        || !(seeds_untitled_thread
            || (command.created_by == Actor::User && only_maintenance_history))
    {
        return;
    }
    let mut thread = projection.thread.clone();
    thread.title = command
        .title_seed
        .clone()
        .unwrap_or_else(|| thread.title.clone());
    thread.title_regeneration = Some(TitleRegeneration {
        request_id: command.command_id.clone(),
        started_at: now,
    });
    thread.updated_at = now;
    let event = emitter.event(EventKind::ThreadMetadataUpdated(thread));
    push_event(projection, decision, event);
    decision.add_effect(
        &command.command_id,
        &command.thread_id,
        "thread-title.generate",
        EffectRequest::ThreadTitleGenerate {
            message_id: command.message_id.clone(),
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn plan_queued_run(
    projection: &mut Projection,
    command: &MessageDispatch,
    model_selection: &ModelSelection,
    follows_thread_selection: bool,
    queue_behind: &Run,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let queue_provider_thread = projection
        .active_provider_thread()
        .cloned()
        .or_else(|| {
            queue_behind
                .provider_thread_id
                .as_deref()
                .and_then(|id| projection.provider_thread(id))
                .cloned()
        })
        .ok_or_else(|| OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: "message.dispatch".into(),
            cause: format!(
                "Active run {} has no provider thread for queued dispatch.",
                queue_behind.id
            ),
        })?;

    let ordinal = projection.next_run_ordinal();
    let run_id = derive_run_id(&command.thread_id, ordinal);
    let provider_thread =
        if queue_provider_thread.provider_instance_id == model_selection.instance_id {
            queue_provider_thread
        } else {
            new_provider_thread(&run_id, model_selection, ordinal, now, &command.thread_id)
        };
    let reserved_provider_thread = !projection
        .provider_threads
        .iter()
        .any(|row| row.id == provider_thread.id);

    let attempt_id = format!("run-attempt:{run_id}:1");
    let root_node_id = format!("root-node:{run_id}");

    let queue_is_held = projection
        .runs
        .iter()
        .any(|run| run.status == RunStatus::Queued && run.queue_held);
    let sent_by_user_from_client = command.steer_follow_up_of_run_id.is_none()
        && command.created_by == Actor::User
        && matches!(
            command.creation_source,
            CreationSource::Web | CreationSource::Mobile
        )
        && command.scheduled_task_id.is_none();
    let held_runs_to_release: Vec<Run> =
        if sent_by_user_from_client && command.steer_follow_up_of_run_id.is_none() {
            projection
                .runs
                .iter()
                .filter(|run| run.status == RunStatus::Queued && run.queue_held)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
    for held in &held_runs_to_release {
        let mut run = held.clone();
        run.queue_held = false;
        let event = emitter
            .event(EventKind::RunUpdated(run))
            .with_run(&held.id)
            .with_provider_instance(&held.provider_instance_id);
        push_event(projection, decision, event);
    }

    let mut queue_position = projection
        .runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued)
        .map(|run| run.queue_position.unwrap_or(run.ordinal))
        .max()
        .unwrap_or(0)
        + 1;
    if let Some(after_ordinal) = command.steer_follow_up_after_run_ordinal {
        queue_position = projection
            .runs
            .iter()
            .filter(|run| run.status == RunStatus::Queued && run.ordinal <= after_ordinal)
            .map(|run| run.queue_position.unwrap_or(run.ordinal))
            .max()
            .unwrap_or(0)
            + 1;
    }

    let queue_held = queue_is_held
        && held_runs_to_release.is_empty()
        && command.steer_follow_up_of_run_id.is_none();
    let run = Run {
        id: run_id.clone(),
        thread_id: command.thread_id.clone(),
        ordinal,
        provider_instance_id: model_selection.instance_id.clone(),
        model_selection: model_selection.clone(),
        provider_thread_id: Some(provider_thread.id.clone()),
        user_message_id: command.message_id.clone(),
        root_node_id: Some(root_node_id.clone()),
        active_attempt_id: Some(attempt_id.clone()),
        status: RunStatus::Queued,
        queue_position: Some(queue_position),
        queue_held,
        steer_when_ready_run_id: None,
        requested_at: now,
        started_at: None,
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: command.fresh_context,
    };
    let attempt = RunAttempt {
        id: attempt_id,
        run_id: run_id.clone(),
        attempt_ordinal: 1,
        root_node_id: root_node_id.clone(),
        provider_instance_id: model_selection.instance_id.clone(),
        provider_thread_id: provider_thread.id.clone(),
        provider_turn_id: None,
        reason: RunAttemptReason::Initial,
        status: RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    };
    let node = ExecutionNode {
        id: root_node_id.clone(),
        thread_id: command.thread_id.clone(),
        run_id: Some(run_id.clone()),
        parent_node_id: None,
        root_node_id: root_node_id.clone(),
        kind: NodeKind::RootTurn,
        status: NodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: None,
        runtime_request_id: None,
        started_at: None,
        completed_at: None,
    };
    let message = build_user_message(
        command,
        &run_id,
        &root_node_id,
        follows_thread_selection,
        now,
    );
    let turn_item = build_user_turn_item(
        &message,
        &run_id,
        &root_node_id,
        &provider_thread.id,
        None,
        InputIntent::QueuedTurn,
        projection.next_turn_item_ordinal(),
        now,
    );

    if reserved_provider_thread {
        let event = emitter
            .event(EventKind::ProviderThreadUpdated(provider_thread.clone()))
            .with_provider_instance(&provider_thread.provider_instance_id);
        push_event(projection, decision, event);
    }
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunCreated(run))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunAttemptCreated(attempt))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::NodeUpdated(node))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::MessageUpdated(message))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::TurnItemUpdated(turn_item))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn plan_new_run(
    projection: &mut Projection,
    command: &MessageDispatch,
    model_selection: &ModelSelection,
    follows_thread_selection: bool,
    dispatch_mode: DispatchMode,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if !model_selections_equal(&projection.thread.model_selection, model_selection)
        || projection.thread.provider_instance_id != model_selection.instance_id
    {
        let mut thread = projection.thread.clone();
        let switched = thread.provider_instance_id != model_selection.instance_id;
        thread.provider_instance_id = model_selection.instance_id.clone();
        thread.model_selection = model_selection.clone();
        thread.updated_at = now;
        let event = emitter.event(if switched {
            EventKind::ThreadProviderSwitched(thread)
        } else {
            EventKind::ThreadModelSelectionUpdated(thread)
        });
        push_event(projection, decision, event);
    }

    let ordinal = projection.next_run_ordinal();
    let run_id = derive_run_id(&command.thread_id, ordinal);
    let existing_provider_thread = projection
        .active_provider_thread()
        .filter(|thread| thread.provider_instance_id == model_selection.instance_id)
        .cloned();
    let provider_thread = existing_provider_thread.unwrap_or_else(|| {
        new_provider_thread(&run_id, model_selection, ordinal, now, &command.thread_id)
    });
    let attempt_id = format!("run-attempt:{run_id}:1");
    let root_node_id = format!("root-node:{run_id}");
    let defer_start = matches!(dispatch_mode, DispatchMode::DeferStart { .. });
    let run = Run {
        id: run_id.clone(),
        thread_id: command.thread_id.clone(),
        ordinal,
        provider_instance_id: model_selection.instance_id.clone(),
        model_selection: model_selection.clone(),
        provider_thread_id: Some(provider_thread.id.clone()),
        user_message_id: command.message_id.clone(),
        root_node_id: Some(root_node_id.clone()),
        active_attempt_id: Some(attempt_id.clone()),
        status: if defer_start {
            RunStatus::Preparing
        } else {
            RunStatus::Starting
        },
        queue_position: None,
        queue_held: false,
        steer_when_ready_run_id: None,
        requested_at: now,
        started_at: None,
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: command.fresh_context,
    };
    let attempt = RunAttempt {
        id: attempt_id,
        run_id: run_id.clone(),
        attempt_ordinal: 1,
        root_node_id: root_node_id.clone(),
        provider_instance_id: model_selection.instance_id.clone(),
        provider_thread_id: provider_thread.id.clone(),
        provider_turn_id: None,
        reason: RunAttemptReason::Initial,
        status: RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    };
    let node = ExecutionNode {
        id: root_node_id.clone(),
        thread_id: command.thread_id.clone(),
        run_id: Some(run_id.clone()),
        parent_node_id: None,
        root_node_id: root_node_id.clone(),
        kind: NodeKind::RootTurn,
        status: NodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: None,
        runtime_request_id: None,
        started_at: None,
        completed_at: None,
    };
    let message = build_user_message(
        command,
        &run_id,
        &root_node_id,
        follows_thread_selection,
        now,
    );
    let turn_item = build_user_turn_item(
        &message,
        &run_id,
        &root_node_id,
        &provider_thread.id,
        None,
        InputIntent::TurnStart,
        projection.next_turn_item_ordinal(),
        now,
    );

    let event = emitter
        .event(EventKind::ProviderThreadUpdated(provider_thread))
        .with_provider_instance(&model_selection.instance_id);
    push_event(projection, decision, event);
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunCreated(run))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunAttemptCreated(attempt))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::NodeUpdated(node))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::MessageUpdated(message))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::TurnItemUpdated(turn_item))
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    if !defer_start {
        decision.add_effect(
            &command.command_id,
            &command.thread_id,
            &format!("provider-turn.start:{run_id}"),
            EffectRequest::ProviderTurnStart { run_id },
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn dispatch_steer_into_run(
    projection: &mut Projection,
    command: &MessageDispatch,
    model_selection: &ModelSelection,
    dispatch_mode: &DispatchMode,
    follows_thread_selection: bool,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let (target_run_id, restart) = match dispatch_mode {
        DispatchMode::SteerActive { target_run_id } => (target_run_id, false),
        DispatchMode::RestartActive { target_run_id } => (target_run_id, true),
        _ => unreachable!("only called for steer/restart"),
    };
    let target =
        projection
            .run(target_run_id)
            .cloned()
            .ok_or_else(|| OrchestratorError::Dispatch {
                command_id: command.command_id.clone(),
                command_type: command.message_type(),
                cause: format!("Target run {target_run_id} was not found."),
            })?;
    if target.status != RunStatus::Running {
        return Err(OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: format!(
                "Target run {} is {:?} and cannot be steered.",
                target.id, target.status
            ),
        });
    }
    if projection.stop_reached(&target.id) {
        return Err(OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: format!(
                "Target run {} is stopping and cannot be steered.",
                target.id
            ),
        });
    }
    if is_native_maintenance_command(&command.text, &command.attachments) {
        return Err(OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: if command.text.trim().eq_ignore_ascii_case("/compact") {
                "Context compaction must run as a separate turn. Queue it or wait for the active turn to finish.".into()
            } else {
                "Signing out must run as a separate turn. Queue it or wait for the active turn to finish.".into()
            },
        });
    }
    if projection
        .message(&target.user_message_id)
        .is_some_and(|message| is_native_maintenance_command(&message.text, &message.attachments))
    {
        return Err(OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: "Wait for context compaction or sign-out to finish before steering.".into(),
        });
    }
    let root_node_id = target
        .root_node_id
        .clone()
        .ok_or_else(|| OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: format!("Target run {} has no root node.", target.id),
        })?;
    let provider_thread_id =
        target
            .provider_thread_id
            .clone()
            .ok_or_else(|| OrchestratorError::Dispatch {
                command_id: command.command_id.clone(),
                command_type: command.message_type(),
                cause: format!("Target run {} has no provider thread.", target.id),
            })?;
    let provider_thread = projection
        .provider_thread(&provider_thread_id)
        .cloned()
        .ok_or_else(|| OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: format!("Provider thread {provider_thread_id} was not found."),
        })?;
    let provider_session_id = provider_thread.provider_session_id.clone().ok_or_else(|| {
        OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: format!(
                "Provider thread {provider_thread_id} has no active provider session for steering."
            ),
        }
    })?;
    let provider_turn = projection
        .running_provider_turn(&target)
        .cloned()
        .ok_or_else(|| OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: format!(
                "No running provider turn found for active run {}.",
                target.id
            ),
        })?;
    if projection.provider_session(&provider_session_id).is_none() {
        return Err(OrchestratorError::Dispatch {
            command_id: command.command_id.clone(),
            command_type: command.message_type(),
            cause: format!("Provider session {provider_session_id} is not active."),
        });
    }

    // Only a user's steer carries the picker's selection; the adapter applies
    // it to the live turn without restarting it.
    if command.notification.is_none()
        && command.delegated_completion.is_none()
        && (!model_selections_equal(&projection.thread.model_selection, model_selection)
            || projection.thread.provider_instance_id != model_selection.instance_id)
    {
        let mut thread = projection.thread.clone();
        let switched = thread.provider_instance_id != model_selection.instance_id;
        thread.provider_instance_id = model_selection.instance_id.clone();
        thread.model_selection = model_selection.clone();
        thread.updated_at = now;
        let event = emitter.event(if switched {
            EventKind::ThreadProviderSwitched(thread)
        } else {
            EventKind::ThreadModelSelectionUpdated(thread)
        });
        push_event(projection, decision, event);
    }

    let message = build_user_message(
        command,
        &target.id,
        &root_node_id,
        follows_thread_selection,
        now,
    );
    let turn_item = build_user_turn_item(
        &message,
        &target.id,
        &root_node_id,
        &provider_thread.id,
        Some(&provider_turn.id),
        InputIntent::Steer,
        projection.next_turn_item_ordinal(),
        now,
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::MessageUpdated(message))
            .with_run(&target.id)
            .with_node(&root_node_id)
            .with_provider_instance(&target.provider_instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::TurnItemUpdated(turn_item))
            .with_run(&target.id)
            .with_node(&root_node_id)
            .with_provider_instance(&target.provider_instance_id),
    );

    if restart {
        restart_target_attempt(
            projection,
            &target,
            &root_node_id,
            &provider_thread.id,
            now,
            emitter,
            decision,
        );
        decision.add_effect(
            &command.command_id,
            &command.thread_id,
            &format!("provider-turn.restart:{}", provider_turn.id),
            EffectRequest::ProviderTurnRestart {
                provider_session_id,
                provider_thread_id: provider_thread.id,
                provider_turn_id: provider_turn.id,
                interrupted_attempt_id: target.active_attempt_id.clone().unwrap_or_default(),
                run_id: target.id.clone(),
            },
        );
    } else {
        decision.add_effect(
            &command.command_id,
            &command.thread_id,
            &format!("provider-turn.steer:{}", provider_turn.id),
            EffectRequest::ProviderTurnSteer {
                provider_session_id,
                provider_thread_id: provider_thread.id,
                provider_turn_id: provider_turn.id,
                message_id: command.message_id.clone(),
                model_selection: command.model_selection.clone(),
            },
        );
    }
    Ok(())
}

fn restart_target_attempt(
    projection: &mut Projection,
    target: &Run,
    root_node_id: &str,
    provider_thread_id: &str,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) {
    let previous_attempt_ordinal = projection
        .attempts
        .iter()
        .filter(|attempt| attempt.run_id == target.id)
        .count() as u32;
    if let Some(mut old_attempt) = target
        .active_attempt_id
        .as_deref()
        .and_then(|id| projection.attempt(id))
        .cloned()
    {
        old_attempt.status = RunAttemptStatus::Superseded;
        old_attempt.completed_at = Some(now);
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::RunAttemptUpdated(old_attempt))
                .with_run(&target.id)
                .with_node(root_node_id)
                .with_provider_instance(&target.provider_instance_id),
        );
    }
    let attempt_id = format!("run-attempt:{}:{}", target.id, previous_attempt_ordinal + 1);
    let attempt = RunAttempt {
        id: attempt_id.clone(),
        run_id: target.id.clone(),
        attempt_ordinal: previous_attempt_ordinal + 1,
        root_node_id: root_node_id.to_owned(),
        provider_instance_id: target.provider_instance_id.clone(),
        provider_thread_id: provider_thread_id.to_owned(),
        provider_turn_id: None,
        reason: RunAttemptReason::SteeringRestart,
        status: RunAttemptStatus::Running,
        started_at: Some(now),
        completed_at: None,
    };
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunAttemptCreated(attempt))
            .with_run(&target.id)
            .with_node(root_node_id)
            .with_provider_instance(&target.provider_instance_id),
    );
    if let Some(mut node) = projection.node(root_node_id).cloned() {
        node.status = NodeStatus::Running;
        node.started_at = Some(now);
        node.completed_at = None;
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::NodeUpdated(node))
                .with_run(&target.id)
                .with_node(root_node_id)
                .with_provider_instance(&target.provider_instance_id),
        );
    }
    let mut run = target.clone();
    run.active_attempt_id = Some(attempt_id);
    run.status = RunStatus::Running;
    run.started_at = run.started_at.or(Some(now));
    run.completed_at = None;
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunUpdated(run))
            .with_run(&target.id)
            .with_node(root_node_id)
            .with_provider_instance(&target.provider_instance_id),
    );
}

// ---------------------------------------------------------------------------
// Stop / interrupt
// ---------------------------------------------------------------------------

fn dispatch_thread_stop(
    projection: &mut Projection,
    command: &ThreadStop,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if projection.thread.deleted_at.is_some() {
        return Ok(());
    }
    let target = projection
        .runs
        .iter()
        .rev()
        .find(|run| {
            matches!(
                run.status,
                RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
            )
        })
        .cloned();

    if let Some(target) = target {
        let interrupt = RunInterrupt {
            command_id: command.command_id.clone(),
            thread_id: command.thread_id.clone(),
            run_id: target.id.clone(),
            reason: command.reason.clone(),
            hold_queue: true,
        };
        if plan_run_interrupt(projection, &interrupt, now, emitter, decision).is_err() {
            mark_stopped_run(
                projection,
                &target,
                command.reason.as_deref(),
                now,
                emitter,
                decision,
            );
        }
    } else if let Some(executed) = projection.latest_executed_run().cloned() {
        mark_stopped_run(
            projection,
            &executed,
            command.reason.as_deref(),
            now,
            emitter,
            decision,
        );
    }

    // Stop holds the queue until the user sends again or resumes it.
    let queued: Vec<Run> = projection
        .runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued && !run.queue_held)
        .cloned()
        .collect();
    for mut run in queued {
        run.queue_held = true;
        let run_id = run.id.clone();
        let provider_instance_id = run.provider_instance_id.clone();
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::RunUpdated(run))
                .with_run(&run_id)
                .with_provider_instance(&provider_instance_id),
        );
    }
    // Queued after the interrupt, since a thread's effects run in order.
    if command.stop_delegated_tasks && !projection.subagent_ids.is_empty() {
        decision.add_effect(
            &command.command_id,
            &command.thread_id,
            "delegated-tasks.stop",
            EffectRequest::DelegatedTasksStop {
                reason: command.reason.clone(),
            },
        );
    }
    Ok(())
}

fn mark_stopped_run(
    projection: &mut Projection,
    run: &Run,
    reason: Option<&str>,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) {
    if projection.stop_reached(&run.id) || run.root_node_id.is_none() {
        return;
    }
    let item = interrupt_request_item(run, reason, now, projection.next_turn_item_ordinal());
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::TurnItemUpdated(item))
            .with_run(&run.id)
            .with_node(run.root_node_id.clone().unwrap_or_default())
            .with_provider_instance(&run.provider_instance_id),
    );
}

fn plan_run_interrupt(
    projection: &mut Projection,
    command: &RunInterrupt,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let run = projection
        .run(&command.run_id)
        .cloned()
        .ok_or_else(|| OrchestratorError::RunNotInterruptible(command.run_id.clone()))?;
    let root_node_id = run
        .root_node_id
        .clone()
        .ok_or_else(|| OrchestratorError::RunNotInterruptible(run.id.clone()))?;
    let provider_thread = run
        .provider_thread_id
        .as_deref()
        .and_then(|id| projection.provider_thread(id))
        .cloned();
    let provider_turn = projection.running_provider_turn(&run).cloned();

    let ordinal = projection.next_turn_item_ordinal();
    let request_item = interrupt_request_item(&run, command.reason.as_deref(), now, ordinal);
    let request_item_id = request_item.id.clone();
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::TurnItemUpdated(request_item))
            .with_run(&run.id)
            .with_node(&root_node_id)
            .with_provider_instance(&run.provider_instance_id),
    );

    let interrupt_target = provider_thread.as_ref().and_then(|thread| {
        let provider_session_id = thread.provider_session_id.clone()?;
        projection
            .provider_session(&provider_session_id)
            .is_some()
            .then_some((thread.id.clone(), provider_session_id))
    });
    if let (Some(provider_turn), Some((provider_thread_id, provider_session_id))) =
        (provider_turn.as_ref(), interrupt_target)
    {
        decision.add_effect(
            &command.command_id,
            &command.thread_id,
            &format!("provider-turn.interrupt:{}", provider_turn.id),
            EffectRequest::ProviderTurnInterrupt {
                provider_session_id,
                provider_thread_id,
                provider_turn_id: provider_turn.id.clone(),
            },
        );
        return Ok(());
    }

    if matches!(
        run.status,
        RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
    ) {
        let result_item = TurnItem {
            id: format!("turn-item:{}:interrupt-result", run.id),
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node_id.clone()),
            provider_thread_id: run.provider_thread_id.clone(),
            provider_turn_id: None,
            parent_item_id: Some(request_item_id),
            ordinal: ordinal + 1,
            status: TurnItemStatus::Interrupted,
            title: Some("Interrupted".into()),
            created_by: Actor::System,
            creation_source: CreationSource::Server,
            started_at: Some(now),
            completed_at: Some(now),
            updated_at: now,
            kind: TurnItemKind::RunInterruptResult {
                message: "Run interrupted before provider start".into(),
            },
        };
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::TurnItemUpdated(result_item))
                .with_run(&run.id)
                .with_node(&root_node_id)
                .with_provider_instance(&run.provider_instance_id),
        );
        mark_run_terminal(
            projection,
            &run,
            RunStatus::Interrupted,
            now,
            emitter,
            decision,
        );
        decision.cancel_unsettled = Some(CancelUnsettled {
            effect_types: vec![
                "provider-turn.start".to_owned(),
                "provider-turn.restart".to_owned(),
            ],
            reason: format!(
                "Run {} was interrupted before its provider turn started.",
                run.id
            ),
        });
        return Ok(());
    }

    if provider_turn.is_some() {
        mark_run_terminal(
            projection,
            &run,
            RunStatus::Interrupted,
            now,
            emitter,
            decision,
        );
        return Ok(());
    }
    Err(OrchestratorError::RunNotInterruptible(run.id))
}

fn interrupt_request_item(
    run: &Run,
    reason: Option<&str>,
    now: Timestamp,
    ordinal: i64,
) -> TurnItem {
    TurnItem {
        id: format!("turn-item:{}:interrupt-request", run.id),
        thread_id: run.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: run.root_node_id.clone(),
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id: None,
        parent_item_id: None,
        ordinal,
        status: TurnItemStatus::Completed,
        title: Some("Interrupt requested".into()),
        created_by: Actor::System,
        creation_source: CreationSource::Server,
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        kind: TurnItemKind::RunInterruptRequest {
            message: reason.unwrap_or("Stopped by user").to_owned(),
        },
    }
}

fn mark_run_terminal(
    projection: &mut Projection,
    run: &Run,
    status: RunStatus,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) {
    if let Some(mut attempt) = run
        .active_attempt_id
        .as_deref()
        .and_then(|id| projection.attempt(id))
        .cloned()
    {
        attempt.status = attempt_status_for_run(status);
        attempt.completed_at = Some(now);
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::RunAttemptUpdated(attempt))
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    if let Some(mut node) = run
        .root_node_id
        .as_deref()
        .and_then(|id| projection.node(id))
        .cloned()
    {
        node.status = node_status_for_run(status);
        node.completed_at = Some(now);
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::NodeUpdated(node))
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    let mut run = run.clone();
    run.status = status;
    run.queue_position = None;
    run.completed_at = Some(now);
    let run_id = run.id.clone();
    let node_id = run.root_node_id.clone().unwrap_or_default();
    let provider_instance_id = run.provider_instance_id.clone();
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunUpdated(run))
            .with_run(&run_id)
            .with_node(&node_id)
            .with_provider_instance(&provider_instance_id),
    );
}

fn attempt_status_for_run(status: RunStatus) -> RunAttemptStatus {
    match status {
        RunStatus::Completed => RunAttemptStatus::Completed,
        RunStatus::Interrupted => RunAttemptStatus::Interrupted,
        RunStatus::Failed => RunAttemptStatus::Failed,
        RunStatus::Cancelled | RunStatus::RolledBack => RunAttemptStatus::Cancelled,
        RunStatus::Preparing
        | RunStatus::Queued
        | RunStatus::Starting
        | RunStatus::Running
        | RunStatus::Waiting => RunAttemptStatus::Running,
    }
}

fn node_status_for_run(status: RunStatus) -> NodeStatus {
    match status {
        RunStatus::Completed => NodeStatus::Completed,
        RunStatus::Interrupted => NodeStatus::Interrupted,
        RunStatus::Failed => NodeStatus::Failed,
        RunStatus::Cancelled | RunStatus::RolledBack => NodeStatus::Cancelled,
        RunStatus::Preparing
        | RunStatus::Queued
        | RunStatus::Starting
        | RunStatus::Running
        | RunStatus::Waiting => NodeStatus::Running,
    }
}

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequested {
    pub request_id: RuntimeRequestId,
    pub run_id: RunId,
    pub node_id: NodeId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub provider_instance_id: String,
    pub kind: RequestKind,
    pub prompt: Option<String>,
    pub event_id: String,
}

/// A provider asked for approval. The request and its card commit together.
pub fn plan_approval_requested(
    projection: &mut Projection,
    input: ApprovalRequested,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    let run = projection
        .run(&input.run_id)
        .cloned()
        .ok_or_else(|| OrchestratorError::ThreadNotFound(input.run_id.clone()))?;
    let mut emitter = Emitter::new(&input.event_id, &run.thread_id, now);
    let mut decision = Decision::new();
    if projection
        .runtime_request(&input.request_id)
        .is_some_and(|request| request.status == RequestStatus::Pending)
    {
        return Ok(decision);
    }
    let live_provider_session_id = run
        .provider_thread_id
        .as_deref()
        .and_then(|id| projection.provider_thread(id))
        .and_then(|thread| thread.provider_session_id.clone());
    let request = RuntimeRequest {
        id: input.request_id.clone(),
        thread_id: run.thread_id.clone(),
        node_id: input.node_id.clone(),
        provider_turn_id: input.provider_turn_id.clone(),
        kind: input.kind,
        status: RequestStatus::Pending,
        provider_instance_id: input.provider_instance_id.clone(),
        created_at: now,
        resolved_at: None,
        decision: None,
        feedback: None,
        live_provider_session_id,
    };
    push_event(
        projection,
        &mut decision,
        emitter
            .event(EventKind::RuntimeRequestUpdated(request))
            .with_run(&run.id)
            .with_node(&input.node_id)
            .with_provider_instance(&input.provider_instance_id),
    );
    let item = TurnItem {
        id: format!("turn-item:{}", input.request_id),
        thread_id: run.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: Some(input.node_id.clone()),
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id: input.provider_turn_id,
        parent_item_id: None,
        ordinal: projection.next_turn_item_ordinal(),
        status: TurnItemStatus::Running,
        title: input.prompt.clone(),
        created_by: Actor::Agent,
        creation_source: CreationSource::Server,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
        kind: TurnItemKind::ApprovalRequest {
            request_id: input.request_id,
            prompt: input.prompt,
        },
    };
    push_event(
        projection,
        &mut decision,
        emitter
            .event(EventKind::TurnItemUpdated(item))
            .with_run(&run.id)
            .with_node(&input.node_id)
            .with_provider_instance(&input.provider_instance_id),
    );
    Ok(decision)
}

fn dispatch_runtime_request_respond(
    projection: &mut Projection,
    command: &RuntimeRequestRespond,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let request = projection
        .runtime_request(&command.request_id)
        .cloned()
        .ok_or_else(|| OrchestratorError::RequestNotPending(command.request_id.clone()))?;
    if request.status != RequestStatus::Pending {
        return Err(OrchestratorError::RequestNotPending(
            command.request_id.clone(),
        ));
    }
    let mut resolved = request.clone();
    resolved.status = RequestStatus::Resolved;
    resolved.resolved_at = Some(now);
    resolved.decision = command.decision;
    resolved.feedback = command.feedback.clone();
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RuntimeRequestUpdated(resolved))
            .with_node(&request.node_id)
            .with_provider_instance(&request.provider_instance_id),
    );
    // The approval card resolves with its request.
    if let Some(mut item) = projection
        .turn_items
        .iter()
        .find(|item| {
            matches!(
                &item.kind,
                TurnItemKind::ApprovalRequest { request_id, .. }
                    if request_id == &command.request_id
            )
        })
        .cloned()
    {
        item.status = TurnItemStatus::Completed;
        item.completed_at = Some(now);
        item.updated_at = now;
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::TurnItemUpdated(item))
                .with_node(&request.node_id)
                .with_provider_instance(&request.provider_instance_id),
        );
    }
    if let Some(provider_session_id) = request.live_provider_session_id {
        decision.add_effect(
            &command.command_id,
            &command.thread_id,
            &format!("runtime-request.respond:{}", command.request_id),
            EffectRequest::RuntimeRequestRespond {
                provider_session_id,
                request_id: command.request_id.clone(),
                decision: command.decision,
            },
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Thread settings
// ---------------------------------------------------------------------------

fn dispatch_model_selection(
    projection: &mut Projection,
    command: &ThreadModelSelectionSet,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if projection.thread.deleted_at.is_some() {
        return Err(OrchestratorError::ThreadDeleted(
            projection.thread.id.clone(),
        ));
    }
    let switched = projection.thread.provider_instance_id != command.model_selection.instance_id;
    let mut thread = projection.thread.clone();
    thread.provider_instance_id = command.model_selection.instance_id.clone();
    thread.model_selection = command.model_selection.clone();
    thread.updated_at = now;
    push_event(
        projection,
        decision,
        emitter
            .event(if switched {
                EventKind::ThreadProviderSwitched(thread)
            } else {
                EventKind::ThreadModelSelectionUpdated(thread)
            })
            .with_provider_instance(&command.model_selection.instance_id),
    );
    // A live turn applies the selection in place when the provider can.
    if !switched
        && let Some(run) = projection
            .runs
            .iter()
            .rev()
            .find(|run| run.status == RunStatus::Running)
            .cloned()
        && let Some(provider_thread) = run
            .provider_thread_id
            .as_deref()
            .and_then(|id| projection.provider_thread(id))
            .cloned()
        && let Some(provider_session_id) = provider_thread.provider_session_id.clone()
        && projection.provider_session(&provider_session_id).is_some()
    {
        decision.add_effect(
            &command.command_id,
            &command.thread_id,
            "provider-session.apply-model-selection",
            EffectRequest::ProviderSessionApplyModelSelection {
                provider_session_id,
                provider_thread_id: provider_thread.id,
                model_selection: command.model_selection.clone(),
            },
        );
    }
    Ok(())
}

fn dispatch_runtime_mode(
    projection: &mut Projection,
    command: &ThreadRuntimeModeSet,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if projection.thread.deleted_at.is_some() {
        return Err(OrchestratorError::ThreadDeleted(
            projection.thread.id.clone(),
        ));
    }
    let mut thread = projection.thread.clone();
    thread.runtime_mode = command.runtime_mode;
    thread.updated_at = now;
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::ThreadRuntimeModeUpdated(thread.clone()))
            .with_provider_instance(&thread.provider_instance_id),
    );
    // Lowering the mode releases a held queue.
    if projection
        .runs
        .iter()
        .any(|run| run.status == RunStatus::Queued && run.queue_held)
        && projection.active_run().is_none()
    {
        plan_start_next_queued_run_with(projection, None, now, emitter, decision)?;
    }
    Ok(())
}

fn emit_thread_metadata(
    projection: &mut Projection,
    decision: &mut Decision,
    emitter: &mut Emitter,
) {
    let thread = projection.thread.clone();
    push_event(
        projection,
        decision,
        emitter.event(EventKind::ThreadMetadataUpdated(thread)),
    );
}

// ---------------------------------------------------------------------------
// Queue drain and run lifecycle
// ---------------------------------------------------------------------------

/// Start the next queued run (or `target`), as the run-end reaction does.
pub fn plan_start_next_queued_run(
    projection: &mut Projection,
    target: Option<&str>,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    let thread_id = projection.thread.id.clone();
    let mut emitter = Emitter::new(
        &format!("command:system:start-queued:{thread_id}"),
        &thread_id,
        now,
    );
    let mut decision = Decision::new();
    plan_start_next_queued_run_with(projection, target, now, &mut emitter, &mut decision)?;
    Ok(decision)
}

fn plan_start_next_queued_run_with(
    projection: &mut Projection,
    target: Option<&str>,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if projection.active_run().is_some() {
        return Ok(());
    }
    let queued = match target {
        Some(run_id) => projection
            .run(run_id)
            .filter(|run| run.status == RunStatus::Queued)
            .cloned(),
        None => next_queued_run(projection),
    };
    let Some(queued) = queued else {
        return Ok(());
    };
    if queued.queue_held {
        return Ok(());
    }

    let message = projection
        .message(&queued.user_message_id)
        .cloned()
        .ok_or_else(|| OrchestratorError::Dispatch {
            command_id: "command:system:start-queued".into(),
            command_type: "message.dispatch".into(),
            cause: format!("Queued run {} has no message.", queued.id),
        })?;
    let root_node_id = queued
        .root_node_id
        .clone()
        .ok_or_else(|| OrchestratorError::Dispatch {
            command_id: "command:system:start-queued".into(),
            command_type: "message.dispatch".into(),
            cause: format!("Queued run {} is missing execution identity.", queued.id),
        })?;
    let attempt_id =
        queued
            .active_attempt_id
            .clone()
            .ok_or_else(|| OrchestratorError::Dispatch {
                command_id: "command:system:start-queued".into(),
                command_type: "message.dispatch".into(),
                cause: format!("Queued run {} is missing execution identity.", queued.id),
            })?;
    let provider_thread = queued
        .provider_thread_id
        .as_deref()
        .and_then(|id| projection.provider_thread(id))
        .cloned()
        .ok_or_else(|| OrchestratorError::Dispatch {
            command_id: "command:system:start-queued".into(),
            command_type: "message.dispatch".into(),
            cause: format!("Queued run {} is missing projection state.", queued.id),
        })?;

    // Move a queued ordinal past runs that started while it waited.
    let started_ahead = projection
        .runs
        .iter()
        .any(|run| run.status != RunStatus::Queued && run.ordinal > queued.ordinal);
    let ordinal = if started_ahead {
        projection.next_run_ordinal()
    } else {
        queued.ordinal
    };

    if !model_selections_equal(&projection.thread.model_selection, &queued.model_selection) {
        let mut thread = projection.thread.clone();
        let switched = thread.provider_instance_id != queued.model_selection.instance_id;
        thread.provider_instance_id = queued.model_selection.instance_id.clone();
        thread.model_selection = queued.model_selection.clone();
        thread.updated_at = now;
        let event = emitter.event(if switched {
            EventKind::ThreadProviderSwitched(thread)
        } else {
            EventKind::ThreadModelSelectionUpdated(thread)
        });
        push_event(projection, decision, event);
    }

    let mut starting_run = queued.clone();
    starting_run.status = RunStatus::Starting;
    starting_run.ordinal = ordinal;
    starting_run.queue_position = None;
    starting_run.started_at = None;
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::ProviderThreadUpdated(provider_thread.clone()))
            .with_provider_instance(&provider_thread.provider_instance_id),
    );
    // The queued user item becomes the started turn.
    let mut turn_item = build_user_turn_item(
        &message,
        &queued.id,
        &root_node_id,
        &provider_thread.id,
        None,
        InputIntent::QueuedTurn,
        projection.next_turn_item_ordinal(),
        now,
    );
    if let Some(existing) = projection.turn_items.iter().find(|item| {
        matches!(&item.kind, TurnItemKind::UserMessage { message_id, .. } if message_id == &message.id)
    }) {
        turn_item.ordinal = existing.ordinal;
    }
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::TurnItemUpdated(turn_item))
            .with_run(&queued.id)
            .with_node(&root_node_id)
            .with_provider_instance(&queued.provider_instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(EventKind::RunUpdated(starting_run))
            .with_run(&queued.id)
            .with_node(&root_node_id)
            .with_provider_instance(&queued.provider_instance_id),
    );
    if let Some(mut attempt) = projection.attempt(&attempt_id).cloned() {
        attempt.provider_thread_id = provider_thread.id.clone();
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::RunAttemptUpdated(attempt))
                .with_run(&queued.id)
                .with_node(&root_node_id)
                .with_provider_instance(&queued.provider_instance_id),
        );
    }
    if let Some(mut node) = projection.node(&root_node_id).cloned() {
        node.provider_thread_id = Some(provider_thread.id.clone());
        push_event(
            projection,
            decision,
            emitter
                .event(EventKind::NodeUpdated(node))
                .with_run(&queued.id)
                .with_node(&root_node_id)
                .with_provider_instance(&queued.provider_instance_id),
        );
    }
    decision.add_effect(
        &format!("command:system:start-queued:{}", queued.id),
        &queued.thread_id,
        &format!("provider-turn.start:{}", queued.id),
        EffectRequest::ProviderTurnStart {
            run_id: queued.id.clone(),
        },
    );
    Ok(())
}

/// A provider turn started: the run, attempt and root node start running.
pub fn plan_provider_turn_started(
    projection: &mut Projection,
    run_id: &str,
    provider_turn: ProviderTurn,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    let run = projection
        .run(run_id)
        .cloned()
        .ok_or_else(|| OrchestratorError::RunNotInterruptible(run_id.to_owned()))?;
    let thread_id = run.thread_id.clone();
    let mut emitter = Emitter::new(
        &format!("command:system:provider-turn-started:{thread_id}"),
        &thread_id,
        now,
    );
    let mut decision = Decision::new();
    let provider_thread_id = provider_turn.provider_thread_id.clone();
    let provider_turn_id = provider_turn.id.clone();
    push_event(
        projection,
        &mut decision,
        emitter
            .event(EventKind::ProviderTurnUpdated(provider_turn))
            .with_run(&run.id)
            .with_provider_instance(&run.provider_instance_id),
    );
    if let Some(mut attempt) = run
        .active_attempt_id
        .as_deref()
        .and_then(|id| projection.attempt(id))
        .cloned()
    {
        attempt.status = RunAttemptStatus::Running;
        attempt.provider_turn_id = Some(provider_turn_id.clone());
        attempt.started_at = attempt.started_at.or(Some(now));
        push_event(
            projection,
            &mut decision,
            emitter
                .event(EventKind::RunAttemptUpdated(attempt))
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    if let Some(mut node) = run
        .root_node_id
        .as_deref()
        .and_then(|id| projection.node(id))
        .cloned()
    {
        node.status = NodeStatus::Running;
        node.provider_thread_id = Some(provider_thread_id);
        node.provider_turn_id = Some(provider_turn_id.clone());
        node.started_at = node.started_at.or(Some(now));
        push_event(
            projection,
            &mut decision,
            emitter
                .event(EventKind::NodeUpdated(node))
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    let mut running = run.clone();
    running.status = RunStatus::Running;
    running.started_at = running.started_at.or(Some(now));
    push_event(
        projection,
        &mut decision,
        emitter
            .event(EventKind::RunUpdated(running))
            .with_run(&run.id)
            .with_node(run.root_node_id.clone().unwrap_or_default())
            .with_provider_instance(&run.provider_instance_id),
    );
    Ok(decision)
}

/// A run ended: complete, fail, interrupt or cancel it, then drain the queue.
pub fn plan_run_finished(
    projection: &mut Projection,
    run_id: &str,
    status: RunStatus,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    if !status.is_terminal() {
        return Err(OrchestratorError::Dispatch {
            command_id: "command:system:run-finished".into(),
            command_type: "run.finish".into(),
            cause: format!("{status:?} is not a terminal run status."),
        });
    }
    let run = projection
        .run(run_id)
        .cloned()
        .ok_or_else(|| OrchestratorError::RunNotInterruptible(run_id.to_owned()))?;
    let thread_id = run.thread_id.clone();
    let mut emitter = Emitter::new(
        &format!("command:system:run-finished:{run_id}"),
        &thread_id,
        now,
    );
    let mut decision = Decision::new();
    mark_run_terminal(projection, &run, status, now, &mut emitter, &mut decision);
    plan_start_next_queued_run_with(projection, None, now, &mut emitter, &mut decision)?;
    Ok(decision)
}

// ---------------------------------------------------------------------------
// Dispatch through the store
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchResult {
    pub sequence: i64,
    pub stored_events: Vec<StoredEvent>,
    pub replayed: bool,
}

/// Decide and commit a command through the [`Store`] seam.
pub fn dispatch<S: Store>(
    store: &mut S,
    command: &Command,
    now: Timestamp,
) -> Result<DispatchResult, OrchestratorError> {
    let command_id = command.command_id().to_owned();
    if let Some(receipt) = store.command_receipt(&command_id) {
        return replay(receipt, command, store);
    }
    let projection = store.projection(command.thread_id());
    let decision = match decide(projection.as_ref(), command, now) {
        Ok(decision) => decision,
        Err(error) => {
            if error.is_recorded_rejection() {
                let _ = store.commit_rejected_command(RejectedCommandInput {
                    command_id: command_id.clone(),
                    thread_id: command.thread_id().clone(),
                    command_type: command.command_type().to_owned(),
                    rejected_at: now,
                    error: error.to_string(),
                });
            }
            return Err(error);
        }
    };
    if decision.is_empty() && !command.allows_empty_events() {
        let error = OrchestratorError::NoEvents(command_id.clone());
        let _ = store.commit_rejected_command(RejectedCommandInput {
            command_id,
            thread_id: command.thread_id().clone(),
            command_type: command.command_type().to_owned(),
            rejected_at: now,
            error: error.to_string(),
        });
        return Err(error);
    }
    let output = store
        .commit_command(CommitCommandInput {
            command_id: command_id.clone(),
            thread_id: command.thread_id().clone(),
            command_type: command.command_type().to_owned(),
            accepted_at: now,
            events: decision.events,
            effects: decision.effects,
            cancel_unsettled: decision.cancel_unsettled,
        })
        .map_err(|error| OrchestratorError::Dispatch {
            command_id: command_id.clone(),
            command_type: command.command_type().to_owned(),
            cause: error.to_string(),
        })?;
    if output.receipt.status == ReceiptStatus::Rejected {
        return Err(OrchestratorError::CommandPreviouslyRejected {
            command_id,
            command_type: command.command_type().to_owned(),
            detail: output.receipt.error.clone().unwrap_or_default(),
        });
    }
    if &output.receipt.thread_id != command.thread_id() {
        return Err(OrchestratorError::CommandIdConflict {
            command_id,
            command_type: command.command_type().to_owned(),
            receipt_thread_id: output.receipt.thread_id.clone(),
            command_thread_id: command.thread_id().clone(),
        });
    }
    Ok(DispatchResult {
        sequence: output.receipt.result_sequence,
        stored_events: output.stored_events,
        replayed: !output.committed,
    })
}

fn replay<S: Store>(
    receipt: CommandReceipt,
    command: &Command,
    store: &S,
) -> Result<DispatchResult, OrchestratorError> {
    if receipt.status == ReceiptStatus::Rejected {
        return Err(OrchestratorError::CommandPreviouslyRejected {
            command_id: receipt.command_id,
            command_type: command.command_type().to_owned(),
            detail: receipt
                .error
                .unwrap_or_else(|| "Previously rejected.".into()),
        });
    }
    if &receipt.thread_id != command.thread_id() {
        return Err(OrchestratorError::CommandIdConflict {
            command_id: receipt.command_id,
            command_type: command.command_type().to_owned(),
            receipt_thread_id: receipt.thread_id,
            command_thread_id: command.thread_id().clone(),
        });
    }
    Ok(DispatchResult {
        sequence: receipt.result_sequence,
        stored_events: store.read_by_command_id(command.command_id()),
        replayed: true,
    })
}

impl MessageDispatch {
    fn message_type(&self) -> String {
        "message.dispatch".to_owned()
    }
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

fn derive_run_id(thread_id: &str, ordinal: u32) -> RunId {
    format!("run:{thread_id}:{ordinal}")
}

fn new_provider_thread(
    run_id: &str,
    model_selection: &ModelSelection,
    ordinal: u32,
    now: Timestamp,
    thread_id: &str,
) -> ProviderThread {
    ProviderThread {
        id: format!("provider-thread:{run_id}"),
        driver: model_selection.instance_id.clone(),
        provider_instance_id: model_selection.instance_id.clone(),
        provider_session_id: None,
        app_thread_id: thread_id.to_owned(),
        native_thread_ref: None,
        status: ProviderThreadStatus::NotLoaded,
        first_run_ordinal: Some(ordinal),
        last_run_ordinal: Some(ordinal),
        created_at: now,
        updated_at: now,
    }
}

fn build_user_message(
    command: &MessageDispatch,
    run_id: &str,
    node_id: &str,
    follows_thread_selection: bool,
    now: Timestamp,
) -> Message {
    Message {
        id: command.message_id.clone(),
        thread_id: command.thread_id.clone(),
        run_id: Some(run_id.to_owned()),
        node_id: Some(node_id.to_owned()),
        role: MessageRole::User,
        text: command.text.clone(),
        attachments: command.attachments.clone(),
        streaming: false,
        created_by: command.created_by,
        creation_source: command.creation_source,
        scheduled_task_id: command.scheduled_task_id.clone(),
        sender_thread_id: command.sender_thread_id.clone(),
        follows_thread_selection,
        created_at: now,
        updated_at: now,
        delegated_completion: command.delegated_completion.clone(),
        notification: command.notification.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_user_turn_item(
    message: &Message,
    run_id: &str,
    node_id: &str,
    provider_thread_id: &str,
    provider_turn_id: Option<&str>,
    input_intent: InputIntent,
    ordinal: i64,
    now: Timestamp,
) -> TurnItem {
    TurnItem {
        id: format!("turn-item:{}", message.id),
        thread_id: message.thread_id.clone(),
        run_id: Some(run_id.to_owned()),
        node_id: Some(node_id.to_owned()),
        provider_thread_id: Some(provider_thread_id.to_owned()),
        provider_turn_id: provider_turn_id.map(str::to_owned),
        parent_item_id: None,
        ordinal,
        status: TurnItemStatus::Completed,
        title: None,
        created_by: message.created_by,
        creation_source: message.creation_source,
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        kind: TurnItemKind::UserMessage {
            message_id: message.id.clone(),
            text: message.text.clone(),
            input_intent,
        },
    }
}

/// The command-level version of `followsCurrentConversation`: the message does
/// not exist yet when the queue tail's selection is chosen.
fn command_follows_current_conversation(command: &MessageDispatch) -> bool {
    command.delegated_completion.is_some()
        || (command.scheduled_task_id.is_some() && command.model_selection.is_none())
        || command
            .notification
            .as_ref()
            .is_some_and(|notification| notification.source_kind == "monitor")
}
