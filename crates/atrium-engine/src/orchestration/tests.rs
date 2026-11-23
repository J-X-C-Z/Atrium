// Ported test expectations from apps/server/src/orchestration-v2/{Orchestrator.receipt-race,ThreadLaunchService,SteeringCompletion.integration,RuntimePolicy,QueuedRunOrder}.test.ts (T3 Code v0.1.11, MIT).
//
// The cases are the pure ones: provider processes become projections and
// outbox effects, and the harness is a `MemoryStore`.

use crate::orchestration::event_sink::{MemoryStore, ReceiptStatus};
use crate::orchestration::orchestrator::{
    ApprovalRequested, decide, dispatch, plan_approval_requested, plan_provider_turn_started,
    plan_run_finished, plan_start_next_queued_run, resolve_dispatch_intent,
};
use crate::orchestration::types::*;

const NOW: Timestamp = 1_000;

fn model() -> ModelSelection {
    ModelSelection::new("codex", "gpt-5.1-codex")
}

fn test_thread() -> Thread {
    Thread {
        id: "thread:a".into(),
        project_id: "project:test".into(),
        title: "Thread".into(),
        provider_instance_id: "codex".into(),
        model_selection: model(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        branch: None,
        worktree_path: None,
        active_provider_thread_id: Some("provider-thread:a".into()),
        created_by: Actor::User,
        creation_source: CreationSource::Web,
        created_at: 1,
        updated_at: 1,
        archived_at: None,
        settled_override: None,
        settled_at: None,
        last_visited_at: Some(1),
        title_regeneration: None,
        deleted_at: None,
    }
}

fn caps(
    supports_active_steering: bool,
    supports_queued_messages: bool,
    supports_steering_by_interrupt_restart: bool,
) -> ProviderCapabilities {
    ProviderCapabilities {
        turns: TurnCapabilities {
            supports_interrupt: true,
            supports_active_steering,
            active_steering_interrupts_tools: false,
            active_steering_accepts_attachments: true,
            supports_steering_by_interrupt_restart,
            supports_queued_messages,
        },
    }
}

fn user_message(id: &str, run_id: Option<&str>, text: &str) -> Message {
    Message {
        id: id.into(),
        thread_id: "thread:a".into(),
        run_id: run_id.map(str::to_owned),
        node_id: Some("root:a".into()),
        role: MessageRole::User,
        text: text.into(),
        created_at: 1,
        updated_at: 1,
        ..Message::default()
    }
}

fn user_turn_item(
    message: &Message,
    run_id: &str,
    node_id: &str,
    provider_thread_id: &str,
    provider_turn_id: Option<&str>,
    input_intent: InputIntent,
    ordinal: i64,
) -> TurnItem {
    TurnItem {
        id: format!("turn-item:{}", message.id),
        thread_id: message.thread_id.clone(),
        run_id: Some(run_id.into()),
        node_id: Some(node_id.into()),
        provider_thread_id: Some(provider_thread_id.into()),
        provider_turn_id: provider_turn_id.map(str::to_owned),
        parent_item_id: None,
        ordinal,
        status: TurnItemStatus::Completed,
        title: None,
        created_by: message.created_by,
        creation_source: message.creation_source,
        started_at: Some(NOW),
        completed_at: Some(NOW),
        updated_at: NOW,
        kind: TurnItemKind::UserMessage {
            message_id: message.id.clone(),
            text: message.text.clone(),
            input_intent,
        },
    }
}

fn active_projection() -> Projection {
    let mut projection = Projection::new(test_thread());
    projection.provider_threads.push(ProviderThread {
        id: "provider-thread:a".into(),
        driver: "codex".into(),
        provider_instance_id: "codex".into(),
        provider_session_id: Some("session:a".into()),
        app_thread_id: "thread:a".into(),
        native_thread_ref: Some("native:a".into()),
        status: ProviderThreadStatus::Active,
        first_run_ordinal: Some(1),
        last_run_ordinal: Some(1),
        created_at: 1,
        updated_at: 1,
    });
    projection.provider_sessions.push(ProviderSession {
        id: "session:a".into(),
        provider_instance_id: "codex".into(),
        provider_thread_id: "provider-thread:a".into(),
        capabilities: caps(true, true, true),
        cwd: Some("/work".into()),
        model: "gpt-5.1-codex".into(),
    });
    projection.provider_turns.push(ProviderTurn {
        id: "turn:a".into(),
        provider_thread_id: "provider-thread:a".into(),
        run_attempt_id: "attempt:a".into(),
        status: ProviderTurnStatus::Running,
        created_at: 1,
        updated_at: 1,
    });
    projection.runs.push(Run {
        id: "run:a".into(),
        thread_id: "thread:a".into(),
        ordinal: 1,
        provider_instance_id: "codex".into(),
        model_selection: model(),
        provider_thread_id: Some("provider-thread:a".into()),
        user_message_id: "message:first".into(),
        root_node_id: Some("root:a".into()),
        active_attempt_id: Some("attempt:a".into()),
        status: RunStatus::Running,
        queue_position: None,
        queue_held: false,
        steer_when_ready_run_id: None,
        requested_at: 1,
        started_at: Some(1),
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: false,
    });
    let first = user_message("message:first", Some("run:a"), "first");
    projection.messages.push(first.clone());
    projection.attempts.push(RunAttempt {
        id: "attempt:a".into(),
        run_id: "run:a".into(),
        attempt_ordinal: 1,
        root_node_id: "root:a".into(),
        provider_instance_id: "codex".into(),
        provider_thread_id: "provider-thread:a".into(),
        provider_turn_id: Some("turn:a".into()),
        reason: RunAttemptReason::Initial,
        status: RunAttemptStatus::Running,
        started_at: Some(1),
        completed_at: None,
    });
    projection.nodes.push(ExecutionNode {
        id: "root:a".into(),
        thread_id: "thread:a".into(),
        run_id: Some("run:a".into()),
        parent_node_id: None,
        root_node_id: "root:a".into(),
        kind: NodeKind::RootTurn,
        status: NodeStatus::Running,
        counts_for_run: true,
        provider_thread_id: Some("provider-thread:a".into()),
        provider_turn_id: Some("turn:a".into()),
        runtime_request_id: None,
        started_at: Some(1),
        completed_at: None,
    });
    projection.turn_items.push(user_turn_item(
        &first,
        "run:a",
        "root:a",
        "provider-thread:a",
        Some("turn:a"),
        InputIntent::TurnStart,
        0,
    ));
    projection
}

fn add_queued_run(projection: &mut Projection, run_id: &str, ordinal: u32) {
    let message_id = format!("message:{run_id}");
    let node_id = format!("root:{run_id}");
    let message = user_message(&message_id, Some(run_id), "queued");
    projection.messages.push(message.clone());
    projection.runs.push(Run {
        id: run_id.into(),
        thread_id: "thread:a".into(),
        ordinal,
        provider_instance_id: "codex".into(),
        model_selection: model(),
        provider_thread_id: Some("provider-thread:a".into()),
        user_message_id: message_id,
        root_node_id: Some(node_id.clone()),
        active_attempt_id: Some(format!("attempt:{run_id}")),
        status: RunStatus::Queued,
        queue_position: Some(ordinal),
        queue_held: false,
        steer_when_ready_run_id: None,
        requested_at: 1,
        started_at: None,
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: false,
    });
    projection.attempts.push(RunAttempt {
        id: format!("attempt:{run_id}"),
        run_id: run_id.into(),
        attempt_ordinal: 1,
        root_node_id: node_id.clone(),
        provider_instance_id: "codex".into(),
        provider_thread_id: "provider-thread:a".into(),
        provider_turn_id: None,
        reason: RunAttemptReason::Initial,
        status: RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    });
    projection.nodes.push(ExecutionNode {
        id: node_id.clone(),
        thread_id: "thread:a".into(),
        run_id: Some(run_id.into()),
        parent_node_id: None,
        root_node_id: node_id.clone(),
        kind: NodeKind::RootTurn,
        status: NodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some("provider-thread:a".into()),
        provider_turn_id: None,
        runtime_request_id: None,
        started_at: None,
        completed_at: None,
    });
    projection.turn_items.push(user_turn_item(
        &message,
        run_id,
        &node_id,
        "provider-thread:a",
        None,
        InputIntent::QueuedTurn,
        ordinal as i64,
    ));
}

fn create_command(thread_id: &str, command_id: &str) -> Command {
    Command::ThreadCreate(ThreadCreate {
        command_id: command_id.into(),
        thread_id: thread_id.into(),
        project_id: "project:test".into(),
        title: "Thread".into(),
        model_selection: model(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        branch: None,
        worktree_path: None,
        created_by: Actor::User,
        creation_source: CreationSource::Web,
        initial_message: None,
        mode_limit: None,
    })
}

fn message_command(text: &str, message_id: &str) -> MessageDispatch {
    MessageDispatch::new("command:message", "thread:a", message_id, text)
}

fn event_types(decision: &crate::orchestration::orchestrator::Decision) -> Vec<&'static str> {
    decision
        .events
        .iter()
        .map(|event| event.kind.type_name())
        .collect()
}

#[test]
fn thread_create_commits_the_thread_first_run_and_message_together() {
    let mut store = MemoryStore::new();
    let Command::ThreadCreate(mut create) = create_command("thread:a", "command:create") else {
        unreachable!()
    };
    create.initial_message = Some(InitialMessage {
        message_id: "message:first".into(),
        text: "hello".into(),
        attachments: Vec::new(),
        title_seed: Some("Hello".into()),
        scheduled_task_id: None,
        sender_thread_id: None,
        workspace_strategy: None,
    });

    let result = dispatch(&mut store, &Command::ThreadCreate(create), NOW).expect("commit");
    let types: Vec<&str> = result
        .stored_events
        .iter()
        .map(|stored| stored.event.kind.type_name())
        .collect();
    assert_eq!(types[0], "thread.created");
    assert!(types.contains(&"run.created"));
    assert!(types.contains(&"message.updated"));
    assert_eq!(
        result.stored_events[0].command_id.as_deref(),
        Some("command:create")
    );

    let projection = store.projection_snapshot("thread:a").expect("projection");
    assert_eq!(projection.runs.len(), 1);
    assert_eq!(projection.runs[0].status, RunStatus::Starting);
    assert_eq!(projection.messages.len(), 1);
    assert_eq!(projection.thread.title, "Hello");
    assert!(projection.thread.title_regeneration.is_some());
    // provider-turn.start and thread-title.generate
    assert_eq!(store.outbox().effects().len(), 2);
}

#[test]
fn a_refused_initial_message_leaves_no_thread() {
    let mut store = MemoryStore::new();
    let Command::ThreadCreate(mut create) = create_command("thread:a", "command:create") else {
        unreachable!()
    };
    create.mode_limit = Some(DispatchModeLimit {
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Default,
    });
    create.initial_message = Some(InitialMessage {
        message_id: "message:first".into(),
        text: "hello".into(),
        attachments: Vec::new(),
        title_seed: Some("Hello".into()),
        scheduled_task_id: None,
        sender_thread_id: None,
        workspace_strategy: None,
    });

    let error = dispatch(&mut store, &Command::ThreadCreate(create), NOW).expect_err("mode limit");
    assert!(matches!(error, OrchestratorError::ModeLimitExceeded { .. }));
    assert!(store.projection_snapshot("thread:a").is_none());
}

#[test]
fn a_command_id_reused_concurrently_for_two_threads_succeeds_for_only_one() {
    // T3 `Orchestrator.receipt-race.test.ts`.
    let mut store = MemoryStore::new();
    for thread_id in ["thread:race-a", "thread:race-b"] {
        dispatch(
            &mut store,
            &create_command(thread_id, &format!("create:{thread_id}")),
            NOW,
        )
        .expect("create");
    }

    let set_model = |thread_id: &str| {
        Command::ThreadModelSelectionSet(ThreadModelSelectionSet {
            command_id: "command:shared".into(),
            thread_id: thread_id.into(),
            model_selection: ModelSelection::new("codex", "gpt-6"),
        })
    };
    let winner = dispatch(&mut store, &set_model("thread:race-a"), NOW).expect("winner");
    let loser = dispatch(&mut store, &set_model("thread:race-b"), NOW).expect_err("loser");
    assert!(matches!(
        loser,
        OrchestratorError::CommandIdConflict {
            ref receipt_thread_id,
            ref command_thread_id,
            ..
        } if receipt_thread_id == "thread:race-a" && command_thread_id == "thread:race-b"
    ));
    assert_eq!(
        store
            .projection_snapshot("thread:race-a")
            .expect("a")
            .thread
            .model_selection
            .model,
        "gpt-6"
    );
    assert_eq!(
        store
            .projection_snapshot("thread:race-b")
            .expect("b")
            .thread
            .model_selection
            .model,
        "gpt-5.1-codex"
    );

    // A no-op command still records a receipt; the loser conflicts even
    // though it would have changed nothing.
    let close_share = |thread_id: &str| Command::ThreadGuestShareClose {
        command_id: "command:shared-noop".into(),
        thread_id: thread_id.into(),
    };
    let no_op_winner =
        dispatch(&mut store, &close_share("thread:race-a"), NOW).expect("no-op winner");
    assert!(no_op_winner.stored_events.is_empty());
    let no_op_loser =
        dispatch(&mut store, &close_share("thread:race-b"), NOW).expect_err("no-op loser");
    assert!(matches!(
        no_op_loser,
        OrchestratorError::CommandIdConflict { .. }
    ));
    let no_op_replay = dispatch(&mut store, &close_share("thread:race-a"), NOW).expect("replay");
    assert!(no_op_replay.replayed);
    assert_eq!(no_op_replay.sequence, no_op_winner.sequence);
    assert!(no_op_replay.stored_events.is_empty());

    // The winner's own model-selection command replays its events.
    let replay = dispatch(&mut store, &set_model("thread:race-a"), NOW).expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.sequence, winner.sequence);
    assert_eq!(
        replay
            .stored_events
            .iter()
            .map(|stored| stored.sequence)
            .collect::<Vec<_>>(),
        winner
            .stored_events
            .iter()
            .map(|stored| stored.sequence)
            .collect::<Vec<_>>()
    );
}

