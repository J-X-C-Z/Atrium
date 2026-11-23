//! `GET /v1/agents/claude/usage` with the token and the HTTP call stubbed:
//! the windows come back, the token never does, and no login gives 204.

use std::{net::SocketAddr, sync::Arc};

use agent_rpc::claude_usage::ClaudeUsage;
use atrium_node::{
    config::Config,
    server::{self, State},
};
use axum::{
    body::Body,
    extract::connect_info::MockConnectInfo,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn get(state: &Arc<State>) -> (StatusCode, String) {
    let app = server::router(state.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 5000))));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/agents/claude/usage")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn usage_is_served_without_the_token() {
    let state = State::new(Arc::new(Config::default()), String::new())
        .await
        .unwrap();
    state.set_claude_usage(ClaudeUsage::with(
        || Some("secret-token".into()),
        |token| {
            assert_eq!(token, "secret-token");
            Ok(json!({
                "five_hour": {"utilization": 20.0, "resets_at": "2025-11-04T04:59:59Z"},
                "seven_day": {"utilization": 55.5, "resets_at": "2025-11-06T03:59:59Z"},
                "token": "secret-token"
            }))
        },
    ));
    let (status, body) = get(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("secret-token"));
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["five_hour"]["utilization"], 20.0);
    assert_eq!(value["seven_day"]["resets_at"], "2025-11-06T03:59:59Z");
}

#[tokio::test]
async fn no_token_or_a_failed_call_is_no_content() {
    let state = State::new(Arc::new(Config::default()), String::new())
        .await
        .unwrap();
    state.set_claude_usage(ClaudeUsage::with(
        || None,
        |_| panic!("no call without a token"),
    ));
    assert_eq!(get(&state).await.0, StatusCode::NO_CONTENT);
    state.set_claude_usage(ClaudeUsage::with(
        || Some("t".into()),
        |_| Err("offline".into()),
    ));
    assert_eq!(get(&state).await.0, StatusCode::NO_CONTENT);
}
