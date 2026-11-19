//! Newline-delimited JSON-RPC on stdio, as `codex app-server` and Agent
//! Client Protocol (ACP) agents speak it. Codex leaves out the `"jsonrpc"`
//! field; ACP requires `"jsonrpc": "2.0"` on every message.
//!
//! Messages carry no `"jsonrpc"` field. Three shapes arrive:
//! a response (`id` + `result`/`error`), a notification (`method`, no `id`)
//! and a server request (`method` + `id`, which we must answer).

use std::{
    collections::HashMap,
    io::{BufRead, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    thread,
};

use futures::channel::{mpsc, oneshot};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum RpcError {
    #[error("{message} (code {code})")]
    Server {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("the connection to the agent closed")]
    Closed,
    #[error("could not write to the agent: {0}")]
    Write(String),
    #[error("unexpected reply: {0}")]
    Decode(String),
}

/// What the server sends us besides responses.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    /// Must be answered with [`Connection::respond`] or [`Connection::respond_error`].
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// The server's output ended; nothing more will arrive.
    Closed,
}

#[derive(Default)]
struct PendingState {
    closed: bool,
    waiters: HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>,
}
type Pending = Arc<Mutex<PendingState>>;

/// A request future owns only its local response waiter, not remote execution.
/// Removing an already answered/closed entry is intentionally a no-op.
struct PendingRegistration {
    pending: Pending,
    id: i64,
}
impl Drop for PendingRegistration {
    fn drop(&mut self) {
        let waiter = self
            .pending
            .lock()
            .expect("pending lock")
            .waiters
            .remove(&self.id);
        // Dropping a sender can wake an executor; do that outside the mutex.
        drop(waiter);
    }
}

/// One JSON-RPC connection. Cheap to clone; all clones share the stream.
#[derive(Clone)]
pub struct Connection {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pending: Pending,
    next_id: Arc<AtomicI64>,
    /// Tag outgoing messages with `"jsonrpc": "2.0"`.
    jsonrpc2: bool,
    /// The event channel, to queue local markers behind what already arrived.
    events: mpsc::UnboundedSender<Incoming>,
    /// Who is on the other end, for logs.
    peer: &'static str,
}

impl Connection {
    /// Starts reading `reader` on its own thread. Notifications and server
    /// requests arrive on the returned channel, in order.
    pub fn new(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> (Self, mpsc::UnboundedReceiver<Incoming>) {
        Self::with(reader, writer, false, "codex")
    }

    /// A strict JSON-RPC 2.0 connection, as ACP needs.
    pub fn jsonrpc2(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        peer: &'static str,
    ) -> (Self, mpsc::UnboundedReceiver<Incoming>) {
        Self::with(reader, writer, true, peer)
    }

    fn with(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        jsonrpc2: bool,
        peer: &'static str,
    ) -> (Self, mpsc::UnboundedReceiver<Incoming>) {
        let (tx, rx) = mpsc::unbounded();
        let pending: Pending = Arc::default();
        let connection = Self {
            writer: Arc::new(Mutex::new(Box::new(writer))),
            pending: pending.clone(),
            next_id: Arc::new(AtomicI64::new(1)),
            jsonrpc2,
            peer,
            events: tx.clone(),
        };
        thread::Builder::new()
            .name(format!("{peer}-rpc-reader"))
            .spawn(move || read_loop(reader, pending, tx))
            .expect("spawn the rpc reader thread");
        (connection, rx)
    }

    /// Sends a request and waits for its result.
    pub async fn request(&self, method: &str, params: impl Serialize) -> Result<Value, RpcError> {
        let params = serde_json::to_value(params).map_err(|e| RpcError::Write(e.to_string()))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            // Registration and EOF closure share one linearization point:
            // either EOF drains this waiter, or we observe closed and never send.
            let mut pending = self.pending.lock().expect("pending lock");
            if pending.closed {
                return Err(RpcError::Closed);
            }
            pending.waiters.insert(id, tx);
        }
        let _registration = PendingRegistration {
            pending: self.pending.clone(),
            id,
        };
        log::debug!("{} >> #{id} {method}", self.peer);
        self.send(&json!({ "id": id, "method": method, "params": params }))?;
        rx.await.unwrap_or(Err(RpcError::Closed))
    }

