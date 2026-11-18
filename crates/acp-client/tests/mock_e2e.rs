//! The ACP client against `mock-acp` in each agent's profile, through a real
//! child process: handshake, sessions, a turn with a permission prompt, a
//! diff, settings, history and cancelling.

use std::path::PathBuf;

use acp_client::{AcpClient, AcpConfig};
use agent_rpc::Incoming;
use agentdesk_core::{AgentEvent, ApprovalDecision, EntryKind, Item, ItemStatus, Transcript};
use futures::{StreamExt, channel::mpsc::UnboundedReceiver, executor::block_on, future};

async fn start(name: &str) -> (AcpClient, UnboundedReceiver<Incoming>) {
    AcpClient::start(AcpConfig {
        backend: name.into(),
        program: PathBuf::from(env!("CARGO_BIN_EXE_mock-acp")),
        args: vec!["--name".into(), name.into()],
        env: Vec::new(),
    })
    .await
    .expect("mock-acp starts")
}

/// Sends `text` and folds everything until the turn ends into `transcript`,
/// answering permission prompts with `decision`.
async fn turn(
    client: &AcpClient,
    events: &mut UnboundedReceiver<Incoming>,
    session: &str,
    text: &str,
    decision: ApprovalDecision,
    transcript: &mut Transcript,
) {
    transcript.push_local_user(text, vec![]);
    let (_, started) = client.begin_turn(session);
    for event in &started {
        transcript.apply(event);
    }
    let prompt = client.prompt(session, text, &[], &[]);
    futures::pin_mut!(prompt);
    loop {
        let next = events.next();
        futures::pin_mut!(next);
        match future::select(prompt, next).await {
            future::Either::Left((ended, _)) => {
                // Updates sent before the response may still be queued.
                while let Ok(incoming) = events.try_recv() {
                    for event in client.translate(incoming) {
                        transcript.apply(&event);
                    }
                }
                for event in ended {
                    transcript.apply(&event);
                }
                return;
            }
            future::Either::Right((incoming, rest)) => {
                prompt = rest;
                let Some(incoming) = incoming else {
                    panic!("agent closed")
                };
                for event in client.translate(incoming) {
                    if let AgentEvent::ApprovalRequested(request) = &event {
                        client
                            .answer_permission(request.request_id.clone(), decision)
                            .unwrap();
                        transcript.apply(&event);
                        transcript.answer_approval(&request.request_id, decision);
                        continue;
                    }
                    transcript.apply(&event);
                }
            }
        }
    }
}

#[test]
fn claude_runs_a_turn_with_permission_diff_and_plan() {
    block_on(async {
        let (client, mut events) = start("claude").await;
        assert!(client.info.load_session && client.info.steering && client.info.images);
        let session = client.new_session(&std::env::temp_dir()).await.unwrap();
        assert!(
            session
                .options
                .iter()
                .any(|o| o.category.as_deref() == Some("model"))
        );

        let mut transcript = Transcript::new();
        turn(
            &client,
            &mut events,
            &session.session_id,
            "make a plan, run the tests and edit lib",
            ApprovalDecision::Accept,
            &mut transcript,
        )
        .await;
        assert!(!transcript.is_running());
        let entries = transcript.entries();
        assert!(entries.iter().any(
            |e| matches!(&e.kind, EntryKind::Plan { steps, .. } if steps.iter().all(|s| s.done))
        ));
        assert!(entries.iter().any(|e| matches!(&e.kind,
            EntryKind::Item(Item::Command { status: ItemStatus::Completed, output, exit_code: Some(0), .. }) if output.contains("9 passed"))));
        assert!(entries.iter().any(|e| matches!(&e.kind, EntryKind::Item(Item::FileChange { changes, .. }) if changes[0].path == "src/lib.rs")));
        assert!(entries.iter().any(|e| matches!(&e.kind, EntryKind::Item(Item::Agent { text, .. }) if text.contains("hello from claude"))));
        assert!(
            entries
                .iter()
                .any(|e| matches!(&e.kind, EntryKind::Item(Item::Reasoning { .. })))
        );
        // Context from usage_update, the turn's counts from the prompt's response.
        let usage = transcript.usage().expect("usage reported");
        assert_eq!(
            (usage.context_used, usage.context_window),
            (Some(18_400), Some(200_000))
        );
        assert_eq!(usage.last.output, 640);
        assert_eq!(usage.last.input, 1_200 + 14_800 + 900);
        assert_eq!(usage.last.cached_input, 14_800);
        assert_eq!(usage.cost.as_ref().unwrap().currency, "USD");
        let turn = transcript.entries().last().unwrap().turn_id.clone();
        let pace = transcript
            .turn_pace(&turn, std::time::Instant::now())
            .expect("the turn was timed");
        assert_eq!(pace.tokens_out(), Some((640, true)));
        assert!(pace.streamed_tokens > 0.);
    });
}

