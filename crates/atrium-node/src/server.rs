use crate::{
    auth::Auth,
    config::{Config, find_binary},
    fs::{FsError, Roots},
    procs::{InputCapacity, InputReplaced, MAX_LINE_BYTES, Process, ProcessManager},
};
use anyhow::Result;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, State as AxState,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use futures::SinkExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore};
pub struct State {
    pub config: Arc<Config>,
    pub path: String,
    pub roots: Arc<Roots>,
    pub procs: ProcessManager,
    info: RwLock<Value>,
    instance_id: String,
    ws_slots: Arc<Semaphore>,
}
impl State {
    pub async fn new(config: Arc<Config>, path: String) -> Result<Arc<Self>> {
        let roots = Arc::new(Roots::new(config.roots.clone())?);
        let s = Arc::new(Self {
            procs: ProcessManager::with_total_output_bytes(
                config.keep_output_lines,
                config.keep_output_bytes,
                config.detached_timeout_secs,
                config.total_output_bytes,
            )
            .with_admission_limits(
                config.total_input_bytes,
                config.max_ws_connections_per_process,
            ),
            ws_slots: Arc::new(Semaphore::new(config.max_ws_connections)),
            config,
            path,
            roots,
            info: RwLock::new(Value::Null),
            instance_id: uuid::Uuid::new_v4().to_string(),
        });
        s.refresh(false).await;
        let c = s.clone();
        tokio::spawn(async move {
            loop {
                c.refresh(true).await;
                tokio::time::sleep(Duration::from_secs(600)).await;
            }
        });
        Ok(s)
    }
    async fn refresh(&self, probe: bool) {
        let mut agents = vec![];
        for (id, a) in &self.config.agents {
            let binary = find_binary(&a.command[0], &self.path);
            let available = binary.is_some();
            let mut version = None;
            if probe && let Some(binary) = binary {
                let mut cmd = tokio::process::Command::new(binary);
                cmd.arg("--version")
                    .env("PATH", &self.path)
                    .kill_on_drop(true);
                if let Ok(Ok(o)) = tokio::time::timeout(Duration::from_secs(3), cmd.output()).await
                    && o.status.success()
                {
                    version = Some(
                        String::from_utf8_lossy(&o.stdout)
                            .trim()
                            .chars()
                            .take(512)
                            .collect::<String>(),
                    );
                }
            }
            let name = match id.as_str() {
                "codex" => "Codex",
                "claude" => "Claude Code",
                "deepseek" => "DeepSeek Harness",
                "antigravity" => "Antigravity",
                _ => id,
            };
            agents.push(json!({"id":id,"name":name,"protocol":a.protocol,"version":version,"available":available,"reason":if available{None}else{Some("executable not found")}}));
        }
        let magpie = if probe && self.config.discover_magpie {
            crate::magpie::discover().await
        } else {
            None
        };
        *self.info.write().await = json!({"instance_id":self.instance_id,"name":"atrium-node","version":env!("CARGO_PKG_VERSION"),"protocol":1,"device":{"name":self.config.name,"hostname":crate::config::hostname(),"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"roots":self.roots.paths(),"home":dirs::home_dir(),"agents":agents,"magpie":magpie});
    }
}
pub fn router(s: Arc<State>) -> Router {
    let auth = Arc::new(Auth::new(s.config.clone(), s.path.clone()));
    Router::new()
        .route("/v1/node", get(info))
        .route("/v1/procs", get(list).post(spawn))
        .route("/v1/procs/{id}", delete(stop))
        .route("/v1/procs/{id}/attach", get(attach))
        .route("/v1/fs/list", get(fs_list))
        .route("/v1/fs/stat", get(fs_stat))
        .route("/v1/fs/read", get(fs_read))
        .route("/v1/fs/write", put(fs_write))
        .route("/v1/fs/mkdir", post(fs_mkdir))
        .route("/v1/fs/changes", get(fs_changes))
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "not_found", "unknown route".into()) })
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::middleware,
        ))
        .layer(axum::middleware::from_fn(json_errors))
        .with_state(s)
}
async fn json_errors(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let response = next.run(req).await;
    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
    {
        return response;
    }
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let code = match status {
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "not_allowed",
        _ => "bad_request",
    };
    (
        status,
        Json(json!({"error":code,"message":String::from_utf8_lossy(&bytes)})),
    )
        .into_response()
}
struct ApiError(StatusCode, &'static str, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.0, Json(json!({"error":self.1,"message":self.2}))).into_response();
        if self.0 == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let (code, name) = match e.downcast_ref::<FsError>() {
            Some(FsError::OutsideRoots) => (StatusCode::FORBIDDEN, "outside_roots"),
            Some(FsError::TooLarge) => (StatusCode::PAYLOAD_TOO_LARGE, "bad_request"),
            Some(FsError::BadRequest) => (StatusCode::BAD_REQUEST, "bad_request"),
            _ => match e.downcast_ref::<std::io::Error>().map(|e| e.kind()) {
                Some(std::io::ErrorKind::NotFound) => (StatusCode::NOT_FOUND, "not_found"),
                Some(std::io::ErrorKind::PermissionDenied) => {
                    (StatusCode::FORBIDDEN, "outside_roots")
                }
                _ => (StatusCode::BAD_REQUEST, "bad_request"),
            },
        };
        Self(code, name, e.to_string())
    }
}
async fn info(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    Json(s.info.read().await.clone())
}
async fn list(
    AxState(s): AxState<Arc<State>>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let mut processes = s.procs.list(q.get("owner").map(String::as_str)).await;
    for process in &mut processes {
        process["instance_id"] = json!(s.instance_id);
    }
    Json(json!(processes))
}
#[derive(Deserialize)]
struct Spawn {
    agent: String,
    cwd: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}
