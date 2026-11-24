//! End-to-end protocol tests against `mock-claude` through a real child
//! process: handshake, plain and streamed turns, permission prompts,
//! interrupts, usage limits, per-turn model/effort and the control queries.

use std::{path::PathBuf, time::Duration};

use claude_client::{
    ClaudeClient, ClaudeConfig, ContentBlock, Effort, Incoming, PermissionDecision, TurnOptions,
    events::{ClaudeEvent, TurnStatus},
};
use futures::{
    StreamExt,
    channel::mpsc::UnboundedReceiver,
    executor::block_on,
    future::{Either, select},
};

fn config(update: impl FnOnce(&mut ClaudeConfig)) -> ClaudeConfig {
    let mut config = ClaudeConfig {
        binary: PathBuf::from(env!("CARGO_BIN_EXE_mock-claude")),
        handshake_timeout: Duration::from_secs(10),
        control_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    update(&mut config);
    config
}

fn start(config: ClaudeConfig) -> (ClaudeClient, UnboundedReceiver<Incoming>) {
    block_on(ClaudeClient::start(config)).expect("mock-claude starts")
}

/// Waits for one frame, panicking after ten seconds so a mock regression fails
/// instead of hanging CI.
fn next(receiver: &mut UnboundedReceiver<Incoming>) -> Incoming {
    block_on(async {
        let (timeout_sender, mut timeout) = futures::channel::mpsc::unbounded::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            let _ = timeout_sender.unbounded_send(());
        });
        let frame = receiver.next();
        let expired = timeout.next();
        futures::pin_mut!(frame, expired);
        match select(frame, expired).await {
            Either::Left((frame, _)) => frame.expect("mock closed the stream"),
            Either::Right(_) => panic!("timed out waiting for a frame"),
        }
    })
}

#[test]
fn handshake_plain_turn_and_streaming_events() {
    let (client, mut events) = start(config(|_| {}));
    let info = client.session_info().expect("initialize reply");
    assert!(
        info.commands.iter().any(|command| command.name == "review"),
        "slash commands are advertised in the handshake"
    );
    assert!(
        info.models
            .iter()
            .any(|model| model.value == "claude-sonnet-4-6")
    );
    assert_eq!(client.command_names(), ["review", "compact"]);

    client.send_text("plain hello").unwrap();

    let mut saw_init = false;
    let mut streamed = String::new();
    let mut assistant = String::new();
    let result = loop {
        match next(&mut events) {
            Incoming::Event(ClaudeEvent::SessionInit(init)) => {
                saw_init = true;
                assert_eq!(init.skills, ["review", "deploy"]);
                assert_eq!(init.agents, ["Explore"]);
            }
            Incoming::Event(ClaudeEvent::Stream(
                claude_client::events::StreamEvent::ContentBlockDelta {
                    delta: claude_client::events::Delta::Text { text },
                    ..
                },
            )) => {
                streamed.push_str(&text);
            }
            Incoming::Event(ClaudeEvent::Assistant(message)) => {
                for block in message.content {
                    if let ContentBlock::Text { text } = block {
                        assistant.push_str(&text);
                    }
                }
            }
            Incoming::Event(ClaudeEvent::Result(result)) => break result,
            Incoming::Permission(_) | Incoming::Control(_) => {}
            _ => {}
        }
    };
    assert!(saw_init, "system/init arrives before the turn");
    assert_eq!(streamed, "echo: plain hello");
    assert_eq!(assistant, "echo: plain hello");
    assert_eq!(result.terminal_status(), TurnStatus::Completed);
    assert_eq!(result.usage.unwrap().output_tokens, 2);
    assert_eq!(
        result.model_usage["claude-sonnet-4-6"].context_window,
        200_000
    );
    assert_eq!(client.skills(), ["review", "deploy"]);
    assert!(!client.session_id().unwrap().is_empty());
}

#[test]
fn an_allowed_tool_call_answers_the_permission_prompt() {
    let (client, mut events) = start(config(|_| {}));
    client.send_text("please run the command").unwrap();

    let mut answered = false;
    let mut tool_result = None;
    let result = loop {
        match next(&mut events) {
            Incoming::Permission(request) => {
                assert_eq!(request.tool_name, "Bash");
                assert_eq!(request.input["command"], "echo mock");
                assert!(request.tool_use_id.is_some());
                assert_eq!(request.permission_suggestions.len(), 1);
                client
                    .answer_permission(
                        &request.request_id,
                        &PermissionDecision::Allow {
                            updated_input: None,
                            updated_permissions: Vec::new(),
                        },
                    )
                    .unwrap();
                answered = true;
            }
            Incoming::Event(ClaudeEvent::User(message)) => {
                for block in message.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = block
                    {
                        tool_result = Some((tool_use_id, content, is_error));
                    }
                }
            }
            Incoming::Event(ClaudeEvent::Result(result)) => break result,
            _ => {}
        }
    };
    assert!(answered);
    let (tool_use_id, content, is_error) = tool_result.expect("tool result");
    assert_eq!(tool_use_id, "toolu_mock_1");
    assert_eq!(content, "mock output");
    assert!(!is_error);
    assert_eq!(result.terminal_status(), TurnStatus::Completed);
    assert_eq!(result.result, "Command finished.");
}

