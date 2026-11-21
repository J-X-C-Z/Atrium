use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    dto::{
        AnswerInput, ApprovalAnswer, MovePackage, ThreadAnnouncement, ThreadSnapshot, TurnInput,
    },
    model::{Op, Vector},
    store::{Store, StoreError},
};

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("sync peer: {0}")]
    Transport(String),
    #[error("sync peer returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("sync peer returned an invalid answer: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    pub sent: usize,
    pub received: usize,
    pub applied: usize,
}

#[derive(Deserialize)]
struct OpsAnswer {
    #[serde(default)]
    ops: Vec<Op>,
}

#[derive(Serialize)]
struct OpsRequest<'a> {
    ops: &'a [Op],
}

/// A tiny synchronous client for another Atrium's node.
///
/// The desktop already uses blocking node calls from background executors;
/// keeping this client synchronous makes it usable from the app's host
/// thread without pulling an HTTP runtime into the pure sync crate.
#[derive(Clone, Debug)]
pub struct PeerClient {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

impl PeerClient {
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(15))
            .build();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token,
            agent,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn directory(&self) -> Result<crate::DeviceDirectory, PeerError> {
        let response = self.get("/v1/atrium")?;
        Ok(serde_json::from_value(response)?)
    }

    pub fn vector(&self) -> Result<Vector, PeerError> {
        let response = self.get("/v1/sync/vector")?;
        Ok(serde_json::from_value(response)?)
    }

    /// Sends the overlay a peer lacks and returns the ops it sent back.
    pub fn exchange(&self, ops: &[Op]) -> Result<Vec<Op>, PeerError> {
        let request = OpsRequest { ops };
        let answer = self.post("/v1/sync/ops", &request)?;
        Ok(serde_json::from_value::<OpsAnswer>(answer)?.ops)
    }

    pub fn threads(&self, since_ms: Option<i64>) -> Result<Vec<ThreadAnnouncement>, PeerError> {
        let path = match since_ms {
            Some(since) => format!("/v1/sync/threads?since={since}"),
            None => "/v1/sync/threads".into(),
        };
        let answer = self.get(&path)?;
        Ok(serde_json::from_value(answer["threads"].clone())?)
    }

    pub fn snapshot(&self, backend: &str, id: &str) -> Result<ThreadSnapshot, PeerError> {
        let answer = self.get(&format!(
            "/v1/sync/threads/{}/{}/snapshot",
            encode(backend),
            encode(id)
        ))?;
        Ok(serde_json::from_value(answer)?)
    }

    pub fn turn(&self, backend: &str, id: &str, input: &TurnInput) -> Result<(), PeerError> {
        self.send_empty(
            &format!("/v1/sync/threads/{}/{}/turn", encode(backend), encode(id)),
            input,
        )
    }

    pub fn steer(&self, backend: &str, id: &str, input: &TurnInput) -> Result<(), PeerError> {
        self.send_empty(
            &format!("/v1/sync/threads/{}/{}/steer", encode(backend), encode(id)),
            input,
        )
    }

    pub fn interrupt(&self, backend: &str, id: &str) -> Result<(), PeerError> {
        self.send_empty(
            &format!(
                "/v1/sync/threads/{}/{}/interrupt",
                encode(backend),
                encode(id)
            ),
            &serde_json::json!({}),
        )
    }

