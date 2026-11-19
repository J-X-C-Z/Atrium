use super::{Action, Command, Decision, Error, Snapshot, State as Desktop, auth};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    middleware,
    response::{
        Html, IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    convert::Infallible,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{oneshot, watch};

pub(crate) fn router(state: Desktop, guard: auth::Guard) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route("/api/projects", get(projects))
        .route("/api/projects/{ix}", get(project))
        .route("/api/projects/{ix}/notices", post(notice))
        .route("/api/threads/{id}", get(thread))
        .route("/api/threads/{id}/messages", post(message))
        .route("/api/approvals", post(approval))
        .route("/api/questions", post(question))
        .route("/api/events", get(events))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(guard, auth::authorize))
        .with_state(state)
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Missing => (StatusCode::NOT_FOUND, "not_found"),
            Self::Invalid => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Stale => (StatusCode::CONFLICT, "stale_request"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::WriteFailed => (StatusCode::INTERNAL_SERVER_ERROR, "write_failed"),
        };
        (status, Json(json!({"error":code}))).into_response()
    }
}

impl Desktop {
    fn read(&self) -> Arc<Snapshot> {
        self.snapshots.borrow().clone()
    }
    async fn dispatch(&self, action: Action) -> Result<StatusCode, Error> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(Command { action, reply })
            .map_err(|_| Error::Unavailable)?;
        result.await.map_err(|_| Error::Unavailable)??;
        Ok(StatusCode::NO_CONTENT)
    }
}

async fn projects(State(s): State<Desktop>) -> Json<Value> {
    Json(Value::Array(
        s.read().iter().map(|p| p.summary.clone()).collect(),
    ))
}
async fn project(State(s): State<Desktop>, Path(ix): Path<usize>) -> Result<Json<Value>, Error> {
    Ok(Json(s.read().get(ix).ok_or(Error::Missing)?.detail.clone()))
}

#[derive(Deserialize)]
struct Cursor {
    after: Option<usize>,
}
async fn thread(
    State(s): State<Desktop>,
    Path(id): Path<String>,
    Query(q): Query<Cursor>,
) -> Result<Json<Value>, Error> {
    let snapshot = s.read();
    let rows = snapshot
        .iter()
        .find_map(|p| p.threads.get(&id))
        .ok_or(Error::Missing)?;
    let start = q.after.map_or(0, |i| i.saturating_add(1)).min(rows.len());
    Ok(Json(Value::Array(rows[start..].to_vec())))
}

#[derive(Deserialize)]
struct Approval {
    thread: String,
    request_id: Value,
    decision: Decision,
}
async fn approval(State(s): State<Desktop>, Json(a): Json<Approval>) -> Result<StatusCode, Error> {
    s.dispatch(Action::Approval {
        thread: a.thread,
        request_id: a.request_id,
        decision: a.decision,
    })
    .await
}
#[derive(Deserialize)]
struct Question {
    thread: String,
    request_id: Value,
    answers: BTreeMap<String, Vec<String>>,
}
async fn question(State(s): State<Desktop>, Json(q): Json<Question>) -> Result<StatusCode, Error> {
    s.dispatch(Action::Question {
        thread: q.thread,
        request_id: q.request_id,
        answers: q.answers,
    })
    .await
}
#[derive(Deserialize)]
struct Notice {
    to: Vec<String>,
    text: String,
}
async fn notice(
    State(s): State<Desktop>,
    Path(ix): Path<usize>,
    Json(n): Json<Notice>,
) -> Result<StatusCode, Error> {
    if n.text.trim().is_empty() || n.to.is_empty() {
        return Err(Error::Invalid);
    }
    let snapshot = s.read();
    let root = snapshot.get(ix).ok_or(Error::Missing)?.detail["root"]
        .as_str()
        .ok_or(Error::Stale)?
        .to_owned();
    s.dispatch(Action::Notice {
        project: ix,
        root,
        to: n.to,
        text: n.text,
    })
    .await
}
#[derive(Deserialize)]
struct Message {
    text: String,
}
async fn message(
    State(s): State<Desktop>,
    Path(id): Path<String>,
    Json(m): Json<Message>,
) -> Result<StatusCode, Error> {
    if m.text.trim().is_empty() {
        return Err(Error::Invalid);
    }
    s.dispatch(Action::Message {
        thread: id,
        text: m.text,
    })
    .await?;
    Ok(StatusCode::ACCEPTED)
}

struct Feed {
    previous: Arc<Snapshot>,
    rx: watch::Receiver<Arc<Snapshot>>,
    queue: VecDeque<Value>,
}
impl Feed {
    fn update(&mut self) {
        let next = self.rx.borrow_and_update().clone();
        for ix in 0..self.previous.len().max(next.len()) {
            let (old, new) = (self.previous.get(ix), next.get(ix));
            if old == new {
                continue;
            }
            self.queue.push_back(json!({"type":"changed","project":ix}));
            if let Some(p) = new {
                for request in &p.pending {
                    if old.is_none_or(|p| !p.pending.contains(request)) {
                        self.queue.push_back(json!({"type":"needs_you","project":ix,"stream":request["stream"],
                            "thread":request["thread"],"request_id":request["request_id"],"kind":request["kind"]}));
                    }
                }
            }
        }
        self.previous = next;
    }
}
async fn events(State(s): State<Desktop>) -> Response {
    let mut feed = Feed {
        previous: Arc::default(),
        rx: s.snapshots,
        queue: VecDeque::new(),
    };
    feed.update();
    // Reconnect periodically so an open stream also renews its peer authorization.
    let expires = tokio::time::Instant::now() + Duration::from_secs(60);
    let stream = stream::unfold(feed, move |mut feed| async move {
        loop {
            if tokio::time::Instant::now() >= expires {
                return None;
            }
            if let Some(data) = feed.queue.pop_front() {
                return Some((
                    Ok::<_, Infallible>(Event::default().data(data.to_string())),
                    feed,
                ));
            }
            tokio::time::timeout_at(expires, feed.rx.changed())
                .await
                .ok()?
                .ok()?;
            feed.update();
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
