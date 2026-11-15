//! The client against `mock-codex`: a whole turn with an approval, end to end
//! through a real child process and the real translation layer.

use std::path::PathBuf;

use agentdesk_core::{AgentEvent, ApprovalDecision, EntryKind, Item, ItemStatus, Transcript};
use codex_client::{CodexClient, CodexConfig, Incoming, StartThread, translate};
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
        transcript.push_local_user("please run the tests");
        transcript.begin_pending_turn();
        client
            .turn_start(&thread.id, "please run the tests", None)
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
            .turn_start(&thread.id, "a question", None)
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