    /// Sends a request and decodes its result into `T`.
    pub async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<T, RpcError> {
        let value = self.request(method, params).await?;
        serde_json::from_value(value).map_err(|e| RpcError::Decode(format!("{method}: {e}")))
    }

    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), RpcError> {
        log::debug!("{} >> {method}", self.peer);
        match params {
            Some(params) => self.send(&json!({ "method": method, "params": params })),
            None => self.send(&json!({ "method": method })),
        }
    }

    /// Queues `incoming` on the event channel, behind everything the peer
    /// sent so far: a marker that arrives in order with the peer's messages.
    pub fn inject(&self, incoming: Incoming) {
        let _ = self.events.unbounded_send(incoming);
    }

    /// Answers a server request.
    pub fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.send(&json!({ "id": id, "result": result }))
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<(), RpcError> {
        self.send(&json!({ "id": id, "error": { "code": code, "message": message } }))
    }

    fn send(&self, message: &Value) -> Result<(), RpcError> {
        let mut message = message.clone();
        if self.jsonrpc2 {
            message["jsonrpc"] = json!("2.0");
        }
        let mut line = serde_json::to_vec(&message).map_err(|e| RpcError::Write(e.to_string()))?;
        line.push(b'\n');
        let mut writer = self.writer.lock().expect("writer lock");
        writer
            .write_all(&line)
            .and_then(|()| writer.flush())
            .map_err(|e| RpcError::Write(e.to_string()))
    }
}

fn read_loop(reader: impl BufRead, pending: Pending, tx: mpsc::UnboundedSender<Incoming>) {
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                log::warn!("codex: read failed: {error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                log::warn!("codex: not JSON ({error}): {}", truncate(&line, 200));
                continue;
            }
        };
        if let Some(incoming) = route(message, &pending)
            && tx.unbounded_send(incoming).is_err()
        {
            // Nobody listens any more; keep answering responses though.
            log::debug!("codex: event dropped, no listener");
        }
    }
    let waiters = {
        let mut pending = pending.lock().expect("pending lock");
        pending.closed = true;
        std::mem::take(&mut pending.waiters)
    };
    // Waking arbitrary executors under the pending mutex can re-enter request.
    for (_, waiter) in waiters {
        let _ = waiter.send(Err(RpcError::Closed));
    }
    let _ = tx.unbounded_send(Incoming::Closed);
}

/// Resolves a response against its waiter, or hands back what the app must see.
fn route(mut message: Value, pending: &Pending) -> Option<Incoming> {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let id = message.get("id").cloned();
    match (method, id) {
        (Some(method), Some(id)) => Some(Incoming::Request {
            id,
            method,
            params: message
                .get_mut("params")
                .map(Value::take)
                .unwrap_or(Value::Null),
        }),
        (Some(method), None) => Some(Incoming::Notification {
            method,
            params: message
                .get_mut("params")
                .map(Value::take)
                .unwrap_or(Value::Null),
        }),
        (None, Some(id)) => {
            let Some(key) = id.as_i64() else {
                log::warn!("codex: response with a foreign id {id}");
                return None;
            };
            let waiter = pending.lock().expect("pending lock").waiters.remove(&key);
            let Some(waiter) = waiter else {
                log::warn!("codex: response to unknown request #{key}");
                return None;
            };
            let result = match message.get_mut("error") {
                Some(error) => Err(RpcError::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                    data: error.get_mut("data").map(Value::take),
                }),
                None => Ok(message
                    .get_mut("result")
                    .map(Value::take)
                    .unwrap_or(Value::Null)),
            };
            log::debug!(
                "codex << #{key} {}",
                if result.is_ok() { "ok" } else { "error" }
            );
            let _ = waiter.send(result);
            None
        }
        (None, None) => {
            log::warn!("codex: message with neither id nor method");
            None
        }
    }
}

