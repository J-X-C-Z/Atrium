// Ported test expectations from apps/server/src/orchestration-v2/{Orchestrator.receipt-race,ThreadLaunchService,SteeringCompletion.integration,RuntimePolicy,QueuedRunOrder,CommandPolicy,ThreadStop,RunFinalizationService,ProviderSwitchService,ProviderSessionTransitionPolicy,SelectionRestart.integration,SubagentProjection}.test.ts (T3 Code v0.1.11, MIT).
//
// The cases are the pure ones: provider processes become projections and
// outbox effects, and the harness is a `MemoryStore`.

use crate::orchestration::event_sink::{MemoryStore, ReceiptStatus};
use crate::orchestration::orchestrator::{
    ApprovalRequested, decide, dispatch, dispatch_with_limit, plan_approval_requested,
    plan_provider_turn_started, plan_run_finished, plan_start_next_queued_run,
    resolve_dispatch_intent,
};
use crate::orchestration::types::*;

const NOW: Timestamp = 1_000;

fn at(timestamp: Timestamp) -> String {
    iso(timestamp)
}

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
        created_at: at(1),
        updated_at: at(1),
        last_visited_at: Some(at(1)),
        ..Thread::default()
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
            active_steering_interrupts_tools: Some(false),
            active_steering_accepts_attachments: Some(true),
            supports_steering_by_interrupt_restart,
            supports_queued_messages,
            ..TurnCapabilities::default()
        },
        ..ProviderCapabilities::default()
    }
}

