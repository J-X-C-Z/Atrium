//! The client against `mock-codex`: a whole turn with an approval, end to end
//! through a real child process and the real translation layer.

use std::{
    fs,
    io::{BufRead, BufReader, Lines, Write},
    path::PathBuf,
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

use atrium_core::{AgentEvent, ApprovalDecision, EntryKind, Item, ItemStatus, Transcript};
use codex_client::{
    AccessMode, CodexClient, CodexConfig, Incoming, StartThread, TurnRequest, translate,
};
use futures::{StreamExt, executor::block_on};
use serde_json::{Value, json};

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
        // Context and speed: the breakdown, and the turn timed as it streamed.
        let usage = transcript.usage().expect("usage reported");
        assert_eq!(usage.context_window, Some(272_000));
        assert!(usage.context_used.unwrap() > 0 && usage.total.cached_input > 0);
        let turn = entries.last().unwrap().turn_id.clone();
        let pace = transcript
            .turn_pace(&turn, std::time::Instant::now())
            .expect("the turn was timed");
        assert!(pace.done && pace.first_token.is_some());
        assert!(pace.tokens_per_sec().unwrap() > 0.);
        assert_eq!(pace.output_tokens, Some(usage.total.output));

        let (_, history) = client.thread_resume(&thread.id).await.unwrap();
        assert_eq!(history.len(), 1);
    });
}

#[test]
fn a_read_snapshot_is_not_a_resume_and_rollout_files_survive_a_new_process() {
    block_on(async {
        let home = std::env::temp_dir().join(format!(
            "mock-codex-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&home).unwrap();
        let config = CodexConfig {
            binary: Some(PathBuf::from(env!("CARGO_BIN_EXE_mock-codex"))),
            env: vec![("CODEX_HOME".into(), home.display().to_string())],
            ..Default::default()
        };
        let (first, events) = CodexClient::start(config.clone()).await.unwrap();
        let thread = first.thread_start(StartThread::default()).await.unwrap();
        drop(events);
        drop(first);

        let (second, _events) = CodexClient::start(config).await.unwrap();
        let (_, history) = second.thread_read(&thread.id).await.unwrap();
        assert_eq!(history.len(), 0);
        let refused = second
            .turn_start(
                &thread.id,
                TurnRequest {
                    text: "send after read".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            format!("{refused:#}").contains("not loaded"),
            "read must not resume: {refused:#}"
        );
        second.thread_resume(&thread.id).await.unwrap();
        second
            .turn_start(
                &thread.id,
                TurnRequest {
                    text: "send after resume".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        drop(second);
        let _ = fs::remove_dir_all(home);
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
                assert_eq!(status, atrium_core::TurnStatus::Interrupted);
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

/// Sends one JSON-RPC request and returns its result, skipping notifications.
fn request(
    stdin: &mut ChildStdin,
    lines: &mut Lines<BufReader<ChildStdout>>,
    id: i64,
    method: &str,
    params: Value,
) -> Value {
    writeln!(
        stdin,
        "{}",
        json!({"id": id, "method": method, "params": params})
    )
    .unwrap();
    stdin.flush().unwrap();
    loop {
        let line = lines.next().expect("mock stays open").unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return value.get("result").cloned().unwrap_or(Value::Null);
        }
    }
}

/// The handshake T3 Code 0.1.11 performs against an unmodified mock binary:
/// `--version`, `initialize`, `skills/list`, `config/read`, `thread/start`
/// and `thread/resume`, checked against T3's decoder expectations.
#[test]
fn the_protocol_shapes_t3_code_decodes() {
    let bin = env!("CARGO_BIN_EXE_mock-codex");
    let version = Command::new(bin).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "codex-cli 0.160.0"
    );

    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    let initialize = request(&mut stdin, &mut lines, 1, "initialize", json!({}));
    let user_agent = initialize["userAgent"].as_str().unwrap();
    // T3 parses the version after the first slash and refuses < 0.159.0.
    assert_eq!(user_agent.split_once('/').unwrap().1, "0.160.0");

    let skills = request(
        &mut stdin,
        &mut lines,
        2,
        "skills/list",
        json!({"cwds": ["/tmp/atrium-t3"]}),
    );
    assert_eq!(skills["data"][0]["cwd"], "/tmp/atrium-t3");
    assert!(skills["data"][0]["skills"].as_array().unwrap().is_empty());
    assert!(skills["data"][0]["errors"].as_array().unwrap().is_empty());

    let config = request(
        &mut stdin,
        &mut lines,
        3,
        "config/read",
        json!({"includeLayers": false}),
    );
    assert!(config["config"].is_object());
    assert!(config["origins"].is_object());

    let started = request(
        &mut stdin,
        &mut lines,
        4,
        "thread/start",
        json!({"cwd": "/tmp/atrium-t3"}),
    );
    assert_eq!(started["approvalPolicy"], "never");
    assert_eq!(started["approvalsReviewer"], "user");
    assert_eq!(started["sandbox"], json!({"type": "dangerFullAccess"}));
    assert_eq!(started["thread"]["cliVersion"], "0.160.0");
    assert!(started["thread"]["projectId"].is_null());
    assert_eq!(started["thread"]["source"], "appServer");
    let thread_id = started["thread"]["id"].as_str().unwrap().to_string();

    let resumed = request(
        &mut stdin,
        &mut lines,
        5,
        "thread/resume",
        json!({"threadId": thread_id}),
    );
    assert_eq!(resumed["approvalPolicy"], "never");
    assert_eq!(resumed["approvalsReviewer"], "user");
    assert_eq!(resumed["sandbox"], json!({"type": "dangerFullAccess"}));
    assert_eq!(resumed["thread"]["cliVersion"], "0.160.0");
    assert_eq!(resumed["thread"]["source"], "appServer");

    drop(stdin);
    let _ = child.wait();
}
