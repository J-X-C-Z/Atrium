use super::*;
use crate::{
    project::{Role, Workstream},
    settings::Settings,
    workspace::Status,
};
use atrium_core::{AgentEvent, ApprovalKind, ApprovalRequest, Question, UserInputRequest};
use gpui::{AppContext, TestAppContext};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    time::Instant,
};

#[gpui::test(iterations = 1)]
fn phone_api_answers_live_requests_and_posts_to_the_desktop(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let root = std::env::temp_dir().join(format!("atrium-phone-{}", uuid::Uuid::new_v4()));
    let mut p = Project::open(&root).unwrap();
    p.file.streams.push(Workstream {
        id: "lead".into(),
        name: "Lead".into(),
        role: Role::Lead,
        thread_id: Some("t".into()),
        backend: "codex".into(),
        ..Default::default()
    });
    p.save().unwrap();
    let backend = TcpListener::bind("127.0.0.1:0").unwrap();
    let sock = TcpStream::connect(backend.local_addr().unwrap()).unwrap();
    let (mut peer, _) = backend.accept().unwrap();
    drop(backend);
    let (connection, _) =
        agent_rpc::Connection::new(BufReader::new(sock.try_clone().unwrap()), sock);
    let mock = std::thread::spawn(move || {
        peer.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let mut received = Vec::new();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            let rpc: Value = serde_json::from_str(&line).unwrap();
            if rpc["method"] == "turn/start" {
                assert_eq!(rpc["params"]["threadId"], "t");
                assert!(
                    rpc["params"]["input"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("continue from phone")
                );
                writeln!(
                    peer,
                    "{}",
                    json!({"id":rpc["id"],"result":{"turn":{"id":"phone-turn"}}})
                )
                .unwrap();
                break;
            }
            received.push(rpc);
        }
        received
    });
    let mut t = Transcript::new();
    t.apply(&AgentEvent::ApprovalRequested(ApprovalRequest {
        thread_id: "t".into(),
        turn_id: "turn".into(),
        item_id: "item".into(),
        request_id: 7.into(),
        kind: ApprovalKind::Tool {
            title: "Run".into(),
            detail: None,
        },
    }));
    t.apply(&AgentEvent::UserInputRequested(UserInputRequest {
        thread_id: "t".into(),
        turn_id: "turn".into(),
        item_id: "form".into(),
        request_id: "q".into(),
        questions: vec![Question {
            id: "color".into(),
            header: "Color".into(),
            question: "Which?".into(),
            options: vec![("Blue".into(), String::new())],
            allow_other: false,
            secret: false,
        }],
    }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let ws = cx.new(|cx| {
        let mut ws = Workspace::with_settings(Settings {
            projects: vec![root.clone()],
            ..Default::default()
        });
        ws.sync_phone(cx);
        assert!(ws.phone.is_none());
        ws.client = Some(codex_client::CodexClient::over(connection));
        ws.status = Status::Ready;
        ws.transcripts.insert("t".into(), t);
        ws.init_projects(cx);
        ws.settings.remote_view = true;
        let (server, commands) = Server::loopback_for_test(listener).unwrap();
        ws.attach_phone(server, commands, cx);
        ws
    });
    let client = std::thread::spawn(move || {
        let base = format!("http://{addr}");
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();
        let deadline = Instant::now() + Duration::from_secs(8);
        let get = |path| {
            http.get(&format!("{base}{path}"))
                .call()
                .unwrap()
                .into_json::<Value>()
                .unwrap()
        };
        loop {
            if get("/api/projects").get(0).is_some() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let post = |path, body| match http.post(&format!("{base}{path}")).send_json(body) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response.status(),
            Err(error) => panic!("POST {path} failed: {error}"),
        };
        assert_eq!(
            get("/api/projects/0")["streams"][0]["needs_you"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let approve = json!({"thread":"t","request_id":7,"decision":"accept_for_session"});
        assert_eq!(post("/api/approvals", approve.clone()), 204);
        assert_eq!(post("/api/approvals", approve), 409);
        let question = |choice| json!({"thread":"t","request_id":"q","answers":{"color":[choice]}});
        assert_eq!(post("/api/questions", question("Red")), 400);
        assert_eq!(post("/api/questions", question("Blue")), 204);
        assert_eq!(
            post("/api/threads/private/messages", json!({"text":"hi"})),
            404
        );
        assert_eq!(
            post(
                "/api/projects/0/notices",
                json!({"text":"phone receipt","to":["lead"]})
            ),
            204
        );
        loop {
            let p = get("/api/projects/0");
            if p["notices"][0]["text"] == "phone receipt" && p["needs_you"] == 0 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            post(
                "/api/threads/t/messages",
                json!({"text":"continue from phone"})
            ),
            202
        );
    });
    let deadline = Instant::now() + Duration::from_secs(12);
    while !client.is_finished() {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    client.join().unwrap();
    ws.update(cx, |ws, cx| {
        assert_eq!(
            super::super::projects::waiting_on_user(&ws.transcripts["t"]),
            0
        );
        assert_eq!(ws.projects[0].notices.last().unwrap().from, YOU);
        ws.settings.remote_view = false;
        ws.sync_phone(cx);
    });
    while TcpStream::connect(addr).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let lines = mock.join().unwrap();
    assert!(lines.contains(&json!({"id":7,"result":{"decision":"acceptForSession"}})));
    assert!(lines.contains(&json!({"id":"q","result":{"answers":{"color":{"answers":["Blue"]}}}})));
    drop(ws);
    cx.run_until_parked();
    fs::remove_dir_all(root).unwrap();
}
