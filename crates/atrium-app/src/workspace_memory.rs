//! The shared memory in the app: what goes into a new session, the memory
//! tools every local agent gets, and the conversations handed on to learn
//! from (exported, queued for a memory service, retained in Hindsight).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use atrium_core::McpServer;
use atrium_core::{EntryKind, Item, Transcript};
use atrium_memory::{
    Config, Store, hindsight,
    inbox::ReviewRequest,
    inject::{self, Injection},
    project_key, project_root,
    session::{Header, Line, Session},
};
use gpui::{Context, Task};

use super::Workspace;
use crate::agents;

/// What the app keeps about memory while it runs.
#[derive(Default)]
pub struct MemoryState {
    /// What each session started with: its tokens and parts.
    pub injected: HashMap<String, Injection>,
    /// Per conversation: rows exported, when they last changed, and how
    /// many rows were last handed on.
    seen: HashMap<String, (usize, Instant, usize)>,
    /// The memory page shows while this is the latest move.
    open_at: Option<u64>,
    _ticker: Option<Task<()>>,
}

/// What a new session needs to make its memory block off the UI thread.
#[derive(Clone)]
pub struct MemoryPrep {
    root: PathBuf,
    config: Config,
}

impl MemoryPrep {
    /// The block for a session in `cwd` whose first message is `query`.
    /// Blocking: Hindsight is asked when it is on.
    pub fn block(&self, cwd: &Path, query: &str) -> Injection {
        let store = Store::new(&self.root);
        let root = project_root(cwd);
        let key = project_key(&root);
        let _ = store.register_project(&key, &root);
        let recalled = match self.config.hindsight() {
            Some(h) if !query.trim().is_empty() => hindsight::recall_all(h, Some(&key), query)
                .unwrap_or_else(|error| {
                    log::info!("hindsight recall: {error:#}");
                    Vec::new()
                }),
            _ => Vec::new(),
        };
        inject::injection(&store, &self.config, Some(cwd), &recalled)
    }
}

pub fn memory_root() -> Option<PathBuf> {
    std::env::var_os("ATRIUM_MEMORY_DIR")
        .map(PathBuf::from)
        .or_else(atrium_memory::default_root)
}

impl Workspace {
    /// Shows the memory page; the next move elsewhere hides it.
    pub fn open_memory(&mut self, cx: &mut Context<Self>) {
        self.nav += 1;
        self.shared_memory.open_at = Some(self.nav);
        cx.notify();
    }

    pub fn close_memory(&mut self, cx: &mut Context<Self>) {
        self.shared_memory.open_at = None;
        self.nav += 1;
        cx.notify();
    }

    pub fn memory_open(&self) -> bool {
        self.shared_memory.open_at == Some(self.nav)
    }

    /// The user's own change to the memory.
    pub fn memory_apply(
        &mut self,
        scope: atrium_memory::Scope,
        ops: Vec<atrium_memory::Op>,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = memory_root() else { return };
        match Store::new(root).apply(&scope, &ops, "user", atrium_memory::Landing::Active) {
            Ok(_) => cx.notify(),
            Err(error) => cx.emit(super::Notice::Error(crate::i18n::tf(
                "Could not change the memory: {error}",
                &[("error", &error)],
            ))),
        }
    }

    pub fn memory_undo(&mut self, change: &str, cx: &mut Context<Self>) {
        let Some(root) = memory_root() else { return };
        match Store::new(root).undo(change, "user") {
            Ok(_) => {
                cx.emit(super::Notice::Success(crate::i18n::t("Undone.").into()));
                cx.notify();
            }
            Err(atrium_memory::OpError::Changed) => cx.emit(super::Notice::Error(
                crate::i18n::t("The memory changed since; undo the later changes first.").into(),
            )),
            Err(error) => cx.emit(super::Notice::Error(error.to_string())),
        }
    }

