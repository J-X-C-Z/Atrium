//! The node client against `mock-node`, through real sockets: device info,
//! a process's input and output, picking up where a lost link left off,
//! the end of a process, and files.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agent_rpc::{
    Connection, Incoming,
    node::{LinkState, NodeClient, NodeError},
};
use futures::{StreamExt, executor::block_on};

struct MockNode {
    child: Child,
    address: String,
    root: std::path::PathBuf,
}

impl Drop for MockNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn mock_node(tag: &str) -> MockNode {
    let root = std::env::temp_dir().join(format!("atrium-node-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let executable = env!("CARGO_BIN_EXE_mock-node");
    let mut child = Command::new(executable)
        .args(["--port", "0", "--name", "studio", "--root"])
        .arg(&root)
        .arg("--agent")
        .arg(format!("echo=raw={executable},--echo-stdio"))
        .stdout(Stdio::piped())
        .spawn()
        .expect("mock-node starts");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let address = line.trim().trim_start_matches("listening on ").to_string();
    MockNode {
        child,
        address,
        root: root.canonicalize().unwrap(),
    }
}

fn wait_for(state: &Arc<Mutex<LinkState>>, want: impl Fn(&LinkState) -> bool) {
    let start = Instant::now();
    while !want(&state.lock().unwrap()) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "link stuck at {:?}",
            state.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_process_runs_on_the_node_and_its_output_is_resumed() {
    let node = mock_node("proc");
    let client = NodeClient::new(&node.address, None);
    let info = client.info().unwrap();
    assert_eq!(info.device.name, "studio");
    assert_eq!(info.agents[0].id, "echo");

    let cwd = node.root.display().to_string();
    let proc = client.spawn("echo", &cwd, "me").unwrap();
    assert_eq!(client.procs(Some("me")).unwrap().len(), 1);
    assert!(client.procs(Some("someone-else")).unwrap().is_empty());

    // Lines go out and come back.
    let mut first = client.attach(&proc.id, 0);
    for n in 1..=3 {
        writeln!(first.writer, "{{\"n\":{n}}}").unwrap();
    }
    let mut seen = Vec::new();
    for _ in 0..3 {
        let mut line = String::new();
        first.reader.read_line(&mut line).unwrap();
        seen.push(line);
    }
    assert_eq!(seen, ["{\"n\":1}\n", "{\"n\":2}\n", "{\"n\":3}\n"]);
    wait_for(&first.state, |s| *s == LinkState::Connected);
    drop(first);

    // A new link that has line 2 gets line 3 again, then new output.
    let mut second = client.attach(&proc.id, 2);
    let mut line = String::new();
    second.reader.read_line(&mut line).unwrap();
    assert_eq!(line, "{\"n\":3}\n");
    writeln!(second.writer, "{{\"n\":4}}").unwrap();
    line.clear();
    second.reader.read_line(&mut line).unwrap();
    assert_eq!(line, "{\"n\":4}\n");

    // The process ends: end of output, and the link says so.
    client.kill(&proc.id).unwrap();
    line.clear();
    assert_eq!(second.reader.read_line(&mut line).unwrap(), 0);
    wait_for(&second.state, |s| matches!(s, LinkState::Ended { .. }));

    // Gone from the node: the link gives up.
    let missing = client.attach("p-404", 0);
    wait_for(&missing.state, |s| matches!(s, LinkState::Gone(_)));
}

#[test]
fn a_lost_link_comes_back_without_losing_or_repeating_lines() {
    let node = mock_node("drop");
    let client = NodeClient::new(&node.address, None);
    let proc = client
        .spawn("echo", &node.root.display().to_string(), "me")
        .unwrap();
    let mut link = client.attach(&proc.id, 0);
    writeln!(link.writer, "one").unwrap();
    let mut line = String::new();
    link.reader.read_line(&mut line).unwrap();
    assert_eq!(line, "one\n");

    // The network goes; a line written meanwhile waits for it to come back.
    ureq::post(&format!("http://{}/_mock/drop/{}", node.address, proc.id))
        .call()
        .unwrap();
    wait_for(&link.state, |s| matches!(s, LinkState::Reconnecting { .. }));
    writeln!(link.writer, "two").unwrap();
    wait_for(&link.state, |s| *s == LinkState::Connected);
    line.clear();
    link.reader.read_line(&mut line).unwrap();
    assert_eq!(line, "two\n", "nothing repeated, nothing lost");
    client.kill(&proc.id).unwrap();
}

#[test]
fn json_rpc_runs_over_an_attachment() {
    let node = mock_node("rpc");
    let client = NodeClient::new(&node.address, None);
    let proc = client
        .spawn("echo", &node.root.display().to_string(), "me")
        .unwrap();
    let attachment = client.attach(&proc.id, 0);
    let (connection, mut events) =
        Connection::jsonrpc2(attachment.reader, attachment.writer, "echo");
    // The echo fixture sends the notification straight back.
    connection
        .notify("hello", Some(serde_json::json!({ "from": "atrium" })))
        .unwrap();
    let echoed = block_on(events.next()).unwrap();
    assert_eq!(
        echoed,
        Incoming::Notification {
            method: "hello".into(),
            params: serde_json::json!({ "from": "atrium" })
        }
    );
    client.kill(&proc.id).unwrap();
    assert_eq!(block_on(events.next()), Some(Incoming::Closed));
}

#[test]
fn files_inside_the_roots_only() {
    let node = mock_node("fs");
    let client = NodeClient::new(&node.address, None);
    let root = node.root.display().to_string();
    let notices = format!("{root}/.atrium/notices.md");
    assert_eq!(client.read(&notices).unwrap(), None);
    client.write(&notices, "# Notices\n").unwrap();
    assert_eq!(
        client.read(&notices).unwrap().as_deref(),
        Some("# Notices\n")
    );
    assert!(client.stat(&notices).unwrap().exists);
    client.mkdir(&format!("{root}/web")).unwrap();
    let names: Vec<String> = client
        .list(&root)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["web"], "hidden entries are left out");
    let refused = client.read("/etc/hostname").unwrap_err();
    assert!(
        matches!(refused, NodeError::Refused { status: 403, .. }),
        "{refused:?}"
    );
    assert!(matches!(
        client.spawn("nope", &root, "me").unwrap_err(),
        NodeError::Refused { status: 404, .. }
    ));
}

/// The acceptance check for a real node:
/// `ATRIUM_TEST_NODE=http://127.0.0.1:7381 cargo test -p agent-rpc --test node -- --ignored`
#[test]
#[ignore = "needs a running atrium-node"]
fn a_real_node_answers() {
    let address = std::env::var("ATRIUM_TEST_NODE").expect("set ATRIUM_TEST_NODE");
    let client = NodeClient::new(&address, std::env::var("ATRIUM_TEST_TOKEN").ok());
    let info = client.info().expect("GET /v1/node");
    eprintln!("{info:#?}");
    let root = info.roots.first().expect("a root").clone();
    let probe = format!("{root}/.atrium-acceptance.txt");
    client.write(&probe, "ok").expect("write");
    assert_eq!(client.read(&probe).expect("read").as_deref(), Some("ok"));
    let agent = info
        .agents
        .iter()
        .find(|a| a.available)
        .expect("an available agent");
    let proc = client.spawn(&agent.id, &root, "acceptance").expect("spawn");
    let attachment = client.attach(&proc.id, 0);
    wait_for(&attachment.state, |s| *s == LinkState::Connected);
    client.kill(&proc.id).expect("kill");
    wait_for(&attachment.state, |s| matches!(s, LinkState::Ended { .. }));
}