#[test]
fn delivery_intent_resolves_start_steer_and_queue_from_capabilities() {
    let idle = Projection::new(test_thread());
    assert_eq!(
        resolve_dispatch_intent(&idle, DispatchMode::StartImmediately, None),
        DispatchMode::StartImmediately
    );

    let active = active_projection();
    assert_eq!(
        resolve_dispatch_intent(
            &active,
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::SteerActive {
            target_run_id: "run:a".into()
        }
    );

    let mut no_steer = active.clone();
    no_steer.provider_sessions[0].capabilities = caps(false, true, true);
    assert_eq!(
        resolve_dispatch_intent(
            &no_steer,
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::QueueAfterActive
    );

    let mut restart = active.clone();
    restart.provider_sessions[0].capabilities = caps(false, false, true);
    assert_eq!(
        resolve_dispatch_intent(
            &restart,
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::RestartActive {
            target_run_id: "run:a".into()
        }
    );

    // A preparing run has no provider turn yet; automatic input queues.
    let mut preparing = active;
    preparing.runs[0].status = RunStatus::Preparing;
    assert_eq!(
        resolve_dispatch_intent(
            &preparing,
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::QueueAfterActive
    );
}

#[test]
fn an_automatic_message_steers_into_a_running_turn() {
    let projection = active_projection();
    let mut command = message_command("follow up", "message:steer");
    command.delivery_intent = Some(DeliveryIntent::Auto);
    let decision =
        decide(Some(&projection), &Command::MessageDispatch(command), NOW).expect("decide");
    assert!(!event_types(&decision).contains(&"run.created"));
    assert!(
        decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "provider-turn.steer")
    );
    let user_item = decision.events.iter().find_map(|event| match &event.kind {
        EventKind::TurnItemUpdated(item) => Some(item),
        _ => None,
    });
    assert!(matches!(
        user_item.map(|item| &item.kind),
        Some(TurnItemKind::UserMessage {
            input_intent: InputIntent::Steer,
            ..
        })
    ));
}

#[test]
fn maintenance_commands_never_steer() {
    // T3 `ThreadLaunchService.test.ts`: a maintenance command sent with an
    // explicit steer is refused, and a normal message steered into a
    // maintenance turn queues instead.
    let projection = active_projection();
    let mut incoming = message_command(" /COMPACT ", "message:compact");
    incoming.dispatch_mode = DispatchMode::SteerActive {
        target_run_id: "run:a".into(),
    };
    let error = decide(Some(&projection), &Command::MessageDispatch(incoming), NOW)
        .expect_err("maintenance cannot steer");
    assert!(
        matches!(error, OrchestratorError::Dispatch { ref cause, .. } if cause.contains("compaction"))
    );

    // The running turn is itself a maintenance turn; a normal steer queues.
    let mut maintenance_turn = active_projection();
    maintenance_turn.messages[0].text = "/compact".into();
    let mut command = message_command("Continue with the parser", "message:continue");
    command.dispatch_mode = DispatchMode::SteerActive {
        target_run_id: "run:a".into(),
    };
    let decision = decide(
        Some(&maintenance_turn),
        &Command::MessageDispatch(command),
        NOW,
    )
    .expect("decide");
    assert!(
        !decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "provider-turn.steer")
    );
    let created = decision.events.iter().find_map(|event| match &event.kind {
        EventKind::RunCreated(run) => Some(run),
        _ => None,
    });
    assert_eq!(created.expect("queued run").status, RunStatus::Queued);
}

#[test]
fn fresh_automation_queues_and_keeps_attribution_and_thread_selection() {
    let projection = active_projection();
    let mut command = message_command("Run the audit", "message:automation");
    command.created_by = Actor::Agent;
    command.creation_source = CreationSource::Mcp;
    command.scheduled_task_id = Some("scheduled-task:queued".into());
    command.sender_thread_id = Some("thread:agent-sender".into());
    command.delivery_intent = Some(DeliveryIntent::Steer);
    command.fresh_context = true;

    let decision =
        decide(Some(&projection), &Command::MessageDispatch(command), NOW).expect("decide");
    let created = decision
        .events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::RunCreated(run) => Some(run),
            _ => None,
        })
        .expect("queued run");
    assert_eq!(created.status, RunStatus::Queued);
    assert_eq!(created.model_selection, projection.thread.model_selection);
    assert!(created.fresh_context);
    let message = decision
        .events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::MessageUpdated(message) => Some(message),
            _ => None,
        })
        .expect("message");
    assert_eq!(
        message.scheduled_task_id.as_deref(),
        Some("scheduled-task:queued")
    );
    assert_eq!(
        message.sender_thread_id.as_deref(),
        Some("thread:agent-sender")
    );
    assert!(message.follows_thread_selection);
    assert_eq!(message.text, "Run the audit");
}

