use super::{Answers, ApprovalDecision, Composed, RequestId, StreamState, Workspace};
use atrium_core::Transcript;
use atrium_phone::{
    Action, Decision, Error, Server,
    snapshot::{Chat, project_snapshot},
};
use atrium_project::{Project, YOU};
use atrium_runtime::AgentRuntime;
use gpui::{Context, Task};
use std::{collections::HashMap, sync::Arc, time::Duration};

pub(super) struct Phone {
    pub(super) server: Server,
    _commands: Task<()>,
    _publish: Task<()>,
}

pub(super) enum Answer {
    Approval(ApprovalDecision),
    Questions(Answers),
}

type ChatCache = HashMap<String, (u64, Arc<Chat>)>;

enum History {
    Empty,
    Published(u64, Arc<Chat>),
    Changed(Box<Transcript>),
}

impl History {
    fn capture(id: Option<&String>, t: Option<&Transcript>, cache: &ChatCache) -> Self {
        let Some(t) = t else { return Self::Empty };
        if let Some((version, chat)) = id.and_then(|id| cache.get(id))
            && *version == t.version()
        {
            return Self::Published(*version, chat.clone());
        }
        Self::Changed(Box::new(t.clone()))
    }

    fn project(self, id: Option<&String>, cache: &mut ChatCache) -> Arc<Chat> {
        let (version, chat) = match self {
            Self::Empty => return Arc::default(),
            Self::Published(version, chat) => (version, chat),
            Self::Changed(t) => (t.version(), Arc::new(Chat::from_transcript(&t))),
        };
        if let Some(id) = id {
            cache.insert(id.clone(), (version, chat.clone()));
        }
        chat
    }
}

impl Workspace {
    pub(super) fn sync_phone(&mut self, cx: &mut Context<Self>) {
        if !self.settings.remote_view {
            self.phone = None;
            return;
        }
        if self.phone.is_some() {
            return;
        }
        let (server, commands) = match Server::start() {
            Ok(server) => server,
            Err(e) => {
                log::error!("phone server startup: {e}");
                return;
            }
        };
        self.attach_phone(server, commands, cx);
    }

    fn attach_phone(
        &mut self,
        server: Server,
        mut commands: atrium_phone::Commands,
        cx: &mut Context<Self>,
    ) {
        let snapshots = server.snapshots.clone();
        let publish = cx.spawn(async move |this, cx| {
            let mut cache = ChatCache::new();
            loop {
                let Ok(inputs) = this.read_with(cx, |ws, _| ws.phone_inputs(&cache)) else {
                    return;
                };
                let write = snapshots.clone();
                cache = cx
                    .background_executor()
                    .spawn(async move {
                        let mut fresh = HashMap::new();
                        let next = inputs
                            .into_iter()
                            .map(|(p, views)| {
                                let mut views = views.into_iter();
                                project_snapshot(&p, |stream| {
                                    let (state, history) =
                                        views.next().expect("one view per captured stream");
                                    (
                                        state,
                                        history.project(stream.thread_id.as_ref(), &mut fresh),
                                    )
                                })
                            })
                            .collect::<Vec<_>>();
                        write.send_if_modified(|current| {
                            if **current == next {
                                return false;
                            }
                            *current = Arc::new(next);
                            true
                        });
                        fresh
                    })
                    .await;
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(cmd) = commands.recv().await {
                if cmd.reply.is_closed() {
                    continue;
                }
                let result = this.update(cx, |ws, cx| ws.phone_action(cmd.action, cx));
                let answer = match result {
                    Ok(Ok(task)) => task.await,
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(Error::Unavailable),
                };
                let _ = cmd.reply.send(answer);
            }
        });
        self.phone = Some(Phone {
            server,
            _commands: pump,
            _publish: publish,
        });
    }

    // Capture memory only. Pool walks, row projection and snapshot comparisons
    // happen on the background executor before publication.
    fn phone_inputs(&self, cache: &ChatCache) -> Vec<(Project, Vec<(&'static str, History)>)> {
        self.projects
            .iter()
            .map(|project| {
                let streams = project
                    .file
                    .streams
                    .iter()
                    .map(|s| {
                        let transcript =
                            s.thread_id.as_ref().and_then(|id| self.transcripts.get(id));
                        let state = match self.stream_state(s, transcript) {
                            StreamState::Starting => "starting",
                            StreamState::Offline => "offline",
                            StreamState::Working => "working",
                            StreamState::NeedsYou(_) => "needs_you",
                            StreamState::Failed(_) => "failed",
                            StreamState::Idle => "idle",
                        };
                        (
                            state,
                            History::capture(s.thread_id.as_ref(), transcript, cache),
                        )
                    })
                    .collect();
                (project.clone(), streams)
            })
            .collect()
    }

    fn phone_action(
        &mut self,
        action: Action,
        cx: &mut Context<Self>,
    ) -> Result<Task<Result<(), Error>>, Error> {
        if !self.settings.remote_view {
            return Err(Error::Unavailable);
        }
        match action {
            Action::Notice {
                project: ix,
                root,
                to,
                text,
            } => {
                let project = self.projects.get(ix).ok_or(Error::Missing)?;
                if project.root.to_string_lossy() != root {
                    return Err(Error::Stale);
                }
                if to
                    .iter()
                    .any(|id| id != "all" && project.stream(id).is_none())
                {
                    return Err(Error::Invalid);
                }
                let root = project.root.clone();
                Ok(cx.spawn(async move |this, cx| {
                    let path = root.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            Project::append_notice(&path, YOU, to, &text)
                                .map_err(|_| Error::WriteFailed)
                        })
                        .await;
                    let _ = this.update(cx, |ws, cx| {
                        if let Some(p) = ws.projects.iter_mut().find(|p| p.root == root) {
                            p.stamp = None; // the normal ticker reloads the appended notice
                        }
                        cx.notify();
                    });
                    result
                }))
            }
            Action::Message { thread, text } => {
                self.stream_of(&thread).ok_or(Error::Missing)?;
                if self.loading.contains(&thread)
                    || !self.submit_to_thread(thread, Composed::text(text), cx)
                {
                    return Err(Error::Unavailable);
                }
                Ok(Task::ready(Ok(())))
            }
            Action::Approval {
                thread,
                request_id,
                decision,
            } => {
                self.stream_of(&thread).ok_or(Error::Missing)?;
                let decision = match decision {
                    Decision::Accept => ApprovalDecision::Accept,
                    Decision::AcceptForSession => ApprovalDecision::AcceptForSession,
                    Decision::Decline => ApprovalDecision::Decline,
                };
                self.submit_answer(&thread, request_id, Answer::Approval(decision), cx)
            }
            Action::Question {
                thread,
                request_id,
                answers,
            } => {
                self.stream_of(&thread).ok_or(Error::Missing)?;
                self.submit_answer(
                    &thread,
                    request_id,
                    Answer::Questions(answers.into_iter().collect()),
                    cx,
                )
            }
        }
    }

