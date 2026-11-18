//! The workspace's project side: projects, their workstreams running side by
//! side in the background, and the atrium they align through.

use std::{collections::HashSet, path::PathBuf, time::Duration};

use atrium_core::{AgentEvent, EntryKind, GoalStatus, Item, ThreadMeta, Transcript, TurnStatus};
use codex_client::StartThread;
use gpui::Context;

use super::{CODEX, Composed, Notice, View, Workspace};
use crate::{
    agents,
    project::{self, ATRIUM, Project, Workstream},
};

/// Where the user is: the everyday chats, or a project.
#[derive(Clone, Debug, PartialEq)]
pub enum Room {
    Chats,
    Project {
        root: PathBuf,
        /// The workstream whose chat is open; the overview when `None`.
        stream: Option<String>,
    },
}

/// How a workstream is doing, for its card.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamState {
    /// Its session is being made.
    Starting,
    /// Its agent is not running, so it cannot go on.
    Offline,
    Working,
    /// Waiting on the user: approvals and questions.
    NeedsYou(usize),
    Failed(String),
    Idle,
}

/// What a workstream's card shows.
#[derive(Clone, Debug)]
pub struct StreamView {
    pub stream: Workstream,
    pub state: StreamState,
    /// Steps done and total, from the agent's latest plan.
    pub plan: Option<(usize, usize)>,
    /// The step in progress.
    pub step: Option<String>,
    /// The latest thing it said, one line.
    pub said: Option<String>,
    /// Its status file's focus, when it wrote one.
    pub focus: Option<String>,
    pub unread: usize,
    /// Changes it made in others' scopes.
    pub crossings: usize,
    pub goal_status: Option<GoalStatus>,
    pub speed: Option<String>,
    pub context: Option<f64>,
}

impl Workspace {
    pub(super) fn init_projects(&mut self, cx: &mut Context<Self>) {
        let roots = self.settings.projects.clone();
        for root in roots {
            match Project::open(&root) {
                Ok(project) => self.projects.push(project),
                Err(error) => log::warn!("could not open project {}: {error:#}", root.display()),
            }
        }
        for project in &self.projects {
            for stream in &project.file.streams {
                if let Some(id) = &stream.thread_id {
                    placeholder(&mut self.threads, id, stream, project);
                }
            }
        }
        if let Some(root) = self.settings.active_project.clone()
            && self.projects.iter().any(|p| p.root == root)
        {
            self.enter_project(root, cx);
        }
        self._atrium_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this.update(cx, |ws, cx| ws.tick_project(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Every couple of seconds in a project: read the atrium again if it
    /// changed, and open workstream chats whose agent has come up.
    fn tick_project(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        self.load_streams(&root, cx);
        if let Some(project) = self.projects.iter_mut().find(|p| p.root == root)
            && project.reload()
        {
            cx.notify();
        }
    }

    pub fn project_root(&self) -> Option<PathBuf> {
        match &self.room {
            Room::Project { root, .. } => Some(root.clone()),
            Room::Chats => None,
        }
    }

    pub fn active_project(&self) -> Option<&Project> {
        let root = self.project_root()?;
        self.projects.iter().find(|p| p.root == root)
    }

    fn project_mut(&mut self, root: &PathBuf) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.root == *root)
    }

    /// Every thread that belongs to a workstream: kept out of the chat list.
    pub fn project_threads(&self) -> HashSet<String> {
        self.projects
            .iter()
            .flat_map(|p| p.file.streams.iter().filter_map(|s| s.thread_id.clone()))
            .collect()
    }

    /// The project and workstream a thread runs, if any.
    pub fn stream_of(&self, thread_id: &str) -> Option<(&Project, &Workstream)> {
        self.projects.iter().find_map(|p| {
            p.file
                .streams
                .iter()
                .find(|s| s.thread_id.as_deref() == Some(thread_id))
                .map(|s| (p, s))
        })
    }

    /// The workstream whose chat is shown.
    pub fn open_stream(&self) -> Option<(&Project, &Workstream)> {
        self.active_thread_id().and_then(|id| self.stream_of(id))
    }

    pub fn enter_chats(&mut self, cx: &mut Context<Self>) {
        self.room = Room::Chats;
        self.settings.active_project = None;
        self.settings.save();
        self.new_chat(cx);
    }

    /// Adds the project in `root` (making its atrium the first time) and enters it.
    pub fn add_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if !self.projects.iter().any(|p| p.root == root) {
            match Project::open(&root) {
                Ok(project) => {
                    self.projects.push(project);
                    self.settings.projects.push(root.clone());
                    self.settings.save();
                }
                Err(error) => {
                    cx.emit(Notice::Error(format!(
                        "Could not open the project: {error:#}"
                    )));
                    return;
                }
            }
        }
        self.enter_project(root, cx);
    }

