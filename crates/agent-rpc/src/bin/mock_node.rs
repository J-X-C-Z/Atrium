//! A small `atrium-node` for tests and as a reference for the real one
//! (`docs/node-protocol.md`). It serves the protocol on loopback with no
//! authentication, keeps everything in memory, and runs the agents it is
//! given on the command line:
//!
//! ```text
//! mock-node --port 0 --name studio --root /tmp \
//!     --agent codex=codex-app-server=/path/mock-codex,app-server \
//!     --agent claude=acp=/path/mock-acp,--name,claude
//! ```
//!
//! It prints `listening on 127.0.0.1:<port>` once ready.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use tungstenite::{Message, protocol::CloseFrame};

#[derive(Clone)]
struct AgentSpec {
    id: String,
    protocol: String,
    command: Vec<String>,
}

#[derive(Clone)]
enum Frame {
    Line(String),
    Stderr(String),
    Exit(Option<i32>),
}

struct Output {
    frames: Vec<(i64, Frame)>,
    next: i64,
    /// Bumped when a new attachment takes over, so the old one stops.
    generation: u64,
    /// Bumped by the test-only `POST /_mock/drop/{id}`: the link is cut
    /// without a goodbye, as a network loss would.
    drops: u64,
}

struct Proc {
    id: String,
    agent: String,
    cwd: String,
    owner: String,
    pid: u32,
    started_at: i64,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    output: Mutex<Output>,
    changed: Condvar,
}

impl Proc {
    fn push(&self, frame: Frame) {
        let mut output = self.output.lock().unwrap();
        let seq = output.next;
        output.next += 1;
        output.frames.push((seq, frame));
        self.changed.notify_all();
    }

    fn exit(&self) -> Option<Value> {
        let output = self.output.lock().unwrap();
        output.frames.iter().find_map(|(_, f)| match f {
            Frame::Exit(code) => Some(json!({ "code": code, "signal": null, "at": now() })),
            _ => None,
        })
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id, "agent": self.agent, "cwd": self.cwd, "owner": self.owner,
            "pid": self.pid, "started_at": self.started_at,
            "attached": false, "exit": self.exit(),
        })
    }
}