#[test]
fn an_agents_message_below_its_launch_limit_is_refused() {
    let mut store = MemoryStore::new();
    dispatch(
        &mut store,
        &create_command("thread:a", "command:create"),
        NOW,
    )
    .expect("create");

    let mut command = message_command("Run this", "message:limited");
    command.created_by = Actor::Agent;
    command.creation_source = CreationSource::Mcp;
    command.mode_limit = Some(DispatchModeLimit {
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Plan,
    });
    let error = dispatch(&mut store, &Command::MessageDispatch(command), NOW).expect_err("refused");
    assert!(matches!(error, OrchestratorError::ModeLimitExceeded { .. }));
    let projection = store.projection_snapshot("thread:a").expect("thread");
    assert!(projection.runs.is_empty());
    assert!(projection.messages.is_empty());
}

#[test]
fn restart_marks_the_running_attempt_superseded_and_queues_restart_effect() {
    let projection = active_projection();
    let mut command = message_command("look at this", "message:restart");
    command.attachments.push(Attachment {
        id: "image".into(),
        name: "screenshot.png".into(),
        mime_type: "image/png".into(),
        size_bytes: 4,
    });
    command.dispatch_mode = DispatchMode::RestartActive {
        target_run_id: "run:a".into(),
    };

    let decision =
        decide(Some(&projection), &Command::MessageDispatch(command), NOW).expect("decide");
    assert!(
        decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "provider-turn.restart")
    );
    assert!(
        !decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "provider-turn.steer")
    );
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RunAttemptUpdated(attempt)
                if attempt.status == RunAttemptStatus::Superseded
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RunAttemptCreated(attempt)
                if attempt.reason == RunAttemptReason::SteeringRestart
                    && attempt.status == RunAttemptStatus::Running
        )
    }));
}

