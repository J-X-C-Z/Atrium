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