#[test]
fn a_declined_command_fails_and_the_turn_still_ends() {
    block_on(async {
        let (client, mut events) = start("claude").await;
        let session = client.new_session(&std::env::temp_dir()).await.unwrap();
        let mut transcript = Transcript::new();
        turn(
            &client,
            &mut events,
            &session.session_id,
            "run it",
            ApprovalDecision::Decline,
            &mut transcript,
        )
        .await;
        assert!(transcript.entries().iter().any(|e| matches!(
            &e.kind,
            EntryKind::Item(Item::Command {
                status: ItemStatus::Failed,
                ..
            })
        )));
        assert!(!transcript.is_running());
    });
}

#[test]
fn claude_reloads_history_and_deepseek_resumes_without_it() {
    block_on(async {
        let (client, mut events) = start("claude").await;
        let listed = client.list_sessions().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].backend, "claude");
        let past = &listed[0];
        client.open_session(&past.id, &past.cwd).await.unwrap();
        let mut transcript = Transcript::new();
        while let Ok(incoming) = events.try_recv() {
            for event in client.translate(incoming) {
                transcript.apply(&event);
            }
        }
        let texts: Vec<String> = transcript
            .entries()
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Item(Item::User { text, .. }) => Some(format!("user:{text}")),
                EntryKind::Item(Item::Agent { text, .. }) => Some(format!("agent:{text}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "user:Why is the list virtualized?",
                "agent:So only rows near the view are drawn."
            ]
        );
        assert!(!transcript.is_running());

        let (deepseek, _events) = start("deepseek").await;
        assert!(
            !deepseek.info.load_session && deepseek.info.resume_session && !deepseek.info.images
        );
        let past = deepseek.list_sessions().await.unwrap().remove(0);
        let options = deepseek.open_session(&past.id, &past.cwd).await.unwrap();
        assert_eq!(
            options.iter().find(|o| o.id == "model").unwrap().choices[0]
                .group
                .as_deref(),
            Some("DeepSeek")
        );
    });
}

#[test]
fn settings_change_and_a_long_turn_can_be_cancelled() {
    block_on(async {
        let (client, mut events) = start("antigravity").await;
        let session = client.new_session(&std::env::temp_dir()).await.unwrap();
        // Antigravity's modes come as session modes, offered as an option.
        let mode = session
            .options
            .iter()
            .find(|o| o.category.as_deref() == Some("mode"))
            .unwrap();
        assert_eq!(mode.current, "agent");
        client
            .set_option(&session.session_id, &mode.id, "ask")
            .await
            .unwrap();
        let updated = client
            .set_option(&session.session_id, "model", "flash")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            updated
                .iter()
                .find(|o| o.id == "model")
                .unwrap()
                .current_name(),
            "Gemini 3 Flash"
        );

        let mut transcript = Transcript::new();
        let id = session.session_id.clone();
        let canceller = client.clone();
        let stopper = async move {
            futures::future::ready(()).await;
            std::thread::sleep(std::time::Duration::from_millis(400));
            canceller.cancel(&id).unwrap();
        };
        let ((), ()) = futures::join!(
            turn(
                &client,
                &mut events,
                &session.session_id,
                "write something long",
                ApprovalDecision::Accept,
                &mut transcript
            ),
            stopper
        );
        let last = transcript.entries().last().unwrap().turn_id.clone();
        assert_eq!(
            transcript.turn_summary(&last).unwrap().status,
            agentdesk_core::TurnStatus::Interrupted
        );
    });
}

/// Handshake with a real agent, without sending a prompt:
/// `AGENTDESK_ACP="dsh --profile acp" cargo test -p acp-client -- --ignored real_agent`
#[test]
#[ignore = "needs a real ACP agent on this machine"]
fn real_agent_handshake_and_session() {
    let command = std::env::var("AGENTDESK_ACP").expect("set AGENTDESK_ACP to the agent's command");
    let mut parts = command.split_whitespace().map(str::to_owned);
    let program = PathBuf::from(parts.next().expect("a program"));
    block_on(async {
        let (client, _events) = AcpClient::start(AcpConfig {
            backend: "real".into(),
            program,
            args: parts.collect(),
            env: Vec::new(),
        })
        .await
        .expect("the agent starts and shakes hands");
        eprintln!("agent: {:?}", client.info);
        let session = client
            .new_session(&std::env::temp_dir())
            .await
            .expect("a new session");
        for option in &session.options {
            eprintln!(
                "option {} ({:?}) = {} of {} choices",
                option.id,
                option.category,
                option.current_name(),
                option.choices.len()
            );
        }
        let listed = client.list_sessions().await.expect("sessions list");
        eprintln!("{} stored sessions", listed.len());
        let _ = client.close_session(&session.session_id).await;
    });
}