#[test]
fn thread_stop_interrupts_the_running_turn_and_holds_the_queue() {
    let mut projection = active_projection();
    add_queued_run(&mut projection, "run:b", 2);
    let command = Command::ThreadStop(ThreadStop {
        command_id: "command:stop".into(),
        thread_id: "thread:a".into(),
        reason: Some("user stop".into()),
        stop_delegated_tasks: false,
    });

    let decision = decide(Some(&projection), &command, NOW).expect("decide");
    assert!(
        decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "provider-turn.interrupt")
    );
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::TurnItemUpdated(item)
                if matches!(item.kind, TurnItemKind::RunInterruptRequest { .. })
                    && item.run_id.as_deref() == Some("run:a")
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RunUpdated(run) if run.id == "run:b" && run.queue_held
        )
    }));
}

#[test]
fn interrupting_a_preparing_run_settles_it_without_a_provider_turn() {
    let mut projection = active_projection();
    projection.provider_turns.clear();
    projection.runs[0].status = RunStatus::Starting;
    projection.attempts[0].status = RunAttemptStatus::Pending;
    let command = Command::RunInterrupt(RunInterrupt {
        command_id: "command:interrupt".into(),
        thread_id: "thread:a".into(),
        run_id: "run:a".into(),
        reason: None,
        hold_queue: true,
    });

    let decision = decide(Some(&projection), &command, NOW).expect("decide");
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RunUpdated(run) if run.status == RunStatus::Interrupted
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::TurnItemUpdated(item)
                if matches!(item.kind, TurnItemKind::RunInterruptResult { .. })
        )
    }));
    assert!(decision.effects.is_empty());
    let cancelled = decision.cancel_unsettled.expect("cancel start effects");
    assert_eq!(
        cancelled.effect_types,
        vec![
            "provider-turn.start".to_owned(),
            "provider-turn.restart".to_owned()
        ]
    );
}