struct Node {
    name: String,
    roots: Vec<PathBuf>,
    agents: Vec<AgentSpec>,
    procs: Mutex<HashMap<String, Arc<Proc>>>,
    counter: Mutex<u64>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--echo-stdio"] {
        let mut output = std::io::stdout().lock();
        for line in std::io::stdin().lock().lines() {
            writeln!(output, "{}", line.expect("read echo input")).expect("write echo output");
            output.flush().expect("flush echo output");
        }
        return;
    }
    let mut port = 7381u16;
    let mut name = "mock".to_string();
    let mut roots = Vec::new();
    let mut agents = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => port = it.next().and_then(|p| p.parse().ok()).unwrap_or(port),
            "--name" => name = it.next().cloned().unwrap_or(name),
            "--root" => roots.extend(it.next().map(PathBuf::from)),
            "--agent" => {
                if let Some(spec) = it.next() {
                    let mut parts = spec.splitn(3, '=');
                    let (id, protocol, command) = (parts.next(), parts.next(), parts.next());
                    if let (Some(id), Some(protocol), Some(command)) = (id, protocol, command) {
                        agents.push(AgentSpec {
                            id: id.into(),
                            protocol: protocol.into(),
                            command: command.split(',').map(str::to_owned).collect(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if roots.is_empty() {
        roots.push(std::env::temp_dir());
    }
    let roots = roots
        .into_iter()
        .map(|r| r.canonicalize().unwrap_or(r))
        .collect();
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    println!("listening on {}", listener.local_addr().unwrap());
    let _ = std::io::stdout().flush();
    let node = Arc::new(Node {
        name,
        roots,
        agents,
        procs: Mutex::new(HashMap::new()),
        counter: Mutex::new(0),
    });
    for stream in listener.incoming().flatten() {
        let node = node.clone();
        thread::spawn(move || {
            if let Err(error) = serve(&node, stream) {
                eprintln!("mock-node: {error}");
            }
        });
    }
}

/// A request head, read off the stream.
struct Head {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    raw: Vec<u8>,
}

fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<Head>> {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            return Ok(None);
        }
        raw.push(byte[0]);
        if raw.len() > 64 * 1024 {
            return Ok(None);
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("/").to_string();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (!k.is_empty()).then(|| (k.to_string(), decode(v)))
        })
        .collect();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    Ok(Some(Head {
        method,
        path: path.to_string(),
        query,
        headers,
        raw,
    }))
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn json_response(stream: &mut TcpStream, status: u16, value: Value) -> std::io::Result<()> {
    respond(
        stream,
        status,
        "application/json",
        value.to_string().as_bytes(),
    )
}

fn error(stream: &mut TcpStream, status: u16, code: &str, message: &str) -> std::io::Result<()> {
    json_response(stream, status, json!({ "error": code, "message": message }))
}

fn read_body(stream: &mut TcpStream, head: &Head) -> std::io::Result<Vec<u8>> {
    let length: usize = head
        .headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    Ok(body)
}

impl Node {
    /// `path` resolved, when it is inside the roots.
    fn inside(&self, path: &str) -> Option<PathBuf> {
        let path = PathBuf::from(path);
        // A path that does not exist yet is judged by its nearest existing parent.
        let mut probe = path.clone();
        let mut rest = Vec::new();
        let resolved = loop {
            if let Ok(real) = probe.canonicalize() {
                break rest
                    .into_iter()
                    .rev()
                    .fold(real, |acc: PathBuf, part| acc.join(part));
            }
            let name = probe.file_name()?.to_owned();
            rest.push(name);
            probe = probe.parent()?.to_path_buf();
        };
        if resolved.components().any(|c| c.as_os_str() == "..") {
            return None;
        }
        self.roots
            .iter()
            .any(|root| resolved.starts_with(root))
            .then_some(resolved)
    }
}

fn serve(node: &Arc<Node>, mut stream: TcpStream) -> std::io::Result<()> {
    let Some(head) = read_head(&mut stream)? else {
        return Ok(());
    };
    let segments: Vec<&str> = head.path.trim_matches('/').split('/').collect();
    match (head.method.as_str(), segments.as_slice()) {
        ("GET", ["v1", "node"]) => {
            let agents: Vec<Value> = node
                .agents
                .iter()
                .map(|a| {
                    json!({
                        "id": a.id, "name": a.id, "protocol": a.protocol,
                        "version": "0.0.0", "available": true, "reason": null,
                    })
                })
                .collect();
            json_response(
                &mut stream,
                200,
                json!({
                    "name": "atrium-node", "version": "0.0.0-mock", "protocol": 1,
                    "device": { "name": node.name, "hostname": node.name, "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
                    "roots": node.roots, "home": dirs_home(),
                    "agents": agents, "magpie": null,
                }),
            )
        }
        ("GET", ["v1", "procs"]) => {
            let owner = head.query.get("owner");
            let procs: Vec<Value> = node
                .procs
                .lock()
                .unwrap()
                .values()
                .filter(|p| owner.is_none_or(|o| *o == p.owner))
                .map(|p| p.json())
                .collect();
            json_response(&mut stream, 200, Value::Array(procs))
        }
        ("POST", ["v1", "procs"]) => {
            let body: Value =
                serde_json::from_slice(&read_body(&mut stream, &head)?).unwrap_or_default();
            let agent = body["agent"].as_str().unwrap_or_default();
            let Some(spec) = node.agents.iter().find(|a| a.id == agent).cloned() else {
                return error(&mut stream, 404, "not_found", "no such agent");
            };
            let cwd = body["cwd"].as_str().unwrap_or_default();
            let Some(dir) = node.inside(cwd).filter(|d| d.is_dir()) else {
                return error(
                    &mut stream,
                    403,
                    "outside_roots",
                    "the folder is not inside the roots",
                );
            };
            let mut command = Command::new(&spec.command[0]);
            command
                .args(&spec.command[1..])
                .current_dir(&dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(e) => return error(&mut stream, 500, "spawn_failed", &e.to_string()),
            };
            let id = {
                let mut counter = node.counter.lock().unwrap();
                *counter += 1;
                format!("p-{}", *counter)
            };
            let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
            let proc = Arc::new(Proc {
                id: id.clone(),
                agent: agent.into(),
                cwd: dir.display().to_string(),
                owner: body["owner"].as_str().unwrap_or_default().into(),
                pid: child.id(),
                started_at: now(),
                stdin: Mutex::new(child.stdin.take()),
                child: Mutex::new(child),
                output: Mutex::new(Output {
                    frames: Vec::new(),
                    next: 1,
                    generation: 0,
                    drops: 0,
                }),
                changed: Condvar::new(),
            });
            let out = proc.clone();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    out.push(Frame::Line(line));
                }
                let code = out.child.lock().unwrap().wait().ok().and_then(|s| s.code());
                out.push(Frame::Exit(code));
            });
            let err = proc.clone();
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    err.push(Frame::Stderr(line));
                }
            });
            let answer = proc.json();
            node.procs.lock().unwrap().insert(id, proc);
            json_response(&mut stream, 201, answer)
        }
        ("DELETE", ["v1", "procs", id]) => {
            let proc = node.procs.lock().unwrap().get(*id).cloned();
            match proc {
                Some(proc) => {
                    proc.stdin.lock().unwrap().take();
                    let _ = proc.child.lock().unwrap().kill();
                    respond(&mut stream, 204, "text/plain", b"")
                }
                None => error(&mut stream, 404, "not_found", "no such process"),
            }
        }
        ("POST", ["_mock", "drop", id]) => {
            let proc = node.procs.lock().unwrap().get(*id).cloned();
            match proc {
                Some(proc) => {
                    proc.output.lock().unwrap().drops += 1;
                    proc.changed.notify_all();
                    respond(&mut stream, 204, "text/plain", b"")
                }
                None => error(&mut stream, 404, "not_found", "no such process"),
            }
        }
        ("GET", ["v1", "procs", id, "attach"]) => {
            let proc = node.procs.lock().unwrap().get(*id).cloned();
            let Some(proc) = proc else {
                return error(&mut stream, 404, "not_found", "no such process");
            };
            let after: i64 = head
                .query
                .get("after")
                .and_then(|a| a.parse().ok())
                .unwrap_or(0);
            attach(proc, stream, head, after)
        }
        ("GET", ["v1", "fs", "list"]) => {
            let Some(dir) = head.query.get("path").and_then(|p| node.inside(p)) else {
                return error(&mut stream, 403, "outside_roots", "outside the roots");
            };
            let hidden = head.query.get("hidden").is_some_and(|h| h == "1");
            let mut entries: Vec<Value> = std::fs::read_dir(&dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| hidden || !e.file_name().to_string_lossy().starts_with('.'))
                        .map(|e| {
                            let meta = e.metadata().ok();
                            json!({
                                "name": e.file_name().to_string_lossy(),
                                "dir": meta.as_ref().is_some_and(|m| m.is_dir()),
                                "size": meta.as_ref().map_or(0, |m| m.len()),
                                "modified": meta.and_then(|m| m.modified().ok()).map_or(0, millis),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            entries.sort_by_key(|e| {
                (
                    !e["dir"].as_bool().unwrap_or(false),
                    e["name"].as_str().unwrap_or("").to_string(),
                )
            });
            json_response(&mut stream, 200, json!({ "path": dir, "entries": entries }))
        }
        ("GET", ["v1", "fs", "stat"]) => {
            let Some(path) = head.query.get("path").and_then(|p| node.inside(p)) else {
                return error(&mut stream, 403, "outside_roots", "outside the roots");
            };
            let meta = std::fs::metadata(&path).ok();
            json_response(
                &mut stream,
                200,
                json!({
                    "exists": meta.is_some(),
                    "dir": meta.as_ref().is_some_and(|m| m.is_dir()),
                    "size": meta.as_ref().map_or(0, |m| m.len()),
                    "modified": meta.and_then(|m| m.modified().ok()).map_or(0, millis),
                }),
            )
        }
        ("GET", ["v1", "fs", "read"]) => {
            let Some(path) = head.query.get("path").and_then(|p| node.inside(p)) else {
                return error(&mut stream, 403, "outside_roots", "outside the roots");
            };
            match std::fs::read(&path) {
                Ok(bytes) => respond(&mut stream, 200, "application/octet-stream", &bytes),
                Err(_) => error(&mut stream, 404, "not_found", "no such file"),
            }
        }
        ("PUT", ["v1", "fs", "write"]) => {
            let body = read_body(&mut stream, &head)?;
            let Some(path) = head.query.get("path").and_then(|p| node.inside(p)) else {
                return error(&mut stream, 403, "outside_roots", "outside the roots");
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let temp = path.with_extension("atrium-tmp");
            match std::fs::write(&temp, &body).and_then(|()| std::fs::rename(&temp, &path)) {
                Ok(()) => respond(&mut stream, 204, "text/plain", b""),
                Err(e) => error(&mut stream, 500, "write_failed", &e.to_string()),
            }
        }
        ("POST", ["v1", "fs", "mkdir"]) => {
            let Some(path) = head.query.get("path").and_then(|p| node.inside(p)) else {
                return error(&mut stream, 403, "outside_roots", "outside the roots");
            };
            match std::fs::create_dir_all(&path) {
                Ok(()) => respond(&mut stream, 204, "text/plain", b""),
                Err(e) => error(&mut stream, 500, "mkdir_failed", &e.to_string()),
            }
        }
        _ => error(&mut stream, 404, "not_found", "no such route"),
    }
}

fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn dirs_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// A stream that first gives back the head already read, for the WebSocket handshake.
struct Replay {
    head: std::io::Cursor<Vec<u8>>,
    stream: TcpStream,
}

impl Read for Replay {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.head.read(buf)?;
        if n > 0 {
            return Ok(n);
        }
        self.stream.read(buf)
    }
}

impl Write for Replay {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn attach(proc: Arc<Proc>, stream: TcpStream, head: Head, after: i64) -> std::io::Result<()> {
    let replay = Replay {
        head: std::io::Cursor::new(head.raw),
        stream: stream.try_clone()?,
    };
    let mut socket =
        tungstenite::accept(replay).map_err(|e| std::io::Error::other(e.to_string()))?;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    let (generation, drops) = {
        let mut output = proc.output.lock().unwrap();
        output.generation += 1;
        proc.changed.notify_all();
        (output.generation, output.drops)
    };
    let mut sent = if after < 0 {
        proc.output.lock().unwrap().next - 1
    } else {
        after
    };
    loop {
        // What the client wrote goes to stdin.
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(frame) = serde_json::from_str::<Value>(&text)
                    && let Some(line) = frame["line"].as_str()
                    && let Some(stdin) = proc.stdin.lock().unwrap().as_mut()
                {
                    let _ = stdin.write_all(line.as_bytes());
                    let _ = stdin.write_all(b"\n");
                    let _ = stdin.flush();
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return Ok(()),
        }
        // What the process wrote goes to the client.
        let (frames, replaced, dropped) = {
            let output = proc.output.lock().unwrap();
            let output = if output.next - 1 > sent
                || output.generation != generation
                || output.drops != drops
            {
                output
            } else {
                proc.changed
                    .wait_timeout(output, Duration::from_millis(30))
                    .unwrap()
                    .0
            };
            let frames: Vec<(i64, Frame)> = output
                .frames
                .iter()
                .filter(|(seq, _)| *seq > sent)
                .cloned()
                .collect();
            (
                frames,
                output.generation != generation,
                output.drops != drops,
            )
        };
        if dropped {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return Ok(());
        }
        if replaced {
            let _ = socket.close(Some(CloseFrame {
                code: 4001.into(),
                reason: "replaced".into(),
            }));
            let _ = socket.flush();
            return Ok(());
        }
        for (seq, frame) in frames {
            let value = match &frame {
                Frame::Line(line) => json!({ "seq": seq, "line": line }),
                Frame::Stderr(line) => json!({ "seq": seq, "stderr": line }),
                Frame::Exit(code) => {
                    json!({ "seq": seq, "exit": { "code": code, "signal": null } })
                }
            };
            if socket.send(Message::text(value.to_string())).is_err() {
                return Ok(());
            }
            sent = seq;
            if matches!(frame, Frame::Exit(_)) {
                let _ = socket.close(None);
                let _ = socket.flush();
                return Ok(());
            }
        }
    }
}