#[test]
fn a_denied_tool_call_reaches_the_model_as_an_error_result() {
    let (client, mut events) = start(config(|_| {}));
    client.send_text("run the command").unwrap();

    let mut denied = false;
    let mut tool_result_error = false;
    let result = loop {
        match next(&mut events) {
            Incoming::Permission(request) => {
                client
                    .answer_permission(
                        &request.request_id,
                        &PermissionDecision::Deny {
                            message: "not in this workspace".into(),
                            interrupt: false,
                        },
                    )
                    .unwrap();
                denied = true;
            }
            Incoming::Event(ClaudeEvent::User(message)) => {
                for block in message.content {
                    if let ContentBlock::ToolResult { is_error, .. } = block {
                        tool_result_error = is_error;
                    }
                }
            }
            Incoming::Event(ClaudeEvent::Result(result)) => break result,
            _ => {}
        }
    };
    assert!(denied);
    assert!(tool_result_error);
    assert_eq!(result.result, "Permission denied.");
}

#[test]
fn interrupt_stops_the_running_turn() {
    let (client, mut events) = start(config(|_| {}));
    client.send_text("interrupt please").unwrap();

    loop {
        if let Incoming::Event(ClaudeEvent::Stream(_)) = next(&mut events) {
            break;
        }
    }
    client.interrupt().unwrap();
    let result = loop {
        if let Incoming::Event(ClaudeEvent::Result(result)) = next(&mut events) {
            break result;
        }
    };
    assert_eq!(result.terminal_status(), TurnStatus::Interrupted);
}

#[test]
fn a_rejected_rate_limit_reports_a_reset_time() {
    let (client, mut events) = start(config(|_| {}));
    client.send_text("we hit the usage limit").unwrap();

    let mut rate_limit = None;
    let result = loop {
        match next(&mut events) {
            Incoming::Event(ClaudeEvent::RateLimit(info)) => rate_limit = Some(info),
            Incoming::Event(ClaudeEvent::Result(result)) => break result,
            _ => {}
        }
    };
    let info = rate_limit.expect("rate limit event");
    assert!(info.is_blocked());
    assert_eq!(
        info.reset_at_iso().as_deref(),
        Some("2026-09-25T01:10:00.000Z")
    );
    assert_eq!(result.terminal_status(), TurnStatus::Failed);
    let failure = result.failure_at(false, 1_790_000_000).expect("failure");
    assert_eq!(
        failure.class,
        claude_client::events::FailureClass::UsageLimit
    );
    assert_eq!(
        failure.reset_at.as_deref(),
        Some("2026-09-21T15:13:20.000Z")
    );
}

#[test]
fn a_turn_can_switch_model_and_inject_ultrathink_effort() {
    let (client, mut events) = start(config(|_| {}));
    client
        .send_turn(
            vec![claude_client::UserContentBlock::text(
                "Investigate this failure",
            )],
            &TurnOptions {
                model: Some("claude-opus-4-7".into()),
                effort: Some(Effort::UltraThink),
                uuid: None,
                client_composed: false,
            },
        )
        .unwrap();
    let mut assistant = String::new();
    let result = loop {
        match next(&mut events) {
            Incoming::Event(ClaudeEvent::Assistant(message)) => {
                for block in message.content {
                    if let ContentBlock::Text { text } = block {
                        assistant.push_str(&text);
                    }
                }
            }
            Incoming::Event(ClaudeEvent::Result(result)) => break result,
            other => {
                if let Incoming::Malformed { line, error } = other {
                    panic!("malformed frame {line}: {error}");
                }
            }
        }
    };
    assert!(
        assistant.starts_with("echo: Ultrathink:"),
        "prompt got {assistant:?}"
    );
    assert_eq!(result.terminal_status(), TurnStatus::Completed);
}

#[test]
fn the_control_queries_return_the_cli_payloads() {
    let (client, _events) = start(config(|_| {}));

    let usage = client.request_usage().unwrap();
    assert_eq!(usage["rate_limits_available"], true);
    assert_eq!(usage["rate_limits"]["five_hour"]["utilization"], 54);

    let context = client.request_context_usage(None).unwrap();
    assert_eq!(context["totalTokens"], 1_000);

    let models = client.list_models().unwrap();
    assert_eq!(models[0]["value"], "claude-sonnet-4-6");

    client
        .set_permission_mode(claude_client::PermissionMode::Plan)
        .unwrap();
    client.set_max_thinking_tokens(Some(2_048)).unwrap();
    client.set_model(Some("claude-haiku-4-5")).unwrap();
}

#[test]
fn resuming_keeps_the_native_session_id() {
    let (client, mut events) = start(config(|config| {
        config.launch.resume = Some("session-from-before".into());
    }));
    client.send_text("plain hello").unwrap();
    loop {
        if let Incoming::Event(ClaudeEvent::Result(_)) = next(&mut events) {
            break;
        }
    }
    assert_eq!(client.session_id().as_deref(), Some("session-from-before"));
}
