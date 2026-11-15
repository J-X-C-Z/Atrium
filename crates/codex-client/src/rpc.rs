//! Newline-delimited JSON-RPC, as `codex app-server` speaks it on stdio.
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

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// One JSON-RPC connection. Cheap to clone; all clones share the stream.
#[derive(Clone)]
pub struct Connection {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pending: Pending,
    next_id: Arc<AtomicI64>,
}

impl Connection {
    /// Starts reading `reader` on its own thread. Notifications and server
    /// requests arrive on the returned channel, in order.
    pub fn new(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> (Self, mpsc::UnboundedReceiver<Incoming>) {
        let (tx, rx) = mpsc::unbounded();
        let pending: Pending = Arc::default();
        let connection = Self {
            writer: Arc::new(Mutex::new(Box::new(writer))),
            pending: pending.clone(),
            next_id: Arc::new(AtomicI64::new(1)),
        };
        thread::Builder::new()
            .name("codex-rpc-reader".into())
            .spawn(move || read_loop(reader, pending, tx))
            .expect("spawn the rpc reader thread");
        (connection, rx)
    }

    /// Sends a request and waits for its result.
    pub async fn request(&self, method: &str, params: impl Serialize) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("pending lock").insert(id, tx);
        let params = serde_json::to_value(params).map_err(|e| RpcError::Write(e.to_string()))?;
        log::debug!("codex >> #{id} {method}");
        if let Err(error) = self.send(&json!({ "id": id, "method": method, "params": params })) {
            self.pending.lock().expect("pending lock").remove(&id);
            return Err(error);
        }
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
        log::debug!("codex >> {method}");
        match params {
            Some(params) => self.send(&json!({ "method": method, "params": params })),
            None => self.send(&json!({ "method": method })),
        }
    }

    /// Answers a server request.
    pub fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.send(&json!({ "id": id, "result": result }))
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<(), RpcError> {
        self.send(&json!({ "id": id, "error": { "code": code, "message": message } }))
    }

    fn send(&self, message: &Value) -> Result<(), RpcError> {
        let mut line = serde_json::to_vec(message).map_err(|e| RpcError::Write(e.to_string()))?;
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
    for (_, waiter) in pending.lock().expect("pending lock").drain() {
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
            let waiter = pending.lock().expect("pending lock").remove(&key);
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

    #[test]
    fn a_closed_server_fails_waiting_requests() {
        let (to_server, _server_in) = pipe();
        let (server_out, from_server) = pipe();
        let (connection, _events) = Connection::new(from_server, to_server);
        drop(server_out);
        let result = block_on(connection.request("initialize", json!({})));
        assert_eq!(result, Err(RpcError::Closed));
    }

    #[test]
    fn server_errors_come_back_as_errors() {
        let pending: Pending = Arc::default();
        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert(3, tx);
        let routed = route(
            json!({"id": 3, "error": {"code": -32600, "message": "bad"}}),
            &pending,
        );
        assert!(routed.is_none());
        let result = block_on(rx).unwrap();
        assert!(matches!(result, Err(RpcError::Server { code: -32600, .. })));
    }
}
