//! The node's own engine routes over a real socket: create a thread, replay
//! from a cursor, and see a send from another client arrive once.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use atrium_node::{config::Config, server};
use serde_json::{Value, json};
use tungstenite::Message;

fn app_thread(id: &str) -> Value {
    json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": id,
        "projectId": "project-1",
        "title": "Thread",
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": {
            "parentThreadId": null,
            "relationshipToParent": null,
            "rootThreadId": id,
        },
        "forkedFrom": null,
        "createdAt": "2026-11-24T00:00:00.000Z",
        "updatedAt": "2026-11-24T00:00:00.000Z",
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "deletedAt": null,
    })
}

fn read_text(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> Value {
    loop {
        if let Message::Text(text) = socket.read().unwrap() {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_replays_from_the_cursor_and_a_second_device_sends_through_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        roots: vec![dir.path().to_path_buf()],
        engine_db: Some(dir.path().join("threads.sqlite")),
        loopback_only: true,
        discover_magpie: false,
        ..Config::default()
    };
    let state = server::State::new(Arc::new(config), "/usr/bin:/bin".into())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            server::router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let created: Value = ureq::post(&format!("http://{address}/v1/threads"))
        .send_json(json!({
            "commandId": "command:create",
            "thread": app_thread("thread-api"),
        }))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(created["sequence"], 1);
    let sent: Value = ureq::post(&format!("http://{address}/v1/threads/thread-api/send"))
        .send_json(json!({ "messageId": "message-1", "text": "hello" }))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(sent["sequence"], 2);

    let result = tokio::task::spawn_blocking(move || {
        let (mut socket, _) = tungstenite::connect(format!(
            "ws://{address}/v1/threads/thread-api/attach?after=1"
        ))
        .unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
        }
        // The cursor sits after thread.created, so the log covers the gap and
        // the node replays the message instead of sending a snapshot.
        let replayed = read_text(&mut socket);
        assert_eq!(replayed["kind"], "event");
        assert_eq!(replayed["sequence"], 2);
        assert_eq!(replayed["event"]["type"], "message.updated");
        assert_eq!(read_text(&mut socket)["kind"], "synchronized");

        // A second Atrium sends through the owning node.
        ureq::post(&format!("http://{address}/v1/threads/thread-api/send"))
            .send_json(json!({ "messageId": "message-2", "text": "again" }))
            .unwrap();

        let message = read_text(&mut socket);
        assert_eq!(message["kind"], "event");
        assert_eq!(message["sequence"], 3);
        assert_eq!(message["event"]["payload"]["text"], "again");
    })
    .await;
    server.abort();
    let _ = server.await;
    result.unwrap();
}
