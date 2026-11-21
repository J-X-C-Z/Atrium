use super::*;
use agent_rpc::Connection;
use atrium_core::{ApprovalKind, ApprovalRequest};
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn thread_meta(id: &str, cwd: &Path) -> ThreadMeta {
    ThreadMeta {
        id: id.into(),
        backend: "codex".into(),
        name: Some("Remote".into()),
        preview: "hello".into(),
        cwd: cwd.display().to_string(),
        model: None,
        created_at: 1,
        updated_at: 2,
    }
}

fn thread_json(id: &str, cwd: &Path) -> Value {
    json!({
        "id": id,
        "preview": "hello",
        "cwd": cwd.display().to_string(),
        "createdAt": 1,
        "updatedAt": 2,
        "turns": [{
            "id": "old-turn",
            "status": "completed",
            "items": [{
                "type": "userMessage",
                "id": "old-item",
                "content": [{"type": "text", "text": "hello", "text_elements": []}]
            }]
        }]
    })
}

fn scripted_client(
    cwd: PathBuf,
    answer: impl Fn(&Value) -> Value + Send + 'static,
) -> (codex_client::CodexClient, thread::JoinHandle<Vec<Value>>) {
    let (client_socket, mut mock_socket) = UnixStream::pair().unwrap();
    let (connection, _) = Connection::new(
        BufReader::new(client_socket.try_clone().unwrap()),
        client_socket,
    );
    let mut client = codex_client::CodexClient::over(connection);
    client.codex_home = cwd.display().to_string();
    let mock = thread::spawn(move || {
        mock_socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(mock_socket.try_clone().unwrap());
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).unwrap();
            if read == 0 {
                break;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            seen.push(request.clone());
            let result = answer(&request);
            writeln!(
                mock_socket,
                "{}",
                json!({"id": request["id"], "result": result})
            )
            .unwrap();
            if request["method"] == "turn/start" || request["method"] == "turn/steer" {
                break;
            }
        }
        seen
    });
    (client, mock)
}

fn shared_scripted_client(
    cwd: PathBuf,
    answer: impl Fn(&Value) -> Value + Send + 'static,
) -> (
    codex_client::CodexClient,
    Arc<Mutex<Vec<Value>>>,
    thread::JoinHandle<()>,
) {
    let (client_socket, mut mock_socket) = UnixStream::pair().unwrap();
    let (connection, _) = Connection::new(
        BufReader::new(client_socket.try_clone().unwrap()),
        client_socket,
    );
    let mut client = codex_client::CodexClient::over(connection);
    client.codex_home = cwd.display().to_string();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let mock = thread::spawn(move || {
        mock_socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut reader = BufReader::new(mock_socket.try_clone().unwrap());
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            log.lock().unwrap().push(request.clone());
            let result = answer(&request);
            writeln!(
                mock_socket,
                "{}",
                json!({"id": request["id"], "result": result})
            )
            .unwrap();
            if matches!(
                request["method"].as_str(),
                Some("turn/start" | "turn/steer")
            ) {
                break;
            }
        }
    });
    (client, seen, mock)
}

fn synced_workspace(
    cx: &mut TestAppContext,
    client: codex_client::CodexClient,
    cwd: &Path,
    install: &str,
    name: &str,
    data: PathBuf,
    thread: Option<ThreadMeta>,
    transcript: Option<Transcript>,
) -> gpui::Entity<Workspace> {
    let cwd = cwd.to_path_buf();
    let install = install.to_string();
    let name = name.to_string();
    cx.new(move |cx| {
        let mut ws = Workspace::with_settings(crate::settings::Settings {
            install_id: install.clone(),
            device_name: name.clone(),
            cwd: Some(cwd.clone()),
            recent_folders: vec![cwd.clone()],
            codex_binary: Some(PathBuf::from("/nonexistent-atrium-test")),
            ..Default::default()
        });
        ws.client = Some(client);
        if let Some(meta) = thread {
            ws.threads.push(meta);
        }
        if let Some(transcript) = transcript {
            ws.transcripts.insert("thread-a".into(), transcript);
            ws.loaded_threads.insert("thread-a".into());
        }
        ws.view = View::Draft;
        ws.start_sync(
            SyncStartup {
                data,
                port: 0,
                install_id: install,
                device_name: name,
                loopback_only: true,
                save_settings: false,
                path: Some(String::new()),
            },
            cx,
        );
        ws
    })
}

