//! The `atrium` MCP server over the node's `/v1/mcp` route: an agent on
//! another computer calls the tools and the scheduler's files change; a
//! caller that is not on the tailnet, or not paired, is refused.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use atrium_node::{
    config::Config,
    server::{self, State},
    sync::{HostError, HostFuture, SyncHost, SyncState},
};
use atrium_project::{Project, Role, TaskStatus, Workstream, mcp};
use atrium_sync::{
    AnswerInput, ApprovalAnswer, DeviceDirectory, SYNC_VERSION, ThreadAnnouncement, ThreadSnapshot,
    TurnInput,
};
use axum::{
    body::Body,
    extract::connect_info::MockConnectInfo,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// What the app does: serves the one project it runs, through `mcp::handle`.
struct Host {
    root: PathBuf,
}

impl SyncHost for Host {
    fn threads<'a>(&'a self, _: Option<i64>) -> HostFuture<'a, Vec<ThreadAnnouncement>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn snapshot<'a>(&'a self, _: &'a str, _: &'a str) -> HostFuture<'a, ThreadSnapshot> {
        Box::pin(async { Err(HostError::NotFound) })
    }
    fn turn<'a>(&'a self, _: &'a str, _: &'a str, _: TurnInput) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn steer<'a>(&'a self, _: &'a str, _: &'a str, _: TurnInput) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn interrupt<'a>(&'a self, _: &'a str, _: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn approve<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Value,
        _: ApprovalAnswer,
    ) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Value,
        _: AnswerInput,
    ) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn mcp<'a>(
        &'a self,
        project: &'a str,
        stream: &'a str,
        message: Value,
    ) -> HostFuture<'a, Option<Value>> {
        Box::pin(async move {
            if std::path::Path::new(project) != self.root {
                return Err(HostError::BadRequest("not a project here".into()));
            }
            let target = mcp::Target {
                root: self.root.clone(),
                stream: stream.to_string(),
            };
            tokio::task::spawn_blocking(move || mcp::handle(&target, &message))
                .await
                .map_err(|e| HostError::Unavailable(e.to_string()))
        })
    }
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Atrium Workspace/photo");
    std::fs::create_dir_all(&root).unwrap();
    let mut project = Project::open(&root).unwrap();
    project.file.streams.push(Workstream {
        id: "boss".into(),
        role: Role::Controller,
        ..Default::default()
    });
    project.save().unwrap();
    (dir, root)
}

async fn state(root: PathBuf) -> (Arc<State>, Arc<SyncState>) {
    let data = tempfile::tempdir().unwrap();
    let sync = SyncState::open(
        data.path(),
        DeviceDirectory {
            install_id: "me".into(),
            device_name: "linux".into(),
            os: "linux".into(),
            app_version: "0.1.0".into(),
            sync_version: SYNC_VERSION,
            owner: "me@example.com".into(),
        },
        Some(Arc::new(Host { root })),
    )
    .unwrap();
    std::mem::forget(data);
    let state = State::new_with_sync(
        Arc::new(Config::default()),
        String::new(),
        Some(sync.clone()),
    )
    .await
    .unwrap();
    (state, sync)
}

fn from(state: &Arc<State>, peer: [u8; 4]) -> axum::Router {
    server::router(state.clone()).layer(MockConnectInfo(SocketAddr::from((peer, 5000))))
}