#[test]
fn approval_requested_then_answered_emits_the_answer_effect() {
    let mut projection = active_projection();
    let requested = plan_approval_requested(
        &mut projection,
        ApprovalRequested {
            request_id: "request:1".into(),
            run_id: "run:a".into(),
            node_id: "root:a".into(),
            provider_turn_id: Some("turn:a".into()),
            provider_instance_id: "codex".into(),
            kind: RequestKind::Command,
            prompt: Some("Run command".into()),
            event_id: "event:approval".into(),
        },
        NOW,
    )
    .expect("approval");
    assert!(requested.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RuntimeRequestUpdated(request)
                if request.status == RequestStatus::Pending
        )
    }));
    assert_eq!(
        projection
            .runtime_request("request:1")
            .expect("request")
            .status,
        RequestStatus::Pending
    );

    let answer = Command::RuntimeRequestRespond(RuntimeRequestRespond {
        command_id: "command:answer".into(),
        thread_id: "thread:a".into(),
        request_id: "request:1".into(),
        decision: Some(ApprovalDecision::Accept),
        feedback: None,
    });
    let decision = decide(Some(&projection), &answer, NOW).expect("answer");
    assert!(
        decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "runtime-request.respond")
    );
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RuntimeRequestUpdated(request)
                if request.status == RequestStatus::Resolved
                    && request.decision == Some(ApprovalDecision::Accept)
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::TurnItemUpdated(item)
                if matches!(item.kind, TurnItemKind::ApprovalRequest { .. })
                    && item.status == TurnItemStatus::Completed
        )
    }));

    let mut after = projection;
    for event in &decision.events {
        after.apply(event);
    }
    assert!(matches!(
        decide(Some(&after), &answer, NOW + 1),
        Err(OrchestratorError::RequestNotPending(_))
    ));
}