fn wait_for(
    cx: &mut TestAppContext,
    seconds: u64,
    mut ready: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !ready(cx) {
        assert!(Instant::now() < deadline, "timed out waiting for sync");
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn sync_startup_accepts_the_documented_overrides() {
    assert_eq!(
        sync_data_dir_choice(Some(PathBuf::from("/tmp/atrium-sync-env-test"))),
        PathBuf::from("/tmp/atrium-sync-env-test")
    );
    assert_eq!(sync_port_choice(Some(7482)), 7482);
    assert_eq!(
        sync_install_id("stored-install", Some("install-from-env")),
        "install-from-env"
    );
    assert_eq!(sync_install_id("stored-install", None), "stored-install");
}

fn workspace(
    cx: &mut TestAppContext,
    client: codex_client::CodexClient,
    cwd: &Path,
    transcript: Transcript,
) -> gpui::Entity<Workspace> {
    let cwd = cwd.to_path_buf();
    cx.new(move |_| {
        let mut ws = Workspace::with_settings(crate::settings::Settings {
            install_id: "alpha".into(),
            device_name: "alpha".into(),
            model: Some("local-model".into()),
            effort: Some("low".into()),
            ..Default::default()
        });
        ws.client = Some(client);
        ws.threads.push(thread_meta("thread-a", &cwd));
        ws.transcripts.insert("thread-a".into(), transcript);
        ws
    })
}

#[gpui::test(iterations = 1)]
fn owner_turn_keeps_the_composer_and_global_settings(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir(&fixture.0).unwrap();
    let cwd = fixture.0.clone();
    let (client, mock) = scripted_client(cwd.clone(), |request| match request["method"].as_str() {
        Some("turn/start") => json!({"turn": {"id": "remote-turn"}}),
        other => panic!("unexpected request {other:?}"),
    });
    let ws = workspace(cx, client, &cwd, Transcript::new());
    ws.update(cx, |ws, _| {
        ws.loaded_threads.insert("thread-a".into());
        ws.draft.push_local_user("my draft", Vec::new());
        ws.view = View::Draft;
    });
    ws.update(cx, |ws, cx| {
        ws.sync_owner_turn(
            "codex".into(),
            "thread-a".into(),
            TurnInput {
                text: "from the laptop".into(),
                attachments: Vec::new(),
                model: Some("remote-model".into()),
                effort: Some("high".into()),
                mode: None,
            },
            false,
            cx,
        )
        .unwrap();
        assert_eq!(ws.view, View::Draft);
        assert_eq!(ws.settings.model.as_deref(), Some("local-model"));
        assert_eq!(ws.settings.effort.as_deref(), Some("low"));
        assert_eq!(ws.draft.entries().len(), 1);
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    while !mock.is_finished() {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(5));
    }
    let seen = mock.join().unwrap();
    let turn = seen
        .iter()
        .find(|request| request["method"] == "turn/start")
        .unwrap();
    assert_eq!(turn["params"]["model"], "remote-model");
    assert_eq!(turn["params"]["effort"], "high");
    assert_eq!(turn["params"]["threadId"], "thread-a");
}

#[gpui::test(iterations = 1)]
fn steer_reaches_turn_steer_instead_of_starting_another_turn(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir(&fixture.0).unwrap();
    let cwd = fixture.0.clone();
    let (client, mock) = scripted_client(cwd.clone(), |request| match request["method"].as_str() {
        Some("turn/steer") => json!({"turnId": "running"}),
        other => panic!("unexpected request {other:?}"),
    });
    let mut transcript = Transcript::new();
    transcript.apply(&atrium_core::AgentEvent::TurnStarted {
        thread_id: "thread-a".into(),
        turn_id: "running".into(),
    });
    let ws = workspace(cx, client, &cwd, transcript);
    ws.update(cx, |ws, _| {
        ws.loaded_threads.insert("thread-a".into());
        ws.view = View::Draft;
    });
    ws.update(cx, |ws, cx| {
        ws.sync_owner_turn(
            "codex".into(),
            "thread-a".into(),
            TurnInput {
                text: "change direction".into(),
                ..Default::default()
            },
            true,
            cx,
        )
        .unwrap();
        assert_eq!(ws.view, View::Draft);
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    while !mock.is_finished() {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(5));
    }
    let seen = mock.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["method"], "turn/steer");
    assert_eq!(seen[0]["params"]["expectedTurnId"], "running");
}

#[gpui::test(iterations = 1)]
fn a_thread_read_snapshot_is_resumed_before_its_turn(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir(&fixture.0).unwrap();
    let cwd = fixture.0.clone();
    let body = thread_json("thread-a", &cwd);
    let (client, mock) = scripted_client(cwd.clone(), move |request| {
        let method = request["method"].as_str().unwrap();
        match method {
            "thread/read" | "thread/resume" => json!({"thread": body}),
            "thread/goal/get" => json!({"goal": null}),
            "turn/start" => json!({"turn": {"id": "after-resume"}}),
            other => panic!("unexpected request {other:?}"),
        }
    });
    let ws = workspace(cx, client, &cwd, Transcript::new());
    let (reply, mut answer) = tokio::sync::oneshot::channel();
    ws.update(cx, |ws, cx| {
        ws.transcripts.remove("thread-a");
        ws.sync_answer_snapshot("codex".into(), "thread-a".into(), reply, cx);
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    while answer.try_recv().is_err() {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(5));
    }
    ws.update(cx, |ws, _| {
        assert!(ws.transcripts.contains_key("thread-a"));
        assert!(!ws.loaded_threads.contains("thread-a"));
    });
    ws.update(cx, |ws, cx| {
        ws.sync_owner_turn(
            "codex".into(),
            "thread-a".into(),
            TurnInput {
                text: "continue after reading".into(),
                ..Default::default()
            },
            false,
            cx,
        )
        .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(6);
    while !mock.is_finished() {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        thread::sleep(Duration::from_millis(5));
    }
    let seen = mock.join().unwrap();
    let methods: Vec<&str> = seen
        .iter()
        .filter_map(|request| request["method"].as_str())
        .collect();
    let methods: Vec<&str> = methods
        .into_iter()
        .filter(|method| *method != "thread/goal/get")
        .collect();
    assert_eq!(methods, vec!["thread/read", "thread/resume", "turn/start"]);
}

#[gpui::test(iterations = 1)]
fn the_first_device_to_answer_an_approval_owns_it(cx: &mut TestAppContext) {
    let fixture = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir(&fixture.0).unwrap();
    let cwd = fixture.0.clone();
    let (client, _mock) = scripted_client(cwd.clone(), |_| Value::Null);
    let mut transcript = Transcript::new();
    transcript.apply(&atrium_core::AgentEvent::ApprovalRequested(
        ApprovalRequest {
            thread_id: "thread-a".into(),
            turn_id: "turn".into(),
            item_id: "item".into(),
            request_id: 7.into(),
            kind: ApprovalKind::Tool {
                title: "Run tests".into(),
                detail: None,
            },
        },
    ));
    let ws = workspace(cx, client, &cwd, transcript);
    ws.update(cx, |ws, cx| {
        let first = ws
            .submit_answer_from(
                "thread-a",
                7.into(),
                super::super::phone::Answer::Approval(ApprovalDecision::Accept),
                "alpha",
                cx,
            )
            .unwrap();
        first.detach();
        assert!(matches!(
            ws.submit_answer_from(
                "thread-a",
                7.into(),
                super::super::phone::Answer::Approval(ApprovalDecision::Decline),
                "beta",
                cx,
            ),
            Err(atrium_phone::Error::Stale)
        ));
        assert_eq!(ws.answered_by("thread-a", &7.into()), Some("alpha"));
    });
}

#[gpui::test(iterations = 1)]
fn two_atriums_publish_read_and_continue_one_conversation(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let root = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir_all(&root.0).unwrap();
    fs::create_dir_all(root.0.join("alpha")).unwrap();
    fs::create_dir_all(root.0.join("beta")).unwrap();
    let alpha_dir = root.0.join("alpha");
    let beta_dir = root.0.join("beta");
    let body = thread_json("thread-a", &alpha_dir);
    let body_for_alpha = body.clone();
    let body_for_beta = body.clone();
    let (alpha_client, alpha_log, alpha_mock) =
        shared_scripted_client(alpha_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body_for_alpha}),
                Some("thread/goal/get") => json!({"goal": null}),
                Some("turn/start") => json!({"turn": {"id": "turn-from-beta"}}),
                other => panic!("alpha mock saw {other:?}"),
            }
        });
    let (beta_client, beta_log, beta_mock) =
        shared_scripted_client(beta_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body_for_beta}),
                Some("thread/goal/get") => json!({"goal": null}),
                other => panic!("beta mock saw {other:?}"),
            }
        });
    let alpha = synced_workspace(
        cx,
        alpha_client,
        &alpha_dir,
        "alpha",
        "studio",
        root.0.join("alpha-data"),
        Some(thread_meta("thread-a", &alpha_dir)),
        None,
    );
    let beta = synced_workspace(
        cx,
        beta_client,
        &beta_dir,
        "beta",
        "laptop",
        root.0.join("beta-data"),
        None,
        None,
    );
    wait_for(cx, 30, |cx| {
        alpha.read_with(cx, |ws, _| ws.sync.is_some())
            && beta.read_with(cx, |ws, _| ws.sync.is_some())
    });
    let alpha_url = alpha.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    let beta_url = beta.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    alpha.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&beta_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("beta".into(), device);
        ws.sync_poll(cx);
    });
    beta.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&alpha_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("alpha".into(), device);
        ws.sync_poll(cx);
    });
    let listed_at = Instant::now();
    wait_for(cx, 60, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.threads.iter().any(|thread| thread.id == "thread-a")
        })
    });
    assert!(
        listed_at.elapsed() < Duration::from_secs(2),
        "the remote conversation took {:?} to appear",
        listed_at.elapsed()
    );
    beta.update(cx, |ws, cx| {
        ws.view = View::Thread("thread-a".into());
        ws.load_thread("thread-a".into(), cx);
    });
    wait_for(cx, 60, |cx| {
        beta.read_with(cx, |ws, _| ws.transcripts.contains_key("thread-a"))
    });
    let read_log = alpha_log.lock().unwrap().clone();
    assert!(
        read_log
            .iter()
            .any(|request| request["method"] == "thread/read")
    );
    assert!(
        !read_log
            .iter()
            .any(|request| request["method"] == "thread/resume"),
        "reading must not resume on the owner"
    );
    alpha.update(cx, |ws, _| {
        ws.draft.push_local_user("draft on studio", Vec::new());
        ws.view = View::Draft;
    });
    beta.update(cx, |ws, cx| {
        assert!(ws.sync_send_remote("thread-a", &Composed::text("from the laptop".into()), cx));
    });
    wait_for(cx, 60, |cx| {
        alpha.read_with(cx, |ws, _| {
            ws.transcripts.get("thread-a").is_some_and(|transcript| {
                transcript.entries().iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        atrium_core::EntryKind::Item(Item::User { text, .. })
                            if text == "from the laptop"
                    )
                })
            })
        })
    });
    assert!(alpha.read_with(cx, |ws, _| ws.view == View::Draft));
    assert_eq!(alpha.read_with(cx, |ws, _| ws.draft.entries().len()), 1);
    let alpha_id = alpha.read_with(cx, |ws, _| {
        ws.transcripts["thread-a"]
            .entries()
            .iter()
            .find_map(|entry| match &entry.kind {
                atrium_core::EntryKind::Item(Item::User { id, text, .. })
                    if text == "from the laptop" =>
                {
                    Some(id.clone())
                }
                _ => None,
            })
            .unwrap()
    });
    wait_for(cx, 60, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.transcripts.get("thread-a").is_some_and(|transcript| {
                transcript.entries().iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        atrium_core::EntryKind::Item(Item::User { id, text, .. })
                            if text == "from the laptop" && id == &alpha_id
                    )
                })
            })
        })
    });
    let alpha_methods: Vec<String> = alpha_log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| request["method"].as_str().map(str::to_owned))
        .collect();
    assert!(alpha_methods.contains(&"thread/resume".into()));
    assert!(alpha_methods.contains(&"turn/start".into()));
    assert!(
        !beta_log
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "turn/start")
    );
    drop(alpha_mock);
    drop(beta_mock);
}

