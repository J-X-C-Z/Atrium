//! Isolated native node acceptance with /bin/cat, never a real agent/account.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};
use tungstenite::{Message, client::IntoClientRequest};
struct Node {
    child: std::process::Child,
    root: std::path::PathBuf,
    url: String,
}
impl Node {
    fn start() -> Self {
        let root =
            std::env::temp_dir().join(format!("atrium-native-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let cfg = root.join("config.toml");
        std::fs::write(&cfg,format!("port=0\ndiscover_magpie=false\nloopback_only=true\nroots=[{:?}]\ntoken=\"test-only-token\"\n[agents.echo]\ncommand=[\"/bin/cat\"]\nprotocol=\"raw\"\n",root.to_string_lossy())).unwrap();
        use std::io::Write;
        let mut extra = std::fs::OpenOptions::new().append(true).open(&cfg).unwrap();
        for name in ["codex", "claude", "deepseek", "antigravity"] {
            writeln!(
                extra,
                "[agents.{name}]\ncommand=[\"/bin/cat\"]\nprotocol=\"raw\""
            )
            .unwrap();
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_atrium-node"))
            .args(["serve", "--config"])
            .arg(cfg)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.starts_with("listening on "), "{line}");
        let url = format!(
            "http://{}",
            line.trim().strip_prefix("listening on ").unwrap()
        );
        Self { child, root, url }
    }
    fn req(&self, method: &str, path: &str) -> ureq::Request {
        ureq::request(method, &format!("{}{path}", self.url))
            .set("Authorization", "Bearer test-only-token")
            .timeout(Duration::from_secs(15))
    }
    fn ws(
        &self,
        id: &str,
        after: i64,
    ) -> tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>> {
        let mut r = format!(
            "{}/v1/procs/{id}/attach?after={after}",
            self.url.replace("http:", "ws:")
        )
        .into_client_request()
        .unwrap();
        r.headers_mut()
            .insert("Authorization", "Bearer test-only-token".parse().unwrap());
        let (mut w, _) = tungstenite::connect(r).unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = w.get_mut() {
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        }
        w
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn frame(
    w: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> Value {
    loop {
        match w.read().unwrap() {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(p) => w.send(Message::Pong(p)).unwrap(),
            m => panic!("unexpected {m:?}"),
        }
    }
}
#[test]
fn authenticated_native_node_replays_and_confines_files() {
    let n = Node::start();
    assert!(matches!(
        ureq::get(&format!("{}/v1/node", n.url)).call(),
        Err(ureq::Error::Status(401, _))
    ));
    assert!(matches!(
        n.req("GET", "/v1/node")
            .set("Origin", "https://untrusted.example")
            .call(),
        Err(ureq::Error::Status(403, _))
    ));
    assert!(matches!(
        n.req("GET", "/v1/node")
            .set("Host", "untrusted.example")
            .call(),
        Err(ureq::Error::Status(403, _))
    ));
    let bad = n
        .req("POST", "/v1/procs")
        .set("Content-Type", "application/json")
        .send_string("{");
    match bad {
        Err(ureq::Error::Status(400, r)) => {
            assert!(
                r.header("content-type")
                    .unwrap_or("")
                    .starts_with("application/json")
            );
            let v: Value = r.into_json().unwrap();
            assert_eq!(v["error"], "bad_request");
        }
        other => panic!("unexpected malformed request: {other:?}"),
    }
    let info: Value = n
        .req("GET", "/v1/node")
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(info["protocol"], 1);
    let file = n.root.join("state.txt");
    n.req("PUT", "/v1/fs/write")
        .query("path", file.to_str().unwrap())
        .send_string("hello")
        .unwrap();
    assert_eq!(
        n.req("GET", "/v1/fs/read")
            .query("path", file.to_str().unwrap())
            .call()
            .unwrap()
            .into_string()
            .unwrap(),
        "hello"
    );
    assert!(matches!(
        n.req("GET", "/v1/fs/read")
            .query("path", "/etc/passwd")
            .call(),
        Err(ureq::Error::Status(403, _))
    ));
    let p: Value = n
        .req("POST", "/v1/procs")
        .send_json(json!({"agent":"echo","cwd":n.root,"owner":"test"}))
        .unwrap()
        .into_json()
        .unwrap();
    let id = p["id"].as_str().unwrap();
    let list: Vec<Value> = n
        .req("GET", "/v1/procs")
        .query("owner", "test")
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(list.len(), 1);
    let mut a = n.ws(id, 0);
    a.send(Message::Text(json!({"line":"one"}).to_string()))
        .unwrap();
    let f = frame(&mut a);
    assert_eq!(f["line"], "one");
    let seq = f["seq"].as_i64().unwrap();
    a.send(Message::Text(json!({"line":"two"}).to_string()))
        .unwrap();
    assert_eq!(frame(&mut a)["line"], "two");
    let mut b = n.ws(id, seq);
    assert_eq!(frame(&mut b)["line"], "two");
    match a.read().unwrap() {
        Message::Close(Some(c)) => assert_eq!(u16::from(c.code), 4001),
        m => panic!("expected replacement: {m:?}"),
    };
    n.req("DELETE", &format!("/v1/procs/{id}")).call().unwrap();
    let e = frame(&mut b);
    assert_eq!(e["exit"]["code"], 0);
    assert!(matches!(
        n.req("GET", "/v1/procs/missing/attach").call(),
        Err(ureq::Error::Status(400, _)) | Err(ureq::Error::Status(404, _))
    ));
}

#[test]
fn concurrent_finished_process_replays_drain_all_pages_before_exit() {
    let node = Node::start();
    let mut ids = Vec::new();
    for process_index in 0..2 {
        let proc: Value = node
            .req("POST", "/v1/procs")
            .send_json(json!({"agent":"echo","cwd":node.root,"owner":"paged-replay-test"}))
            .unwrap()
            .into_json()
            .unwrap();
        let id = proc["id"].as_str().unwrap().to_owned();
        let mut socket = node.ws(&id, 0);
        // A 500 KiB backlog is more than one 256 KiB replay page, without
        // requiring giant allocations just to exercise the production limit.
        for line_index in 0..5 {
            let line = format!("{process_index}:{line_index}:{}", "x".repeat(100 * 1024));
            socket
                .send(Message::Text(json!({"line":line}).to_string()))
                .unwrap();
            let reply = frame(&mut socket);
            assert_eq!(reply["seq"], line_index + 1);
            assert_eq!(reply["line"], line);
        }
        drop(socket);
        node.req("DELETE", &format!("/v1/procs/{id}"))
            .call()
            .unwrap();
        ids.push(id);
    }
    let sockets: Vec<_> = ids.iter().map(|id| node.ws(id, 0)).collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(sockets.len()));
    let workers: Vec<_> = sockets
        .into_iter()
        .enumerate()
        .map(|(process_index, mut socket)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for seq in 1..=5 {
                    let reply = frame(&mut socket);
                    assert_eq!(reply["seq"], seq, "pagination skipped/duplicated a frame");
                    assert!(
                        reply.get("exit").is_none(),
                        "exit arrived before the backlog drained"
                    );
                    let line = reply["line"].as_str().unwrap();
                    assert!(line.starts_with(&format!("{process_index}:{}:", seq - 1)));
                    assert_eq!(line.len(), 100 * 1024 + 4);
                }
                let exit = frame(&mut socket);
                assert_eq!(exit["seq"], 6);
                assert_eq!(exit["exit"]["code"], 0);
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    for id in ids {
        let mut socket = node.ws(&id, -1);
        let exit = frame(&mut socket);
        assert_eq!(exit["seq"], 6);
        assert!(exit.get("line").is_none());
        assert_eq!(exit["exit"]["code"], 0);
    }
}

#[test]
fn instance_identity_detects_restart_without_replaying_queued_input() {
    use agent_rpc::node::{LinkState, NodeClient};
    use std::{io::Write, time::Instant};
    fn wait_for(
        state: &std::sync::Arc<std::sync::Mutex<LinkState>>,
        check: impl Fn(&LinkState) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if check(&state.lock().unwrap()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "link state: {:?}",
                state.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn ws_request(
        n: &Node,
        id: &str,
        expected: Option<&str>,
        token: bool,
    ) -> tungstenite::handshake::client::Request {
        let mut req = format!(
            "{}/v1/procs/{id}/attach?after=0",
            n.url.replace("http:", "ws:")
        )
        .into_client_request()
        .unwrap();
        if token {
            req.headers_mut()
                .insert("Authorization", "Bearer test-only-token".parse().unwrap());
        }
        if let Some(expected) = expected {
            req.headers_mut()
                .insert("x-atrium-instance-id", expected.parse().unwrap());
        }
        req
    }
    fn refused(req: tungstenite::handshake::client::Request, status: u16, code: &str) {
        match tungstenite::connect(req) {
            Err(tungstenite::Error::Http(r)) => {
                assert_eq!(r.status().as_u16(), status);
                let v: Value = serde_json::from_slice(r.body().as_ref().unwrap()).unwrap();
                assert_eq!(v["error"], code);
            }
            _ => panic!("expected HTTP refusal"),
        }
    }
    let mut n = Node::start();
    let client = NodeClient::new(&n.url, Some("test-only-token".into()));
    let instance = client.info().unwrap().instance_id.unwrap();
    let p = client
        .spawn("echo", n.root.to_str().unwrap(), "test")
        .unwrap();
    assert_eq!(p.instance_id.as_deref(), Some(instance.as_str()));
    assert_eq!(
        client.procs(None).unwrap()[0].instance_id.as_deref(),
        Some(instance.as_str())
    );
    let (mut socket, response) =
        tungstenite::connect(ws_request(&n, &p.id, Some(&instance), true)).unwrap();
    assert_eq!(response.headers()["x-atrium-instance-id"], instance);
    socket.close(None).unwrap();
    refused(
        ws_request(&n, "missing", Some(&instance), true),
        404,
        "not_found",
    );
    refused(
        ws_request(&n, "missing", Some("other"), false),
        401,
        "unauthorized",
    );
    refused(
        ws_request(&n, "missing", Some(&"x".repeat(129)), true),
        400,
        "bad_request",
    );
    let mut link = client.attach(&p.id, 0);
    wait_for(&link.state, |s| matches!(s, LinkState::Connected));
    // A fresh client has no spawn/list cache: successful upgrade must teach
    // this link its instance identity before it can reconnect.
    let second = client
        .spawn("echo", n.root.to_str().unwrap(), "test")
        .unwrap();
    let fresh = NodeClient::new(&n.url, Some("test-only-token".into()));
    let learned = fresh.attach(&second.id, 0);
    wait_for(&learned.state, |s| matches!(s, LinkState::Connected));
    // Kill only this fixture's node. Its cat has no descendants; closing the
    // owned stdin pipe ends cat, never restarting any command automatically.
    n.child.kill().unwrap();
    n.child.wait().unwrap();
    // Inspect only the two PIDs returned by this fixture's own spawn requests.
    // Orphaned zombies count as exited; their OS adopter, not this test, reaps.
    for pid in [p.pid.unwrap(), second.pid.unwrap()] {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&status.stdout);
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned cat {pid} did not exit on stdin EOF"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    wait_for(&link.state, |s| matches!(s, LinkState::Reconnecting { .. }));
    link.writer.write_all(b"must-not-replay\n").unwrap();
    let cfg = n.root.join("config.toml");
    let text = std::fs::read_to_string(&cfg).unwrap().replacen(
        "port=0",
        &format!("port={}", n.url.rsplit(':').next().unwrap()),
        1,
    );
    std::fs::write(&cfg, text).unwrap();
    n.child = Command::new(env!("CARGO_BIN_EXE_atrium-node"))
        .args(["serve", "--config"])
        .arg(cfg)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(n.child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("listening on "));
    assert_ne!(
        client.info().unwrap().instance_id.as_deref(),
        Some(instance.as_str())
    );
    wait_for(
        &link.state,
        |s| matches!(s,LinkState::Gone(why) if why.contains("node restarted")),
    );
    wait_for(
        &learned.state,
        |s| matches!(s, LinkState::Gone(why) if why.contains("node restarted")),
    );
    assert!(link.writer.write_all(b"closed\n").is_err());
    assert!(
        client.procs(None).unwrap().is_empty(),
        "no commands replayed on restart"
    );
    refused(
        ws_request(&n, &p.id, Some(&instance), true),
        409,
        "node_restarted",
    );
    refused(ws_request(&n, &p.id, None, true), 404, "not_found");
    // Fresh metadata must not overwrite the original proc pin.
    let retry = client.attach(&p.id, 0);
    wait_for(
        &retry.state,
        |s| matches!(s,LinkState::Gone(why) if why.contains("node restarted")),
    );
}