async fn spawn(
    AxState(s): AxState<Arc<State>>,
    Json(q): Json<Spawn>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let a = s
        .config
        .agents
        .get(&q.agent)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not_found", "unknown agent".into()))?;
    let Some(_binary) = find_binary(&a.command[0], &s.path) else {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "agent_unavailable",
            "executable not found".into(),
        ));
    };
    let cwd = s.roots.resolve(&q.cwd)?;
    if !cwd.is_dir() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "cwd must be an existing directory".into(),
        ));
    }
    let mut command = a.command.clone();
    #[cfg(windows)]
    {
        // Rust launches a resolved .cmd/.bat using its Windows batch quoting.
        command[0] = _binary.into_os_string().into_string().map_err(|_| {
            ApiError(
                StatusCode::CONFLICT,
                "agent_unavailable",
                "executable path is not UTF-8".into(),
            )
        })?;
    }
    command.extend(q.args);
    let p = s
        .procs
        .spawn(q.agent, command, cwd, q.owner, q.env, s.path.clone())
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "spawn_failed",
                e.to_string(),
            )
        })?;
    let mut process = p.json().await;
    process["instance_id"] = json!(s.instance_id);
    Ok((StatusCode::CREATED, Json(process)))
}
async fn stop(
    AxState(s): AxState<Arc<State>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.procs
        .stop(&id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "not_found", "unknown process".into()))?;
    Ok(StatusCode::NO_CONTENT)
}
async fn attach(
    AxState(s): AxState<Arc<State>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // Authentication middleware runs first. This is lifecycle identity, not auth.
    if let Some(expected) = headers.get("x-atrium-instance-id") {
        let expected = expected
            .to_str()
            .ok()
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .ok_or_else(|| {
                ApiError(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "invalid instance identity".into(),
                )
            })?;
        if expected != s.instance_id {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "node_restarted",
                "node restarted; previous process state is lost".into(),
            ));
        }
    }
    let p =
        s.procs.get(&id).await.ok_or_else(|| {
            ApiError(StatusCode::NOT_FOUND, "not_found", "unknown process".into())
        })?;
    let after = q
        .get("after")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| {
            ApiError(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "invalid after".into(),
            )
        })?
        .unwrap_or(0);
    if after < -1 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invalid after".into(),
        ));
    }
    // Fail before changing generation: saturation must not replace the old link.
    let global_slot = s.ws_slots.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "capacity_exhausted",
            "node WebSocket capacity exhausted".into(),
        )
    })?;
    let process_slot = p.reserve_ws().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "capacity_exhausted",
            "process WebSocket capacity exhausted".into(),
        )
    })?;
    let mut response = ws
        .max_message_size(MAX_LINE_BYTES * 6 + 1024)
        .max_frame_size(MAX_LINE_BYTES * 6 + 1024)
        .on_upgrade(move |socket| socket_loop(socket, p, after, (global_slot, process_slot)));
    response.headers_mut().insert(
        "x-atrium-instance-id",
        HeaderValue::from_str(&s.instance_id).expect("UUID header"),
    );
    Ok(response)
}
async fn send_message(socket: &mut WebSocket, m: Message) -> bool {
    tokio::time::timeout(Duration::from_secs(10), socket.send(m))
        .await
        .is_ok_and(|r| r.is_ok())
}
async fn send(socket: &mut WebSocket, v: Value) -> bool {
    send_message(socket, Message::Text(v.to_string().into())).await
}
async fn socket_loop(
    mut socket: WebSocket,
    p: Arc<Process>,
    mut after: i64,
    _slots: (OwnedSemaphorePermit, OwnedSemaphorePermit),
) {
    let (generation, mut changed) = p.attach().await;
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    ping.tick().await;
    let mut missed = 0;
    let mut replay = true;
    loop {
        if !p.is_current(generation).await {
            let _ = send_message(
                &mut socket,
                Message::Close(Some(CloseFrame {
                    code: 4001,
                    reason: "replaced".into(),
                })),
            )
            .await;
            break;
        }
        if replay {
            let snap = p.snapshot(after).await;
            if after == -1 {
                after = snap.latest;
            }
            if let Some(gap) = snap.gap
                && !send(&mut socket, gap).await
            {
                break;
            }
            let mut failed = false;
            for frame in snap.frames {
                if !p.is_current(generation).await {
                    let _ = send_message(
                        &mut socket,
                        Message::Close(Some(CloseFrame {
                            code: 4001,
                            reason: "replaced".into(),
                        })),
                    )
                    .await;
                    failed = true;
                    break;
                }
                let seq = frame["seq"].as_i64().unwrap_or(after);
                if !send(&mut socket, frame).await {
                    failed = true;
                    break;
                }
                after = seq;
            }
            if failed || snap.ended {
                break;
            }
            replay = snap.more;
            if replay {
                // Backlog may belong to an already exited producer: no future
                // changed notification is required to drain the next page.
                tokio::task::yield_now().await;
                continue;
            }
        }
        tokio::select! {r=changed.changed()=>{if r.is_err(){break}replay=true;},_=ping.tick()=>{if missed>=2{break}missed+=1;if !send_message(&mut socket,Message::Ping(Bytes::new())).await{break}},msg=socket.recv()=>{match msg{Some(Ok(Message::Text(text)))=>{if let Ok(v)=serde_json::from_str::<Value>(&text)&& let Some(line)=v["line"].as_str()&& let Err(error) = p.try_input_for(generation,line.into()).await {
            let (code, reason) = if error.is::<InputReplaced>() { (4001, "replaced") } else if error.is::<InputCapacity>() { (1013, "stdin capacity exhausted; current input not accepted") } else { (1008, "current input not accepted; invalid or closed stream") };
            let _ = send_message(&mut socket, Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
            break
        }},Some(Ok(Message::Pong(_)))=>missed=0,Some(Ok(Message::Ping(p)))=>{if !send_message(&mut socket,Message::Pong(p)).await{break}},Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,_=>{}}}}
    }
    p.detach(generation).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.close()).await;
}
#[derive(Deserialize)]
struct FsQuery {
    path: String,
    #[serde(default)]
    hidden: u8,
    #[serde(default)]
    since: u64,
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "bad_request",
                e.to_string(),
            )
        })?
        .map_err(Into::into)
}