fn pending_approval(request: i64) -> Transcript {
    let mut transcript = Transcript::new();
    transcript.apply(&atrium_core::AgentEvent::ApprovalRequested(
        ApprovalRequest {
            thread_id: "thread-a".into(),
            turn_id: "turn".into(),
            item_id: format!("item-{request}"),
            request_id: request.into(),
            kind: ApprovalKind::Tool {
                title: "Run tests".into(),
                detail: None,
            },
        },
    ));
    transcript
}

#[gpui::test(iterations = 1)]
fn two_atriums_answer_an_approval_once(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let root = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir_all(&root.0).unwrap();
    let alpha_dir = root.0.join("alpha");
    let beta_dir = root.0.join("beta");
    fs::create_dir_all(&alpha_dir).unwrap();
    fs::create_dir_all(&beta_dir).unwrap();
    let body = thread_json("thread-a", &alpha_dir);
    let body_for_alpha = body.clone();
    let body_for_beta = body.clone();
    let (alpha_client, _alpha_log, alpha_mock) =
        shared_scripted_client(alpha_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body_for_alpha}),
                Some("thread/goal/get") => json!({"goal": null}),
                _ => Value::Null,
            }
        });
    let (beta_client, _beta_log, beta_mock) =
        shared_scripted_client(beta_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body_for_beta}),
                Some("thread/goal/get") => json!({"goal": null}),
                _ => Value::Null,
            }
        });
    let mut transcript = pending_approval(7);
    transcript.apply(&atrium_core::AgentEvent::ApprovalRequested(
        ApprovalRequest {
            thread_id: "thread-a".into(),
            turn_id: "turn".into(),
            item_id: "item-8".into(),
            request_id: 8.into(),
            kind: ApprovalKind::Tool {
                title: "Run more tests".into(),
                detail: None,
            },
        },
    ));
    let alpha = synced_workspace(
        cx,
        alpha_client,
        &alpha_dir,
        "alpha",
        "studio",
        root.0.join("alpha-data"),
        Some(thread_meta("thread-a", &alpha_dir)),
        Some(transcript),
    );
    let beta = synced_workspace(
        cx,
        beta_client,
        &beta_dir,
        "beta",
        "laptop",
        root.0.join("beta-data"),
        None,
        None,
    );
    wait_for(cx, 30, |cx| {
        alpha.read_with(cx, |ws, _| ws.sync.is_some())
            && beta.read_with(cx, |ws, _| ws.sync.is_some())
    });
    let alpha_url = alpha.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    let beta_url = beta.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    alpha.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&beta_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("beta".into(), device);
        ws.sync_poll(cx);
    });
    beta.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&alpha_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("alpha".into(), device);
        ws.sync_poll(cx);
    });
    wait_for(cx, 60, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.threads.iter().any(|thread| thread.id == "thread-a")
        })
    });
    beta.update(cx, |ws, cx| {
        ws.view = View::Thread("thread-a".into());
        ws.load_thread("thread-a".into(), cx);
    });
    wait_for(cx, 60, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.transcripts
                .get("thread-a")
                .is_some_and(|transcript| transcript.pending_approvals().count() == 2)
        })
    });

    alpha.update(cx, |ws, cx| {
        ws.submit_answer_from(
            "thread-a",
            7.into(),
            super::super::phone::Answer::Approval(ApprovalDecision::Accept),
            "studio",
            cx,
        )
        .unwrap()
        .detach();
    });
    let (peer, _, backend) = beta
        .read_with(cx, |ws, _| ws.sync_peer_for("thread-a"))
        .unwrap();
    let (reply, answer) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ =
            reply.send(peer.approve(&backend, "thread-a", &json!(7), "decline", Some("laptop")));
    });
    let result = loop {
        match answer.try_recv() {
            Ok(result) => break result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                cx.run_until_parked();
                cx.executor().advance_clock(Duration::from_millis(100));
                thread::sleep(Duration::from_millis(5));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("approval request stopped")
            }
        }
    };
    let error = result.expect_err("the second answer must lose");
    assert!(error.to_string().contains("answered on studio"), "{error}");

    let (peer, _, backend) = beta
        .read_with(cx, |ws, _| ws.sync_peer_for("thread-a"))
        .unwrap();
    let (reply, answer) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = reply.send(peer.approve(&backend, "thread-a", &json!(8), "accept", Some("laptop")));
    });
    let result = loop {
        match answer.try_recv() {
            Ok(result) => break result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                cx.run_until_parked();
                cx.executor().advance_clock(Duration::from_millis(100));
                thread::sleep(Duration::from_millis(5));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("approval request stopped")
            }
        }
    };
    assert!(result.is_ok(), "{result:?}");
    wait_for(cx, 10, |cx| {
        alpha.read_with(cx, |ws, _| {
            ws.answered_by("thread-a", &8.into()) == Some("laptop")
        })
    });
    alpha.update(cx, |ws, cx| {
        assert!(matches!(
            ws.submit_answer_from(
                "thread-a",
                8.into(),
                super::super::phone::Answer::Approval(ApprovalDecision::Decline),
                "studio",
                cx,
            ),
            Err(atrium_phone::Error::Stale)
        ));
    });
    drop(alpha_mock);
    drop(beta_mock);
}

