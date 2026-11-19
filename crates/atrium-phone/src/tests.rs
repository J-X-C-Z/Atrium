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

#[test]
fn project_fixture_matches_overview_and_pool() {
    use crate::snapshot::{Chat, project_snapshot};
    use atrium_core::{AgentEvent, ApprovalKind, ApprovalRequest, Transcript};
    use atrium_project::{Project, Role, Workstream};
    use std::fs;
    let root = std::env::temp_dir().join(format!("atrium-snapshot-{}", std::process::id()));
    let mut p = Project::open(&root).unwrap();
    p.file.streams = vec![
        Workstream {
            id: "controller".into(),
            role: Role::Controller,
            ..Default::default()
        },
        Workstream {
            id: "lead".into(),
            role: Role::Lead,
            parent: Some("controller".into()),
            task: Some("controller/ui".into()),
            thread_id: Some("t".into()),
            ..Default::default()
        },
        Workstream {
            id: "worker".into(),
            role: Role::Worker,
            parent: Some("lead".into()),
            ..Default::default()
        },
    ];
    p.save().unwrap();
    let dir = p.dir();
    for owner in ["controller", "lead"] {
        fs::create_dir_all(dir.join("tasks").join(owner)).unwrap()
    }
    fs::write(
        dir.join("tasks/controller/ui.md"),
        "# UI\nstatus: running\n\n## Roadmap\n### Build\n- [x] Model\n- [ ] View\n",
    )
    .unwrap();
    fs::write(dir.join("tasks/lead/polish.md"), "# Polish\nstatus: open\n").unwrap();
    fs::write(dir.join("plan.md"), "Night shift").unwrap();
    let p = Project::open(&root).unwrap();
    let mut t = Transcript::new();
    t.apply(&AgentEvent::ApprovalRequested(ApprovalRequest {
        request_id: 7.into(),
        thread_id: "t".into(),
        turn_id: "1".into(),
        item_id: "i".into(),
        kind: ApprovalKind::Tool {
            title: "Run".into(),
            detail: None,
        },
    }));
    let snap = project_snapshot(&p, |s| match s.id.as_str() {
        "lead" => ("needs_you", Arc::new(Chat::from_transcript(&t))),
        "worker" => ("working", Arc::default()),
        _ => ("idle", Arc::default()),
    });
    assert_eq!(snap.summary["streams"], 3);
    assert_eq!(snap.summary["working"], 1);
    assert_eq!(snap.summary["needs_you"], 1);
    assert_eq!(snap.detail["plan"].as_str().unwrap().trim(), "Night shift");
    let lead = &snap.detail["streams"][1];
    assert_eq!(lead["pool"]["total"], p.pool("lead").total());
    assert_eq!(lead["pool"]["ready"], json!(["lead/polish"]));
    assert_eq!(
        lead["task"]["phases"][0]["steps"],
        json!([{"text":"Model","done":true},{"text":"View","done":false}])
    );
    assert_eq!(lead["needs_you"][0]["text"], "Run");
    fs::remove_dir_all(root).unwrap();
}