fn user_message(id: &str, run_id: Option<&str>, text: &str) -> Message {
    Message {
        id: id.into(),
        thread_id: "thread:a".into(),
        run_id: run_id.map(RunId::new),
        node_id: Some("root:a".into()),
        role: MessageRole::User,
        text: text.into(),
        created_at: at(1),
        updated_at: at(1),
        ..Message::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn user_turn_item(
    message: &Message,
    run_id: &str,
    node_id: &str,
    provider_thread_id: &str,
    provider_turn_id: Option<&str>,
    input_intent: InputIntent,
    ordinal: u32,
) -> TurnItem {
    TurnItem::UserMessage(UserMessageTurnItem {
        base: TurnItemBase {
            id: TurnItemId::new(format!("turn-item:{}", message.id)),
            thread_id: message.thread_id.clone(),
            run_id: Some(RunId::new(run_id)),
            node_id: Some(NodeId::new(node_id)),
            provider_thread_id: Some(ProviderThreadId::new(provider_thread_id)),
            provider_turn_id: provider_turn_id.map(ProviderTurnId::new),
            parent_item_id: None,
            ordinal,
            status: TurnItemStatus::Completed,
            title: None,
            started_at: Some(at(NOW)),
            completed_at: Some(at(NOW)),
            updated_at: at(NOW),
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

fn active_projection() -> Projection {
    let mut projection = Projection::new(test_thread());
    projection.provider_threads.push(ProviderThread {
        id: "provider-thread:a".into(),
        driver: "codex".into(),
        provider_instance_id: "codex".into(),
        provider_session_id: Some("session:a".into()),
        app_thread_id: Some("thread:a".into()),
        native_thread_ref: None,
        status: ProviderThreadStatus::Active,
        first_run_ordinal: Some(1),
        last_run_ordinal: Some(1),
        created_at: at(1),
        updated_at: at(1),
        ..ProviderThread::default()
    });
    projection.provider_sessions.push(ProviderSession {
        id: "session:a".into(),
        driver: "codex".into(),
        provider_instance_id: "codex".into(),
        status: crate::contracts::ProviderSessionStatus::Ready,
        cwd: "/work".into(),
        model: Some("gpt-5.1-codex".into()),
        capabilities: caps(true, true, true),
        created_at: at(1),
        updated_at: at(1),
        ..ProviderSession::default()
    });
    projection.provider_turns.push(ProviderTurn {
        id: "turn:a".into(),
        provider_thread_id: "provider-thread:a".into(),
        node_id: "root:a".into(),
        run_attempt_id: Some("attempt:a".into()),
        ordinal: 1,
        status: ProviderTurnStatus::Running,
        started_at: Some(at(1)),
        ..ProviderTurn::default()
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
        queue_held: Some(false),
        steer_when_ready_run_id: None,
        requested_at: at(1),
        started_at: Some(at(1)),
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: Some(false),
        ..Run::default()
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
        started_at: Some(at(1)),
        completed_at: None,
        ..RunAttempt::default()
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
        started_at: Some(at(1)),
        completed_at: None,
        ..ExecutionNode::default()
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
        user_message_id: message_id.into(),
        root_node_id: Some(NodeId::new(node_id.clone())),
        active_attempt_id: Some(RunAttemptId::new(format!("attempt:{run_id}"))),
        status: RunStatus::Queued,
        queue_position: Some(ordinal),
        queue_held: Some(false),
        steer_when_ready_run_id: None,
        requested_at: at(1),
        started_at: None,
        completed_at: None,
        restart_continuation_of_run_id: None,
        manual_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        fresh_context: Some(false),
        ..Run::default()
    });
    projection.attempts.push(RunAttempt {
        id: format!("attempt:{run_id}").into(),
        run_id: run_id.into(),
        attempt_ordinal: 1,
        root_node_id: node_id.clone().into(),
        provider_instance_id: "codex".into(),
        provider_thread_id: "provider-thread:a".into(),
        provider_turn_id: None,
        reason: RunAttemptReason::Initial,
        status: RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
        ..RunAttempt::default()
    });
    projection.nodes.push(ExecutionNode {
        id: node_id.clone().into(),
        thread_id: "thread:a".into(),
        run_id: Some(run_id.into()),
        parent_node_id: None,
        root_node_id: node_id.clone().into(),
        kind: NodeKind::RootTurn,
        status: NodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some("provider-thread:a".into()),
        provider_turn_id: None,
        runtime_request_id: None,
        started_at: None,
        completed_at: None,
        ..ExecutionNode::default()
    });
    projection.turn_items.push(user_turn_item(
        &message,
        run_id,
        &node_id,
        "provider-thread:a",
        None,
        InputIntent::QueuedTurn,
        ordinal,
    ));
}

fn create_command(thread_id: &str, command_id: &str) -> Command {
    Command::ThreadCreate(ThreadCreate {
        created_by: Actor::User,
        creation_source: CreationSource::Web,
        command_id: command_id.into(),
        thread_id: thread_id.into(),
        project_id: "project:test".into(),
        title: "Thread".into(),
        model_selection: model(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        branch: None,
        worktree_path: None,
        link_origin: None,
        delegated_from: None,
        imported_native_thread: None,
        initial_message: None,
    })
}

fn message_command(text: &str, message_id: &str) -> MessageDispatch {
    MessageDispatch::new("command:message", "thread:a", message_id, text)
}

fn event_types(decision: &crate::orchestration::orchestrator::Decision) -> Vec<&str> {
    decision.events.iter().map(DomainEvent::type_name).collect()
}

fn user_message_item(
    decision: &crate::orchestration::orchestrator::Decision,
) -> Option<&UserMessageTurnItem> {
    decision.events.iter().find_map(|event| match event {
        DomainEvent::TurnItemUpdated(payload) => match &payload.payload {
            TurnItem::UserMessage(item) => Some(item),
            _ => None,
        },
        _ => None,
    })
}

fn run_created(decision: &crate::orchestration::orchestrator::Decision) -> Option<&Run> {
    decision.events.iter().find_map(|event| match event {
        DomainEvent::RunCreated(payload) => Some(&payload.payload),
        _ => None,
    })
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
        context: None,
        title_seed: Some("Hello".into()),
        scheduled_task_id: None,
        sender_thread_id: None,
        workspace_strategy: None,
    });

    let result = dispatch(&mut store, &Command::ThreadCreate(create), NOW).expect("commit");
    let types: Vec<&str> = result
        .stored_events
        .iter()
        .map(|stored| stored.event.type_name())
        .collect();
    assert_eq!(types[0], "thread.created");
    assert!(types.contains(&"run.created"));
    assert!(types.contains(&"message.updated"));
    assert_eq!(
        result.stored_events[0]
            .command_id
            .as_ref()
            .map(CommandId::as_str),
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
    let limit = DispatchModeLimit {
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Default,
    };
    create.initial_message = Some(InitialMessage {
        message_id: "message:first".into(),
        text: "hello".into(),
        attachments: Vec::new(),
        context: None,
        title_seed: Some("Hello".into()),
        scheduled_task_id: None,
        sender_thread_id: None,
        workspace_strategy: None,
    });

    let error = dispatch_with_limit(
        &mut store,
        &Command::ThreadCreate(create),
        Some(&limit),
        NOW,
    )
    .expect_err("mode limit");
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
        resolve_dispatch_intent(&idle, DispatchMode::StartImmediately {}, None),
        DispatchMode::StartImmediately {}
    );

    let active = active_projection();
    assert_eq!(
        resolve_dispatch_intent(
            &active,
            DispatchMode::StartImmediately {},
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
            DispatchMode::StartImmediately {},
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::QueueAfterActive {}
    );

    let mut restart = active.clone();
    restart.provider_sessions[0].capabilities = caps(false, false, true);
    assert_eq!(
        resolve_dispatch_intent(
            &restart,
            DispatchMode::StartImmediately {},
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
            DispatchMode::StartImmediately {},
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::QueueAfterActive {}
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
    let item = user_message_item(&decision).expect("user item");
    assert_eq!(item.input_intent, InputIntent::Steer);
}

#[test]
fn maintenance_commands_never_steer() {
    // T3 `ThreadLaunchService.test.ts`: a maintenance command sent with an
    // explicit steer is refused, and a normal message steered into a
    // maintenance turn queues instead.
    let projection = active_projection();
    let mut incoming = message_command(" /COMPACT ", "message:compact");
    incoming.dispatch_mode = Some(DispatchMode::SteerActive {
        target_run_id: "run:a".into(),
    });
    let error = decide(Some(&projection), &Command::MessageDispatch(incoming), NOW)
        .expect_err("maintenance cannot steer");
    assert!(
        matches!(error, OrchestratorError::Dispatch { ref cause, .. } if cause.contains("compaction"))
    );

    // The running turn is itself a maintenance turn; a normal steer queues.
    let mut maintenance_turn = active_projection();
    maintenance_turn.messages[0].text = "/compact".into();
    let mut command = message_command("Continue with the parser", "message:continue");
    command.dispatch_mode = Some(DispatchMode::SteerActive {
        target_run_id: "run:a".into(),
    });
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
    let created = run_created(&decision).expect("queued run");
    assert_eq!(created.status, RunStatus::Queued);
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
    command.fresh_context = Some(true);

    let decision =
        decide(Some(&projection), &Command::MessageDispatch(command), NOW).expect("decide");
    let created = run_created(&decision).expect("queued run");
    assert_eq!(created.status, RunStatus::Queued);
    assert_eq!(created.model_selection, projection.thread.model_selection);
    assert_eq!(created.fresh_context, Some(true));
    let message = decision
        .events
        .iter()
        .find_map(|event| match event {
            DomainEvent::MessageUpdated(payload) => Some(&payload.payload),
            _ => None,
        })
        .expect("message");
    assert_eq!(
        message
            .scheduled_task_id
            .as_ref()
            .map(ScheduledTaskId::as_str),
        Some("scheduled-task:queued")
    );
    assert_eq!(
        message.sender_thread_id.as_ref().map(ThreadId::as_str),
        Some("thread:agent-sender")
    );
    assert_eq!(message.follows_thread_selection, Some(true));
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
    let limit = DispatchModeLimit {
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Plan,
    };
    let error = dispatch_with_limit(
        &mut store,
        &Command::MessageDispatch(command),
        Some(&limit),
        NOW,
    )
    .expect_err("refused");
    assert!(matches!(error, OrchestratorError::ModeLimitExceeded { .. }));
    let projection = store.projection_snapshot("thread:a").expect("thread");
    assert!(projection.runs.is_empty());
    assert!(projection.messages.is_empty());
}

#[test]
fn restart_marks_the_running_attempt_superseded_and_queues_restart_effect() {
    let projection = active_projection();
    let mut command = message_command("look at this", "message:restart");
    command
        .attachments
        .push(crate::contracts::ChatAttachment::File(
            crate::contracts::ChatFileAttachment {
                id: "image".into(),
                name: "screenshot.png".into(),
                mime_type: "image/png".into(),
                size_bytes: 4,
                source: None,
            },
        ));
    command.dispatch_mode = Some(DispatchMode::RestartActive {
        target_run_id: "run:a".into(),
    });

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
            event,
            DomainEvent::RunAttemptUpdated(payload)
                if payload.payload.status == RunAttemptStatus::Superseded
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            event,
            DomainEvent::RunAttemptCreated(payload)
                if payload.payload.reason == RunAttemptReason::SteeringRestart
                    && payload.payload.status == RunAttemptStatus::Running
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
        stop_delegated_tasks: Some(false),
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
            event,
            DomainEvent::TurnItemUpdated(payload)
                if matches!(payload.payload, TurnItem::RunInterruptRequest(_))
                    && payload.payload.base().and_then(|base| base.run_id.as_ref())
                        .is_some_and(|id| id.as_str() == "run:a")
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            event,
            DomainEvent::RunUpdated(payload)
                if payload.payload.id == "run:b" && payload.payload.queue_held == Some(true)
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
        hold_queue: Some(true),
    });

    let decision = decide(Some(&projection), &command, NOW).expect("decide");
    assert!(decision.events.iter().any(|event| {
        matches!(
            event,
            DomainEvent::RunUpdated(payload) if payload.payload.status == RunStatus::Interrupted
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            event,
            DomainEvent::TurnItemUpdated(payload)
                if matches!(payload.payload, TurnItem::RunInterruptResult(_))
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
            event,
            DomainEvent::RuntimeRequestUpdated(payload)
                if payload.payload.status == RequestStatus::Pending
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
        answers: None,
        attachments_by_question_id: None,
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
            event,
            DomainEvent::RuntimeRequestUpdated(payload)
                if payload.payload.status == RequestStatus::Resolved
                    && payload.payload.decision == Some(ApprovalDecision::Accept)
        )
    }));
    assert!(decision.events.iter().any(|event| {
        matches!(
            event,
            DomainEvent::TurnItemUpdated(payload)
                if matches!(payload.payload, TurnItem::ApprovalRequest(_))
                    && payload.payload.base().is_some_and(|base| base.status == TurnItemStatus::Completed)
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
            event,
            DomainEvent::ThreadMetadata { event, .. }
                if event.payload.model_selection.model == "gpt-6"
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
            EffectRequest::ProviderTurnStart { run_id } if run_id.as_str() == "run:b"
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
            event,
            DomainEvent::ThreadMetadata { event, .. }
                if event.payload.title_regeneration.is_some() && event.payload.title == "Failing parser"
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
            node_id: "root:a".into(),
            run_attempt_id: Some("attempt:a".into()),
            ordinal: 1,
            status: ProviderTurnStatus::Running,
            started_at: Some(at(NOW)),
            ..ProviderTurn::default()
        },
        NOW,
    )
    .expect("started");
    assert_eq!(
        projection.run("run:a").expect("run").status,
        RunStatus::Running
    );
    assert_eq!(
        projection.run("run:a").expect("run").started_at,
        Some(at(NOW))
    );
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
        app_thread_id: Some("thread:a".into()),
        native_thread_ref: None,
        status: ProviderThreadStatus::Active,
        first_run_ordinal: Some(1),
        last_run_ordinal: None,
        created_at: at(1),
        updated_at: at(1),
        ..ProviderThread::default()
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
            EffectRequest::ProviderTurnStart { run_id } if run_id.as_str() == "run:b"
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
    projection.runs[1].queue_held = Some(true);
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
            .any(|run| run.id == "run:b" && run.queue_held.unwrap_or(false))
    );
    assert!(decision.events.iter().any(|event| {
        matches!(
            event,
            DomainEvent::RunUpdated(payload)
                if payload.payload.id == "run:b" && payload.payload.queue_held == Some(false)
        )
    }));
    let new_run = after
        .runs
        .iter()
        .find(|run| run.id.as_str() != "run:a" && run.id.as_str() != "run:b")
        .expect("new run");
    assert_ne!(new_run.queue_held, Some(true));
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
        stop_delegated_tasks: Some(false),
    });
    let result = dispatch(&mut store, &command, NOW).expect("stop");
    assert!(result.stored_events.is_empty());
    assert_eq!(
        store.receipt("command:stop").expect("receipt").status,
        ReceiptStatus::Accepted
    );
}

// ---------------------------------------------------------------------------
// Item 25: queue editing, delivery, terminal settling
// ---------------------------------------------------------------------------

use crate::orchestration::event_sink::Store;
use crate::orchestration::orchestrator::{
    SendDeliverySetting, resolve_message_delivery, resolve_send_delivery,
};
use crate::store::EventStore;

fn queued_projection() -> Projection {
    active_projection()
}

#[test]
fn queued_runs_are_persisted_edited_reordered_and_cancelled() {
    // T3 `Orchestrator` queued-run.edit / reorder / cancel.
    let mut store = EventStore::in_memory().expect("store");
    dispatch(
        &mut store,
        &create_command("thread:a", "command:create"),
        NOW,
    )
    .expect("create");
    for (command_id, message_id) in [
        ("command:first", "message:first"),
        ("command:second", "message:second"),
        ("command:third", "message:third"),
    ] {
        let mut command = message_command(message_id, message_id);
        command.command_id = command_id.into();
        dispatch(&mut store, &Command::MessageDispatch(command), NOW).expect("send");
    }

    let projection = store.projection(&"thread:a".into()).expect("projection");
    let run_second = projection
        .runs
        .iter()
        .find(|run| run.user_message_id == "message:second")
        .expect("second queued")
        .id
        .clone();
    let run_third = projection
        .runs
        .iter()
        .find(|run| run.user_message_id == "message:third")
        .expect("third queued")
        .id
        .clone();

    // Edit replaces the text and attachments of the queued message.
    let attachment = Attachment::File(crate::contracts::ChatFileAttachment {
        id: "notes".into(),
        name: "notes.txt".into(),
        mime_type: "text/plain".into(),
        size_bytes: 4,
        source: None,
    });
    dispatch(
        &mut store,
        &Command::QueuedRunEdit {
            command_id: "command:edit".into(),
            thread_id: "thread:a".into(),
            run_id: run_second.clone(),
            text: "edited text".into(),
            attachments: Some(vec![attachment.clone()]),
        },
        NOW,
    )
    .expect("edit");
    let projection = store.projection(&"thread:a".into()).expect("projection");
    let message = projection.message("message:second").expect("message");
    assert_eq!(message.text, "edited text");
    assert_eq!(message.attachments, vec![attachment]);

    // Reorder moves the third run ahead of the second.
    dispatch(
        &mut store,
        &Command::QueuedRunReorder {
            command_id: "command:reorder".into(),
            thread_id: "thread:a".into(),
            run_id: run_third.clone(),
            before_run_id: Some(run_second.clone()),
        },
        NOW,
    )
    .expect("reorder");
    let projection = store.projection(&"thread:a".into()).expect("projection");
    assert_eq!(projection.run(&run_third).unwrap().queue_position, Some(1));
    assert_eq!(projection.run(&run_second).unwrap().queue_position, Some(2));

    // Cancel settles the queued run and its message-side work.
    dispatch(
        &mut store,
        &Command::QueuedRunCancel {
            command_id: "command:cancel".into(),
            thread_id: "thread:a".into(),
            run_id: run_third.clone(),
        },
        NOW,
    )
    .expect("cancel");
    let projection = store.projection(&"thread:a".into()).expect("projection");
    assert_eq!(
        projection.run(&run_third).unwrap().status,
        RunStatus::Cancelled
    );
    assert_eq!(projection.run(&run_third).unwrap().queue_position, None);
}

#[test]
fn an_empty_queued_run_edit_is_refused() {
    let mut store = EventStore::in_memory().expect("store");
    dispatch(
        &mut store,
        &create_command("thread:a", "command:create"),
        NOW,
    )
    .expect("create");
    let mut first = message_command("first", "message:first");
    first.command_id = "command:first".into();
    dispatch(&mut store, &Command::MessageDispatch(first), NOW).expect("send");
    let mut second = message_command("second", "message:second");
    second.command_id = "command:second".into();
    dispatch(&mut store, &Command::MessageDispatch(second), NOW).expect("send");
    let projection = store.projection(&"thread:a".into()).expect("projection");
    let queued = projection
        .runs
        .iter()
        .find(|run| run.user_message_id == "message:second")
        .expect("queued")
        .id
        .clone();
    let error = dispatch(
        &mut store,
        &Command::QueuedRunEdit {
            command_id: "command:edit-empty".into(),
            thread_id: "thread:a".into(),
            run_id: queued,
            text: "   ".into(),
            attachments: None,
        },
        NOW,
    )
    .expect_err("empty edit");
    assert!(matches!(error, OrchestratorError::Dispatch { .. }));
}

#[test]
fn every_terminal_path_settles_open_tools_subagents_and_requests() {
    // T3 `RunFinalizationService.test.ts` and `subagentProjection.ts`: no open
    // work survives its run.
    let mut projection = queued_projection();
    let tool_item = TurnItem::CommandExecution(crate::contracts::CommandExecutionTurnItem {
        base: TurnItemBase {
            id: "turn-item:tool".into(),
            thread_id: "thread:a".into(),
            run_id: Some("run:a".into()),
            node_id: Some("root:a".into()),
            ordinal: 9,
            status: TurnItemStatus::Running,
            updated_at: at(NOW),
            ..TurnItemBase::default()
        },
        input: "cargo test".into(),
        output: None,
        output_omitted: None,
        output_indicates_failure: None,
        exit_code: None,
        monitor: None,
        completion_wake: None,
    });
    projection.turn_items.push(tool_item);
    projection.nodes.push(ExecutionNode {
        id: "node:tool".into(),
        thread_id: "thread:a".into(),
        run_id: Some("run:a".into()),
        parent_node_id: Some("root:a".into()),
        root_node_id: "root:a".into(),
        kind: NodeKind::ToolCall,
        status: NodeStatus::Running,
        counts_for_run: true,
        provider_thread_id: Some("provider-thread:a".into()),
        provider_turn_id: Some("turn:a".into()),
        runtime_request_id: None,
        started_at: Some(at(1)),
        completed_at: None,
        ..ExecutionNode::default()
    });
    projection.runtime_requests.push(RuntimeRequest {
        id: "request:open".into(),
        node_id: "root:a".into(),
        provider_turn_id: Some("turn:a".into()),
        native_request_ref: None,
        kind: RequestKind::Command,
        status: RequestStatus::Pending,
        response_capability: ResponseCapability::Message {},
        created_at: at(1),
        resolved_at: None,
        decision: None,
        feedback: None,
        answers: None,
    });
    projection.subagents.push(crate::contracts::Subagent {
        id: "node:subagent".into(),
        thread_id: "thread:a".into(),
        run_id: Some("run:a".into()),
        parent_node_id: "root:a".into(),
        origin: crate::contracts::SubagentOrigin::ProviderNative,
        provider_instance_id: "codex".into(),
        status: crate::contracts::SubagentStatus::Running,
        updated_at: at(1),
        ..crate::contracts::Subagent::default()
    });

    let decision =
        plan_run_finished(&mut projection, "run:a", RunStatus::Completed, NOW).expect("finish");
    let mut after = queued_projection();
    after.turn_items = projection.turn_items.clone();
    after.nodes = projection.nodes.clone();
    after.runtime_requests = projection.runtime_requests.clone();
    after.subagents = projection.subagents.clone();
    for event in &decision.events {
        after.apply(event);
    }
    assert_eq!(
        after
            .turn_item("turn-item:tool")
            .unwrap()
            .base()
            .unwrap()
            .status,
        TurnItemStatus::Completed
    );
    assert_eq!(
        after.node("node:tool").unwrap().status,
        NodeStatus::Completed
    );
    assert_eq!(
        after.runtime_request("request:open").unwrap().status,
        RequestStatus::Cancelled
    );
    assert_eq!(
        after.subagents[0].status,
        crate::contracts::SubagentStatus::Completed
    );
}

#[test]
fn per_send_delivery_follows_the_setting_and_the_modifier() {
    // T3 composer setting: the modifier asks for the other delivery.
    assert_eq!(
        resolve_send_delivery(SendDeliverySetting::Auto, false),
        DeliveryIntent::Auto
    );
    assert_eq!(
        resolve_send_delivery(SendDeliverySetting::Auto, true),
        DeliveryIntent::Steer
    );
    assert_eq!(
        resolve_send_delivery(SendDeliverySetting::Steer, true),
        DeliveryIntent::Auto
    );
    assert_eq!(
        resolve_send_delivery(SendDeliverySetting::Restart, false),
        DeliveryIntent::Restart
    );
}

#[test]
fn an_agent_steer_without_steering_capability_falls_back_to_capability_delivery() {
    let projection = queued_projection();
    assert_eq!(
        resolve_message_delivery(&projection, SendDeliverySetting::Steer, false, Actor::User),
        DeliveryIntent::Steer
    );
    let mut no_steering = projection;
    no_steering.provider_sessions[0].capabilities = caps(false, true, true);
    assert_eq!(
        resolve_message_delivery(
            &no_steering,
            SendDeliverySetting::Steer,
            false,
            Actor::Agent
        ),
        DeliveryIntent::Auto
    );
    assert_eq!(
        resolve_dispatch_intent(
            &no_steering,
            DispatchMode::StartImmediately {},
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::QueueAfterActive {}
    );
}

#[test]
fn an_agent_send_after_stop_does_not_release_the_held_queue() {
    // T3 `ThreadStop.test.ts`: only a user send releases the held queue.
    let mut projection = queued_projection();
    add_queued_run(&mut projection, "run:b", 2);
    projection.runs[1].queue_held = Some(true);
    projection
        .turn_items
        .push(TurnItem::RunInterruptRequest(RunInterruptRequestTurnItem {
            base: TurnItemBase {
                id: "turn-item:stop".into(),
                thread_id: "thread:a".into(),
                run_id: Some("run:a".into()),
                node_id: Some("root:a".into()),
                ordinal: 7,
                status: TurnItemStatus::Completed,
                updated_at: at(NOW),
                ..TurnItemBase::default()
            },
            message: "Stopped by user".into(),
            unresponsive_at: None,
        }));
    let mut command = message_command("agent follow-up", "message:agent");
    command.created_by = Actor::Agent;
    command.creation_source = CreationSource::Mcp;
    let decision =
        decide(Some(&projection), &Command::MessageDispatch(command), NOW).expect("decide");
    let mut after = projection;
    for event in &decision.events {
        after.apply(event);
    }
    assert_eq!(after.run("run:b").unwrap().queue_held, Some(true));
    let new_run = after
        .runs
        .iter()
        .find(|run| run.id.as_str() != "run:a" && run.id.as_str() != "run:b")
        .expect("new run");
    assert_eq!(new_run.queue_held, Some(true));
}

#[test]
fn a_repeated_stop_that_changes_nothing_is_accepted() {
    let mut store = EventStore::in_memory().expect("store");
    dispatch(
        &mut store,
        &create_command("thread:a", "command:create"),
        NOW,
    )
    .expect("create");
    let mut first_send = message_command("run", "message:run");
    first_send.command_id = "command:first".into();
    dispatch(&mut store, &Command::MessageDispatch(first_send), NOW).expect("send");
    let stop = |command_id: &str| {
        Command::ThreadStop(ThreadStop {
            command_id: command_id.into(),
            thread_id: "thread:a".into(),
            reason: None,
            stop_delegated_tasks: Some(false),
        })
    };
    let first = dispatch(&mut store, &stop("command:stop"), NOW).expect("stop");
    assert!(!first.stored_events.is_empty());
    let second = dispatch(&mut store, &stop("command:stop-again"), NOW).expect("stop again");
    assert!(second.stored_events.is_empty());
    assert_eq!(
        store
            .command_receipt(&"command:stop-again".into())
            .expect("receipt")
            .status,
        ReceiptStatus::Accepted
    );
}

#[test]
fn an_accepted_steer_replays_after_its_target_turn_completes() {
    // T3 `SteeringCompletion.integration.test.ts`: a replay returns the
    // recorded events even when the target has since completed.
    let mut store = EventStore::in_memory().expect("store");
    dispatch(
        &mut store,
        &create_command("thread:a", "command:create"),
        NOW,
    )
    .expect("create");
    let first = message_command("first", "message:first");
    dispatch(
        &mut store,
        &Command::MessageDispatch(message_command_with_id(first, "command:first")),
        NOW,
    )
    .expect("first");
    let projection = store.projection(&"thread:a".into()).expect("projection");
    assert_eq!(projection.active_run().unwrap().status, RunStatus::Starting);

    let mut steer = message_command("steer", "message:steer");
    steer.command_id = "command:steer".into();
    steer.delivery_intent = Some(DeliveryIntent::Auto);
    let accepted = dispatch(&mut store, &Command::MessageDispatch(steer.clone()), NOW)
        .expect("steer accepted or queued");
    let mut projection = store.projection(&"thread:a".into()).expect("projection");
    let run_id = projection.active_run().unwrap().id.clone();
    let finished = plan_run_finished(
        &mut projection,
        run_id.as_str(),
        RunStatus::Completed,
        NOW + 1,
    )
    .expect("finish");
    store
        .write(crate::orchestration::event_sink::WriteInput {
            command_id: None,
            events: finished.events,
            effects: finished.effects,
            now: NOW + 1,
        })
        .expect("write");
    let replay = dispatch(&mut store, &Command::MessageDispatch(steer), NOW + 2).expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.sequence, accepted.sequence);
}

fn message_command_with_id(mut command: MessageDispatch, command_id: &str) -> MessageDispatch {
    command.command_id = command_id.into();
    command
}

#[test]
fn command_policy_keeps_explicit_steer_and_restart_when_the_turn_has_not_started() {
    // T3 `CommandPolicy.test.ts`: explicit delivery intents are not narrowed by
    // a provider turn that has not started; automatic delivery queues.
    let mut projection = queued_projection();
    projection.provider_turns.clear();
    assert_eq!(
        resolve_dispatch_intent(
            &projection,
            DispatchMode::StartImmediately {},
            Some(DeliveryIntent::Steer)
        ),
        DispatchMode::SteerActive {
            target_run_id: "run:a".into()
        }
    );
    assert_eq!(
        resolve_dispatch_intent(
            &projection,
            DispatchMode::StartImmediately {},
            Some(DeliveryIntent::Restart)
        ),
        DispatchMode::RestartActive {
            target_run_id: "run:a".into()
        }
    );
    assert_eq!(
        resolve_dispatch_intent(
            &projection,
            DispatchMode::StartImmediately {},
            Some(DeliveryIntent::Auto)
        ),
        DispatchMode::QueueAfterActive {}
    );
    // An explicit target that does not exist stays exactly as requested.
    assert_eq!(
        resolve_dispatch_intent(
            &projection,
            DispatchMode::SteerActive {
                target_run_id: "run:missing".into()
            },
            None
        ),
        DispatchMode::SteerActive {
            target_run_id: "run:missing".into()
        }
    );
}

#[test]
fn command_policy_queues_a_steer_for_a_provider_that_cannot_steer_either_way() {
    let mut projection = queued_projection();
    projection.provider_sessions[0].capabilities = caps(false, true, false);
    for intent in [DeliveryIntent::Auto, DeliveryIntent::Steer] {
        assert_eq!(
            resolve_dispatch_intent(&projection, DispatchMode::StartImmediately {}, Some(intent)),
            DispatchMode::QueueAfterActive {}
        );
    }
    assert_eq!(
        resolve_dispatch_intent(
            &projection,
            DispatchMode::SteerActive {
                target_run_id: "run:a".into()
            },
            None
        ),
        DispatchMode::QueueAfterActive {}
    );
    assert_eq!(
        resolve_dispatch_intent(
            &projection,
            DispatchMode::StartImmediately {},
            Some(DeliveryIntent::Restart)
        ),
        DispatchMode::RestartActive {
            target_run_id: "run:a".into()
        }
    );
}

#[test]
fn command_policy_queues_steer_and_auto_while_maintenance_runs_but_restarts_it() {
    for text in ["/compact", " /LOGOUT "] {
        let mut projection = queued_projection();
        projection.messages[0].text = text.into();
        for intent in [DeliveryIntent::Auto, DeliveryIntent::Steer] {
            assert_eq!(
                resolve_dispatch_intent(
                    &projection,
                    DispatchMode::StartImmediately {},
                    Some(intent)
                ),
                DispatchMode::QueueAfterActive {}
            );
        }
        assert_eq!(
            resolve_dispatch_intent(
                &projection,
                DispatchMode::StartImmediately {},
                Some(DeliveryIntent::Restart)
            ),
            DispatchMode::RestartActive {
                target_run_id: "run:a".into()
            }
        );
    }
}

#[test]
fn stop_keeps_a_restart_continuation_of_the_stopped_run_from_starting() {
    // T3 `ThreadStop.test.ts`: a restart continuation queued behind the stopped
    // run waits for the user instead of starting on the stop's turn end.
    let mut projection = queued_projection();
    add_queued_run(&mut projection, "run:b", 2);
    projection.runs[1].restart_continuation_of_run_id = Some("run:a".into());
    let stop = Command::ThreadStop(ThreadStop {
        command_id: "command:stop".into(),
        thread_id: "thread:a".into(),
        reason: Some("user stop".into()),
        stop_delegated_tasks: Some(false),
    });
    let decision = decide(Some(&projection), &stop, NOW).expect("stop");
    for event in &decision.events {
        projection.apply(event);
    }
    assert_eq!(projection.run("run:b").unwrap().queue_held, Some(true));
    let start = plan_start_next_queued_run(&mut projection, None, NOW + 1).expect("start");
    assert!(start.is_empty());
    assert_eq!(projection.run("run:b").unwrap().status, RunStatus::Queued);
}