#[gpui::test(iterations = 1)]
fn a_follower_opens_the_cache_while_the_owner_is_offline_then_catches_up(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let root = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir_all(&root.0).unwrap();
    let alpha_dir = root.0.join("alpha");
    let beta_dir = root.0.join("beta");
    fs::create_dir_all(&alpha_dir).unwrap();
    fs::create_dir_all(&beta_dir).unwrap();
    let body = thread_json("thread-a", &alpha_dir);
    let body_for_alpha = body.clone();
    let body_for_beta = body.clone();
    let (alpha_client, _alpha_log, alpha_mock) =
        shared_scripted_client(alpha_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body_for_alpha}),
                Some("thread/goal/get") => json!({"goal": null}),
                Some("turn/start") => json!({"turn": {"id": "turn-while-away"}}),
                _ => Value::Null,
            }
        });
    let (beta_client, _beta_log, beta_mock) =
        shared_scripted_client(beta_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body_for_beta}),
                Some("thread/goal/get") => json!({"goal": null}),
                _ => Value::Null,
            }
        });
    let transcript = Transcript::from_items(vec![Item::User {
        id: "native-old".into(),
        text: "hello from studio".into(),
        attachments: Vec::new(),
    }]);
    let alpha = synced_workspace(
        cx,
        alpha_client,
        &alpha_dir,
        "alpha",
        "studio",
        root.0.join("alpha-data"),
        Some(thread_meta("thread-a", &alpha_dir)),
        Some(transcript),
    );
    let beta = synced_workspace(
        cx,
        beta_client,
        &beta_dir,
        "beta",
        "laptop",
        root.0.join("beta-data"),
        None,
        None,
    );
    wait_for(cx, 30, |cx| {
        alpha.read_with(cx, |ws, _| ws.sync.is_some())
            && beta.read_with(cx, |ws, _| ws.sync.is_some())
    });
    let alpha_url = alpha.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    let beta_url = beta.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    alpha.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&beta_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("beta".into(), device);
        ws.sync_poll(cx);
    });
    let alpha_device = beta.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&alpha_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("alpha".into(), device.clone());
        ws.sync_poll(cx);
        device
    });
    wait_for(cx, 60, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.threads.iter().any(|thread| thread.id == "thread-a")
        })
    });
    beta.update(cx, |ws, cx| {
        ws.view = View::Thread("thread-a".into());
        ws.load_thread("thread-a".into(), cx);
    });
    wait_for(cx, 60, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.sync
                .as_ref()
                .and_then(|sync| sync.cache.get(&alpha_device, "codex", "thread-a"))
                .is_some()
        })
    });
    beta.update(cx, |ws, _| {
        ws.devices
            .iter_mut()
            .find(|device| device.id == alpha_device)
            .unwrap()
            .node = agent_rpc::node::NodeClient::new("http://127.0.0.1:9", None);
        ws.transcripts.remove("thread-a");
        ws.loaded_threads.remove("thread-a");
        ws.sync.as_mut().unwrap().cursors.remove("thread-a");
    });
    beta.update(cx, |ws, cx| {
        ws.load_thread("thread-a".into(), cx);
    });
    wait_for(cx, 30, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.transcripts.get("thread-a").is_some_and(|transcript| {
                transcript.entries().iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        atrium_core::EntryKind::Item(Item::User { text, .. })
                            if text == "hello from studio"
                    )
                })
            })
        })
    });
    assert!(!beta.read_with(cx, |ws, _| { ws.loaded_threads.contains("thread-a") }));
    beta.update(cx, |ws, _| {
        ws.devices
            .iter_mut()
            .find(|device| device.id == alpha_device)
            .unwrap()
            .node = agent_rpc::node::NodeClient::new(&alpha_url, None);
    });
    alpha.update(cx, |ws, cx| {
        ws.sync_owner_turn(
            "codex".into(),
            "thread-a".into(),
            TurnInput {
                text: "while you were away".into(),
                ..Default::default()
            },
            false,
            cx,
        )
        .unwrap();
    });
    wait_for(cx, 30, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.transcripts.get("thread-a").is_some_and(|transcript| {
                transcript.entries().iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        atrium_core::EntryKind::Item(Item::User { text, .. })
                            if text == "while you were away"
                    )
                })
            })
        })
    });
    assert_eq!(
        beta.read_with(cx, |ws, _| {
            ws.transcripts["thread-a"]
                .entries()
                .iter()
                .filter(|entry| {
                    matches!(
                        &entry.kind,
                        atrium_core::EntryKind::Item(Item::User { text, .. })
                            if text == "hello from studio"
                    )
                })
                .count()
        }),
        1
    );
    drop(alpha_mock);
    drop(beta_mock);
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Atrium Test")
        .env("GIT_AUTHOR_EMAIL", "atrium@example.invalid")
        .env("GIT_COMMITTER_NAME", "Atrium Test")
        .env("GIT_COMMITTER_EMAIL", "atrium@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[gpui::test(iterations = 1)]
fn two_atriums_move_a_conversation_and_its_uncommitted_tree(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let root = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir_all(&root.0).unwrap();
    let repo_dir = root.0.join("repo");
    fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init", "-q"]);
    fs::write(repo_dir.join("base.txt"), "base\n").unwrap();
    git(&repo_dir, &["add", "base.txt"]);
    git(&repo_dir, &["commit", "-qm", "base"]);
    fs::write(repo_dir.join("moved.txt"), "uncommitted\n").unwrap();
    let alpha_home = root.0.join("alpha-codex");
    let beta_home = root.0.join("beta-codex");
    let session_dir = alpha_home.join("sessions/2026/11/21");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("rollout-thread-a.jsonl"),
        br#"{"id":"thread-a"}"#,
    )
    .unwrap();
    let body = thread_json("thread-a", &repo_dir);
    let body_for_beta = body.clone();
    let (mut alpha_client, _alpha_log, alpha_mock) =
        shared_scripted_client(repo_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body}),
                Some("thread/goal/get") => json!({"goal": null}),
                _ => Value::Null,
            }
        });
    alpha_client.codex_home = alpha_home.display().to_string();
    let (mut beta_client, _beta_log, beta_mock) =
        shared_scripted_client(repo_dir.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/read" | "thread/resume") => json!({"thread": body_for_beta}),
                Some("thread/goal/get") => json!({"goal": null}),
                _ => Value::Null,
            }
        });
    beta_client.codex_home = beta_home.display().to_string();
    let mut transcript = Transcript::new();
    transcript.push_local_user("move this", Vec::new());
    let alpha = synced_workspace(
        cx,
        alpha_client,
        &repo_dir,
        "alpha",
        "studio",
        root.0.join("alpha-data"),
        Some(thread_meta("thread-a", &repo_dir)),
        Some(transcript),
    );
    let beta = synced_workspace(
        cx,
        beta_client,
        &repo_dir,
        "beta",
        "laptop",
        root.0.join("beta-data"),
        None,
        None,
    );
    wait_for(cx, 30, |cx| {
        alpha.read_with(cx, |ws, _| ws.sync.is_some())
            && beta.read_with(cx, |ws, _| ws.sync.is_some())
    });
    let beta_url = beta.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    let alpha_url = alpha.read_with(cx, |ws, _| ws.sync.as_ref().unwrap()._node.local_url());
    alpha.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&beta_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("beta".into(), device);
        ws.sync_poll(cx);
    });
    let alpha_device = beta.update(cx, |ws, cx| {
        let device = ws.attach_test_node(&alpha_url, None);
        ws.sync
            .as_mut()
            .unwrap()
            .devices
            .insert("alpha".into(), device.clone());
        ws.sync
            .as_mut()
            .unwrap()
            .owners
            .insert("thread-a".into(), "alpha".into());
        ws.sync_poll(cx);
        device
    });
    let _ = alpha_device;
    beta.update(cx, |ws, cx| {
        ws.move_to_this_computer("thread-a".into(), cx);
    });
    wait_for(cx, 30, |cx| {
        beta.read_with(cx, |ws, _| {
            ws.loaded_threads.contains("thread-a")
                && ws
                    .threads
                    .iter()
                    .find(|thread| thread.id == "thread-a")
                    .is_some_and(|thread| thread.cwd != repo_dir.display().to_string())
        })
    });
    let folder = beta.read_with(cx, |ws, _| {
        PathBuf::from(
            &ws.threads
                .iter()
                .find(|thread| thread.id == "thread-a")
                .unwrap()
                .cwd,
        )
    });
    assert_eq!(
        fs::read_to_string(folder.join("moved.txt")).unwrap(),
        "uncommitted\n"
    );
    assert!(beta.read_with(cx, |ws, _| {
        ws.sync
            .as_ref()
            .unwrap()
            .state
            .store()
            .registers()
            .iter()
            .any(|(key, register)| {
                key.ends_with("/moved_to") && register.value == Some(json!("beta"))
            })
    }));
    let resume_body = thread_json("thread-a", &repo_dir);
    let (mut alpha_again, _alpha_again_log, alpha_again_mock) =
        shared_scripted_client(alpha_home.clone(), move |request| {
            match request["method"].as_str() {
                Some("thread/resume") => json!({"thread": resume_body}),
                Some("thread/goal/get") => json!({"goal": null}),
                _ => Value::Null,
            }
        });
    alpha_again.codex_home = alpha_home.display().to_string();
    alpha.update(cx, |ws, _| {
        ws.client = Some(alpha_again);
    });
    wait_for(cx, 60, |cx| {
        alpha.read_with(cx, |ws, _| {
            ws.threads
                .iter()
                .any(|thread| thread.id == "thread-a" && thread.backend.ends_with("@laptop"))
        })
    });
    alpha.update(cx, |ws, cx| {
        ws.move_to_this_computer("thread-a".into(), cx);
    });
    wait_for(cx, 30, |cx| {
        alpha.read_with(cx, |ws, _| {
            ws.loaded_threads.contains("thread-a")
                && ws
                    .threads
                    .iter()
                    .find(|thread| thread.id == "thread-a")
                    .is_some_and(|thread| thread.cwd != repo_dir.display().to_string())
        })
    });
    let back = alpha.read_with(cx, |ws, _| {
        PathBuf::from(
            &ws.threads
                .iter()
                .find(|thread| thread.id == "thread-a")
                .unwrap()
                .cwd,
        )
    });
    assert_ne!(back, folder);
    assert_eq!(
        fs::read_to_string(back.join("moved.txt")).unwrap(),
        "uncommitted\n"
    );
    assert!(alpha.read_with(cx, |ws, _| {
        ws.sync
            .as_ref()
            .unwrap()
            .state
            .store()
            .registers()
            .iter()
            .any(|(key, register)| {
                key.contains("/moved_to") && register.value == Some(json!("alpha"))
            })
    }));
    let copied = fs::read_dir(&beta_home)
        .unwrap()
        .flatten()
        .flat_map(|entry| walk_test_paths(&entry.path()))
        .any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("thread-a"))
        });
    assert!(copied, "the rollout file must be copied to the target");
    let _ = atrium_project::repo::remove_worktree(&folder);
    let _ = atrium_project::repo::remove_worktree(&back);
    drop(alpha_mock);
    drop(alpha_again_mock);
    drop(beta_mock);
}