#[test]
fn model_selection_update_applies_to_a_live_turn() {
    let projection = active_projection();
    let command = Command::ThreadModelSelectionSet(ThreadModelSelectionSet {
        command_id: "command:model".into(),
        thread_id: "thread:a".into(),
        model_selection: ModelSelection::new("codex", "gpt-6"),
    });
    let decision = decide(Some(&projection), &command, NOW).expect("decide");
    assert!(event_types(&decision).contains(&"thread.model-selection-updated"));
    assert!(
        decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "provider-session.apply-model-selection")
    );
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::ThreadModelSelectionUpdated(thread)
                if thread.model_selection.model == "gpt-6"
        )
    }));
}

#[test]
fn run_completion_starts_the_next_queued_run() {
    let mut projection = active_projection();
    add_queued_run(&mut projection, "run:b", 2);
    let decision =
        plan_run_finished(&mut projection, "run:a", RunStatus::Completed, NOW).expect("finish");
    assert_eq!(
        projection.run("run:a").expect("a").status,
        RunStatus::Completed
    );
    assert_eq!(
        projection.run("run:b").expect("b").status,
        RunStatus::Starting
    );
    assert!(decision.effects.iter().any(|effect| {
        matches!(
            &effect.request,
            EffectRequest::ProviderTurnStart { run_id } if run_id == "run:b"
        )
    }));
}