    /// Forgets a project here. Its folder and atrium stay as they are.
    pub fn remove_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.projects.retain(|p| p.root != root);
        self.settings.projects.retain(|p| *p != root);
        if self.project_root().as_ref() == Some(&root) {
            self.enter_chats(cx);
        }
        self.settings.save();
        cx.notify();
    }

    /// Shows a project's overview.
    pub fn enter_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.room = Room::Project {
            root: root.clone(),
            stream: None,
        };
        self.settings.active_project = Some(root.clone());
        self.settings.save();
        if let Some(project) = self.project_mut(&root) {
            project.stamp = None;
            project.reload();
        }
        self.load_streams(&root, cx);
        cx.notify();
    }

    /// Opens a workstream's chat.
    pub fn show_stream(&mut self, stream: &str, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        let thread = self
            .active_project()
            .and_then(|p| p.stream(stream))
            .and_then(|s| s.thread_id.clone());
        self.room = Room::Project {
            root,
            stream: Some(stream.to_string()),
        };
        match thread {
            Some(thread) => self.open_thread(thread, cx),
            None => cx.notify(),
        }
    }

    pub fn show_overview(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = self.project_root() {
            self.room = Room::Project { root, stream: None };
            cx.notify();
        }
    }

    /// Opens the transcripts of a project's workstreams in the background,
    /// for those whose agent is up.
    fn load_streams(&mut self, root: &PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
            return;
        };
        let threads: Vec<(String, String)> = project
            .file
            .streams
            .iter()
            .filter_map(|s| Some((s.thread_id.clone()?, s.backend.clone())))
            .collect();
        for (thread, backend) in threads {
            if self.transcripts.contains_key(&thread) || self.loading.contains(&thread) {
                continue;
            }
            let ready = match backend.as_str() {
                CODEX => self.client.is_some(),
                agent => self
                    .agent(agent)
                    .is_some_and(|a| a.status == super::AgentStatus::Ready),
            };
            if ready {
                self.load_thread(thread, cx);
            }
        }
    }

    /// Starts a new workstream: its session, its goal and its kickoff.
    pub fn create_stream(
        &mut self,
        name: String,
        backend: String,
        goal: String,
        scope: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.project_root() else {
            return;
        };
        let ready = match backend.as_str() {
            CODEX => self.client.is_some(),
            agent => self
                .agent(agent)
                .is_some_and(|a| a.status == super::AgentStatus::Ready),
        };
        if !ready {
            cx.emit(Notice::Error(format!(
                "{} is not running.",
                agents::backend_name(&backend)
            )));
            return;
        }
        let Some(project) = self.project_mut(&root) else {
            return;
        };
        let stream = Workstream {
            id: project.new_id(&name),
            name: name.trim().to_string(),
            backend: backend.clone(),
            thread_id: None,
            goal: goal.trim().to_string(),
            scope,
            notices_seen: project.notices.len(),
            created_at: jiff::Timestamp::now().as_second(),
        };
        let id = stream.id.clone();
        let title = format!("{} · {}", project.file.name, stream.name);
        project.file.streams.push(stream.clone());
        if let Err(error) = project.save() {
            cx.emit(Notice::Error(format!("{error:#}")));
        }
        let kickoff = project.kickoff(&stream);
        let cwd = root.clone();
        cx.notify();

        if backend == CODEX {
            let Some(client) = self.client.clone() else {
                return;
            };
            let options = StartThread {
                cwd: Some(cwd.display().to_string()),
                model: self.settings.model.clone(),
                approval_policy: Some(self.settings.mode.approval_policy().into()),
                sandbox: Some(self.settings.mode.sandbox_mode().into()),
            };
            let objective = stream.goal.clone();
            cx.spawn(async move |this, cx| {
                let meta = match client.thread_start(options).await {
                    Ok(meta) => meta,
                    Err(error) => {
                        let _ = this.update(cx, |ws, cx| ws.stream_failed(&root, &id, error, cx));
                        return;
                    }
                };
                let thread = meta.id.clone();
                let _ = client.thread_set_name(&thread, &title).await;
                let _ = this.update(cx, |ws, cx| {
                    ws.upsert_thread(ThreadMeta {
                        name: Some(title.clone()),
                        ..meta
                    });
                    ws.transcripts.insert(thread.clone(), Transcript::new());
                    ws.attach_thread(&root, &id, &thread);
                    ws.send_to_thread(thread.clone(), Composed::text(kickoff), cx);
                });
                // Codex keeps at it across turns once it has a goal.
                if !objective.is_empty()
                    && let Ok(Some(goal)) = client
                        .goal_set(&thread, Some(&objective), Some(GoalStatus::Active), None)
                        .await
                {
                    let _ = this.update(cx, |ws, cx| {
                        ws.goals.insert(thread, goal);
                        cx.notify();
                    });
                }
            })
            .detach();
        } else {
            let Some(client) = self.acp(&backend) else {
                return;
            };
            cx.spawn(async move |this, cx| {
                let start = match client.new_session(&cwd).await {
                    Ok(start) => start,
                    Err(error) => {
                        let _ = this.update(cx, |ws, cx| ws.stream_failed(&root, &id, error, cx));
                        return;
                    }
                };
                let _ = this.update(cx, |ws, cx| {
                    let now = jiff::Timestamp::now().as_second();
                    let thread = start.session_id.clone();
                    ws.upsert_thread(ThreadMeta {
                        id: thread.clone(),
                        backend: backend.clone(),
                        name: Some(title.clone()),
                        preview: title.clone(),
                        cwd: cwd.display().to_string(),
                        model: None,
                        created_at: now,
                        updated_at: now,
                    });
                    ws.known_options
                        .insert(backend.clone(), start.options.clone());
                    ws.session_options.insert(thread.clone(), start.options);
                    if let Some(commands) = ws.agent_commands.get(&backend).cloned() {
                        ws.commands.insert(thread.clone(), commands);
                    }
                    ws.transcripts.insert(thread.clone(), Transcript::new());
                    ws.attach_thread(&root, &id, &thread);
                    ws.send_to_thread(thread, Composed::text(kickoff), cx);
                });
            })
            .detach();
        }
    }

    fn attach_thread(&mut self, root: &PathBuf, stream: &str, thread: &str) {
        if let Some(project) = self.project_mut(root) {
            if let Some(s) = project.stream_mut(stream) {
                s.thread_id = Some(thread.to_string());
            }
            if let Err(error) = project.save() {
                log::warn!("could not save the project: {error:#}");
            }
        }
    }

    fn stream_failed(
        &mut self,
        root: &PathBuf,
        stream: &str,
        error: anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project_mut(root) {
            project.file.streams.retain(|s| s.id != stream);
            let _ = project.save();
        }
        cx.emit(Notice::Error(format!(
            "Could not start the workstream: {error:#}"
        )));
        cx.notify();
    }

    /// Takes a workstream out of its project. Its chat stays, as a normal chat.
    pub fn remove_stream(&mut self, stream: &str, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if let Some(project) = self.project_mut(&root) {
            project.file.streams.retain(|s| s.id != stream);
            let _ = project.save();
        }
        self.show_overview(cx);
    }

    /// Sends `composed` to a thread whatever the main column shows.
    pub fn send_to_thread(&mut self, thread: String, composed: Composed, cx: &mut Context<Self>) {
        let shown = std::mem::replace(&mut self.view, View::Thread(thread));
        self.send(composed, cx);
        self.view = shown;
        cx.notify();
    }

    /// When the shown chat is a workstream's: puts the notices it has not
    /// seen in front of the message, and marks them handed over.
    pub(super) fn with_stream_notices(&mut self, composed: Composed) -> Composed {
        let Some(thread) = self.active_thread_id().map(str::to_owned) else {
            return composed;
        };
        let Some((root, stream)) = self
            .stream_of(&thread)
            .map(|(p, s)| (p.root.clone(), s.id.clone()))
        else {
            return composed;
        };
        let Some(project) = self.project_mut(&root) else {
            return composed;
        };
        let unread = project.unread(&stream);
        if unread.is_empty() {
            return composed;
        }
        let text = project::with_notices(&unread, &composed.text);
        project.mark_read(&stream);
        let _ = project.save();
        Composed { text, ..composed }
    }

    /// Hands a workstream its new notices now, asking it to carry on.
    pub fn deliver_notices(&mut self, stream: &str, cx: &mut Context<Self>) {
        let Some(thread) = self
            .active_project()
            .and_then(|p| p.stream(stream))
            .and_then(|s| s.thread_id.clone())
        else {
            return;
        };
        let running = self
            .transcripts
            .get(&thread)
            .is_some_and(Transcript::is_running);
        if running && !crate::agents::is_codex(self.backend_of(&thread)) {
            cx.emit(Notice::Info(
                "It is working; the notices go with its next message.".into(),
            ));
            return;
        }
        // Codex reads them at its next step (a steer); others start a turn.
        self.send_to_thread(
            thread,
            Composed::text("Take these into account and carry on toward your goal.".into()),
            cx,
        );
    }

    /// Asks an idle workstream to keep going toward its goal.
    pub fn continue_stream(&mut self, stream: &str, cx: &mut Context<Self>) {
        let Some(thread) = self
            .active_project()
            .and_then(|p| p.stream(stream))
            .and_then(|s| s.thread_id.clone())
        else {
            return;
        };
        self.send_to_thread(
            thread,
            Composed::text(
                "Carry on toward your goal. Update your status file when you reach a milestone."
                    .into(),
            ),
            cx,
        );
    }

    /// Posts a notice from the user.
    pub fn post_notice(&mut self, to: Vec<String>, text: String, cx: &mut Context<Self>) {
        let Some(root) = self.project_root() else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        if let Some(project) = self.project_mut(&root)
            && let Err(error) = project.post(project::YOU, to, &text)
        {
            cx.emit(Notice::Error(format!("{error:#}")));
        }
        cx.notify();
    }

    /// Watches what workstreams do: a change in another's scope is posted
    /// to its owner; a finished turn may have written to the atrium.
    pub(super) fn watch_streams(&mut self, event: &AgentEvent, cx: &mut Context<Self>) {
        let Some(thread) = event.thread_id() else {
            return;
        };
        let Some((root, stream)) = self
            .stream_of(thread)
            .map(|(p, s)| (p.root.clone(), s.id.clone()))
        else {
            return;
        };
        match event {
            AgentEvent::ItemCompleted {
                item: Item::FileChange { changes, .. },
                ..
            } => {
                let Some(project) = self.project_mut(&root) else {
                    return;
                };
                for change in changes {
                    let Some(crossing) = project.crossing(&stream, &change.path) else {
                        continue;
                    };
                    if project.crossings.contains(&crossing) {
                        continue;
                    }
                    let name = |id: &str| {
                        project
                            .stream(id)
                            .map(|s| s.name.clone())
                            .unwrap_or_else(|| id.to_string())
                    };
                    let said = format!(
                        "{} changed `{}`, which is in your scope. Check it fits your plans.",
                        name(&crossing.stream),
                        crossing.path
                    );
                    cx.emit(Notice::Info(format!(
                        "{} changed {} in {}'s scope",
                        name(&crossing.stream),
                        crossing.path,
                        name(&crossing.owner)
                    )));
                    let owner = crossing.owner.clone();
                    project.crossings.push(crossing);
                    if let Err(error) = project.post(ATRIUM, vec![owner], &said) {
                        log::warn!("could not post a notice: {error:#}");
                    }
                }
            }
            AgentEvent::TurnCompleted { status, .. } => {
                if let Some(project) = self.project_mut(&root) {
                    project.reload();
                }
                if *status == TurnStatus::Failed {
                    let name = self
                        .projects
                        .iter()
                        .find(|p| p.root == root)
                        .and_then(|p| p.stream(&stream))
                        .map_or(stream.clone(), |s| s.name.clone());
                    cx.emit(Notice::Error(format!("{name} hit an error")));
                }
            }
            _ => {}
        }
    }

    /// What each workstream of the shown project is doing.
    pub fn stream_views(&self) -> Vec<StreamView> {
        let Some(project) = self.active_project() else {
            return Vec::new();
        };
        let now = std::time::Instant::now();
        project
            .file
            .streams
            .iter()
            .map(|stream| {
                let transcript = stream
                    .thread_id
                    .as_ref()
                    .and_then(|id| self.transcripts.get(id));
                let agent_up = match stream.backend.as_str() {
                    CODEX => self.client.is_some(),
                    agent => self
                        .agent(agent)
                        .is_some_and(|a| a.status == super::AgentStatus::Ready),
                };
                let waiting = transcript.map_or(0, waiting_on_user);
                let state = match (transcript, &stream.thread_id) {
                    (_, None) => StreamState::Starting,
                    (None, Some(_)) if !agent_up => StreamState::Offline,
                    (None, Some(_)) => StreamState::Starting,
                    (Some(_), _) if waiting > 0 => StreamState::NeedsYou(waiting),
                    (Some(t), _) if t.is_running() => StreamState::Working,
                    (Some(t), _) => match last_failure(t) {
                        Some(error) => StreamState::Failed(error),
                        None if !agent_up => StreamState::Offline,
                        None => StreamState::Idle,
                    },
                };
                let (plan, step) = transcript.map_or((None, None), latest_plan);
                let said = transcript.and_then(|t| {
                    t.entries().iter().rev().find_map(|e| match &e.kind {
                        EntryKind::Item(Item::Agent { text, .. }) if !text.trim().is_empty() => {
                            Some(
                                project::first_line(text)
                                    .trim_start_matches('#')
                                    .trim()
                                    .to_string(),
                            )
                        }
                        _ => None,
                    })
                });
                let focus = project
                    .statuses
                    .get(&stream.id)
                    .and_then(|text| focus_of(text));
                let pace = transcript.and_then(|t| t.latest_pace(now));
                StreamView {
                    unread: project.unread(&stream.id).len(),
                    crossings: project
                        .crossings
                        .iter()
                        .filter(|c| c.stream == stream.id)
                        .count(),
                    goal_status: stream
                        .thread_id
                        .as_ref()
                        .and_then(|id| self.goals.get(id))
                        .map(|g| g.status),
                    speed: pace
                        .filter(|p| p.writing_now || p.done)
                        .as_ref()
                        .and_then(crate::ui::format_speed),
                    context: transcript
                        .and_then(|t| t.usage())
                        .and_then(|u| u.context_share()),
                    stream: stream.clone(),
                    state,
                    plan,
                    step,
                    said,
                    focus,
                }
            })
            .collect()
    }
}