    pub fn approve(
        &self,
        backend: &str,
        id: &str,
        request: &serde_json::Value,
        decision: &str,
    ) -> Result<(), PeerError> {
        let request = match request {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        self.send_empty(
            &format!(
                "/v1/sync/threads/{}/{}/approvals/{}",
                encode(backend),
                encode(id),
                encode(&request)
            ),
            &ApprovalAnswer {
                decision: decision.into(),
            },
        )
    }

    pub fn answer(
        &self,
        backend: &str,
        id: &str,
        request: &serde_json::Value,
        answers: &std::collections::BTreeMap<String, Vec<String>>,
    ) -> Result<(), PeerError> {
        let request = match request {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        self.send_empty(
            &format!(
                "/v1/sync/threads/{}/{}/answers/{}",
                encode(backend),
                encode(id),
                encode(&request)
            ),
            &AnswerInput {
                answers: answers.clone(),
            },
        )
    }

    /// Uploads one attachment into the owner's inbox and returns its path
    /// on the owner's disk.
    pub fn upload(&self, name: &str, bytes: &[u8]) -> Result<String, PeerError> {
        let mut request = self.agent.post(&format!(
            "{}/v1/sync/inbox?name={}",
            self.base,
            encode(name)
        ));
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let response = self.parse_response(request.send_bytes(bytes))?;
        response["path"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| PeerError::Transport("the owner did not return a path".into()))
    }

    pub fn move_package(&self, backend: &str, id: &str) -> Result<MovePackage, PeerError> {
        let answer = self.get(&format!(
            "/v1/sync/threads/{}/{}/move-package",
            encode(backend),
            encode(id)
        ))?;
        Ok(serde_json::from_value(answer)?)
    }

    pub fn move_release(&self, backend: &str, id: &str) -> Result<(), PeerError> {
        self.send_empty(
            &format!(
                "/v1/sync/threads/{}/{}/move-release",
                encode(backend),
                encode(id)
            ),
            &serde_json::json!({}),
        )
    }

    /// One full pull/push round with a peer.
    pub fn sync(&self, store: &mut Store) -> Result<SyncOutcome, PeerError> {
        let remote = self.vector().unwrap_or_default();
        let sending = store.missing_for(&remote);
        let received = self.exchange(&sending)?;
        let applied = store.merge(received.clone())?;
        Ok(SyncOutcome {
            sent: sending.len(),
            received: received.len(),
            applied: applied.len(),
        })
    }

    /// Trades a pairing code shown on that device for a lasting token.
    pub fn pair_join(&self, code: &str) -> Result<String, PeerError> {
        let answer = self.post("/v1/sync/pair/join", &serde_json::json!({ "code": code }))?;
        answer["token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| PeerError::Transport("pairing returned no token".into()))
    }

    fn get(&self, path: &str) -> Result<serde_json::Value, PeerError> {
        let mut request = self.agent.get(&format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        self.parse_response(request.call())
    }

    fn post<T: Serialize>(&self, path: &str, body: &T) -> Result<serde_json::Value, PeerError> {
        let mut request = self.agent.post(&format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        self.parse_response(request.send_json(body))
    }

    fn send_empty<T: Serialize>(&self, path: &str, body: &T) -> Result<(), PeerError> {
        let mut request = self.agent.post(&format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        match request.send_json(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, response)) => Err(PeerError::Status {
                status,
                body: response.into_string().unwrap_or_default(),
            }),
            Err(ureq::Error::Transport(error)) => Err(PeerError::Transport(error.to_string())),
        }
    }

    fn parse_response(
        &self,
        result: Result<ureq::Response, ureq::Error>,
    ) -> Result<serde_json::Value, PeerError> {
        match result {
            Ok(response) => response
                .into_json()
                .map_err(|error| PeerError::Transport(error.to_string())),
            Err(ureq::Error::Status(status, response)) => Err(PeerError::Status {
                status,
                body: response.into_string().unwrap_or_default(),
            }),
            Err(ureq::Error::Transport(error)) => Err(PeerError::Transport(error.to_string())),
        }
    }
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_client_trims_one_trailing_slash() {
        let peer = PeerClient::new("http://127.0.0.1:7381/", None);
        assert_eq!(peer.base(), "http://127.0.0.1:7381");
    }

    #[test]
    fn the_exchange_body_is_a_list_of_ops() {
        let op = Op::set("k", json!(1), crate::Hlc::new(1, 0, "a")).unwrap();
        let body = serde_json::to_value(OpsRequest { ops: &[op] }).unwrap();
        assert!(body["ops"].is_array());
    }
}