#[test]
fn the_first_message_arms_title_generation() {
    let projection = Projection::new(test_thread());
    let mut command = message_command("Fix the failing parser", "message:title");
    command.title_seed = Some("Failing parser".into());
    let decision =
        decide(Some(&projection), &Command::MessageDispatch(command), NOW).expect("decide");
    assert!(
        decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "thread-title.generate")
    );
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::ThreadMetadataUpdated(thread)
                if thread.title_regeneration.is_some() && thread.title == "Failing parser"
        )
    }));

    let mut maintenance = message_command(" /COMPACT ", "message:maintenance");
    maintenance.title_seed = Some("Ignored".into());
    let decision = decide(
        Some(&projection),
        &Command::MessageDispatch(maintenance),
        NOW,
    )
    .expect("decide");
    assert!(
        !decision
            .effects
            .iter()
            .any(|effect| effect.request.type_name() == "thread-title.generate")
    );
}

#[test]
fn a_provider_turn_start_moves_the_run_to_running() {
    let mut projection = active_projection();
    projection.runs[0].status = RunStatus::Starting;
    projection.runs[0].started_at = None;
    projection.attempts[0].status = RunAttemptStatus::Pending;
    projection.nodes[0].status = NodeStatus::Pending;
    let decision = plan_provider_turn_started(
        &mut projection,
        "run:a",
        ProviderTurn {
            id: "turn:a".into(),
            provider_thread_id: "provider-thread:a".into(),
            run_attempt_id: "attempt:a".into(),
            status: ProviderTurnStatus::Running,
            created_at: NOW,
            updated_at: NOW,
        },
        NOW,
    )
    .expect("started");
    assert_eq!(
        projection.run("run:a").expect("run").status,
        RunStatus::Running
    );
    assert_eq!(projection.run("run:a").expect("run").started_at, Some(NOW));
    assert_eq!(
        projection.provider_turn("turn:a").expect("turn").status,
        ProviderTurnStatus::Running
    );
    assert!(event_types(&decision).contains(&"provider-turn.updated"));
}

