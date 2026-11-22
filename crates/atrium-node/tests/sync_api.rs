use std::{net::SocketAddr, sync::Arc};

use atrium_node::{
    config::Config,
    server::{self, State},
    sync::{HostError, HostFuture, SyncHost, SyncState},
};
use atrium_sync::{
    AnswerInput, ApprovalAnswer, DeviceDirectory, SYNC_VERSION, ThreadAnnouncement, ThreadSnapshot,
    TurnInput,
};
use axum::{
    body::Body,
    extract::connect_info::MockConnectInfo,
    http::{Request, StatusCode},
    response::Response,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct MockHost;

impl SyncHost for MockHost {
    fn threads<'a>(&'a self, _since: Option<i64>) -> HostFuture<'a, Vec<ThreadAnnouncement>> {
        Box::pin(async {
            Ok(vec![ThreadAnnouncement {
                id: "t-1".into(),
                backend: "codex".into(),
                name: Some("Hello".into()),
                preview: "hello".into(),
                cwd: "/tmp".into(),
                model: None,
                created_at: 1,
                updated_at: 2,
                owner: "me".into(),
                state: "idle".into(),
            }])
        })
    }

    fn snapshot<'a>(&'a self, backend: &'a str, id: &'a str) -> HostFuture<'a, ThreadSnapshot> {
        Box::pin(async move {
            if backend != "codex" || id != "t-1" {
                return Err(HostError::NotFound);
            }
            Ok(ThreadSnapshot {
                thread: ThreadAnnouncement {
                    id: "t-1".into(),
                    backend: "codex".into(),
                    name: Some("Hello".into()),
                    preview: "hello".into(),
                    cwd: "/tmp".into(),
                    model: None,
                    created_at: 1,
                    updated_at: 2,
                    owner: "me".into(),
                    state: "idle".into(),
                },
                items: vec![atrium_core::Item::Agent {
                    id: "i-1".into(),
                    text: "hello".into(),
                }],
                approvals: Vec::new(),
                questions: Vec::new(),
                cursor: 1,
            })
        })
    }

    fn turn<'a>(
        &'a self,
        _backend: &'a str,
        _id: &'a str,
        _input: TurnInput,
    ) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn steer<'a>(
        &'a self,
        _backend: &'a str,
        _id: &'a str,
        _input: TurnInput,
    ) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn interrupt<'a>(&'a self, _backend: &'a str, _id: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn approve<'a>(
        &'a self,
        _backend: &'a str,
        _id: &'a str,
        _request: Value,
        _answer: ApprovalAnswer,
    ) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn answer<'a>(
        &'a self,
        _backend: &'a str,
        _id: &'a str,
        _request: Value,
        _input: AnswerInput,
    ) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

async fn app() -> (axum::Router, Arc<SyncState>) {
    let dir = tempfile::tempdir().unwrap();
    let sync = SyncState::open(
        dir.path(),
        DeviceDirectory {
            install_id: "me".into(),
            device_name: "studio".into(),
            os: "macos".into(),
            app_version: "0.1.0".into(),
            sync_version: SYNC_VERSION,
            owner: "me@example.com".into(),
        },
        Some(Arc::new(MockHost)),
    )
    .unwrap();
    // Keep the temporary directory alive for the state's lifetime by
    // leaking it in this test process.
    std::mem::forget(dir);
    let state = State::new_with_sync(
        Arc::new(Config::default()),
        String::new(),
        Some(sync.clone()),
    )
    .await
    .unwrap();
    let app =
        server::router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 5000))));
    (app, sync)
}

async fn call(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response: Response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn the_directory_and_threads_are_served() {
    let (app, _sync) = app().await;
    let (status, directory) = call(
        &app,
        Request::builder()
            .uri("/v1/atrium")
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(directory["install_id"], "me");
    assert_eq!(directory["sync_version"], SYNC_VERSION);

    let (status, threads) = call(
        &app,
        Request::builder()
            .uri("/v1/sync/threads")
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(threads["threads"][0]["id"], "t-1");
}

#[tokio::test]
async fn overlay_ops_round_trip() {
    let (app, sync) = app().await;
    let op = atrium_sync::Op::set(
        "thread/me/codex/t-1/title",
        json!("New"),
        atrium_sync::Hlc::new(10, 0, "other"),
    )
    .unwrap();
    let (status, answer) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/sync/ops")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Body::from(json!({"ops": [op]}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["applied"], 1);
    assert_eq!(
        sync.store().value("thread/me/codex/t-1/title"),
        Some(&json!("New"))
    );
}

#[tokio::test]
async fn a_pairing_token_is_accepted_as_a_bearer() {
    let (app, _sync) = app().await;
    let (status, started) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/sync/pair/start")
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let code = started["code"].as_str().unwrap().to_string();
    let (status, joined) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/sync/pair/join")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Body::from(json!({"code": code}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = joined["token"].as_str().unwrap();
    let (status, _) = call(
        &app,
        Request::builder()
            .uri("/v1/sync/vector")
            .header("host", "localhost")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_attachment_lands_in_the_owner_inbox() {
    let (app, _sync) = app().await;
    let (status, answer) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/v1/sync/inbox?name=note.txt")
            .header("host", "localhost")
            .body(Body::from("hello"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let path = answer["path"].as_str().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"hello");
    assert!(path.ends_with("note.txt"));
}
