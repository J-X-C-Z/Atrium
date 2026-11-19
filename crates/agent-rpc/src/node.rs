//! A client for `atrium-node`, which runs agents on another device of the
//! user's tailnet (see `docs/node-protocol.md`).
//!
//! An attached process looks like a local child to the rest of Atrium: a
//! reader of its output lines and a writer of input lines, which
//! [`crate::Connection`] takes as it takes a child's stdio. Underneath, a
//! thread keeps the WebSocket up: when it drops (a laptop sleeps, Wi-Fi
//! changes) it reconnects and asks for what came after the last line it has,
//! replaying retained output after that sequence. Evicted history is reported
//! as a gap. Input may wait while disconnected, but delivery during connection
//! failure can be uncertain; this transport does not guarantee exactly-once
//! input delivery or execution. A confirmed runtime change stops the old link
//! rather than replaying its queued input into a new runtime.

use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufReader, Read, Write},
    net::TcpStream,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

/// The node protocol version this client speaks.
pub const PROTOCOL: u32 = 1;
pub const DEFAULT_PORT: u16 = 7381;

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum NodeError {
    /// The node answered with an error.
    #[error("{message} ({code}, HTTP {status})")]
    Refused {
        status: u16,
        code: String,
        message: String,
    },
    /// The node could not be reached, or answered nonsense.
    #[error("{0}")]
    Unreachable(String),
}