#[test]
fn queued_run_starts_with_the_effect() {
    let mut projection = Projection::new(test_thread());
    projection.provider_threads.push(ProviderThread {
        id: "provider-thread:a".into(),
        driver: "codex".into(),
        provider_instance_id: "codex".into(),
        provider_session_id: Some("session:a".into()),
        app_thread_id: "thread:a".into(),
        native_thread_ref: Some("native:a".into()),
        status: ProviderThreadStatus::Active,
        first_run_ordinal: Some(1),
        last_run_ordinal: None,
        created_at: 1,
        updated_at: 1,
    });
    add_queued_run(&mut projection, "run:b", 1);
    let decision = plan_start_next_queued_run(&mut projection, None, NOW).expect("start");
    assert_eq!(
        projection.run("run:b").expect("run").status,
        RunStatus::Starting
    );
    assert_eq!(projection.run("run:b").expect("run").queue_position, None);
    assert!(decision.effects.iter().any(|effect| {
        matches!(
            &effect.request,
            EffectRequest::ProviderTurnStart { run_id } if run_id == "run:b"
        )
    }));
}

#[test]
fn send_now_racing_a_running_turn_keeps_the_message_queued() {
    // T3 `SteeringCompletion.integration.test.ts`: a send-now that races the
    // running turn fails without starting or changing the queued runs.
    let mut projection = active_projection();
    add_queued_run(&mut projection, "run:b", 2);
    let before = projection.runs.clone();
    let command = Command::QueuedRunStart {
        command_id: "command:send-now".into(),
        thread_id: "thread:a".into(),
        run_id: "run:b".into(),
    };
    let decision = decide(Some(&projection), &command, NOW).expect("decide");
    assert!(decision.is_empty());
    assert_eq!(projection.runs, before);
}

#[test]
fn a_user_send_releases_a_held_queue() {
    // T3 `SteeringCompletion.integration.test.ts`: a plain send behind the
    // stopped queue releases its hold.
    let mut projection = active_projection();
    add_queued_run(&mut projection, "run:b", 2);
    projection.runs[1].queue_held = true;
    let command = Command::MessageDispatch(message_command("send again", "message:release"));
    let decision = decide(Some(&projection), &command, NOW).expect("decide");
    let mut after = projection;
    for event in &decision.events {
        after.apply(event);
    }
    assert!(
        !after
            .runs
            .iter()
            .any(|run| run.id == "run:b" && run.queue_held)
    );
    assert!(decision.events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::RunUpdated(run) if run.id == "run:b" && !run.queue_held
        )
    }));
    let new_run = after
        .runs
        .iter()
        .find(|run| run.id != "run:a" && run.id != "run:b")
        .expect("new run");
    assert!(!new_run.queue_held);
}

#[test]
fn empty_commands_still_record_a_receipt() {
    let mut store = MemoryStore::new();
    dispatch(
        &mut store,
        &create_command("thread:a", "command:create"),
        NOW,
    )
    .expect("create");
    let command = Command::ThreadStop(ThreadStop {
        command_id: "command:stop".into(),
        thread_id: "thread:a".into(),
        reason: None,
        stop_delegated_tasks: false,
    });
    let result = dispatch(&mut store, &command, NOW).expect("stop");
    assert!(result.stored_events.is_empty());
    assert_eq!(
        store.receipt("command:stop").expect("receipt").status,
        ReceiptStatus::Accepted
    );
}