    /// Both desktop buttons and HTTP actions claim the live request before writing
    /// to the agent, so a slow pipe or a second tap cannot block or answer twice.
    pub(super) fn submit_answer(
        &mut self,
        thread: &str,
        id: RequestId,
        answer: Answer,
        cx: &mut Context<Self>,
    ) -> Result<Task<Result<(), Error>>, Error> {
        let device = self.settings.device_name.clone();
        self.submit_answer_from(thread, id, answer, &device, cx)
    }

    /// Answers on behalf of `device`; the owner claims an approval once, so
    /// two devices cannot both execute it.
    pub(super) fn submit_answer_from(
        &mut self,
        thread: &str,
        id: RequestId,
        answer: Answer,
        device: &str,
        cx: &mut Context<Self>,
    ) -> Result<Task<Result<(), Error>>, Error> {
        let transcript = self.transcripts.get(thread).ok_or(Error::Stale)?;
        let write: Box<dyn FnOnce() -> anyhow::Result<()> + Send> = match &answer {
            Answer::Approval(d) => {
                if !transcript.pending_approvals().any(|r| r.request_id == id) {
                    return Err(Error::Stale);
                }
                let (id, d) = (id.clone(), *d);
                self.ensure_runtime_clients();
                if self.runtime.has_session(thread) {
                    let runtime = self.runtime.clone();
                    let thread = thread.to_owned();
                    Box::new(move || {
                        runtime
                            .answer_approval(&thread, id, d)
                            .map_err(|error| anyhow::anyhow!("{error}"))
                    })
                } else if crate::agents::is_codex(self.backend_of(thread)) {
                    let client = self.thread_codex(thread).ok_or(Error::Unavailable)?;
                    Box::new(move || client.answer_approval(id, d))
                } else {
                    let client = self
                        .acp(self.backend_of(thread))
                        .ok_or(Error::Unavailable)?;
                    Box::new(move || client.answer_permission(id, d))
                }
            }
            Answer::Questions(a) => {
                let req = transcript
                    .pending_questions()
                    .find(|r| r.request_id == id)
                    .ok_or(Error::Stale)?;
                if a.len() != req.questions.len()
                    || !req.questions.iter().all(|q| {
                        a.iter().any(|(id, values)| {
                            id == &q.id
                                && values.len() == 1
                                && !values[0].trim().is_empty()
                                && (q.options.is_empty()
                                    || q.allow_other
                                    || q.options.iter().any(|(label, _)| label == &values[0]))
                        })
                    })
                {
                    return Err(Error::Invalid);
                }
                let client = self.thread_codex(thread).ok_or(Error::Unavailable)?;
                let (id, a) = (id.clone(), a.clone());
                Box::new(move || client.answer_user_input(id, a))
            }
        };
        let key = (thread.to_owned(), id.to_string());
        if self.answered_by.contains_key(&key) {
            return Err(Error::Stale);
        }
        if !self.answering.insert(key.clone()) {
            return Err(Error::Stale);
        }
        self.answered_by.insert(key.clone(), device.to_string());
        let (sent, received) = futures::channel::oneshot::channel();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { write() }).await;
            if let Err(e) = &result {
                log::warn!("answering agent failed: {e:#}")
            }
            let _ = this.update(cx, |ws, cx| {
                ws.answering.remove(&key);
                if result.is_err() {
                    ws.answered_by.remove(&key);
                }
                if result.is_ok()
                    && let Some(t) = ws.transcripts.get_mut(&key.0)
                {
                    match answer {
                        Answer::Approval(d) => t.answer_approval(&id, d),
                        Answer::Questions(a) => t.answer_questions(&id, a),
                    }
                    cx.notify();
                }
            });
            let _ = sent.send(result.map_err(|_| Error::Unavailable));
        })
        .detach();
        Ok(cx.spawn(async move |_, _| received.await.unwrap_or(Err(Error::Unavailable))))
    }

    pub(super) fn answered_by(&self, thread: &str, request: &RequestId) -> Option<&str> {
        self.answered_by
            .get(&(thread.to_owned(), request.to_string()))
            .map(String::as_str)
    }
}

#[cfg(all(test, feature = "workspace-ticker-test"))]
#[path = "workspace_phone_test.rs"]
mod tests;
