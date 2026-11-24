//! Atrium's multi-device overlay and the owner-side sync host.
//!
//! The node serves this state only when an Atrium embeds it. A standalone
//! node has no overlay and answers 404 for `/v1/atrium` and `/v1/sync/*`,
//! keeping the old `agent@device` behaviour.

use std::{
    collections::HashSet,
    fs::{self, File},
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use atrium_sync::{
    AnswerInput, ApprovalAnswer, DeviceDirectory, MovePackage, Op, Store, StoreError,
    ThreadAnnouncement, ThreadSnapshot, TurnInput, Vector,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;

const PAIR_CODE_TTL_MS: i64 = 5 * 60 * 1_000;
const PAIR_ATTEMPTS: u8 = 5;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("not found")]
    NotFound,
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// The Atrium-side operations the node exposes to other devices.
///
/// The app implements this over its Workspace; tests implement it with
/// fixtures. Everything here is owner-side: the follower never talks to an
/// agent process directly.
pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + 'a>>;

pub trait SyncHost: Send + Sync {
    fn threads<'a>(&'a self, since_ms: Option<i64>) -> HostFuture<'a, Vec<ThreadAnnouncement>>;
    fn snapshot<'a>(&'a self, backend: &'a str, id: &'a str) -> HostFuture<'a, ThreadSnapshot>;
    fn turn<'a>(&'a self, backend: &'a str, id: &'a str, input: TurnInput) -> HostFuture<'a, ()>;
    fn steer<'a>(&'a self, backend: &'a str, id: &'a str, input: TurnInput) -> HostFuture<'a, ()>;
    fn interrupt<'a>(&'a self, backend: &'a str, id: &'a str) -> HostFuture<'a, ()>;
    fn approve<'a>(
        &'a self,
        backend: &'a str,
        id: &'a str,
        request: Value,
        answer: ApprovalAnswer,
    ) -> HostFuture<'a, ()>;
    fn answer<'a>(
        &'a self,
        backend: &'a str,
        id: &'a str,
        request: Value,
        input: AnswerInput,
    ) -> HostFuture<'a, ()>;

    /// Builds a move package. The default keeps older hosts working: they
    /// simply cannot move a conversation to another computer yet.
    fn move_package<'a>(&'a self, _backend: &'a str, _id: &'a str) -> HostFuture<'a, MovePackage> {
        Box::pin(async {
            Err(HostError::Unavailable(
                "this Atrium cannot move conversations yet".into(),
            ))
        })
    }

    /// Answers one message of the `atrium` MCP server (JSON-RPC) for a
    /// workstream of a project this Atrium runs, for an agent on another
    /// computer. None for a notification. The default serves nothing.
    fn mcp<'a>(
        &'a self,
        _project: &'a str,
        _stream: &'a str,
        _message: Value,
    ) -> HostFuture<'a, Option<Value>> {
        Box::pin(async {
            Err(HostError::Unavailable(
                "this Atrium does not serve the atrium tools".into(),
            ))
        })
    }

    /// Closes the owner's writer after the target applied the package.
    fn move_release<'a>(&'a self, _backend: &'a str, _id: &'a str) -> HostFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub struct SyncState {
    directory: DeviceDirectory,
    store: Mutex<Store>,
    host: Option<Arc<dyn SyncHost>>,
    events: broadcast::Sender<Op>,
    pairing: Mutex<Pairing>,
    inbox: PathBuf,
}

struct Pairing {
    code: Option<PairCode>,
    tokens: HashSet<String>,
    /// Failed redemptions of the current code. A single counter, not a map
    /// keyed by the submitted text: only the live code can be redeemed, and
    /// arbitrary guesses must not accumulate.
    attempts: u8,
    path: PathBuf,
}

