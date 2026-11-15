//! The client against `mock-codex`: a whole turn with an approval, end to end
//! through a real child process and the real translation layer.

use std::path::PathBuf;

use agentdesk_core::{AgentEvent, ApprovalDecision, EntryKind, Item, ItemStatus, Transcript};
use codex_client::{
    AccessMode, CodexClient, CodexConfig, Incoming, StartThread, TurnRequest, translate,
};
use futures::{StreamExt, executor::block_on};

#[test]
fn a_turn_with_an_approved_command_runs_to_completion() {
    block_on(async {
        let config = CodexConfig {
            binary: Some(PathBuf::from(env!("CARGO_BIN_EXE_mock-codex"))),
            ..Default::default()
        };
        let (client, mut events) = CodexClient::start(config).await.expect("mock starts");
        assert!(client.user_agent.starts_with("mock-codex"));
        assert_eq!(client.thread_list(20).await.unwrap().len(), 2);
        assert!(
            client
                .model_list()
                .await
                .unwrap()
                .iter()
                .any(|m| m.is_default)
        );

        let thread = client.thread_start(StartThread::default()).await.unwrap();
        let mut transcript = Transcript::new();
        transcript.push_local_user("please run the tests", vec![]);
        transcript.begin_pending_turn();
        client
            .turn_start(
                &thread.id,
                TurnRequest {
                    text: "please run the tests".into(),
                    mode: Some(AccessMode::Auto),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        while let Some(incoming) = events.next().await {
            let event = match incoming {
                Incoming::Notification { method, params } => translate::event(&method, &params),
                Incoming::Request { id, method, params } => {
                    let event = translate::request(&id, &method, &params);
                    if event.is_some() {
                        client
                            .answer_approval(id.clone(), ApprovalDecision::Accept)
                            .unwrap();
                        transcript.answer_approval(&id, ApprovalDecision::Accept);
                    } else {
                        client.reject_request(id, &method).unwrap();
                    }
                    event
                }
                Incoming::Closed => panic!("mock closed early"),
            };
            let Some(event) = event else { continue };
            if event.thread_id() != Some(thread.id.as_str()) {
                continue;
            }
            transcript.apply(&event);
            if matches!(event, AgentEvent::TurnCompleted { .. }) {
                break;
            }
        }

        assert!(!transcript.is_running());
        let entries = transcript.entries();
        let users = entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Item(Item::User { .. })))
            .count();
        assert_eq!(users, 1, "the echo replaced the optimistic row");
        assert!(entries.iter().any(|e| matches!(
            &e.kind,
            EntryKind::Item(Item::Command { status: ItemStatus::Completed, output, .. }) if output.contains("6 passed")
        )));
        assert!(entries.iter().any(|e| matches!(
            &e.kind,
            EntryKind::Item(Item::Agent { text, .. }) if text.contains("hello from mock-codex")
        )));
        assert!(entries.iter().all(|e| !e.live));
        assert!(transcript.usage().is_some());

        let (_, history) = client.thread_resume(&thread.id).await.unwrap();
        assert_eq!(history.len(), 1);
    });
}

