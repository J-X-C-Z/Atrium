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

#[derive(Clone, Debug, Default, PartialEq)]
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
        command_id: impl ToString,
        thread_id: impl ToString,
        suffix: &str,
        request: EffectRequest,
    ) {
        let command_id = command_id.to_string();
        self.effects.push(PendingEffect {
            id: format!("effect:{command_id}:{suffix}"),
            command_id: CommandId::new(command_id),
            thread_id: ThreadId::new(thread_id.to_string()),
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
    fn new(command_id: impl ToString, thread_id: impl ToString, now: Timestamp) -> Self {
        Self {
            command_id: CommandId::new(command_id.to_string()),
            thread_id: ThreadId::new(thread_id.to_string()),
            now,
            next: 0,
        }
    }

    fn event<T>(
        &mut self,
        variant: impl FnOnce(EventPayload<T>) -> DomainEvent,
        payload: T,
    ) -> BuiltEvent {
        self.next += 1;
        BuiltEvent(variant(EventPayload {
            base: DomainEventBase {
                id: EventId::new(format!("event:{}:{}", self.command_id, self.next)),
                thread_id: self.thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: None,
                provider_instance_id: None,
                raw_event_id: None,
                occurred_at: iso(self.now),
            },
            payload,
        }))
    }
}

/// An event whose index fields can still be filled in before it is pushed.
struct BuiltEvent(DomainEvent);

impl BuiltEvent {
    fn with_run(mut self, run_id: impl ToString) -> Self {
        let run_id = RunId::new(run_id.to_string());
        if let Some(base) = event_base_mut(&mut self.0) {
            base.run_id = Some(run_id);
        }
        self
    }

    fn with_node(mut self, node_id: impl ToString) -> Self {
        let node_id = NodeId::new(node_id.to_string());
        if let Some(base) = event_base_mut(&mut self.0) {
            base.node_id = Some(node_id);
        }
        self
    }

    fn with_provider_instance(mut self, instance_id: impl ToString) -> Self {
        let instance_id = ProviderInstanceId::new(instance_id.to_string());
        if let Some(base) = event_base_mut(&mut self.0) {
            base.provider_instance_id = Some(instance_id);
        }
        self
    }
}

impl From<BuiltEvent> for DomainEvent {
    fn from(event: BuiltEvent) -> Self {
        event.0
    }
}

fn event_base_mut(event: &mut DomainEvent) -> Option<&mut DomainEventBase> {
    Some(match event {
        DomainEvent::ThreadCreated(event) => &mut event.base,
        DomainEvent::ThreadMetadata { event, .. } => &mut event.base,
        DomainEvent::ThreadVisitRecorded(event) => &mut event.base,
        DomainEvent::RunCreated(event) => &mut event.base,
        DomainEvent::RunUpdated(event) => &mut event.base,
        DomainEvent::RunBackgroundWorkCancelled(event) => &mut event.base,
        DomainEvent::RunAttemptCreated(event) => &mut event.base,
        DomainEvent::RunAttemptUpdated(event) => &mut event.base,
        DomainEvent::NodeUpdated(event) => &mut event.base,
        DomainEvent::ProviderSessionAttached(event) => &mut event.base,
        DomainEvent::ProviderSessionUpdated(event) => &mut event.base,
        DomainEvent::ProviderSessionDetached(event) => &mut event.base,
        DomainEvent::ProviderThreadUpdated(event) => &mut event.base,
        DomainEvent::ProviderTurnUpdated(event) => &mut event.base,
        DomainEvent::RuntimeRequestUpdated(event) => &mut event.base,
        DomainEvent::MessageUpdated(event) => &mut event.base,
        DomainEvent::TurnItemUpdated(event) => &mut event.base,
        DomainEvent::SubagentUpdated(event) => &mut event.base,
        DomainEvent::PlanUpdated(event) => &mut event.base,
        DomainEvent::Unknown(_) => return None,
    })
}

fn thread_event(
    kind: crate::contracts::ThreadMetadataEventType,
    payload: EventPayload<Thread>,
) -> DomainEvent {
    DomainEvent::ThreadMetadata {
        type_: kind,
        event: payload,
    }
}

fn push_event(projection: &mut Projection, decision: &mut Decision, event: impl Into<DomainEvent>) {
    let event: DomainEvent = event.into();
    projection.apply(&event);
    decision.events.push(event);
}

/// Decide one command against an optional projection snapshot.
pub fn decide(
    existing: Option<&Projection>,
    command: &Command,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    decide_with_limit(existing, command, None, now)
}

/// Decide a command with the server's launch-mode policy applied to intake.
pub fn decide_with_limit(
    existing: Option<&Projection>,
    command: &Command,
    mode_limit: Option<&DispatchModeLimit>,
    now: Timestamp,
) -> Result<Decision, OrchestratorError> {
    match command {
        Command::ThreadCreate(command) => decide_thread_create(existing, command, mode_limit, now),
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
                    dispatch_message(
                        &mut projection,
                        command,
                        mode_limit,
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
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
                Command::QueuedRunReorder {
                    run_id,
                    before_run_id,
                    ..
                } => {
                    dispatch_queued_run_reorder(
                        &mut projection,
                        command,
                        run_id,
                        before_run_id.as_ref(),
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::QueuedRunCancel { run_id, .. } => {
                    dispatch_queued_run_cancel(
                        &mut projection,
                        command,
                        run_id,
                        now,
                        &mut emitter,
                        &mut decision,
                    )?;
                }
                Command::QueuedRunEdit {
                    run_id,
                    text,
                    attachments,
                    ..
                } => {
                    dispatch_queued_run_edit(
                        &mut projection,
                        command,
                        run_id,
                        text,
                        attachments.as_deref(),
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
    mode_limit: Option<&DispatchModeLimit>,
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
        created_at: iso(now),
        updated_at: iso(now),
        lineage: crate::contracts::AppThreadLineage {
            parent_thread_id: None,
            relationship_to_parent: None,
            root_thread_id: command.thread_id.clone(),
            side_chat: None,
        },
        forked_from: None,
        ..Thread::default()
    };
    let mut projection = Projection::new(thread.clone());
    let mut emitter = Emitter::new(&command.command_id, &command.thread_id, now);
    let mut decision = Decision::new();
    let created = emitter.event(DomainEvent::ThreadCreated, thread);
    push_event(&mut projection, &mut decision, created);

    // A first message commits with the thread and its run; no client ever sees
    // the new thread idle. A failure here rejects the whole command.
    if let Some(initial) = &command.initial_message {
        let mut dispatch = MessageDispatch::new(
            command.command_id.clone(),
            command.thread_id.clone(),
            initial.message_id.clone(),
            initial.text.clone(),
        );
        dispatch.attachments = initial.attachments.clone();
        dispatch.created_by = command.created_by;
        dispatch.creation_source = command.creation_source;
        dispatch.model_selection = Some(command.model_selection.clone());
        dispatch.dispatch_mode = Some(match &initial.workspace_strategy {
            Some(workspace_strategy) => DispatchMode::DeferStart {
                workspace_strategy: Some(workspace_strategy.clone()),
            },
            None => DispatchMode::StartImmediately {},
        });
        dispatch.scheduled_task_id = initial.scheduled_task_id.clone();
        dispatch.sender_thread_id = initial.sender_thread_id.clone();
        dispatch.title_seed = initial.title_seed.clone();
        dispatch_message(
            &mut projection,
            &dispatch,
            mode_limit,
            now,
            &mut emitter,
            &mut decision,
        )?;
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
        return DispatchMode::QueueAfterActive {};
    }
    // A provider that can neither steer nor restart its turn takes the message
    // after the turn.
    let turns = target
        .status
        .is_active()
        .then(|| projection.provider_turn_capabilities(target))
        .flatten();
    match turns {
        Some(turns)
            if !turns.supports_active_steering && provider_switch_waits_for_turn(turns.clone()) =>
        {
            DispatchMode::QueueAfterActive {}
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
        return DispatchMode::StartImmediately {};
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
                return DispatchMode::QueueAfterActive {};
            }
            match projection.provider_turn_capabilities(active_run) {
                Some(capabilities) if capabilities.supports_active_steering => {
                    DispatchMode::SteerActive {
                        target_run_id: active_run.id.clone(),
                    }
                }
                Some(capabilities) if capabilities.supports_queued_messages => {
                    DispatchMode::QueueAfterActive {}
                }
                Some(capabilities) if capabilities.supports_steering_by_interrupt_restart => {
                    DispatchMode::RestartActive {
                        target_run_id: active_run.id.clone(),
                    }
                }
                _ => DispatchMode::QueueAfterActive {},
            }
        }
    }
}

/// The per-send delivery the composer's user setting asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendDeliverySetting {
    Auto,
    Steer,
    Restart,
}

impl SendDeliverySetting {
    /// The modifier asks for the other delivery: steer becomes auto, auto or
    /// restart becomes steer.
    pub const fn with_modifier(self, modifier: bool) -> Self {
        if !modifier {
            return self;
        }
        match self {
            Self::Auto | Self::Restart => Self::Steer,
            Self::Steer => Self::Auto,
        }
    }

    pub const fn delivery_intent(self) -> DeliveryIntent {
        match self {
            Self::Auto => DeliveryIntent::Auto,
            Self::Steer => DeliveryIntent::Steer,
            Self::Restart => DeliveryIntent::Restart,
        }
    }
}

/// The delivery intent for one send: the user's setting plus the modifier.
pub fn resolve_send_delivery(setting: SendDeliverySetting, modifier: bool) -> DeliveryIntent {
    setting.with_modifier(modifier).delivery_intent()
}

/// An agent's explicit steer becomes capability-driven auto delivery when the
/// live turn cannot accept steering; a provider without steering then queues
/// instead of rejecting the message.
pub fn resolve_message_delivery(
    projection: &Projection,
    setting: SendDeliverySetting,
    modifier: bool,
    sender: Actor,
) -> DeliveryIntent {
    let intent = resolve_send_delivery(setting, modifier);
    if sender == Actor::Agent
        && intent == DeliveryIntent::Steer
        && !projection
            .active_run()
            .and_then(|run| projection.provider_turn_capabilities(run))
            .is_some_and(|turns| turns.supports_active_steering)
    {
        return DeliveryIntent::Auto;
    }
    intent
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
    mode_limit: Option<&DispatchModeLimit>,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if projection.thread.deleted_at.is_some() {
        return Err(OrchestratorError::ThreadDeleted(
            projection.thread.id.clone(),
        ));
    }
    refuse_above_dispatch_mode_limit(projection, command, mode_limit)?;

    // A steer follow-up that lost its source records an accepted no-op so the
    // sender's retry does not repeat.
    if let Some(source_run_id) = &command.steer_follow_up_of_run_id {
        let valid = projection.run(source_run_id).is_some()
            && command
                .dispatch_mode
                .clone()
                .unwrap_or(DispatchMode::StartImmediately {})
                == DispatchMode::QueueAfterActive {}
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
        command
            .dispatch_mode
            .clone()
            .unwrap_or(DispatchMode::StartImmediately {}),
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
        .as_ref()
        .and_then(|run_id| projection.run(run_id))
        .cloned();
    let queue_behind = active_run.or(steer_follow_up_source);
    let starts_or_queues = matches!(
        dispatch_mode,
        DispatchMode::DeferStart { .. }
            | DispatchMode::StartImmediately {}
            | DispatchMode::QueueAfterActive {}
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
    mode_limit: Option<&DispatchModeLimit>,
) -> Result<(), OrchestratorError> {
    let Some(limit) = mode_limit else {
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
            mode = DispatchMode::StartImmediately {};
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
        mode = DispatchMode::QueueAfterActive {};
    }
    if command.fresh_context.unwrap_or(false)
        && matches!(
            mode,
            DispatchMode::SteerActive { .. } | DispatchMode::RestartActive { .. }
        )
    {
        mode = DispatchMode::QueueAfterActive {};
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
        started_at: iso(now),
    });
    thread.updated_at = iso(now);
    let event = emitter.event(
        |event| {
            thread_event(
                crate::contracts::ThreadMetadataEventType::MetadataUpdated,
                event,
            )
        },
        thread,
    );
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
                .as_ref()
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

    let attempt_id = RunAttemptId::new(format!("run-attempt:{run_id}:1"));
    let root_node_id = NodeId::new(format!("root-node:{run_id}"));

    let queue_is_held = projection
        .runs
        .iter()
        .any(|run| run.status == RunStatus::Queued && run.queue_held.unwrap_or(false));
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
                .filter(|run| run.status == RunStatus::Queued && run.queue_held.unwrap_or(false))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
    for held in &held_runs_to_release {
        let mut run = held.clone();
        run.queue_held = Some(false);
        let event = emitter
            .event(DomainEvent::RunUpdated, run)
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
        queue_held: Some(queue_held),
        steer_when_ready_run_id: None,
        requested_at: iso(now),
        started_at: None,
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: Some(command.fresh_context.unwrap_or(false)),
        ..Run::default()
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
        ..RunAttempt::default()
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
        ..ExecutionNode::default()
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
            .event(DomainEvent::ProviderThreadUpdated, provider_thread.clone())
            .with_provider_instance(&provider_thread.provider_instance_id);
        push_event(projection, decision, event);
    }
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunCreated, run)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunAttemptCreated, attempt)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::NodeUpdated, node)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::MessageUpdated, message)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::TurnItemUpdated, turn_item)
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
        thread.updated_at = iso(now);
        let event = if switched {
            emitter.event(
                |event| {
                    thread_event(
                        crate::contracts::ThreadMetadataEventType::ProviderSwitched,
                        event,
                    )
                },
                thread,
            )
        } else {
            emitter.event(
                |event| {
                    thread_event(
                        crate::contracts::ThreadMetadataEventType::ModelSelectionUpdated,
                        event,
                    )
                },
                thread,
            )
        };
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
    let attempt_id = RunAttemptId::new(format!("run-attempt:{run_id}:1"));
    let root_node_id = NodeId::new(format!("root-node:{run_id}"));
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
        queue_held: Some(false),
        steer_when_ready_run_id: None,
        requested_at: iso(now),
        started_at: None,
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: Some(command.fresh_context.unwrap_or(false)),
        ..Run::default()
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
        ..RunAttempt::default()
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
        ..ExecutionNode::default()
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
        .event(DomainEvent::ProviderThreadUpdated, provider_thread)
        .with_provider_instance(&model_selection.instance_id);
    push_event(projection, decision, event);
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunCreated, run)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunAttemptCreated, attempt)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::NodeUpdated, node)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::MessageUpdated, message)
            .with_run(&run_id)
            .with_node(&root_node_id)
            .with_provider_instance(&model_selection.instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::TurnItemUpdated, turn_item)
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
        thread.updated_at = iso(now);
        let event = if switched {
            emitter.event(
                |event| {
                    thread_event(
                        crate::contracts::ThreadMetadataEventType::ProviderSwitched,
                        event,
                    )
                },
                thread,
            )
        } else {
            emitter.event(
                |event| {
                    thread_event(
                        crate::contracts::ThreadMetadataEventType::ModelSelectionUpdated,
                        event,
                    )
                },
                thread,
            )
        };
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
        Some(provider_turn.id.as_str()),
        InputIntent::Steer,
        projection.next_turn_item_ordinal(),
        now,
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::MessageUpdated, message)
            .with_run(&target.id)
            .with_node(&root_node_id)
            .with_provider_instance(&target.provider_instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::TurnItemUpdated, turn_item)
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
    root_node_id: &NodeId,
    provider_thread_id: &ProviderThreadId,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) {
    let previous_attempt_ordinal = projection
        .attempts
        .iter()
        .filter(|attempt| attempt.run_id.as_str() == target.id.as_str())
        .count() as u32;
    if let Some(mut old_attempt) = target
        .active_attempt_id
        .as_ref()
        .and_then(|id| projection.attempt(id))
        .cloned()
    {
        old_attempt.status = RunAttemptStatus::Superseded;
        old_attempt.completed_at = Some(iso(now));
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::RunAttemptUpdated, old_attempt)
                .with_run(&target.id)
                .with_node(root_node_id)
                .with_provider_instance(&target.provider_instance_id),
        );
    }
    let attempt_id = RunAttemptId::new(format!(
        "run-attempt:{}:{}",
        target.id,
        previous_attempt_ordinal + 1
    ));
    let attempt = RunAttempt {
        id: attempt_id.clone(),
        run_id: target.id.clone(),
        attempt_ordinal: previous_attempt_ordinal + 1,
        root_node_id: root_node_id.clone(),
        provider_instance_id: target.provider_instance_id.clone(),
        provider_thread_id: provider_thread_id.clone(),
        provider_turn_id: None,
        reason: RunAttemptReason::SteeringRestart,
        status: RunAttemptStatus::Running,
        started_at: Some(iso(now)),
        completed_at: None,
        ..RunAttempt::default()
    };
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunAttemptCreated, attempt)
            .with_run(&target.id)
            .with_node(root_node_id)
            .with_provider_instance(&target.provider_instance_id),
    );
    if let Some(mut node) = projection.node(root_node_id).cloned() {
        node.status = NodeStatus::Running;
        node.started_at = Some(iso(now));
        node.completed_at = None;
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::NodeUpdated, node)
                .with_run(&target.id)
                .with_node(root_node_id)
                .with_provider_instance(&target.provider_instance_id),
        );
    }
    let mut run = target.clone();
    run.active_attempt_id = Some(attempt_id);
    run.status = RunStatus::Running;
    run.started_at = run.started_at.or(Some(iso(now)));
    run.completed_at = None;
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunUpdated, run)
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
            hold_queue: Some(true),
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
        .filter(|run| run.status == RunStatus::Queued && !run.queue_held.unwrap_or(false))
        .cloned()
        .collect();
    for mut run in queued {
        run.queue_held = Some(true);
        let run_id = run.id.clone();
        let provider_instance_id = run.provider_instance_id.clone();
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::RunUpdated, run)
                .with_run(&run_id)
                .with_provider_instance(&provider_instance_id),
        );
    }
    // Queued after the interrupt, since a thread's effects run in order.
    if command.stop_delegated_tasks.unwrap_or(false) && !projection.subagent_ids.is_empty() {
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
            .event(DomainEvent::TurnItemUpdated, item)
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
        .as_ref()
        .and_then(|id| projection.provider_thread(id))
        .cloned();
    let provider_turn = projection.running_provider_turn(&run).cloned();

    let ordinal = projection.next_turn_item_ordinal();
    let request_item = interrupt_request_item(&run, command.reason.as_deref(), now, ordinal);
    let request_item_id = request_item.id().cloned();
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::TurnItemUpdated, request_item)
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
        let result_item = TurnItem::RunInterruptResult(RunInterruptResultTurnItem {
            base: TurnItemBase {
                id: TurnItemId::new(format!("turn-item:{}:interrupt-result", run.id)),
                thread_id: run.thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(root_node_id.clone()),
                provider_thread_id: run.provider_thread_id.clone(),
                provider_turn_id: None,
                parent_item_id: request_item_id,
                ordinal: ordinal + 1,
                status: TurnItemStatus::Interrupted,
                title: Some("Interrupted".into()),
                started_at: Some(iso(now)),
                completed_at: Some(iso(now)),
                updated_at: iso(now),
                ..TurnItemBase::default()
            },
            message: "Run interrupted before provider start".into(),
        });
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::TurnItemUpdated, result_item)
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
    ordinal: u32,
) -> TurnItem {
    TurnItem::RunInterruptRequest(RunInterruptRequestTurnItem {
        base: TurnItemBase {
            id: TurnItemId::new(format!("turn-item:{}:interrupt-request", run.id)),
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: run.root_node_id.clone(),
            provider_thread_id: run.provider_thread_id.clone(),
            provider_turn_id: None,
            parent_item_id: None,
            ordinal,
            status: TurnItemStatus::Completed,
            title: Some("Interrupt requested".into()),
            started_at: Some(iso(now)),
            completed_at: Some(iso(now)),
            updated_at: iso(now),
            ..TurnItemBase::default()
        },
        message: reason.unwrap_or("Stopped by user").to_owned(),
        unresponsive_at: None,
    })
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
        .as_ref()
        .and_then(|id| projection.attempt(id))
        .cloned()
    {
        attempt.status = attempt_status_for_run(status);
        attempt.completed_at = Some(iso(now));
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::RunAttemptUpdated, attempt)
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    if let Some(mut node) = run
        .root_node_id
        .as_ref()
        .and_then(|id| projection.node(id))
        .cloned()
    {
        node.status = node_status_for_run(status);
        node.completed_at = Some(iso(now));
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::NodeUpdated, node)
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    let mut run = run.clone();
    run.status = status;
    run.queue_position = None;
    run.completed_at = Some(iso(now));
    let run_id = run.id.clone();
    let node_id = run.root_node_id.clone().unwrap_or_default();
    let provider_instance_id = run.provider_instance_id.clone();
    let active_attempt_id = run.active_attempt_id.clone();
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunUpdated, run)
            .with_run(&run_id)
            .with_node(&node_id)
            .with_provider_instance(&provider_instance_id),
    );

    // Every terminal path settles the run's open work: nodes, turn items,
    // subagents, provider turns and approval/input requests. Nothing may be
    // left looking live after its run ended.
    let open_nodes: Vec<ExecutionNode> = projection
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.run_id.as_ref() == Some(&run_id)
                && crate::orchestration::subagents::is_work_active(candidate.status)
        })
        .cloned()
        .collect();
    for mut node in open_nodes {
        node.status = crate::orchestration::subagents::settled_node_status(status);
        node.completed_at = Some(iso(now));
        let id = node.id.clone();
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::NodeUpdated, node)
                .with_run(&run_id)
                .with_node(&id)
                .with_provider_instance(&provider_instance_id),
        );
    }

    let open_items: Vec<TurnItem> = projection
        .turn_items
        .iter()
        .filter(|item| {
            item.base().is_some_and(|base| {
                base.run_id.as_ref() == Some(&run_id)
                    && matches!(
                        base.status,
                        TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
                    )
            })
        })
        .cloned()
        .collect();
    for mut item in open_items {
        if let Some(base) = item.base_mut() {
            base.status = match status {
                RunStatus::Completed => TurnItemStatus::Completed,
                RunStatus::Interrupted => TurnItemStatus::Interrupted,
                RunStatus::Failed => TurnItemStatus::Failed,
                RunStatus::Cancelled | RunStatus::RolledBack => TurnItemStatus::Cancelled,
                _ => base.status,
            };
            base.completed_at = Some(iso(now));
            base.updated_at = iso(now);
        }
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::TurnItemUpdated, item)
                .with_run(&run_id)
                .with_provider_instance(&provider_instance_id),
        );
    }

    let open_subagents: Vec<crate::contracts::Subagent> = projection
        .subagents
        .iter()
        .filter(|subagent| subagent.run_id.as_ref() == Some(&run_id) && subagent.status.is_active())
        .cloned()
        .collect();
    for mut subagent in open_subagents {
        subagent.status = crate::orchestration::subagents::settled_status(status);
        subagent.completed_at = Some(iso(now));
        subagent.updated_at = iso(now);
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::SubagentUpdated, subagent)
                .with_run(&run_id)
                .with_provider_instance(&provider_instance_id),
        );
    }

    let open_turns: Vec<ProviderTurn> = projection
        .provider_turns
        .iter()
        .filter(|turn| {
            active_attempt_id.as_ref() == turn.run_attempt_id.as_ref()
                && matches!(
                    turn.status,
                    ProviderTurnStatus::Pending | ProviderTurnStatus::Running
                )
        })
        .cloned()
        .collect();
    for mut turn in open_turns {
        turn.status = match status {
            RunStatus::Completed => ProviderTurnStatus::Completed,
            RunStatus::Interrupted => ProviderTurnStatus::Interrupted,
            RunStatus::Failed => ProviderTurnStatus::Failed,
            RunStatus::Cancelled | RunStatus::RolledBack => ProviderTurnStatus::Cancelled,
            _ => turn.status,
        };
        turn.completed_at = Some(iso(now));
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::ProviderTurnUpdated, turn)
                .with_run(&run_id)
                .with_provider_instance(&provider_instance_id),
        );
    }

    let run_node_ids: std::collections::BTreeSet<NodeId> = projection
        .nodes
        .iter()
        .filter(|node| node.run_id.as_ref() == Some(&run_id))
        .map(|node| node.id.clone())
        .collect();
    let open_requests: Vec<RuntimeRequest> = projection
        .runtime_requests
        .iter()
        .filter(|request| {
            request.status == RequestStatus::Pending && run_node_ids.contains(&request.node_id)
        })
        .cloned()
        .collect();
    for mut request in open_requests {
        request.status = RequestStatus::Cancelled;
        request.resolved_at = Some(iso(now));
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::RuntimeRequestUpdated, request)
                .with_run(&run_id)
                .with_provider_instance(&provider_instance_id),
        );
    }
}