pub(crate) fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read};

    use futures::{StreamExt, executor::block_on};

    use super::*;

    /// A pipe: what one side writes, the other reads.
    fn pipe() -> (PipeWriter, BufReader<PipeReader>) {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        (
            PipeWriter(tx),
            BufReader::new(PipeReader {
                rx,
                buf: Vec::new(),
            }),
        )
    }

    struct PipeWriter(std::sync::mpsc::Sender<Vec<u8>>);
    impl Write for PipeWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct PipeReader {
        rx: std::sync::mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
    }
    impl Read for PipeReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(bytes) => self.buf = bytes,
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn requests_match_responses_and_events_flow_in_order() {
        // client -> server
        let (to_server, mut server_in) = pipe();
        // server -> client
        let (mut server_out, from_server) = pipe();
        let (connection, mut events) = Connection::new(from_server, to_server);

        let server = thread::spawn(move || {
            let mut line = String::new();
            server_in.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "thread/list");
            let id = request["id"].clone();
            writeln!(
                server_out,
                r#"{{"method":"warning","params":{{"message":"hi"}}}}"#
            )
            .unwrap();
            writeln!(
                server_out,
                r#"{{"id":"srv-1","method":"item/fileChange/requestApproval","params":{{}}}}"#
            )
            .unwrap();
            writeln!(server_out, "{}", json!({"id": id, "result": {"data": []}})).unwrap();
            drop(server_out);
        });

        let result = block_on(connection.request("thread/list", json!({}))).unwrap();
        assert_eq!(result, json!({"data": []}));
        server.join().unwrap();

        let got: Vec<Incoming> = block_on(events.by_ref().take(3).collect());
        assert!(matches!(&got[0], Incoming::Notification { method, .. } if method == "warning"));
        assert!(matches!(&got[1], Incoming::Request { id, .. } if id == "srv-1"));
        assert_eq!(got[2], Incoming::Closed);
    }

    fn within<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(work());
        });
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .expect("RPC operation exceeded test deadline")
    }

    fn poll_pending<F: std::future::Future>(future: std::pin::Pin<&mut F>) {
        let mut cx = std::task::Context::from_waker(futures::task::noop_waker_ref());
        assert!(future.poll(&mut cx).is_pending());
    }

    #[test]
    fn cancelled_requests_release_waiters_without_sending_cancel_commands() {
        within(|| {
            let (to_server, server_in) = pipe();
            let (_server_out, from_server) = pipe(); // open forever, never responds
            let (connection, _events) = Connection::new(from_server, to_server);
            for _ in 0..128 {
                let mut request = Box::pin(connection.request("never-replies", json!({})));
                poll_pending(request.as_mut());
                assert_eq!(connection.pending.lock().unwrap().waiters.len(), 1);
                let bytes = server_in.get_ref().rx.try_recv().unwrap();
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes).unwrap()["method"],
                    "never-replies"
                );
                drop(request);
                assert!(
                    connection.pending.lock().unwrap().waiters.is_empty(),
                    "dropped request leaked a pending sender"
                );
                assert!(
                    server_in.get_ref().rx.try_recv().is_err(),
                    "local cancellation must not send an interrupt or retry"
                );
            }
        });
    }

    #[test]
    fn cancellation_ignores_late_response_and_preserves_other_waiters() {
        within(|| {
            let (to_server, server_in) = pipe();
            let (_server_out, from_server) = pipe();
            let (connection, _events) = Connection::new(from_server, to_server);
            let mut cancelled = Box::pin(connection.request("cancelled", json!({})));
            let mut surviving = Box::pin(connection.request("survives", json!({})));
            poll_pending(cancelled.as_mut());
            let first: Value =
                serde_json::from_slice(&server_in.get_ref().rx.try_recv().unwrap()).unwrap();
            poll_pending(surviving.as_mut());
            let second: Value =
                serde_json::from_slice(&server_in.get_ref().rx.try_recv().unwrap()).unwrap();
            drop(cancelled);
            assert_eq!(connection.pending.lock().unwrap().waiters.len(), 1);
            assert!(
                route(
                    json!({"id":first["id"],"result":"late"}),
                    &connection.pending
                )
                .is_none()
            );
            assert_eq!(connection.pending.lock().unwrap().waiters.len(), 1);
            route(
                json!({"id":second["id"],"result":"expected"}),
                &connection.pending,
            );
            assert_eq!(block_on(surviving), Ok(json!("expected")));
            assert!(connection.pending.lock().unwrap().waiters.is_empty());
        });
    }

    #[test]
    fn cancel_response_and_eof_race_releases_all_waiters() {
        within(|| {
            for _ in 0..64 {
                let (to_server, server_in) = pipe();
                let (mut server_out, from_server) = pipe();
                let (connection, mut events) = Connection::new(from_server, to_server);
                let worker_connection = connection.clone();
                let barrier = Arc::new(std::sync::Barrier::new(2));
                let worker_barrier = barrier.clone();
                let (registered, ready) = std::sync::mpsc::channel();
                let (finished, done) = std::sync::mpsc::channel();
                let worker = thread::spawn(move || {
                    let mut request = Box::pin(worker_connection.request("race", json!({})));
                    poll_pending(request.as_mut());
                    registered.send(()).unwrap();
                    worker_barrier.wait();
                    drop(request);
                    finished.send(()).unwrap();
                });
                ready
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap();
                let sent: Value =
                    serde_json::from_slice(&server_in.get_ref().rx.try_recv().unwrap()).unwrap();
                barrier.wait();
                writeln!(server_out, "{}", json!({"id":sent["id"],"result":"racing"})).unwrap();
                drop(server_out);
                done.recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap();
                worker.join().unwrap();
                assert_eq!(block_on(events.next()), Some(Incoming::Closed));
                let pending = connection.pending.lock().unwrap();
                assert!(pending.closed && pending.waiters.is_empty());
            }
        });
    }

    #[test]
    fn serialization_and_write_errors_leave_no_pending_entries() {
        struct BadParams;
        impl Serialize for BadParams {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("fixture serialization failure"))
            }
        }
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "fixture write failure",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        within(|| {
            let (_server_out, from_server) = pipe();
            let (connection, _events) = Connection::new(from_server, BrokenWriter);
            assert!(
                matches!(block_on(connection.request("bad-params", BadParams)), Err(RpcError::Write(e)) if e.contains("serialization"))
            );
            assert!(connection.pending.lock().unwrap().waiters.is_empty());
            assert!(
                matches!(block_on(connection.request("broken-write", json!({}))), Err(RpcError::Write(e)) if e.contains("write failure"))
            );
            assert!(connection.pending.lock().unwrap().waiters.is_empty());
        });
    }

    #[test]
    fn a_closed_server_fails_waiting_requests() {
        let result = within(|| {
            let (to_server, _server_in) = pipe();
            let (server_out, from_server) = pipe();
            let (connection, _events) = Connection::new(from_server, to_server);
            drop(server_out);
            block_on(connection.request("initialize", json!({})))
        });
        assert_eq!(result, Err(RpcError::Closed));
    }

    #[test]
    fn close_before_request_fails_immediately_without_writing() {
        within(|| {
            let (to_server, server_in) = pipe();
            let (server_out, from_server) = pipe();
            let (connection, mut events) = Connection::new(from_server, to_server);
            drop(server_out);
            assert_eq!(block_on(events.next()), Some(Incoming::Closed));
            assert_eq!(
                block_on(connection.request("after-close", json!({}))),
                Err(RpcError::Closed)
            );
            assert!(
                server_in.get_ref().rx.try_recv().is_err(),
                "request after observed EOF must not write"
            );
        });
    }

    #[test]
    fn concurrent_close_and_request_always_resolve() {
        for _ in 0..64 {
            let (to_server, _server_in) = pipe();
            let (server_out, from_server) = pipe();
            let (connection, _events) = Connection::new(from_server, to_server);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let worker_barrier = barrier.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                worker_barrier.wait();
                let result = block_on(connection.request("racing-close", json!({})));
                let _ = tx.send(result);
            });
            barrier.wait();
            drop(server_out);
            assert_eq!(
                rx.recv_timeout(std::time::Duration::from_secs(3))
                    .expect("close/request race hung"),
                Err(RpcError::Closed)
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn server_errors_come_back_as_errors() {
        let pending: Pending = Arc::default();
        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().waiters.insert(3, tx);
        let routed = route(
            json!({"id": 3, "error": {"code": -32600, "message": "bad"}}),
            &pending,
        );
        assert!(routed.is_none());
        let result = block_on(rx).unwrap();
        assert!(matches!(result, Err(RpcError::Server { code: -32600, .. })));
    }
}