fn walk_test_paths(path: &Path) -> Vec<PathBuf> {
    if path.is_dir() {
        fs::read_dir(path)
            .unwrap()
            .flatten()
            .flat_map(|entry| walk_test_paths(&entry.path()))
            .collect()
    } else {
        vec![path.to_path_buf()]
    }
}

#[gpui::test(iterations = 1)]
fn two_atriums_hand_the_scheduler_lease_over_after_it_expires(cx: &mut TestAppContext) {
    let root = Fixture(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
    fs::create_dir_all(&root.0).unwrap();
    let project = root.0.join("project");
    fs::create_dir_all(&project).unwrap();
    let alpha_dir = root.0.join("alpha");
    let beta_dir = root.0.join("beta");
    fs::create_dir_all(&alpha_dir).unwrap();
    fs::create_dir_all(&beta_dir).unwrap();
    let (alpha_client, _alpha_log, alpha_mock) =
        shared_scripted_client(alpha_dir.clone(), |_| Value::Null);
    let (beta_client, _beta_log, beta_mock) =
        shared_scripted_client(beta_dir.clone(), |_| Value::Null);
    let alpha = synced_workspace(
        cx,
        alpha_client,
        &alpha_dir,
        "alpha",
        "studio",
        root.0.join("alpha-data"),
        None,
        None,
    );
    let beta = synced_workspace(
        cx,
        beta_client,
        &beta_dir,
        "beta",
        "laptop",
        root.0.join("beta-data"),
        None,
        None,
    );
    wait_for(cx, 30, |cx| {
        alpha.read_with(cx, |ws, _| ws.sync.is_some())
            && beta.read_with(cx, |ws, _| ws.sync.is_some())
    });
    assert!(alpha.update(cx, |ws, _| ws.sync_scheduler_allowed(&project)));
    let key = alpha.read_with(cx, |ws, _| {
        ws.sync
            .as_ref()
            .unwrap()
            .state
            .store()
            .registers()
            .iter()
            .find(|(key, _)| key.starts_with("project/") && key.ends_with("/scheduler"))
            .map(|(key, _)| key.clone())
            .unwrap()
    });
    let ops = alpha.read_with(cx, |ws, _| {
        let sync = ws.sync.as_ref().unwrap();
        beta.read_with(cx, |beta, _| {
            sync.state
                .missing_for(&beta.sync.as_ref().unwrap().state.vector())
        })
    });
    beta.update(cx, |ws, _| {
        ws.sync.as_ref().unwrap().state.merge(ops).unwrap();
    });
    assert!(!beta.update(cx, |ws, _| ws.sync_scheduler_allowed(&project)));

    let expired = Lease {
        device: "alpha".into(),
        until_ms: atrium_node::sync::now_ms()
            - atrium_sync::LEASE_RENEW_MS
            - atrium_sync::LEASE_TAKEOVER_GRACE_MS
            - 1,
    };
    alpha.update(cx, |ws, _| {
        ws.sync
            .as_ref()
            .unwrap()
            .state
            .set(key.clone(), serde_json::to_value(expired).unwrap())
            .unwrap();
    });
    let ops = alpha.read_with(cx, |ws, _| {
        let sync = ws.sync.as_ref().unwrap();
        beta.read_with(cx, |beta, _| {
            sync.state
                .missing_for(&beta.sync.as_ref().unwrap().state.vector())
        })
    });
    beta.update(cx, |ws, _| {
        ws.sync.as_ref().unwrap().state.merge(ops).unwrap();
    });
    assert!(beta.update(cx, |ws, _| ws.sync_scheduler_allowed(&project)));
    let beta_lease = beta.update(cx, |ws, _| {
        serde_json::from_value::<Lease>(
            ws.sync
                .as_ref()
                .unwrap()
                .state
                .store()
                .value(&key)
                .unwrap()
                .clone(),
        )
        .unwrap()
    });
    assert_eq!(beta_lease.device, "beta");
    let ops = beta.read_with(cx, |ws, _| {
        let sync = ws.sync.as_ref().unwrap();
        alpha.read_with(cx, |alpha, _| {
            sync.state
                .missing_for(&alpha.sync.as_ref().unwrap().state.vector())
        })
    });
    alpha.update(cx, |ws, _| {
        ws.sync.as_ref().unwrap().state.merge(ops).unwrap();
    });
    assert!(!alpha.update(cx, |ws, _| ws.sync_scheduler_allowed(&project)));
    drop(alpha_mock);
    drop(beta_mock);
}