    /// Whether Hindsight answers, off the UI thread.
    pub fn check_hindsight(&mut self, cx: &mut Context<Self>) {
        let Some(h) = self.memory_config().hindsight.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { hindsight::Hindsight::new(&h).health() })
                .await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(()) => cx.emit(super::Notice::Success(
                    crate::i18n::t("Hindsight answers.").into(),
                )),
                Err(error) => cx.emit(super::Notice::Error(crate::i18n::tf(
                    "Hindsight does not answer: {error}",
                    &[("error", &format!("{error:#}"))],
                ))),
            });
        })
        .detach();
    }

    pub fn memory_config(&self) -> Config {
        memory_root().map(|r| Config::load(&r)).unwrap_or_default()
    }

    pub fn set_memory_config(&mut self, config: Config, cx: &mut Context<Self>) {
        if let Some(root) = memory_root()
            && let Err(error) = config.save(&root)
        {
            cx.emit(super::Notice::Error(error.to_string()));
        }
        cx.notify();
    }

    /// The memory for a new session with `backend`, when it gets one:
    /// local agents only, and not those the user left out.
    pub(super) fn memory_prep(&self, backend: &str) -> Option<MemoryPrep> {
        if agents::split_backend(backend).1.is_some() {
            return None;
        }
        let root = memory_root()?;
        let config = Config::load(&root);
        config
            .on_for(backend)
            .then_some(MemoryPrep { root, config })
    }

    /// The `recall` and `remember` tools, as an MCP server this executable
    /// serves, for a local agent working in `cwd`.
    pub(super) fn memory_mcp(&self, backend: &str, cwd: &str) -> Option<McpServer> {
        self.memory_prep(backend)?;
        let exe = std::env::current_exe().ok()?;
        let root = memory_root()?;
        Some(McpServer {
            name: "atrium-memory".into(),
            command: exe.display().to_string(),
            args: vec![
                "memory-mcp".into(),
                "--root".into(),
                root.display().to_string(),
                "--cwd".into(),
                cwd.into(),
                "--agent".into(),
                agents::split_backend(backend).0.to_string(),
            ],
            env: Vec::new(),
        })
    }

    /// The user's MCP servers and the memory tools, for a new chat.
    pub(super) fn chat_mcp(&self, backend: &str) -> Vec<McpServer> {
        let mut servers = self.user_mcp(backend);
        servers.extend(self.memory_mcp(backend, &self.folder_for(backend)));
        servers
    }

    pub(super) fn init_memory(&mut self, cx: &mut Context<Self>) {
        self.shared_memory._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(20))
                    .await;
                if this.update(cx, |ws, cx| ws.memory_tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Exports conversations that changed, and hands on those that went
    /// quiet with something new.
    fn memory_tick(&mut self, cx: &mut Context<Self>) {
        let Some(root) = memory_root() else { return };
        let config = Config::load(&root);
        if !config.learn {
            return;
        }
        let quiet = Duration::from_secs(u64::from(config.review_after_idle_min.max(1)) * 60);
        let mut exports = Vec::new();
        let mut handoffs = Vec::new();
        for thread in &self.threads {
            if agents::split_backend(&thread.backend).1.is_some()
                || !config.on_for(&thread.backend)
                || self.prediction_forks.contains_key(&thread.id)
            {
                continue;
            }
            let Some(transcript) = self.transcripts.get(&thread.id) else {
                continue;
            };
            if transcript.is_running() {
                continue;
            }
            let len = transcript.len();
            let entry =
                self.shared_memory
                    .seen
                    .entry(thread.id.clone())
                    .or_insert((0, Instant::now(), 0));
            if entry.0 != len {
                *entry = (len, Instant::now(), entry.2);
                if let Some(session) = to_session(
                    &thread.id,
                    &thread.backend,
                    &thread.cwd,
                    &thread.title(),
                    thread.created_at,
                    transcript,
                ) {
                    exports.push(session);
                }
            } else if entry.2 != len && entry.1.elapsed() >= quiet && len > 0 {
                entry.2 = len;
                handoffs.push(thread.id.clone());
            }
        }
        if exports.is_empty() && handoffs.is_empty() {
            return;
        }
        cx.background_executor()
            .spawn(async move {
                let store = Store::new(&root);
                for session in &exports {
                    if let Err(error) = store.export_session(session) {
                        log::warn!("memory export: {error}");
                    }
                }
                for thread in handoffs {
                    if let Some(session) = store.read_session(&thread) {
                        hand_on(&store, &config, &session, "idle");
                    }
                }
            })
            .detach();
    }

    /// Hands the shown conversation on now, from the memory page or the
    /// chat menu.
    pub fn learn_from_chat(&mut self, thread: &str, cx: &mut Context<Self>) {
        let Some(root) = memory_root() else { return };
        let Some(meta) = self.threads.iter().find(|t| t.id == thread).cloned() else {
            return;
        };
        let Some(session) = self.transcripts.get(thread).and_then(|t| {
            to_session(
                &meta.id,
                &meta.backend,
                &meta.cwd,
                &meta.title(),
                meta.created_at,
                t,
            )
        }) else {
            return;
        };
        let config = Config::load(&root);
        cx.background_executor()
            .spawn(async move {
                let store = Store::new(&root);
                if store.export_session(&session).is_ok() {
                    hand_on(&store, &config, &session, "asked");
                }
            })
            .detach();
        cx.emit(super::Notice::Success(
            crate::i18n::t("Sent to be learned from.").into(),
        ));
    }
}

