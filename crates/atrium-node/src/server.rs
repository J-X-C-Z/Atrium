use crate::{
    auth::Auth,
    config::{Config, find_binary},
    fs::{FsError, Roots},
    procs::{MAX_LINE_BYTES, Process, ProcessManager},
};
use anyhow::Result;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, State as AxState,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use futures::SinkExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;
pub struct State {
    pub config: Arc<Config>,
    pub path: String,
    pub roots: Arc<Roots>,
    pub procs: ProcessManager,
    info: RwLock<Value>,
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
            ),
            config,
            path,
            roots,
            info: RwLock::new(Value::Null),
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
        *self.info.write().await = json!({"name":"atrium-node","version":env!("CARGO_PKG_VERSION"),"protocol":1,"device":{"name":self.config.name,"hostname":crate::config::hostname(),"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"roots":self.roots.paths(),"home":dirs::home_dir(),"agents":agents,"magpie":magpie});
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
        (self.0, Json(json!({"error":self.1,"message":self.2}))).into_response()
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
    Json(json!(
        s.procs.list(q.get("owner").map(String::as_str)).await
    ))
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
    if find_binary(&a.command[0], &s.path).is_none() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "agent_unavailable",
            "executable not found".into(),
        ));
    }
    let cwd = s.roots.resolve(&q.cwd)?;
    if !cwd.is_dir() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "cwd must be an existing directory".into(),
        ));
    }
    let mut command = a.command.clone();
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
    Ok((StatusCode::CREATED, Json(p.json().await)))
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
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
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
    Ok(ws
        .max_message_size(MAX_LINE_BYTES * 6 + 1024)
        .max_frame_size(MAX_LINE_BYTES * 6 + 1024)
        .on_upgrade(move |socket| socket_loop(socket, p, after)))
}
async fn send_message(socket: &mut WebSocket, m: Message) -> bool {
    tokio::time::timeout(Duration::from_secs(10), socket.send(m))
        .await
        .is_ok_and(|r| r.is_ok())
}
async fn send(socket: &mut WebSocket, v: Value) -> bool {
    send_message(socket, Message::Text(v.to_string().into())).await
}
async fn socket_loop(mut socket: WebSocket, p: Arc<Process>, mut after: i64) {
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
        tokio::select! {r=changed.changed()=>{if r.is_err(){break}replay=true;},_=ping.tick()=>{if missed>=2{break}missed+=1;if !send_message(&mut socket,Message::Ping(Bytes::new())).await{break}},msg=socket.recv()=>{match msg{Some(Ok(Message::Text(text)))=>{if let Ok(v)=serde_json::from_str::<Value>(&text)&& let Some(line)=v["line"].as_str()&& p.try_input_for(generation,line.into()).await.is_err(){break}},Some(Ok(Message::Pong(_)))=>missed=0,Some(Ok(Message::Ping(p)))=>{if !send_message(&mut socket,Message::Pong(p)).await{break}},Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,_=>{}}}}
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