impl NodeError {
    pub fn code(&self) -> Option<&str> {
        match self {
            NodeError::Refused { code, .. } => Some(code),
            NodeError::Unreachable(_) => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, NodeError>;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceInfo {
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeAgent {
    pub id: String,
    pub name: String,
    /// `codex-app-server` or `acp`.
    pub protocol: String,
    pub version: Option<String>,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MagpieInfo {
    pub version: String,
    pub port: u16,
}

/// `GET /v1/node`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeInfo {
    pub instance_id: Option<String>,
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub device: DeviceInfo,
    pub roots: Vec<String>,
    pub home: String,
    pub agents: Vec<NodeAgent>,
    pub magpie: Option<MagpieInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcExit {
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub at: Option<i64>,
}

/// A process the node started.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Proc {
    pub instance_id: Option<String>,
    pub id: String,
    pub agent: String,
    pub cwd: String,
    pub owner: String,
    pub pid: Option<u32>,
    pub started_at: i64,
    pub attached: bool,
    pub exit: Option<ProcExit>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub modified: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsStat {
    pub exists: bool,
    pub dir: bool,
    pub size: u64,
    pub modified: i64,
}

/// One device's node.
#[derive(Clone)]
pub struct NodeClient {
    /// Such as `http://studio.tail1234.ts.net:7381`.
    pub base: String,
    token: Option<String>,
    agent: ureq::Agent,
    instances: Arc<Mutex<VecDeque<(String, String)>>>,
}

impl std::fmt::Debug for NodeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeClient")
            .field("base", &self.base)
            .finish()
    }
}

impl NodeClient {
    /// `address` is a base URL, or a host with an optional port.
    pub fn new(address: &str, token: Option<String>) -> Self {
        let address = address.trim().trim_end_matches('/');
        let base = if address.starts_with("http://") || address.starts_with("https://") {
            address.to_string()
        } else if address.contains(':')
            && !address.starts_with('[')
            && address.matches(':').count() == 1
        {
            format!("http://{address}")
        } else {
            format!("http://{address}:{DEFAULT_PORT}")
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .build();
        Self {
            base,
            token: token.filter(|t| !t.is_empty()),
            agent,
            instances: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let request = self.agent.request(method, &format!("{}{path}", self.base));
        match &self.token {
            Some(token) => request.set("Authorization", &format!("Bearer {token}")),
            None => request,
        }
    }

    fn json<T: serde::de::DeserializeOwned>(
        &self,
        result: std::result::Result<ureq::Response, ureq::Error>,
    ) -> Result<T> {
        let response = answer(result)?;
        response
            .into_json()
            .map_err(|e| NodeError::Unreachable(format!("unexpected answer: {e}")))
    }

    /// What the device has. Fails on a node of another protocol version.
    pub fn info(&self) -> Result<NodeInfo> {
        let info: NodeInfo = self.json(
            self.request("GET", "/v1/node")
                .timeout(Duration::from_secs(3))
                .call(),
        )?;
        if info.protocol != PROTOCOL {
            return Err(NodeError::Unreachable(format!(
                "the node speaks protocol {}, Atrium speaks {PROTOCOL}",
                info.protocol
            )));
        }
        Ok(info)
    }

    pub fn procs(&self, owner: Option<&str>) -> Result<Vec<Proc>> {
        let mut request = self.request("GET", "/v1/procs");
        if let Some(owner) = owner {
            request = request.query("owner", owner);
        }
        let processes: Vec<Proc> = self.json(request.call())?;
        for process in &processes {
            self.remember(process);
        }
        Ok(processes)
    }

    fn remember(&self, process: &Proc) {
        if let Some(instance) = &process.instance_id {
            let mut cache = self.instances.lock().expect("process identities");
            if !cache.iter().any(|(id, _)| id == &process.id) {
                // Active links pin their own identity independently of this
                // bounded convenience cache. Explicit attach can pin older IDs.
                if cache.len() >= 256 {
                    cache.pop_front();
                }
                cache.push_back((process.id.clone(), instance.clone()));
            }
        }
    }

    /// Starts `agent` in `cwd` on the device.
    pub fn spawn(&self, agent: &str, cwd: &str, owner: &str) -> Result<Proc> {
        let process: Proc = self.json(self.request("POST", "/v1/procs").send_json(json!({
            "agent": agent,
            "cwd": cwd,
            "owner": owner,
        })))?;
        self.remember(&process);
        Ok(process)
    }

    pub fn kill(&self, id: &str) -> Result<()> {
        answer(self.request("DELETE", &format!("/v1/procs/{id}")).call()).map(|_| ())
    }

    pub fn list(&self, path: &str) -> Result<Vec<FsEntry>> {
        #[derive(Deserialize)]
        struct Listing {
            entries: Vec<FsEntry>,
        }
        let listing: Listing = self.json(
            self.request("GET", "/v1/fs/list")
                .query("path", path)
                .call(),
        )?;
        Ok(listing.entries)
    }

    pub fn stat(&self, path: &str) -> Result<FsStat> {
        self.json(
            self.request("GET", "/v1/fs/stat")
                .query("path", path)
                .call(),
        )
    }

    /// A file's text; `None` when it does not exist.
    pub fn read(&self, path: &str) -> Result<Option<String>> {
        match answer(
            self.request("GET", "/v1/fs/read")
                .query("path", path)
                .call(),
        ) {
            Ok(response) => response
                .into_string()
                .map(Some)
                .map_err(|e| NodeError::Unreachable(e.to_string())),
            Err(NodeError::Refused { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn write(&self, path: &str, text: &str) -> Result<()> {
        answer(
            self.request("PUT", "/v1/fs/write")
                .query("path", path)
                .send_string(text),
        )
        .map(|_| ())
    }

    pub fn mkdir(&self, path: &str) -> Result<()> {
        answer(
            self.request("POST", "/v1/fs/mkdir")
                .query("path", path)
                .call(),
        )
        .map(|_| ())
    }

    /// Attaches to a process's input and output. `after` is the last output
    /// line already seen (0 for all kept, -1 for only new output).
    pub fn attach(&self, id: &str, after: i64) -> Attachment {
        let expected = self
            .instances
            .lock()
            .expect("process identities")
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, instance)| instance.clone());
        self.attach_with_instance(id, after, expected.as_deref())
    }

    /// Optionally pin a saved lifecycle identity, without persisting work content.
    /// Missing identities keep legacy-server behavior until a handshake supplies one.
    pub fn attach_with_instance(&self, id: &str, after: i64, expected: Option<&str>) -> Attachment {
        let (to_reader, from_link) = mpsc::channel::<Vec<u8>>();
        let (to_link, outgoing) = mpsc::channel::<String>();
        let state = Arc::new(Mutex::new(LinkState::Connecting));
        let link = Link {
            ws_base: self.base.replacen("http", "ws", 1),
            id: id.to_string(),
            token: self.token.clone(),
            last: after,
            instance: expected.map(str::to_owned),
            state: state.clone(),
        };
        let peer = self.base.clone();
        let spawned = thread::Builder::new()
            .name(format!("node-{id}"))
            .spawn(move || link.run(to_reader, outgoing));
        if let Err(error) = spawned {
            log::warn!("could not start the link to {peer}: {error}");
        }
        Attachment {
            reader: BufReader::new(PipeReader {
                incoming: from_link,
                buffer: Vec::new(),
                at: 0,
            }),
            writer: LineWriter {
                outgoing: to_link,
                pending: Vec::new(),
            },
            state,
        }
    }
}

fn answer(result: std::result::Result<ureq::Response, ureq::Error>) -> Result<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(status, response)) => {
            let body: Value = response.into_json().unwrap_or(Value::Null);
            Err(NodeError::Refused {
                status,
                code: body["error"].as_str().unwrap_or("error").to_string(),
                message: body["message"]
                    .as_str()
                    .unwrap_or("the node refused the request")
                    .to_string(),
            })
        }
        Err(error) => Err(NodeError::Unreachable(error.to_string())),
    }
}

/// A process on a node, stopped when the last holder lets go of it (Atrium
/// quitting, or an agent restarted).
pub struct RemoteProc {
    pub client: NodeClient,
    pub id: String,
}

impl Drop for RemoteProc {
    fn drop(&mut self) {
        if let Err(error) = self.client.kill(&self.id) {
            log::debug!(
                "could not stop {} on {}: {error}",
                self.id,
                self.client.base
            );
        }
    }
}

/// How an attachment's link is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkState {
    Connecting,
    Connected,
    /// Lost; trying again.
    Reconnecting {
        attempt: u32,
        error: String,
    },
    /// The process ended.
    Ended {
        code: Option<i32>,
    },
    /// The process is gone from the node, or the link was given up.
    Gone(String),
}

/// A process's output and input, shaped like a child's stdio.
pub struct Attachment {
    pub reader: BufReader<PipeReader>,
    pub writer: LineWriter,
    pub state: Arc<Mutex<LinkState>>,
}

/// Output lines, each ending in `\n`; end of file once the process ended or
/// the link was given up.
pub struct PipeReader {
    incoming: Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    at: usize,
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.buffer.len() {
            match self.incoming.recv() {
                Ok(bytes) => {
                    self.buffer = bytes;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buffer.len() - self.at);
        out[..n].copy_from_slice(&self.buffer[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Input: whole lines go to the link as they are completed.
pub struct LineWriter {
    outgoing: Sender<String>,
    pending: Vec<u8>,
}

impl Write for LineWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            self.outgoing
                .send(line)
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the link is closed"))?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

struct Link {
    ws_base: String,
    id: String,
    token: Option<String>,
    /// The last output line delivered.
    last: i64,
    instance: Option<String>,
    state: Arc<Mutex<LinkState>>,
}

/// How a connected stretch ended.
enum Stop {
    /// The process ended or the client let go: stop for good.
    Done,
    /// The socket broke: try again.
    Lost(String),
}

impl Link {
    fn set(&self, state: LinkState) {
        *self.state.lock().expect("link state") = state;
    }

    fn run(mut self, to_reader: Sender<Vec<u8>>, outgoing: Receiver<String>) {
        let mut attempt = 0u32;
        // Lines written while disconnected, sent once back.
        let mut queued: Vec<String> = Vec::new();
        loop {
            match self.connect() {
                Ok(mut socket) => {
                    attempt = 0;
                    self.set(LinkState::Connected);
                    match self.pump(&mut socket, &to_reader, &outgoing, &mut queued) {
                        Stop::Done => {
                            let _ = socket.close(None);
                            return;
                        }
                        Stop::Lost(error) => {
                            log::info!("link to {} lost: {error}", self.id);
                            attempt += 1;
                            self.set(LinkState::Reconnecting { attempt, error });
                        }
                    }
                }
                Err(ConnectError::Gone(why)) => {
                    self.set(LinkState::Gone(why));
                    return;
                }
                Err(ConnectError::Failed(error)) => {
                    attempt += 1;
                    self.set(LinkState::Reconnecting { attempt, error });
                }
            }
            // Back off, still taking lines to send later; stop when the
            // client let go.
            let wait = Duration::from_millis((500u64 << attempt.min(5)).min(15_000));
            match outgoing.recv_timeout(wait) {
                Ok(line) => queued.push(line),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.set(LinkState::Gone("closed".into()));
                    return;
                }
            }
        }
    }

    fn connect(&mut self) -> std::result::Result<Socket, ConnectError> {
        let url = format!(
            "{}/v1/procs/{}/attach?after={}",
            self.ws_base, self.id, self.last
        );
        let mut request = tungstenite::client::IntoClientRequest::into_client_request(url.as_str())
            .map_err(|e| ConnectError::Failed(e.to_string()))?;
        if let Some(token) = &self.token
            && let Ok(value) = format!("Bearer {token}").parse()
        {
            request.headers_mut().insert("Authorization", value);
        }
        if let Some(instance) = &self.instance {
            if instance.is_empty() || instance.len() > 128 || !instance.is_ascii() {
                return Err(ConnectError::Gone(
                    "invalid saved node instance identity".into(),
                ));
            }
            let value = instance
                .parse()
                .map_err(|_| ConnectError::Gone("invalid saved node instance identity".into()))?;
            request.headers_mut().insert("x-atrium-instance-id", value);
        }
        match tungstenite::connect(request) {
            Ok((socket, response)) => {
                if let Some(instance) = response.headers().get("x-atrium-instance-id") {
                    let instance = instance
                        .to_str()
                        .ok()
                        .filter(|v| !v.is_empty() && v.len() <= 128)
                        .ok_or_else(|| {
                            ConnectError::Gone("invalid node instance identity".into())
                        })?;
                    if self
                        .instance
                        .as_deref()
                        .is_some_and(|expected| expected != instance)
                    {
                        return Err(ConnectError::Gone(
                            "node restarted; previous process state is lost".into(),
                        ));
                    }
                    self.instance = Some(instance.to_owned());
                } else if self.instance.is_some() {
                    return Err(ConnectError::Gone(
                        "node no longer supports pinned lifecycle identity".into(),
                    ));
                }
                if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                    let _ = stream.set_nodelay(true);
                }
                Ok(socket)
            }
            Err(tungstenite::Error::Http(response))
                if response.status() == 409
                    && response
                        .body()
                        .as_deref()
                        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
                        .is_some_and(|body| {
                            body["error"] == "node_restarted" || body["code"] == "node_restarted"
                        }) =>
            {
                Err(ConnectError::Gone(
                    "node restarted; previous process state is lost".into(),
                ))
            }
            Err(tungstenite::Error::Http(response)) if response.status() == 404 => Err(
                ConnectError::Gone("the process is no longer on the node".into()),
            ),
            Err(tungstenite::Error::Http(response)) if response.status() == 403 => Err(
                ConnectError::Gone("the node does not allow this device".into()),
            ),
            Err(error) => Err(ConnectError::Failed(error.to_string())),
        }
    }

    fn pump(
        &mut self,
        socket: &mut Socket,
        to_reader: &Sender<Vec<u8>>,
        outgoing: &Receiver<String>,
        queued: &mut Vec<String>,
    ) -> Stop {
        for line in queued.drain(..) {
            if let Err(error) = socket.send(Message::text(json!({ "line": line }).to_string())) {
                return Stop::Lost(error.to_string());
            }
        }
        loop {
            // Send what the client wrote.
            loop {
                match outgoing.try_recv() {
                    Ok(line) => {
                        if let Err(error) =
                            socket.send(Message::text(json!({ "line": line }).to_string()))
                        {
                            queued.push(line);
                            return Stop::Lost(error.to_string());
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.set(LinkState::Gone("closed".into()));
                        return Stop::Done;
                    }
                }
            }
            // Take what the node sent.
            match socket.read() {
                Ok(Message::Text(text)) => {
                    let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    if let Some(seq) = frame["seq"].as_i64() {
                        if seq <= self.last && self.last >= 0 {
                            continue;
                        }
                        self.last = seq;
                    }
                    if let Some(line) = frame["line"].as_str() {
                        let mut bytes = line.as_bytes().to_vec();
                        bytes.push(b'\n');
                        if to_reader.send(bytes).is_err() {
                            self.set(LinkState::Gone("closed".into()));
                            return Stop::Done;
                        }
                    } else if let Some(line) = frame["stderr"].as_str() {
                        log::debug!("{} stderr: {line}", self.id);
                    } else if frame.get("gap").is_some() {
                        log::warn!("{}: the node dropped output: {}", self.id, frame["gap"]);
                    } else if frame.get("exit").is_some() {
                        let code = frame["exit"]["code"].as_i64().map(|c| c as i32);
                        self.set(LinkState::Ended { code });
                        return Stop::Done;
                    }
                }
                Ok(Message::Close(frame)) => {
                    if frame.as_ref().is_some_and(|f| u16::from(f.code) == 4001) {
                        self.set(LinkState::Gone("attached elsewhere".into()));
                        return Stop::Done;
                    }
                    return Stop::Lost("the node closed the connection".into());
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Stop::Lost(error.to_string()),
            }
        }
    }
}

enum ConnectError {
    Gone(String),
    Failed(String),
}

/// The Tailscale CLI, where the app or a package put it.
fn tailscale_binary() -> Option<std::path::PathBuf> {
    crate::process::which("tailscale").or_else(|| {
        [
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
        ]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
    })
}

/// A device on the tailnet, as `tailscale status` lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub name: String,
    /// The MagicDNS name, without the trailing dot, when there is one.
    pub dns: Option<String>,
    pub ip: Option<String>,
    pub online: bool,
    pub os: String,
}

/// The tailnet's other devices, or why they could not be listed.
pub fn tailnet_peers() -> std::result::Result<Vec<Peer>, String> {
    let binary = tailscale_binary().ok_or("Tailscale is not installed here")?;
    let output = std::process::Command::new(binary)
        .args(["status", "--json"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let status: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    Ok(parse_peers(&status))
}

/// Reads the peers out of `tailscale status --json`.
pub fn parse_peers(status: &Value) -> Vec<Peer> {
    let mut peers: Vec<Peer> = status["Peer"]
        .as_object()
        .map(|peers| {
            peers
                .values()
                .map(|p| Peer {
                    name: p["HostName"].as_str().unwrap_or_default().to_string(),
                    dns: p["DNSName"]
                        .as_str()
                        .map(|d| d.trim_end_matches('.').to_string())
                        .filter(|d| !d.is_empty()),
                    ip: p["TailscaleIPs"]
                        .as_array()
                        .and_then(|ips| {
                            ips.iter()
                                .find_map(|ip| ip.as_str().filter(|ip| ip.contains('.')))
                        })
                        .map(str::to_owned),
                    online: p["Online"].as_bool().unwrap_or(false),
                    os: p["OS"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    peers
}

/// Asks each online peer whether it runs a node, a few at a time.
pub fn find_nodes(peers: &[Peer], token: Option<String>) -> Vec<(NodeClient, NodeInfo)> {
    let found = Arc::new(Mutex::new(HashMap::new()));
    let handles: Vec<_> = peers
        .iter()
        .filter(|p| p.online)
        .filter_map(|p| p.ip.clone().or_else(|| p.dns.clone()))
        .map(|address| {
            let (found, token) = (found.clone(), token.clone());
            thread::spawn(move || {
                let client = NodeClient::new(&address, token);
                if let Ok(info) = client.info() {
                    found.lock().expect("found").insert(address, (client, info));
                }
            })
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
    let mut found: Vec<(NodeClient, NodeInfo)> = std::mem::take(&mut *found.lock().expect("found"))
        .into_values()
        .collect();
    found.sort_by(|a, b| a.1.device.name.cmp(&b.1.device.name));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_become_base_urls() {
        assert_eq!(NodeClient::new("studio", None).base, "http://studio:7381");
        assert_eq!(
            NodeClient::new("100.64.0.2:9000", None).base,
            "http://100.64.0.2:9000"
        );
        assert_eq!(
            NodeClient::new("http://studio.tail1.ts.net:7381/", None).base,
            "http://studio.tail1.ts.net:7381"
        );
    }

    #[test]
    fn peers_are_read_from_tailscale_status() {
        let status = json!({
            "Self": {"HostName": "laptop"},
            "Peer": {
                "nodekey:a": {"HostName": "studio", "DNSName": "studio.tail1.ts.net.",
                    "TailscaleIPs": ["100.64.0.2", "fd7a:115c:a1e0::2"], "Online": true, "OS": "macOS"},
                "nodekey:b": {"HostName": "nas", "DNSName": "", "TailscaleIPs": ["100.64.0.3"],
                    "Online": false, "OS": "linux"}
            }
        });
        let peers = parse_peers(&status);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[1].name, "studio");
        assert_eq!(peers[1].dns.as_deref(), Some("studio.tail1.ts.net"));
        assert_eq!(peers[1].ip.as_deref(), Some("100.64.0.2"));
        assert!(!peers[0].online && peers[0].dns.is_none());
    }

    #[test]
    fn temporary_disconnect_reconnects_with_learned_instance() {
        struct IdentityHandshake {
            reconnect: bool,
        }
        impl tungstenite::handshake::server::Callback for IdentityHandshake {
            fn on_request(
                self,
                req: &tungstenite::handshake::server::Request,
                mut response: tungstenite::handshake::server::Response,
            ) -> std::result::Result<
                tungstenite::handshake::server::Response,
                tungstenite::handshake::server::ErrorResponse,
            > {
                if self.reconnect {
                    assert_eq!(req.headers()["x-atrium-instance-id"], "same-runtime");
                }
                response
                    .headers_mut()
                    .insert("x-atrium-instance-id", "same-runtime".parse().unwrap());
                Ok(response)
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (disconnect, drop_now) = mpsc::channel();
        let server = thread::spawn(move || {
            for attempt in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut ws = tungstenite::accept_hdr(
                    stream,
                    IdentityHandshake {
                        reconnect: attempt == 1,
                    },
                )
                .unwrap();
                if attempt == 0 {
                    drop_now.recv_timeout(Duration::from_secs(5)).unwrap();
                    drop(ws);
                } else {
                    let message = ws.read().unwrap();
                    assert_eq!(
                        serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap()["line"],
                        "queued"
                    );
                    ws.send(Message::text(
                        json!({"seq":1,"exit":{"code":0,"signal":null}}).to_string(),
                    ))
                    .unwrap();
                }
            }
        });
        let client = NodeClient::new(&format!("http://{address}"), None);
        let mut attachment = client.attach("p", 0);
        let wait = |predicate: fn(&LinkState) -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            while !predicate(&attachment.state.lock().unwrap()) {
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(10));
            }
        };
        wait(|s| matches!(s, LinkState::Connected));
        client.remember(&Proc {
            id: "p".into(),
            instance_id: Some("same-runtime".into()),
            ..Proc::default()
        });
        for index in 0..257 {
            client.remember(&Proc {
                id: format!("other-{index}"),
                instance_id: Some("other-runtime".into()),
                ..Proc::default()
            });
        }
        assert_eq!(client.instances.lock().unwrap().len(), 256);
        assert!(
            !client
                .instances
                .lock()
                .unwrap()
                .iter()
                .any(|(id, _)| id == "p")
        );
        disconnect.send(()).unwrap();
        wait(|s| matches!(s, LinkState::Reconnecting { .. }));
        attachment.writer.write_all(b"queued\n").unwrap();
        wait(|s| matches!(s, LinkState::Ended { code: Some(0) }));
        server.join().unwrap();
    }

    #[test]
    fn pinned_handshake_without_identity_never_sends_queued_input() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            // A legacy/downgraded server completes WS upgrade without identity.
            let mut ws = tungstenite::accept(stream).unwrap();
            assert!(
                !matches!(ws.read(), Ok(Message::Text(_))),
                "queued stdin escaped before instance verification"
            );
        });
        let state = Arc::new(Mutex::new(LinkState::Connecting));
        let link = Link {
            ws_base: format!("ws://{address}"),
            id: "old".into(),
            token: None,
            last: 0,
            instance: Some("previous-runtime".into()),
            state: state.clone(),
        };
        let (reader, _incoming) = mpsc::channel();
        let (writer, outgoing) = mpsc::channel();
        writer.send("must-not-send".into()).unwrap();
        link.run(reader, outgoing);
        assert_eq!(
            *state.lock().unwrap(),
            LinkState::Gone("node no longer supports pinned lifecycle identity".into())
        );
        server.join().unwrap();
    }

    #[test]
    fn pinned_handshake_with_different_identity_never_sends_queued_input() {
        struct WrongIdentity;
        impl tungstenite::handshake::server::Callback for WrongIdentity {
            fn on_request(
                self,
                request: &tungstenite::handshake::server::Request,
                mut response: tungstenite::handshake::server::Response,
            ) -> std::result::Result<
                tungstenite::handshake::server::Response,
                tungstenite::handshake::server::ErrorResponse,
            > {
                assert_eq!(
                    request.headers()["x-atrium-instance-id"],
                    "previous-runtime"
                );
                response
                    .headers_mut()
                    .insert("x-atrium-instance-id", "different-runtime".parse().unwrap());
                Ok(response)
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            // A faulty server accepts upgrade despite the expected identity mismatch.
            let mut ws = tungstenite::accept_hdr(stream, WrongIdentity).unwrap();
            assert!(
                !matches!(ws.read(), Ok(Message::Text(_))),
                "queued stdin escaped before instance verification"
            );
        });
        let state = Arc::new(Mutex::new(LinkState::Connecting));
        let link = Link {
            ws_base: format!("ws://{address}"),
            id: "old".into(),
            token: None,
            last: 0,
            instance: Some("previous-runtime".into()),
            state: state.clone(),
        };
        let (reader, _incoming) = mpsc::channel();
        let (writer, outgoing) = mpsc::channel();
        writer.send("must-not-send".into()).unwrap();
        link.run(reader, outgoing);
        assert_eq!(
            *state.lock().unwrap(),
            LinkState::Gone("node restarted; previous process state is lost".into())
        );
        server.join().unwrap();
    }

    #[test]
    fn invalid_saved_identity_is_terminal_before_network_io() {
        for identity in [
            "".to_owned(),
            "x".repeat(129),
            "é".to_owned(),
            "bad\r\nheader".to_owned(),
        ] {
            let mut link = Link {
                ws_base: "ws://127.0.0.1:1".into(),
                id: "p".into(),
                token: None,
                last: 0,
                instance: Some(identity),
                state: Arc::new(Mutex::new(LinkState::Connecting)),
            };
            assert!(
                matches!(link.connect(), Err(ConnectError::Gone(why)) if why.contains("invalid saved"))
            );
        }
    }

    #[test]
    fn legacy_metadata_and_bounded_process_identity_pins() {
        let info: NodeInfo = serde_json::from_value(json!({"protocol":1})).unwrap();
        assert_eq!(info.instance_id, None);
        let client = NodeClient::new("http://127.0.0.1:1", None);
        let mut p = Proc {
            id: "p-1".into(),
            instance_id: Some("old".into()),
            ..Proc::default()
        };
        client.remember(&p);
        p.instance_id = Some("new".into());
        client.remember(&p);
        assert_eq!(client.instances.lock().unwrap().front().unwrap().1, "old");
        for i in 2..300 {
            p.id = format!("p-{i}");
            client.remember(&p);
        }
        assert_eq!(client.instances.lock().unwrap().len(), 256);
    }

    #[test]
    fn written_lines_go_out_whole() {
        let (tx, rx) = mpsc::channel();
        let mut writer = LineWriter {
            outgoing: tx,
            pending: Vec::new(),
        };
        writer.write_all(b"{\"a\":1}\n{\"b\"").unwrap();
        writer.write_all(b":2}\n").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "{\"a\":1}");
        assert_eq!(rx.try_recv().unwrap(), "{\"b\":2}");
    }
}