/// A stand-in thread for a workstream whose agent has not listed it yet, so
/// its backend is known.
fn placeholder(threads: &mut Vec<ThreadMeta>, id: &str, stream: &Workstream, project: &Project) {
    if threads.iter().any(|t| t.id == id) {
        return;
    }
    let title = format!("{} · {}", project.file.name, stream.name);
    threads.push(ThreadMeta {
        id: id.to_string(),
        backend: stream.backend.clone(),
        name: Some(title.clone()),
        preview: title,
        cwd: project.root.display().to_string(),
        model: None,
        created_at: stream.created_at,
        updated_at: stream.created_at,
    });
}

/// Approvals and questions still waiting for an answer.
pub fn waiting_on_user(transcript: &Transcript) -> usize {
    transcript
        .entries()
        .iter()
        .filter(|e| match &e.kind {
            EntryKind::Approval {
                answer, resolved, ..
            } => answer.is_none() && !resolved,
            EntryKind::Question {
                answers, resolved, ..
            } => answers.is_none() && !resolved,
            _ => false,
        })
        .count()
}

fn last_failure(transcript: &Transcript) -> Option<String> {
    let last = transcript.entries().last()?;
    let summary = transcript.turn_summary(&last.turn_id)?;
    if summary.status != TurnStatus::Failed {
        return None;
    }
    transcript
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EntryKind::Error(message) if e.turn_id == last.turn_id => Some(message.clone()),
            _ => None,
        })
}

fn latest_plan(transcript: &Transcript) -> (Option<(usize, usize)>, Option<String>) {
    transcript
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EntryKind::Plan { steps, .. } if !steps.is_empty() => {
                let done = steps.iter().filter(|s| s.done).count();
                let step = steps
                    .iter()
                    .find(|s| s.active)
                    .or_else(|| steps.iter().find(|s| !s.done))
                    .map(|s| s.step.clone());
                Some((Some((done, steps.len())), step))
            }
            _ => None,
        })
        .unwrap_or((None, None))
}

/// The first line under a status file's "Focus" heading.
fn focus_of(status: &str) -> Option<String> {
    let mut lines = status.lines();
    lines.find(|l| {
        let l = l.trim().trim_start_matches('#').trim().to_lowercase();
        l == "focus" || l.starts_with("focus") || l == "当前" || l == "焦点"
    })?;
    lines
        .map(|l| l.trim().trim_start_matches(['-', '*']).trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_is_read_from_a_status_file() {
        let status = "# Frontend\n\n## Focus\n\n- Timeline drag and drop\n\n## Done\n- Shell\n";
        assert_eq!(focus_of(status).as_deref(), Some("Timeline drag and drop"));
        assert_eq!(focus_of("nothing here"), None);
    }
}