#[derive(Clone)]
struct PairCode {
    code: String,
    expires_ms: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedPairing {
    #[serde(default)]
    tokens: Vec<String>,
}

impl SyncState {
    pub fn open(
        dir: impl AsRef<Path>,
        directory: DeviceDirectory,
        host: Option<Arc<dyn SyncHost>>,
    ) -> Result<Arc<Self>, StoreError> {
        let dir = dir.as_ref().to_path_buf();
        let store = Store::open(dir.join("overlay"), directory.install_id.clone())?;
        let pairing = Pairing::load(dir.join("paired-tokens.json"));
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            directory,
            store: Mutex::new(store),
            host,
            events,
            pairing: Mutex::new(pairing),
            inbox: dir.join("inbox"),
        }))
    }

    pub fn directory(&self) -> &DeviceDirectory {
        &self.directory
    }

    pub fn host(&self) -> Option<Arc<dyn SyncHost>> {
        self.host.clone()
    }

    pub fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().expect("sync store")
    }

    /// Applies a local overlay write and wakes connected followers.
    pub fn set(&self, key: impl Into<String>, value: Value) -> Result<Op, StoreError> {
        let op = self.store().set(key, value)?;
        let _ = self.events.send(op.clone());
        Ok(op)
    }

    pub fn delete(&self, key: impl Into<String>) -> Result<Op, StoreError> {
        let op = self.store().delete(key)?;
        let _ = self.events.send(op.clone());
        Ok(op)
    }

    pub fn vector(&self) -> Vector {
        self.store().vector()
    }

    pub fn missing_for(&self, remote: &Vector) -> Vec<Op> {
        self.store().missing_for(remote)
    }

    pub fn merge(&self, ops: Vec<Op>) -> Result<Vec<Op>, StoreError> {
        let applied = self.store().merge(ops)?;
        for op in &applied {
            let _ = self.events.send(op.clone());
        }
        Ok(applied)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Op> {
        self.events.subscribe()
    }

    /// Shows a 6-digit code valid for five minutes.
    pub fn start_pairing(&self, now_ms: i64) -> String {
        let code = format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000);
        let mut pairing = self.pairing.lock().expect("pairing");
        pairing.code = Some(PairCode {
            code: code.clone(),
            expires_ms: now_ms + PAIR_CODE_TTL_MS,
        });
        pairing.attempts = 0;
        code
    }

    /// Redeems a code once and returns a long-lived bearer token.
    pub fn join_pairing(&self, code: &str, now_ms: i64) -> Result<String, PairError> {
        let mut pairing = self.pairing.lock().expect("pairing");
        let Some(active) = pairing.code.clone() else {
            return Err(PairError::NoCode);
        };
        if active.expires_ms < now_ms {
            pairing.code = None;
            pairing.attempts = 0;
            return Err(PairError::Expired);
        }
        if pairing.attempts >= PAIR_ATTEMPTS {
            return Err(PairError::TooManyAttempts);
        }
        pairing.attempts = pairing.attempts.saturating_add(1);
        if active.code != code {
            return Err(PairError::WrongCode);
        }
        let token = format!(
            "atr_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        pairing.tokens.insert(token.clone());
        pairing.code = None;
        pairing.attempts = 0;
        pairing.persist().map_err(PairError::Io)?;
        Ok(token)
    }

    pub fn accepts_token(&self, token: &str) -> bool {
        self.pairing.lock().expect("pairing").tokens.contains(token)
    }

    /// Stores one uploaded attachment outside the shared roots and returns
    /// the path the owner's agent should open.
    pub fn write_inbox(&self, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        fs::create_dir_all(&self.inbox)?;
        let name = Path::new(name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("attachment");
        let path = self
            .inbox
            .join(format!("{}-{name}", uuid::Uuid::new_v4().simple()));
        let temp = path.with_extension("part");
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(temp, &path)?;
        Ok(path)
    }

    pub fn inbox(&self) -> &Path {
        &self.inbox
    }
}

#[derive(Debug, Error)]
pub enum PairError {
    #[error("no pairing code is active")]
    NoCode,
    #[error("the pairing code expired")]
    Expired,
    #[error("the pairing code is wrong")]
    WrongCode,
    #[error("too many attempts")]
    TooManyAttempts,
    #[error("could not save paired devices: {0}")]
    Io(std::io::Error),
}

impl Pairing {
    fn load(path: PathBuf) -> Self {
        let tokens = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PersistedPairing>(&bytes).ok())
            .map(|saved| saved.tokens.into_iter().collect())
            .unwrap_or_default();
        Self {
            code: None,
            tokens,
            attempts: 0,
            path,
        }
    }

    fn persist(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let saved = PersistedPairing {
            tokens: self.tokens.iter().cloned().collect(),
        };
        let text = serde_json::to_vec_pretty(&saved).map_err(std::io::Error::other)?;
        let temp = self.path.with_extension("tmp");
        let mut file = File::create(&temp)?;
        file.write_all(&text)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(temp, &self.path)
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_sync::SYNC_VERSION;
    use serde_json::json;

    fn state(dir: &Path) -> Arc<SyncState> {
        SyncState::open(
            dir,
            DeviceDirectory {
                install_id: "me".into(),
                device_name: "studio".into(),
                os: "macos".into(),
                app_version: "0.1.0".into(),
                sync_version: SYNC_VERSION,
                owner: "me@example.com".into(),
            },
            None,
        )
        .unwrap()
    }

    #[test]
    fn pairing_is_one_shot_and_tokens_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let sync = state(dir.path());
        let code = sync.start_pairing(1_000);
        let token = sync.join_pairing(&code, 1_001).unwrap();
        assert!(sync.accepts_token(&token));
        assert!(matches!(
            sync.join_pairing(&code, 1_002),
            Err(PairError::NoCode)
        ));
        drop(sync);
        let reopened = state(dir.path());
        assert!(reopened.accepts_token(&token));
    }

    #[test]
    fn pairing_attempts_are_bounded_and_a_new_code_resets_them() {
        let dir = tempfile::tempdir().unwrap();
        let sync = state(dir.path());
        let _code = sync.start_pairing(1_000);
        // Guesses are counted, not remembered by text: an arbitrary number of
        // distinct wrong guesses must not grow any per-guess state.
        for _ in 0..PAIR_ATTEMPTS {
            assert!(matches!(
                sync.join_pairing("******", 1_001),
                Err(PairError::WrongCode)
            ));
        }
        assert!(matches!(
            sync.join_pairing("******", 1_002),
            Err(PairError::TooManyAttempts)
        ));
        let next = sync.start_pairing(2_000);
        assert!(sync.join_pairing(&next, 2_001).is_ok());
    }

    #[test]
    fn a_wrong_or_expired_code_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sync = state(dir.path());
        let code = sync.start_pairing(1_000);
        assert!(matches!(
            sync.join_pairing("000000", 1_001),
            Err(PairError::WrongCode)
        ));
        assert!(matches!(
            sync.join_pairing(&code, 1_000 + PAIR_CODE_TTL_MS + 1),
            Err(PairError::Expired)
        ));
    }

    #[test]
    fn overlay_ops_are_persisted_and_broadcast() {
        let dir = tempfile::tempdir().unwrap();
        let sync = state(dir.path());
        let mut events = sync.subscribe();
        sync.set("thread/me/codex/t/title", json!("Hello")).unwrap();
        let event = events.try_recv().unwrap();
        assert_eq!(event.key, "thread/me/codex/t/title");
        assert_eq!(
            sync.store().value("thread/me/codex/t/title"),
            Some(&json!("Hello"))
        );
    }
}