fn request_kind_for_approval(kind: RequestKind) -> ProviderRequestKind {
    match kind {
        RequestKind::Command => ProviderRequestKind::Command,
        RequestKind::FileRead => ProviderRequestKind::FileRead,
        RequestKind::FileChange => ProviderRequestKind::FileChange,
        RequestKind::McpElicitation => ProviderRequestKind::McpElicitation,
        RequestKind::Permission => ProviderRequestKind::Permission,
        RequestKind::DynamicToolCall | RequestKind::UserInput | RequestKind::AuthRefresh => {
            ProviderRequestKind::Permission
        }
    }
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
// Queued run editing
// ---------------------------------------------------------------------------

fn dispatch_error(command: &Command, cause: impl Into<String>) -> OrchestratorError {
    OrchestratorError::Dispatch {
        command_id: command.command_id().clone(),
        command_type: command.command_type().to_owned(),
        cause: cause.into(),
    }
}

fn queued_run_for_command(
    projection: &Projection,
    command: &Command,
    run_id: &RunId,
) -> Result<Run, OrchestratorError> {
    projection
        .run(run_id)
        .filter(|run| run.status == RunStatus::Queued)
        .cloned()
        .ok_or_else(|| dispatch_error(command, format!("Run {run_id} is not queued.")))
}

fn is_automatic_delivery(projection: &Projection, run: &Run) -> bool {
    projection
        .message(&run.user_message_id)
        .is_some_and(|message| {
            message.delegated_completion.is_some() || message.notification.is_some()
        })
}

/// Move a queued run to another place in the manual queue. Automatic
/// completion deliveries stay ahead of the manual queue and are never moved.
fn dispatch_queued_run_reorder(
    projection: &mut Projection,
    command: &Command,
    run_id: &RunId,
    before_run_id: Option<&RunId>,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let moving = queued_run_for_command(projection, command, run_id)?;
    if is_automatic_delivery(projection, &moving) {
        return Err(dispatch_error(
            command,
            "Automatic completion deliveries cannot be reordered.",
        ));
    }
    let queued = crate::orchestration::queued_run_order::queued_runs_in_delivery_order(projection);
    let automatic: Vec<Run> = queued
        .iter()
        .filter(|run| is_automatic_delivery(projection, run))
        .cloned()
        .collect();
    if before_run_id.is_some_and(|target| automatic.iter().any(|run| run.id == *target)) {
        return Err(dispatch_error(
            command,
            "Queued messages cannot be reordered ahead of automatic completion delivery.",
        ));
    }
    let mut manual: Vec<Run> = queued
        .into_iter()
        .filter(|run| !is_automatic_delivery(projection, run) && run.id != *run_id)
        .collect();
    let before_index = match before_run_id {
        None => manual.len(),
        Some(target) => manual
            .iter()
            .position(|run| run.id == *target)
            .ok_or_else(|| {
                dispatch_error(command, format!("Queue target {target} is not queued."))
            })?,
    };
    manual.insert(before_index, moving);
    let mut reordered = automatic;
    reordered.extend(manual);

    for (index, run) in reordered.iter().enumerate() {
        let position = (index + 1) as u32;
        if run.queue_position == Some(position) {
            continue;
        }
        let mut updated = run.clone();
        updated.queue_position = Some(position);
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::RunUpdated, updated)
                .with_run(&run.id)
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    let _ = now;
    Ok(())
}

/// Cancel a queued run and settle its message-side bookkeeping.
fn dispatch_queued_run_cancel(
    projection: &mut Projection,
    command: &Command,
    run_id: &RunId,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let run = queued_run_for_command(projection, command, run_id)?;
    let message = projection.message(&run.user_message_id).cloned();

    if let Some(completion) = message
        .as_ref()
        .and_then(|message| message.delegated_completion.as_ref())
        && let Some(mut parent) = projection.run(&completion.parent_run_id).cloned()
    {
        let parent_id = parent.id.clone();
        if let Some(cohort) = parent.delegated_completion.as_mut() {
            cohort.disposition = crate::contracts::DelegatedCompletionDisposition::Disposed;
            cohort.delivery = None;
        }
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::RunUpdated, parent)
                .with_run(&parent_id)
                .with_provider_instance(&run.provider_instance_id),
        );
    }

    if let Some(mut attempt) = run
        .active_attempt_id
        .as_ref()
        .and_then(|id| projection.attempt(id))
        .cloned()
    {
        attempt.status = RunAttemptStatus::Cancelled;
        attempt.completed_at = Some(iso(now));
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::RunAttemptUpdated, attempt)
                .with_run(&run.id)
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    if let Some(mut node) = run
        .root_node_id
        .as_ref()
        .and_then(|id| projection.node(id))
        .cloned()
    {
        node.status = NodeStatus::Cancelled;
        node.completed_at = Some(iso(now));
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::NodeUpdated, node)
                .with_run(&run.id)
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    let open_items: Vec<TurnItem> = projection
        .turn_items
        .iter()
        .filter(|item| {
            item.base().is_some_and(|base| {
                base.run_id.as_ref() == Some(&run.id)
                    && matches!(
                        base.status,
                        TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
                    )
            })
        })
        .cloned()
        .collect();
    for mut item in open_items {
        if let Some(base) = item.base_mut() {
            base.status = TurnItemStatus::Cancelled;
            base.completed_at = Some(iso(now));
            base.updated_at = iso(now);
        }
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::TurnItemUpdated, item)
                .with_run(&run.id)
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    let mut cancelled = run.clone();
    cancelled.status = RunStatus::Cancelled;
    cancelled.queue_position = None;
    cancelled.completed_at = Some(iso(now));
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunUpdated, cancelled)
            .with_run(&run.id)
            .with_provider_instance(&run.provider_instance_id),
    );
    Ok(())
}