#[cfg(all(test, windows))]
#[tokio::test]
async fn windows_discovered_cmd_is_launched() {
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let dir = Fixture(std::env::temp_dir().join(format!("atrium shim & {}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    std::fs::write(dir.0.join("agent.cmd"), "@echo off\r\necho shim-ready\r\n").unwrap();
    let config = Config {
        roots: vec![dir.0.clone()],
        agents: [(
            "shim".into(),
            crate::config::Agent {
                command: vec!["agent".into()],
                protocol: "acp".into(),
            },
        )]
        .into(),
        ..Config::default()
    };
    let path = std::env::join_paths([&dir.0])
        .unwrap()
        .into_string()
        .unwrap();
    let state = State::new(Arc::new(config), path).await.unwrap();
    let (_, Json(created)) = spawn(
        AxState(state.clone()),
        Json(Spawn {
            agent: "shim".into(),
            cwd: dir.0.to_str().unwrap().into(),
            owner: "fixture".into(),
            args: vec![],
            env: HashMap::new(),
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("{}", error.2));
    let child = state
        .procs
        .get(created["id"].as_str().unwrap())
        .await
        .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(10), child.wait_ended()).await;
    state.procs.shutdown().await;
    ended.unwrap();
    assert_eq!(child.json().await["exit"]["code"], 0);
    assert!(
        child
            .snapshot(0)
            .await
            .frames
            .iter()
            .any(|frame| frame["line"] == "shim-ready")
    );
}
async fn fs_list(
    AxState(s): AxState<Arc<State>>,
    Query(q): Query<FsQuery>,
) -> Result<Json<Value>, ApiError> {
    blocking(move || s.roots.list(&q.path, q.hidden != 0))
        .await
        .map(Json)
}
async fn fs_stat(
    AxState(s): AxState<Arc<State>>,
    Query(q): Query<FsQuery>,
) -> Result<Json<Value>, ApiError> {
    blocking(move || s.roots.stat(&q.path)).await.map(Json)
}
async fn fs_read(
    AxState(s): AxState<Arc<State>>,
    Query(q): Query<FsQuery>,
) -> Result<Response, ApiError> {
    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        blocking(move || s.roots.read(&q.path)).await?,
    )
        .into_response())
}
async fn fs_write(
    AxState(s): AxState<Arc<State>>,
    Query(q): Query<FsQuery>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    blocking(move || s.roots.write(&q.path, &body)).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn fs_mkdir(
    AxState(s): AxState<Arc<State>>,
    Query(q): Query<FsQuery>,
) -> Result<StatusCode, ApiError> {
    blocking(move || s.roots.mkdir(&q.path)).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn fs_changes(
    AxState(s): AxState<Arc<State>>,
    Query(q): Query<FsQuery>,
) -> Result<Json<Value>, ApiError> {
    blocking(move || s.roots.changes(&q.path, q.since))
        .await
        .map(Json)
}

#[cfg(all(test, unix))]
mod admission_tests {
    use super::*;
    use tungstenite::client::IntoClientRequest;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_websocket_admission_preserves_old_link_and_releases_every_slot() {
        let config = Config {
            roots: vec![std::env::temp_dir()],
            loopback_only: true,
            discover_magpie: false,
            token: "admission-fixture".into(),
            total_input_bytes: 4,
            max_ws_connections: 2,
            max_ws_connections_per_process: 2,
            ..Config::default()
        };
        let state = State::new(Arc::new(config), "/usr/bin:/bin".into())
            .await
            .unwrap();
        let process = state
            .procs
            .spawn(
                "echo".into(),
                vec!["/bin/cat".into()],
                std::env::temp_dir(),
                "test".into(),
                HashMap::new(),
                "/usr/bin:/bin".into(),
            )
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let s = state.clone();
        let p = process.clone();
        let result = tokio::task::spawn_blocking(move || {
            let request = || {
                let mut r = format!("ws://{address}/v1/procs/{}/attach?after=-1", p.id)
                    .into_client_request()
                    .unwrap();
                r.headers_mut()
                    .insert("Authorization", "Bearer admission-fixture".parse().unwrap());
                r
            };
            let connect = || {
                let (ws, _) = tungstenite::connect(request()).unwrap();
                if let tungstenite::stream::MaybeTlsStream::Plain(stream) = ws.get_ref() {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                }
                ws
            };
            let reject = || match tungstenite::connect(request()) {
                Err(tungstenite::Error::Http(r)) => {
                    assert_eq!(r.status(), 429);
                    assert_eq!(r.headers()["retry-after"], "1");
                    let body: Value = serde_json::from_slice(r.body().as_ref().unwrap()).unwrap();
                    assert_eq!(body["error"], "capacity_exhausted");
                }
                _ => panic!("expected 429"),
            };
            let echo = |ws: &mut tungstenite::WebSocket<
                tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
            >| {
                ws.send(tungstenite::Message::text("{\"line\":\"ok\"}"))
                    .unwrap();
                loop {
                    if let tungstenite::Message::Text(t) = ws.read().unwrap() {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        assert_eq!(v["line"], "ok");
                        break;
                    }
                }
            };
            let pending_global = s.ws_slots.clone().try_acquire_owned().unwrap();
            let mut old = connect();
            reject();
            echo(&mut old);
            drop(pending_global);
            let pending_process = p.reserve_ws().unwrap();
            reject();
            assert_eq!(
                s.ws_slots.available_permits(),
                1,
                "per-process failure rolls back global slot"
            );
            echo(&mut old);
            drop(pending_process);
            let mut replacement = connect();
            loop {
                if let tungstenite::Message::Close(frame) = old.read().unwrap() {
                    assert_eq!(u16::from(frame.unwrap().code), 4001);
                    break;
                }
            }
            drop(old);
            echo(&mut replacement);
            replacement
                .send(tungstenite::Message::text(
                    "{\"line\":\"too-large-for-tiny-test-budget\"}",
                ))
                .unwrap();
            loop {
                if let tungstenite::Message::Close(frame) = replacement.read().unwrap() {
                    let frame = frame.unwrap();
                    assert_eq!(u16::from(frame.code), 1013);
                    assert!(frame.reason.contains("not accepted"));
                    break;
                }
            }
            drop(replacement);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while s.ws_slots.available_permits() != 2 {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
            let a = p.reserve_ws().unwrap();
            let b = p.reserve_ws().unwrap();
            assert!(p.reserve_ws().is_err());
            drop((a, b));
            // Abandon a valid upgrade immediately after its response. The
            // on_upgrade task must return both captured permits on socket EOF.
            for _ in 0..3 {
                use std::io::{BufRead, Write};
                let mut stream=std::net::TcpStream::connect(address).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                write!(stream,"GET /v1/procs/{}/attach HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer admission-fixture\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",p.id).unwrap();
                let mut reader=std::io::BufReader::new(stream);
                let mut line=String::new();reader.read_line(&mut line).unwrap();assert!(line.contains("101"));
                loop { line.clear();reader.read_line(&mut line).unwrap();if line=="\r\n" {break;} }
                reader.get_ref().shutdown(std::net::Shutdown::Both).unwrap();drop(reader);
                let deadline=std::time::Instant::now()+Duration::from_secs(5);
                while s.ws_slots.available_permits()!=2 { assert!(std::time::Instant::now()<deadline);std::thread::sleep(Duration::from_millis(10)); }
            }
            // Invalid HTTP upgrade is rejected before admission; cannot consume slots.
            assert!(
                ureq::get(&format!("http://{address}/v1/procs/{}/attach", p.id))
                    .set("Authorization", "Bearer admission-fixture")
                    .call()
                    .is_err()
            );
            assert_eq!(s.ws_slots.available_permits(), 2);
        })
        .await;
        state.procs.shutdown().await;
        server.abort();
        let _ = server.await;
        result.unwrap();
        let snapshot = process.snapshot(0).await;
        assert_eq!(
            snapshot
                .frames
                .iter()
                .filter(|f| f.get("line").is_some())
                .count(),
            3,
            "accepted input preserved, rejected input absent"
        );
    }
}