/// Queues a review for the memory service and retains the conversation in
/// Hindsight, as the config says.
fn hand_on(store: &Store, config: &Config, session: &Session, reason: &str) {
    let request = ReviewRequest {
        version: atrium_memory::VERSION,
        thread: session.header.thread.clone(),
        agent: session.header.agent.clone(),
        cwd: session.header.cwd.clone(),
        project: session.header.project.clone(),
        session: store.session_file(&session.header.thread),
        turns: session.turns(),
        reason: reason.into(),
        at_ms: session.header.updated_ms,
    };
    if let Err(error) = store.enqueue(&request) {
        log::warn!("memory inbox: {error}");
    }
    if let Some(h) = config.hindsight().filter(|h| h.retain) {
        let bank = h.banks(session.header.project.as_deref()).remove(0);
        let tags = vec![agents::split_backend(&session.header.agent).0.to_string()];
        if let Err(error) = hindsight::Hindsight::new(h).retain(
            &bank,
            &session.transcript(),
            &session.header.thread,
            "A conversation between the user and a coding agent",
            &tags,
        ) {
            log::info!("hindsight retain: {error:#}");
        }
    }
}

/// A conversation in the export format: messages, and a line per tool
/// step. Reasoning and approvals stay out.
pub fn to_session(
    thread: &str,
    backend: &str,
    cwd: &str,
    title: &str,
    created_at: i64,
    transcript: &Transcript,
) -> Option<Session> {
    let mut lines = Vec::new();
    let mut turn = 0usize;
    let mut last_turn: Option<String> = None;
    for entry in transcript.entries() {
        if last_turn.as_deref() != Some(entry.turn_id.as_str()) {
            if last_turn.is_some() {
                turn += 1;
            }
            last_turn = Some(entry.turn_id.clone());
        }
        let line = match &entry.kind {
            EntryKind::Item(Item::User { text, .. }) => {
                Line::new("user", turn, inject::without_memory(text))
            }
            EntryKind::Item(Item::Agent { text, .. }) if !text.trim().is_empty() => {
                Line::new("agent", turn, text.clone())
            }
            EntryKind::Item(Item::Command {
                command,
                exit_code,
                output,
                ..
            }) => {
                let tail: String = output
                    .lines()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                Line::new(
                    "tool",
                    turn,
                    format!(
                        "$ {command}  [exit {}]\n{tail}",
                        exit_code.map_or("?".into(), |c| c.to_string())
                    ),
                )
            }
            EntryKind::Item(Item::FileChange { changes, .. }) => Line::new(
                "tool",
                turn,
                format!(
                    "edited {}",
                    changes
                        .iter()
                        .map(|c| c.path.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            EntryKind::Item(Item::Tool { name, .. }) => {
                Line::new("tool", turn, format!("used {name}"))
            }
            EntryKind::Error(message) => Line::new("tool", turn, format!("error: {message}")),
            _ => continue,
        };
        lines.push(line);
    }
    if !lines.iter().any(|l| l.kind == "user") {
        return None;
    }
    let root = project_root(Path::new(cwd));
    let now = jiff::Timestamp::now().as_millisecond();
    Some(Session {
        header: Header {
            kind: "session".into(),
            version: atrium_memory::VERSION,
            thread: thread.into(),
            agent: backend.into(),
            cwd: cwd.into(),
            project: (!cwd.is_empty()).then(|| project_key(&root)),
            title: title.into(),
            started_ms: created_at * 1000,
            updated_ms: now,
        },
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::AgentEvent;

    #[test]
    fn a_transcript_exports_messages_and_tool_steps() {
        let mut t = Transcript::new();
        t.push_local_user(
            "<atrium-memory>\n# Memory\n</atrium-memory>\n\nFix the build",
            Vec::new(),
        );
        t.apply(&AgentEvent::ItemCompleted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
            item: Item::Agent {
                id: "a".into(),
                text: "Done.".into(),
            },
        });
        let session = to_session("t", "claude", "/tmp", "Fix", 1, &t).unwrap();
        assert_eq!(session.lines[0].text, "Fix the build");
        assert!(
            session
                .lines
                .iter()
                .any(|l| l.kind == "agent" && l.text == "Done.")
        );
        assert!(to_session("t", "claude", "/tmp", "x", 1, &Transcript::new()).is_none());
    }
}