/// Edit a queued run's text and attachments before it starts.
#[allow(clippy::too_many_arguments)]
fn dispatch_queued_run_edit(
    projection: &mut Projection,
    command: &Command,
    run_id: &RunId,
    text: &str,
    attachments: Option<&[Attachment]>,
    now: Timestamp,
    emitter: &mut Emitter,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    if text.trim().is_empty() {
        return Err(dispatch_error(
            command,
            format!("Queued run {run_id} cannot be edited to an empty message."),
        ));
    }
    let run = queued_run_for_command(projection, command, run_id)?;
    let Some(mut message) = projection.message(&run.user_message_id).cloned() else {
        return Err(dispatch_error(
            command,
            format!("Queued run {} has no user message.", run.id),
        ));
    };
    if is_automatic_delivery(projection, &run) {
        return Err(dispatch_error(
            command,
            "Automatic completion deliveries cannot be edited.",
        ));
    }
    message.text = text.to_owned();
    if let Some(attachments) = attachments {
        message.attachments = attachments.to_vec();
    }
    message.updated_at = iso(now);
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::MessageUpdated, message)
            .with_run(&run.id)
            .with_provider_instance(&run.provider_instance_id),
    );

    if let Some(mut item) = projection
        .turn_items
        .iter()
        .find(|item| {
            matches!(
                item,
                TurnItem::UserMessage(turn) if turn.message_id == run.user_message_id
            )
        })
        .cloned()
    {
        if let TurnItem::UserMessage(turn) = &mut item {
            turn.text = text.to_owned();
            if let Some(attachments) = attachments {
                turn.attachments = attachments.to_vec();
            }
        }
        if let Some(base) = item.base_mut() {
            base.updated_at = iso(now);
        }
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::TurnItemUpdated, item)
                .with_run(&run.id)
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    Ok(())
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
        .ok_or_else(|| OrchestratorError::RunNotInterruptible(input.run_id.clone()))?;
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
        .as_ref()
        .and_then(|id| projection.provider_thread(id))
        .and_then(|thread| thread.provider_session_id.clone());
    let response_capability = match live_provider_session_id {
        Some(provider_session_id) => ResponseCapability::Live {
            provider_session_id,
            supports_feedback: None,
        },
        None => ResponseCapability::Message {},
    };
    let request = RuntimeRequest {
        id: input.request_id.clone(),
        node_id: input.node_id.clone(),
        provider_turn_id: input.provider_turn_id.clone(),
        native_request_ref: None,
        kind: input.kind,
        status: RequestStatus::Pending,
        response_capability,
        created_at: iso(now),
        resolved_at: None,
        decision: None,
        feedback: None,
        answers: None,
    };
    push_event(
        projection,
        &mut decision,
        emitter
            .event(DomainEvent::RuntimeRequestUpdated, request)
            .with_run(&run.id)
            .with_node(&input.node_id)
            .with_provider_instance(&input.provider_instance_id),
    );
    let item = TurnItem::ApprovalRequest(ApprovalRequestTurnItem {
        base: TurnItemBase {
            id: TurnItemId::new(format!("turn-item:{}", input.request_id)),
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(input.node_id.clone()),
            provider_thread_id: run.provider_thread_id.clone(),
            provider_turn_id: input.provider_turn_id,
            parent_item_id: None,
            ordinal: projection.next_turn_item_ordinal(),
            status: TurnItemStatus::Running,
            title: input.prompt.clone(),
            started_at: Some(iso(now)),
            completed_at: None,
            updated_at: iso(now),
            ..TurnItemBase::default()
        },
        request_id: input.request_id,
        request_kind: request_kind_for_approval(input.kind),
        prompt: input.prompt,
        reason: None,
        full_access_override: None,
        app_name: None,
        options: None,
    });
    push_event(
        projection,
        &mut decision,
        emitter
            .event(DomainEvent::TurnItemUpdated, item)
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
    resolved.resolved_at = Some(iso(now));
    resolved.decision = command.decision;
    resolved.feedback = command.feedback.clone();
    // A user-input answer is part of the request's resolved record, as T3's
    // `runtimeRequest.updated` payload carries it.
    resolved.answers = command.answers.clone();
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RuntimeRequestUpdated, resolved)
            .with_node(&request.node_id),
    );
    // The approval card resolves with its request.
    if let Some(mut item) = projection
        .turn_items
        .iter()
        .find(|item| {
            matches!(
                item,
                TurnItem::ApprovalRequest(approval)
                    if approval.request_id == command.request_id
            )
        })
        .cloned()
    {
        if let Some(base) = item.base_mut() {
            base.status = TurnItemStatus::Completed;
            base.completed_at = Some(iso(now));
            base.updated_at = iso(now);
        }
        push_event(
            projection,
            decision,
            emitter
                .event(DomainEvent::TurnItemUpdated, item)
                .with_node(&request.node_id),
        );
    }
    if let Some(provider_session_id) = match &request.response_capability {
        ResponseCapability::Live {
            provider_session_id,
            ..
        } => Some(provider_session_id.clone()),
        ResponseCapability::Message {} | ResponseCapability::NotResumable { .. } => None,
    } {
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
    thread.updated_at = iso(now);
    let event = if switched {
        emitter.event(
            |event| {
                thread_event(
                    crate::contracts::ThreadMetadataEventType::ProviderSwitched,
                    event,
                )
            },
            thread,
        )
    } else {
        emitter.event(
            |event| {
                thread_event(
                    crate::contracts::ThreadMetadataEventType::ModelSelectionUpdated,
                    event,
                )
            },
            thread,
        )
    };
    push_event(
        projection,
        decision,
        event.with_provider_instance(&command.model_selection.instance_id),
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
            .as_ref()
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
    thread.updated_at = iso(now);
    push_event(
        projection,
        decision,
        emitter
            .event(
                |event| {
                    thread_event(
                        crate::contracts::ThreadMetadataEventType::RuntimeModeUpdated,
                        event,
                    )
                },
                thread.clone(),
            )
            .with_provider_instance(&thread.provider_instance_id),
    );
    // Lowering the mode releases a held queue.
    if projection
        .runs
        .iter()
        .any(|run| run.status == RunStatus::Queued && run.queue_held.unwrap_or(false))
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
        emitter.event(
            |event| {
                thread_event(
                    crate::contracts::ThreadMetadataEventType::MetadataUpdated,
                    event,
                )
            },
            thread,
        ),
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
        format!("command:system:start-queued:{thread_id}"),
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
    if queued.queue_held.unwrap_or(false) {
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
        .as_ref()
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
        thread.updated_at = iso(now);
        let event = if switched {
            emitter.event(
                |event| {
                    thread_event(
                        crate::contracts::ThreadMetadataEventType::ProviderSwitched,
                        event,
                    )
                },
                thread,
            )
        } else {
            emitter.event(
                |event| {
                    thread_event(
                        crate::contracts::ThreadMetadataEventType::ModelSelectionUpdated,
                        event,
                    )
                },
                thread,
            )
        };
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
            .event(DomainEvent::ProviderThreadUpdated, provider_thread.clone())
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
        matches!(item, TurnItem::UserMessage(turn_message) if turn_message.message_id == message.id)
    })
        && let (Some(base), Some(existing_base)) = (turn_item.base_mut(), existing.base()) {
            base.ordinal = existing_base.ordinal;
        }
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::TurnItemUpdated, turn_item)
            .with_run(&queued.id)
            .with_node(&root_node_id)
            .with_provider_instance(&queued.provider_instance_id),
    );
    push_event(
        projection,
        decision,
        emitter
            .event(DomainEvent::RunUpdated, starting_run)
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
                .event(DomainEvent::RunAttemptUpdated, attempt)
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
                .event(DomainEvent::NodeUpdated, node)
                .with_run(&queued.id)
                .with_node(&root_node_id)
                .with_provider_instance(&queued.provider_instance_id),
        );
    }
    decision.add_effect(
        format!("command:system:start-queued:{}", queued.id),
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
        .ok_or_else(|| OrchestratorError::RunNotInterruptible(RunId::new(run_id)))?;
    let thread_id = run.thread_id.clone();
    let mut emitter = Emitter::new(
        format!("command:system:provider-turn-started:{thread_id}"),
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
            .event(DomainEvent::ProviderTurnUpdated, provider_turn)
            .with_run(&run.id)
            .with_provider_instance(&run.provider_instance_id),
    );
    if let Some(mut attempt) = run
        .active_attempt_id
        .as_ref()
        .and_then(|id| projection.attempt(id))
        .cloned()
    {
        attempt.status = RunAttemptStatus::Running;
        attempt.provider_turn_id = Some(provider_turn_id.clone());
        attempt.started_at = attempt.started_at.or(Some(iso(now)));
        push_event(
            projection,
            &mut decision,
            emitter
                .event(DomainEvent::RunAttemptUpdated, attempt)
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    if let Some(mut node) = run
        .root_node_id
        .as_ref()
        .and_then(|id| projection.node(id))
        .cloned()
    {
        node.status = NodeStatus::Running;
        node.provider_thread_id = Some(provider_thread_id);
        node.provider_turn_id = Some(provider_turn_id.clone());
        node.started_at = node.started_at.or(Some(iso(now)));
        push_event(
            projection,
            &mut decision,
            emitter
                .event(DomainEvent::NodeUpdated, node)
                .with_run(&run.id)
                .with_node(run.root_node_id.clone().unwrap_or_default())
                .with_provider_instance(&run.provider_instance_id),
        );
    }
    let mut running = run.clone();
    running.status = RunStatus::Running;
    running.started_at = running.started_at.or(Some(iso(now)));
    push_event(
        projection,
        &mut decision,
        emitter
            .event(DomainEvent::RunUpdated, running)
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
        .ok_or_else(|| OrchestratorError::RunNotInterruptible(RunId::new(run_id)))?;
    let thread_id = run.thread_id.clone();
    let mut emitter = Emitter::new(
        format!("command:system:run-finished:{run_id}"),
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

#[derive(Clone, Debug, PartialEq)]
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
    dispatch_with_limit(store, command, None, now)
}

/// Decide and commit a command with a server-side launch-mode policy.
pub fn dispatch_with_limit<S: Store>(
    store: &mut S,
    command: &Command,
    mode_limit: Option<&DispatchModeLimit>,
    now: Timestamp,
) -> Result<DispatchResult, OrchestratorError> {
    let command_id = command.command_id().clone();
    if let Some(receipt) = store.command_receipt(&command_id) {
        return replay(receipt, command, store);
    }
    let projection = store.projection(command.thread_id());
    let decision = match decide_with_limit(projection.as_ref(), command, mode_limit, now) {
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

fn derive_run_id(thread_id: impl ToString, ordinal: u32) -> RunId {
    RunId::new(format!("run:{}:{ordinal}", thread_id.to_string()))
}

fn new_provider_thread(
    run_id: impl ToString,
    model_selection: &ModelSelection,
    ordinal: u32,
    now: Timestamp,
    thread_id: impl ToString,
) -> ProviderThread {
    ProviderThread {
        id: ProviderThreadId::new(format!("provider-thread:{}", run_id.to_string())),
        driver: ProviderDriverKind::new(model_selection.instance_id.as_str()),
        provider_instance_id: model_selection.instance_id.clone(),
        provider_session_id: None,
        app_thread_id: Some(ThreadId::new(thread_id.to_string())),
        native_thread_ref: None,
        status: ProviderThreadStatus::NotLoaded,
        first_run_ordinal: Some(ordinal),
        last_run_ordinal: Some(ordinal),
        created_at: iso(now),
        updated_at: iso(now),
        ..ProviderThread::default()
    }
}

fn build_user_message(
    command: &MessageDispatch,
    run_id: impl ToString,
    node_id: impl ToString,
    follows_thread_selection: bool,
    now: Timestamp,
) -> Message {
    Message {
        id: command.message_id.clone(),
        thread_id: command.thread_id.clone(),
        run_id: Some(RunId::new(run_id.to_string())),
        node_id: Some(NodeId::new(node_id.to_string())),
        role: MessageRole::User,
        text: command.text.clone(),
        attachments: command.attachments.clone(),
        streaming: false,
        created_by: command.created_by,
        creation_source: command.creation_source,
        scheduled_task_id: command.scheduled_task_id.clone(),
        sender_thread_id: command.sender_thread_id.clone(),
        follows_thread_selection: Some(follows_thread_selection),
        created_at: iso(now),
        updated_at: iso(now),
        delegated_completion: command.delegated_completion.as_ref().map(|completion| {
            DelegatedCompletion {
                parent_run_id: completion.parent_run_id.clone(),
                generation: completion.generation,
                task_ids: completion.task_ids.clone(),
            }
        }),
        notification: command.notification.clone(),
        ..Message::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_user_turn_item(
    message: &Message,
    run_id: impl ToString,
    node_id: impl ToString,
    provider_thread_id: impl ToString,
    provider_turn_id: Option<&str>,
    input_intent: InputIntent,
    ordinal: u32,
    now: Timestamp,
) -> TurnItem {
    TurnItem::UserMessage(UserMessageTurnItem {
        base: TurnItemBase {
            id: TurnItemId::new(format!("turn-item:{}", message.id)),
            thread_id: message.thread_id.clone(),
            run_id: Some(RunId::new(run_id.to_string())),
            node_id: Some(NodeId::new(node_id.to_string())),
            provider_thread_id: Some(ProviderThreadId::new(provider_thread_id.to_string())),
            provider_turn_id: provider_turn_id.map(ProviderTurnId::new),
            parent_item_id: None,
            ordinal,
            status: TurnItemStatus::Completed,
            title: None,
            started_at: Some(iso(now)),
            completed_at: Some(iso(now)),
            updated_at: iso(now),
            ..TurnItemBase::default()
        },
        created_by: message.created_by,
        creation_source: message.creation_source,
        message_id: message.id.clone(),
        input_intent,
        text: message.text.clone(),
        attachments: Vec::new(),
        scheduled_task_id: None,
        sender_thread_id: None,
        linked_sender: None,
        idle_compaction: None,
        workflow_script: None,
        fresh_context: None,
        text_omitted: None,
        context: None,
        model_switch: None,
    })
}

/// The command-level version of `followsCurrentConversation`: the message does
/// not exist yet when the queue tail's selection is chosen.
fn command_follows_current_conversation(command: &MessageDispatch) -> bool {
    command.delegated_completion.is_some()
        || (command.scheduled_task_id.is_some() && command.model_selection.is_none())
        || command
            .notification
            .as_ref()
            .is_some_and(notification_is_monitor)
}