#[test]
fn an_interrupted_turn_ends_as_interrupted() {
    block_on(async {
        let config = CodexConfig {
            binary: Some(PathBuf::from(env!("CARGO_BIN_EXE_mock-codex"))),
            ..Default::default()
        };
        let (client, mut events) = CodexClient::start(config).await.expect("mock starts");
        let thread = client.thread_start(StartThread::default()).await.unwrap();
        let turn_id = client
            .turn_start(
                &thread.id,
                TurnRequest {
                    text: "a long question".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut transcript = Transcript::new();
        let mut interrupted = false;
        while let Some(incoming) = events.next().await {
            let Incoming::Notification { method, params } = incoming else {
                continue;
            };
            let Some(event) = translate::event(&method, &params) else {
                continue;
            };
            transcript.apply(&event);
            if !interrupted && matches!(event, AgentEvent::ReasoningDelta { .. }) {
                client.turn_interrupt(&thread.id, &turn_id).await.unwrap();
                interrupted = true;
            }
            if let AgentEvent::TurnCompleted { status, .. } = event {
                assert_eq!(status, agentdesk_core::TurnStatus::Interrupted);
                break;
            }
        }
        assert!(!transcript.is_running());
        assert!(
            !transcript
                .entries()
                .iter()
                .any(|e| matches!(&e.kind, EntryKind::Item(Item::Agent { .. }))),
            "no answer after the interrupt"
        );
    });
}

async fn start() -> (
    CodexClient,
    futures::channel::mpsc::UnboundedReceiver<Incoming>,
) {
    let config = CodexConfig {
        binary: Some(PathBuf::from(env!("CARGO_BIN_EXE_mock-codex"))),
        ..Default::default()
    };
    CodexClient::start(config).await.expect("mock starts")
}

/// Folds events for `thread_id` into `transcript` until a turn completes.
async fn until_turn_ends(
    events: &mut futures::channel::mpsc::UnboundedReceiver<Incoming>,
    thread_id: &str,
    transcript: &mut Transcript,
) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    while let Some(incoming) = events.next().await {
        let Incoming::Notification { method, params } = incoming else {
            continue;
        };
        let Some(event) = translate::event(&method, &params) else {
            continue;
        };
        if event.thread_id() != Some(thread_id) {
            continue;
        }
        transcript.apply(&event);
        let done = matches!(event, AgentEvent::TurnCompleted { .. });
        seen.push(event);
        if done {
            break;
        }
    }
    seen
}

#[test]
fn attachments_plans_and_durations_come_through() {
    block_on(async {
        let (client, mut events) = start().await;
        let models = client.model_list().await.unwrap();
        assert!(models[0].efforts.contains(&"high".to_string()));
        let thread = client.thread_start(StartThread::default()).await.unwrap();
        client
            .turn_start(
                &thread.id,
                TurnRequest {
                    text: "make a plan".into(),
                    images: vec![PathBuf::from("/tmp/shot.png")],
                    mentions: vec![PathBuf::from("/repo/src/lib.rs")],
                    effort: Some("high".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut transcript = Transcript::new();
        let seen = until_turn_ends(&mut events, &thread.id, &mut transcript).await;
        assert!(seen.iter().any(
            |e| matches!(e, AgentEvent::PlanUpdated { steps, .. } if steps.iter().all(|s| s.done))
        ));
        let user = transcript.entries().iter().find_map(|e| match &e.kind {
            EntryKind::Item(Item::User { attachments, .. }) => Some(attachments.clone()),
            _ => None,
        });
        assert_eq!(
            user.unwrap(),
            vec!["/tmp/shot.png".to_string(), "/repo/src/lib.rs".to_string()]
        );
        let last = transcript.entries().len() - 1;
        assert!(transcript.ends_turn(last));
        let turn = &transcript.entries()[last].turn_id;
        assert!(transcript.turn_summary(turn).unwrap().duration_ms.is_some());
        assert!(
            transcript
                .turn_answer(turn)
                .contains("hello from mock-codex")
        );
    });
}

#[test]
fn rename_compact_review_and_search() {
    block_on(async {
        let (client, mut events) = start().await;
        let thread = client.thread_start(StartThread::default()).await.unwrap();
        client.thread_set_name(&thread.id, "Renamed").await.unwrap();
        let mut transcript = Transcript::new();
        // The rename notification arrives before any turn.
        loop {
            let Some(Incoming::Notification { method, params }) = events.next().await else {
                continue;
            };
            if let Some(AgentEvent::ThreadNameUpdated { name, .. }) =
                translate::event(&method, &params)
            {
                assert_eq!(name.as_deref(), Some("Renamed"));
                break;
            }
        }
        client.thread_compact(&thread.id).await.unwrap();
        until_turn_ends(&mut events, &thread.id, &mut transcript).await;
        assert!(transcript.entries().iter().any(|e| matches!(&e.kind, EntryKind::Item(Item::Other { label, .. }) if label == "Context compacted")));

        client
            .review_start(&thread.id, codex_client::ReviewTarget::UncommittedChanges)
            .await
            .unwrap();
        until_turn_ends(&mut events, &thread.id, &mut transcript).await;
        assert!(!transcript.is_running());

        let root = env!("CARGO_MANIFEST_DIR");
        let found = client.fuzzy_file_search("mock_codex", root).await.unwrap();
        assert!(
            found.iter().any(|path| path.ends_with("mock_codex.rs")),
            "{found:?}"
        );
    });
}
