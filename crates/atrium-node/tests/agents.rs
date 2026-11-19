//! Native node -> real remote transport/client -> scripted agents, no accounts.
//! Run after `cargo build -p codex-client --bin mock-codex -p acp-client --bin mock-acp`:
//! `cargo test -p atrium-node --test agents -- --ignored`

#[path = "support/hierarchy.rs"]
mod hierarchy;

use acp_client::AcpClient;
use agent_rpc::{
    Incoming,
    node::{NodeClient, RemoteProc},
};
use atrium_core::{
    AgentEvent, ApprovalDecision, EntryKind, Item, ItemStatus, Transcript, TurnStatus,
};
use codex_client::{AccessMode, CodexClient, StartThread, TurnRequest, translate};
use futures::StreamExt;
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

struct Node {
    child: Child,
    root: PathBuf,
    client: NodeClient,
}
impl Node {
    fn start() -> Self {
        Self::start_with_hierarchy(false)
    }
    fn start_with_hierarchy(hierarchy: bool) -> Self {
        // Use Cargo's binary output path, not the version-dependent test layout.
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_atrium-node"));
        let bin = executable.parent().unwrap().to_path_buf();
        let codex = bin.join(format!("mock-codex{}", std::env::consts::EXE_SUFFIX));
        let acp = bin.join(format!("mock-acp{}", std::env::consts::EXE_SUFFIX));
        assert!(
            codex.is_file() && acp.is_file(),
            "build mock-codex and mock-acp in the same Cargo target directory first"
        );
        let root =
            std::env::temp_dir().join(format!("atrium-native-agents-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        let mut config = format!(
            "port=0\ndiscover_magpie=false\nloopback_only=true\nroots=[{}]\ntoken={token:?}\n",
            serde_json::json!(root)
        );
        config += &format!(
            "[agents.codex]\ncommand=[{}]\nprotocol=\"codex-app-server\"\n",
            serde_json::json!(codex)
        );
        for name in ["claude", "deepseek", "antigravity"] {
            config += &format!(
                "[agents.{name}]\ncommand=[{},\"--name\",{name:?}]\nprotocol=\"acp\"\n",
                serde_json::json!(acp)
            );
        }
        let cfg = root.join("config.toml");
        std::fs::write(&cfg, config).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_atrium-node"));
        command.env_remove("ATRIUM_HIERARCHY_MOCK");
        if hierarchy {
            command.env("ATRIUM_HIERARCHY_MOCK", "1");
        }
        #[cfg(unix)]
        command.env("SHELL", "/bin/sh");
        let mut child = command
            .args(["serve", "--config"])
            .arg(cfg)
            .env("HOME", &root)
            .env("USERPROFILE", &root)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut node = Self {
            child,
            root,
            client: NodeClient::new("http://127.0.0.1:1", None),
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = tx.send(result);
        });
        let line = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("node startup timeout")
            .unwrap();
        let address = line
            .trim()
            .strip_prefix("listening on ")
            .expect("node listening announcement");
        node.client = NodeClient::new(&format!("http://{address}"), Some(token));
        assert_eq!(node.client.info().unwrap().protocol, 1);
        node
    }
    fn spawn(&self, agent: &str) -> (agent_rpc::node::Attachment, Arc<RemoteProc>) {
        let p = self
            .client
            .spawn(
                agent,
                self.root.to_str().unwrap(),
                "isolated-native-acceptance",
            )
            .unwrap();
        let attachment = self.client.attach(&p.id, 0);
        (
            attachment,
            Arc::new(RemoteProc {
                client: self.client.clone(),
                id: p.id,
            }),
        )
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            // Only our own child, never global process-name cleanup.
            #[cfg(unix)]
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    if self.child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built mock-codex and mock-acp binaries; never uses real agents"]
async fn native_node_codex_approval_turn() {
    let node = Node::start();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (attachment, keep) = node.spawn("codex");
        let (client, mut events) = CodexClient::remote(attachment.reader, attachment.writer, keep).await.unwrap();
        assert!(client.user_agent.starts_with("mock-codex"));
        let thread = client.thread_start(StartThread { cwd: Some(node.root.to_string_lossy().into()), ..Default::default() }).await.unwrap();
        client.turn_start(&thread.id, TurnRequest { text: "please run the tests".into(), mode: Some(AccessMode::Auto), ..Default::default() }).await.unwrap();
        let mut transcript = Transcript::new();
        let mut approvals = 0;
        let mut completed = false;
        while let Some(incoming) = events.next().await {
            let event = match incoming {
                Incoming::Notification { method, params } => translate::event(&method, &params),
                Incoming::Request { id, method, params } => {
                    let event = translate::request(&id, &method, &params);
                    if matches!(event, Some(AgentEvent::ApprovalRequested(_))) {
                        approvals += 1;
                        client.answer_approval(id, ApprovalDecision::Accept).unwrap();
                    } else { client.reject_request(id, &method).unwrap(); }
                    event
                }
                Incoming::Closed => panic!("Codex transport closed before completion"),
            };
            let Some(event) = event else { continue };
            if event.thread_id() != Some(thread.id.as_str()) { continue; }
            transcript.apply(&event);
            if let AgentEvent::TurnCompleted { status, .. } = event {
                assert_eq!(status, TurnStatus::Completed);
                completed = true;
                break;
            }
        }
        assert!(completed && approvals > 0);
        assert!(transcript.entries().iter().any(|e| matches!(&e.kind, EntryKind::Item(Item::Command { status: ItemStatus::Completed, output, .. }) if output.contains("6 passed"))));
        assert!(transcript.entries().iter().any(|e| matches!(&e.kind, EntryKind::Item(Item::Agent { text, .. }) if text.contains("hello from mock-codex"))));
    }).await.expect("Codex native turn exceeded 30 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built mock-codex and mock-acp binaries; never uses real agents"]
async fn native_node_acp_approval_turn() {
    let node = Node::start();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (attachment, keep) = node.spawn("claude");
        let (client, mut events) = AcpClient::over("claude".into(), attachment.reader, attachment.writer, Some(keep)).await.unwrap();
        assert!(client.info.name.contains("mock"));
        let session = client.new_session(&node.root).await.unwrap();
        let mut transcript = Transcript::new();
        for event in client.begin_turn(&session.session_id).1 { transcript.apply(&event); }
        let prompt = client.prompt(&session.session_id, "run the tests", &[], &[]);
        tokio::pin!(prompt);
        let mut approvals = 0;
        loop {
            tokio::select! {
                ended = &mut prompt => {
                    while let Ok(incoming) = events.try_recv() {
                        for event in client.translate(incoming) { transcript.apply(&event); }
                    }
                    assert!(ended.iter().any(|e| matches!(e, AgentEvent::TurnCompleted { status: TurnStatus::Completed, .. })));
                    for event in ended { transcript.apply(&event); }
                    break;
                }
                incoming = events.next() => {
                    for event in client.translate(incoming.expect("ACP transport closed early")) {
                        if let AgentEvent::ApprovalRequested(request) = &event {
                            approvals += 1;
                            client.answer_permission(request.request_id.clone(), ApprovalDecision::Accept).unwrap();
                        }
                        transcript.apply(&event);
                    }
                }
            }
        }
        assert!(approvals > 0);
        assert!(!transcript.is_running());
        assert!(transcript.entries().iter().any(|e| matches!(&e.kind, EntryKind::Item(Item::Command { status: ItemStatus::Completed, output, .. }) if output.contains("9 passed"))));
        assert!(transcript.entries().iter().any(|e| matches!(&e.kind, EntryKind::Item(Item::Agent { text, .. }) if text.contains("hello from claude"))));
    }).await.expect("ACP native turn exceeded 30 seconds");
}
