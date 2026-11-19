use super::*;
use axum::extract::connect_info::MockConnectInfo;
use serde_json::json;
use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
};

#[tokio::test]
async fn localhost_routes_events_and_browser_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, snapshots) = watch::channel(Arc::new(vec![ProjectSnapshot {
        summary: json!({"name":"demo"}),
        detail: json!({"root":"/demo","name":"demo"}),
        threads: BTreeMap::from([("t".into(), Arc::new(vec![json!("first"), json!("next")]))]),
        pending: vec![json!({"stream":"lead","thread":"t","kind":"approval","request_id":4})],
    }]));
    let (commands, mut receive) = mpsc::channel(1);
    let router = http::router(
        State {
            snapshots,
            commands,
        },
        auth::Guard::new(addr.ip(), addr.port(), None),
    );
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let worker = tokio::spawn(async move {
        let cmd = receive.recv().await.unwrap();
        assert!(matches!(
            cmd.action,
            Action::Approval {
                decision: Decision::Accept,
                ..
            }
        ));
        cmd.reply.send(Ok(())).unwrap();
        let cmd = receive.recv().await.unwrap();
        assert!(
            matches!(cmd.action, Action::Notice { project:0, ref root, .. } if root == "/demo")
        );
        cmd.reply.send(Ok(())).unwrap();
        tx.send_replace(Arc::default());
    });
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}");
        let client = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();
        let get = |path| client.get(&format!("{base}{path}"));
        let post = |path| client.post(&format!("{base}{path}"));
        assert_eq!(
            get("/api/projects")
                .call()
                .unwrap()
                .into_json::<Value>()
                .unwrap()[0]["name"],
            "demo"
        );
        assert_eq!(
            get("/api/threads/t?after=0")
                .call()
                .unwrap()
                .into_json::<Value>()
                .unwrap(),
            json!(["next"])
        );
        let mut sse = BufReader::new(get("/api/events").call().unwrap().into_reader());
        let read_event = |sse: &mut BufReader<_>| {
            let mut line = String::new();
            loop {
                line.clear();
                assert!(sse.read_line(&mut line).unwrap() > 0);
                if let Some(data) = line.strip_prefix("data:") {
                    break serde_json::from_str::<Value>(data).unwrap();
                }
            }
        };
        assert_eq!(read_event(&mut sse)["type"], "changed");
        assert_eq!(read_event(&mut sse)["type"], "needs_you");
        for (header, value) in [
            ("Origin", "https://other.invalid"),
            ("Host", "other.invalid:7879"),
            ("Sec-Fetch-Site", "cross-site"),
        ] {
            assert!(matches!(
                get("/api/projects").set(header, value).call(),
                Err(ureq::Error::Status(403, _))
            ));
        }
        assert!(matches!(
            post("/api/approvals")
                .set("Content-Type", "text/plain")
                .send_string("{}"),
            Err(ureq::Error::Status(415, _))
        ));
        let body = json!({"thread":"t","request_id":4,"decision":"accept"});
        assert!(matches!(
            post("/api/approvals")
                .set("Origin", "null")
                .send_json(body.clone()),
            Err(ureq::Error::Status(403, _))
        ));
        assert_eq!(
            post("/api/approvals")
                .set("Origin", &base)
                .send_json(body)
                .unwrap()
                .status(),
            204
        );
        assert_eq!(
            post("/api/projects/0/notices")
                .send_json(json!({"to":["all"],"text":"hello"}))
                .unwrap()
                .status(),
            204
        );
        assert_eq!(read_event(&mut sse), json!({"type":"changed","project":0}));
    })
    .await
    .unwrap();
    worker.await.unwrap();
}

#[tokio::test]
async fn unvouched_tailnet_socket_gets_403() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_, snapshots) = watch::channel(Arc::default());
    let (commands, _) = mpsc::channel(1);
    let router = http::router(
        State {
            snapshots,
            commands,
        },
        auth::Guard::new("100.64.0.1".parse().unwrap(), addr.port(), None),
    )
    .layer(MockConnectInfo(
        "100.64.0.2:12345".parse::<SocketAddr>().unwrap(),
    ));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::task::spawn_blocking(move || {
        assert!(matches!(
            ureq::get(&format!("http://{addr}/api/projects"))
                .set("Host", &format!("100.64.0.1:{}", addr.port()))
                .call(),
            Err(ureq::Error::Status(403, _))
        ));
    })
    .await
    .unwrap();
}