async fn post(
    app: &axum::Router,
    root: &std::path::Path,
    stream: &str,
    token: Option<&str>,
    message: Value,
) -> (StatusCode, Value) {
    let uri = format!(
        "/v1/mcp?project={}&stream={stream}",
        root.display().to_string().replace(' ', "%20")
    );
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", "linux:7381")
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(message.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

/// A paired device's token, taken the way a device pairs: with the code
/// from the loopback.
async fn paired_token(state: &Arc<State>) -> String {
    let local = from(state, [127, 0, 0, 1]);
    let request = |uri: &str, body: Body| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(body)
            .unwrap()
    };
    let started = local
        .clone()
        .oneshot(request("/v1/sync/pair/start", Body::empty()))
        .await
        .unwrap();
    let started: Value =
        serde_json::from_slice(&started.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let joined = local
        .oneshot(request(
            "/v1/sync/pair/join",
            Body::from(json!({"code": started["code"]}).to_string()),
        ))
        .await
        .unwrap();
    let joined: Value =
        serde_json::from_slice(&joined.into_body().collect().await.unwrap().to_bytes()).unwrap();
    joined["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_remote_session_calls_whoami_and_finish_task_and_the_task_file_changes() {
    let (_dir, root) = project();
    let (state, _sync) = state(root.clone()).await;
    let token = paired_token(&state).await;
    // The remote device's address on the tailnet; it is paired.
    let remote = from(&state, [100, 64, 0, 2]);

    let (status, reply) = post(
        &remote,
        &root,
        "boss",
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["result"]["serverInfo"]["name"], "atrium");
    let (status, _) = post(
        &remote,
        &root,
        "boss",
        Some(&token),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "a notification has no reply");

    // The controller asks for a lead on the other computer.
    let (_, reply) = post(
        &remote,
        &root,
        "boss",
        Some(&token),
        call(
            2,
            "create_task",
            json!({"title":"Perf","goal":"Measure it.","scope":["perf/**"],
                   "phases":[{"title":"Run","steps":["Bench"]}]}),
        ),
    )
    .await;
    assert!(reply["result"]["isError"].is_null(), "{reply}");
    assert!(root.join(".atrium/tasks/boss/perf.md").exists());

    // The scheduler starts it there: a workstream on `win`.
    let mut project = Project::load(&root).unwrap();
    let task = project.task("boss/perf").unwrap().clone();
    let lead = project.stream_from_task(&task, "codex@win").unwrap();
    project.file.streams.push(lead.clone());
    project.save().unwrap();

    let (status, reply) = post(
        &remote,
        &root,
        &lead.id,
        Some(&token),
        call(3, "whoami", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Bench"), "{text}");

    let (status, reply) = post(
        &remote,
        &root,
        &lead.id,
        Some(&token),
        call(
            4,
            "finish_task",
            json!({"status":"done","result":"Ran the benchmark; 12% faster."}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(reply["result"]["isError"].is_null(), "{reply}");
    let task = Project::load(&root)
        .unwrap()
        .task("boss/perf")
        .unwrap()
        .clone();
    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(
        task.result.as_deref(),
        Some("Ran the benchmark; 12% faster.")
    );
}

#[tokio::test]
async fn callers_off_the_tailnet_or_unpaired_are_refused() {
    let (_dir, root) = project();
    let (state, _sync) = state(root.clone()).await;
    let token = paired_token(&state).await;
    let whoami = call(1, "whoami", json!({}));
    // From the open internet or a LAN: refused, token or not.
    for peer in [[203, 0, 113, 9], [192, 168, 1, 5]] {
        let app = from(&state, peer);
        let (status, _) = post(&app, &root, "boss", None, whoami.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{peer:?}");
        let (status, _) = post(&app, &root, "boss", Some(&token), whoami.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{peer:?} with a token");
    }
    // On the tailnet but not the owner's (no identity, no token): refused.
    let tailnet = from(&state, [100, 64, 0, 2]);
    let (status, _) = post(&tailnet, &root, "boss", None, whoami.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A wrong token is refused outright.
    let (status, _) = post(&tailnet, &root, "boss", Some("nope"), whoami.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A browser page cannot borrow the loopback.
    let local = from(&state, [127, 0, 0, 1]);
    let response = local
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp?project=x&stream=boss")
                .header("host", "localhost")
                .header("origin", "https://evil.example")
                .body(Body::from(whoami.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // The app decides which projects are served.
    let (status, _) = post(
        &tailnet,
        std::path::Path::new("/etc"),
        "boss",
        Some(&token),
        whoami,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
