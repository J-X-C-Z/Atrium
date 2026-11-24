//! Unattended scheduling. All wake sources (including pool digests in the
//! notice journal) pass through the same live preflight and turn budget.
use super::*;
use crate::project::{autopilot::Wake, scheduler::StreamClock};
use crate::workspace::{AgentStatus, Status};

#[derive(Default)]
pub(super) struct Pilot {
    streams: HashMap<(PathBuf, String), PilotStream>,
    last_restart: Option<Instant>,
    last_resume: Option<Instant>,
    reconnect: HashMap<String, (Instant, u32)>,
}

#[derive(Default)]
struct PilotStream {
    clock: StreamClock,
    extra: Vec<String>,
    requested: Option<Wake>,
    needs_resume: bool,
    retire_after_interrupt: Option<String>,
}

impl Pilot {
    /// Forgets every stream entry of a project that is no longer open.
    pub(super) fn forget_root(&mut self, root: &Path) {
        self.streams.retain(|(project, _), _| project != root);
    }

    /// Drops entries of retired streams (pool workers replaced after their
    /// session filled): they have no work and are never woken again, so their
    /// clocks and queued text would otherwise stay for the life of the app.
    pub(super) fn prune_retired(&mut self, root: &Path, live: &HashSet<String>) {
        self.streams
            .retain(|(project, stream), _| project != root || live.contains(stream));
    }
}

impl Workspace {
    pub(super) fn init_autopilot(&mut self) {
        let mut at = Instant::now();
        for p in self.projects.iter().filter(|p| p.file.autopilot.on) {
            for s in &p.file.streams {
                if s.thread_id.is_some() && p.has_work(s) {
                    self.project_schedule
                        .pilot
                        .streams
                        .entry((p.root.to_path_buf(), s.id.clone()))
                        .or_default()
                        .clock
                        .restarted(at);
                    at += Duration::from_secs(5);
                }
            }
        }
    }

    /// Manual opens stay immediate. Stagger automatic resume RPCs as well as wakes,
    /// including when a backend becomes ready after several deadlines have passed.
    pub(super) fn pilot_resume_slot(&mut self, root: &Path, id: &str, now: Instant) -> bool {
        let pilot = &mut self.project_schedule.pilot;
        let Some(at) = pilot
            .streams
            .get(&(root.to_owned(), id.to_owned()))
            .and_then(|s| s.clock.restart_at())
        else {
            return true;
        };
        if now < at
            || pilot
                .last_resume
                .is_some_and(|last| now.duration_since(last) < Duration::from_secs(5))
        {
            return false;
        }
        pilot.last_resume = Some(now);
        true
    }

    pub(super) fn clear_autopilot(&mut self, root: &Path) {
        self.project_schedule
            .pilot
            .streams
            .retain(|(p, _), _| p != root);
    }

    /// #2 can post a durable digest as a notice and set ScheduleState::pending.
    /// For transient context, enqueue once; a blocked wake retains and coalesces the text.
    #[allow(dead_code)] // Called by the parallel pool/PR scheduler integrations.
    pub(in crate::workspace) fn queue_autopilot_wake(
        &mut self,
        root: &Path,
        stream: &str,
        why: Wake,
        text: String,
    ) {
        if !self.projects.iter().any(|p| {
            p.root == root && p.file.autopilot.on && p.stream(stream).is_some_and(|s| !s.retired)
        }) {
            return;
        }
        let state = self
            .project_schedule
            .pilot
            .streams
            .entry((root.into(), stream.into()))
            .or_default();
        state.requested = Some(why);
        if !text.is_empty() && !state.extra.contains(&text) {
            state.extra.push(text);
        }
    }

    pub(in crate::workspace) fn pilot_failure(&mut self, thread: &str, resume: bool) {
        let Some((p, s)) = self.stream_of(thread) else {
            return;
        };
        if !p.file.autopilot.on || s.retired {
            return;
        }
        let key = (p.root.to_path_buf(), s.id.clone());
        let state = self.project_schedule.pilot.streams.entry(key).or_default();
        state.clock.failed(Instant::now());
        state.needs_resume |= resume;
    }

    pub(in crate::workspace) fn pilot_event(&mut self, event: &AgentEvent) {
        let Some(thread) = event.thread_id() else {
            return;
        };
        if self.loading.contains(thread) {
            return;
        }
        let Some((p, s)) = self.stream_of(thread) else {
            return;
        };
        if !p.file.autopilot.on || s.retired {
            return;
        }
        let key = (p.root.to_path_buf(), s.id.clone());
        let now = Instant::now();
        match event {
            AgentEvent::TurnStarted { .. } => {
                let phase = p.task_of(s).and_then(Task::current_phase);
                self.project_schedule
                    .pilot
                    .streams
                    .entry(key)
                    .or_default()
                    .clock
                    .started(now, phase);
            }
            AgentEvent::TurnCompleted { status, .. } => {
                let role = s.role;
                let phase = p.task_of(s).and_then(Task::current_phase);
                let pool = role == Role::Lead && !p.pool(&s.id).running.is_empty();
                let config = p.file.autopilot.clone();
                let state = self.project_schedule.pilot.streams.entry(key).or_default();
                match status {
                    TurnStatus::Completed => state.clock.completed(now, &config, role, pool, phase),
                    TurnStatus::Interrupted => state.clock.interrupted(now),
                    TurnStatus::Failed => state.clock.failed(now),
                    TurnStatus::InProgress => {}
                }
            }
            AgentEvent::Error {
                will_retry: false, ..
            } => self.pilot_failure(thread, false),
            _ => self
                .project_schedule
                .pilot
                .streams
                .entry(key)
                .or_default()
                .clock
                .activity(now),
        }
    }

    /// #2 calls retire_pool_worker(worker, Some(key)) after observing this idle claim.
    #[allow(dead_code)] // Consumed by the separate pool retirement scheduler.
    pub(in crate::workspace) fn stalled_pool_claim(
        &self,
        root: &Path,
        worker: &str,
    ) -> Option<&str> {
        let p = self
            .projects
            .iter()
            .find(|p| p.root.as_path() == root && p.file.autopilot.on)?;
        let s = p.stream(worker)?;
        let thread = s.thread_id.as_ref()?;
        if s.retired || self.loading.contains(thread) || self.transcripts.get(thread)?.is_running()
        {
            return None;
        }
        let key = self
            .project_schedule
            .pilot
            .streams
            .get(&(root.to_path_buf(), worker.to_owned()))?
            .retire_after_interrupt
            .as_deref()?;
        p.task_of(s).filter(|t| t.key() == key).map(|_| key)
    }

    /// Also used before a scheduler-owned kickoff (pool workers included).
    pub(in crate::workspace) fn scheduler_turn_slot(&mut self, root: &Path) -> bool {
        let Some(p) = self.project_mut(root) else {
            return false;
        };
        match p.reserve_scheduler_turn(&jiff::Zoned::now().date().to_string()) {
            Ok(slot) => slot,
            Err(e) => {
                log::warn!("could not reserve an unattended turn: {e:#}");
                false
            }
        }
    }

    fn reconnect_pilot_backend(&mut self, backend: &str, now: Instant, cx: &mut Context<Self>) {
        if self
            .project_schedule
            .pilot
            .reconnect
            .get(backend)
            .is_some_and(|(at, _)| now < *at)
        {
            return;
        }
        match agents::split_backend(backend) {
            ("codex", None) if matches!(self.status, Status::Failed(_)) => self.connect(cx),
            ("codex", Some(device))
                if self
                    .device(device)
                    .is_some_and(|d| matches!(d.codex_status, AgentStatus::Failed(_))) =>
            {
                self.start_remote_codex(device, cx)
            }
            _ => {
                let Some(ix) = self.agents.iter().position(|a| {
                    a.profile.id == backend && matches!(a.status, AgentStatus::Failed(_))
                }) else {
                    return;
                };
                self.start_agent(ix, cx);
            }
        }
        let backoff = self
            .project_schedule
            .pilot
            .reconnect
            .entry(backend.to_owned())
            .or_insert((now, 0));
        backoff.1 = backoff.1.saturating_add(1);
        backoff.0 = now + project::scheduler::retry_delay(backoff.1);
    }

    pub(super) fn tick_autopilot(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(p) = self.projects.iter().find(|p| p.root.as_path() == root) else {
            return;
        };
        if !p.file.autopilot.on {
            self.clear_autopilot(root);
            return;
        }
        let streams: Vec<_> = p
            .file
            .streams
            .iter()
            .filter(|s| !s.retired && s.thread_id.is_some())
            .cloned()
            .collect();
        for s in streams {
            self.tick_pilot_stream(root, &s, cx);
        }
    }

    fn tick_pilot_stream(&mut self, root: &Path, s: &Workstream, cx: &mut Context<Self>) {
        let p = self
            .projects
            .iter()
            .find(|p| p.root.as_path() == root)
            .unwrap();
        let thread = s.thread_id.as_deref().unwrap();
        let now = Instant::now();
        let key = (root.to_path_buf(), s.id.clone());
        let has_work = p.has_work(s);
        let notice = self.project_schedule.pending.contains(&key) && !p.unread(&s.id).is_empty();
        let state = self
            .project_schedule
            .pilot
            .streams
            .entry(key.clone())
            .or_default();
        let clock = &mut state.clock;
        if self.loading.contains(thread) {
            return;
        }
        if state.needs_resume {
            if !clock.may_wake(now) || (!has_work && !notice && state.requested.is_none()) {
                return;
            }
            if !self.backend_ready(&s.backend) {
                self.reconnect_pilot_backend(&s.backend, now, cx);
                return;
            }
            self.project_schedule.pilot.reconnect.remove(&s.backend);
            let state = self.project_schedule.pilot.streams.get_mut(&key).unwrap();
            state.needs_resume = false;
            state.clock.resuming();
            self.transcripts.remove(thread);
            self.loaded_threads.remove(thread);
            self.load_thread(thread.to_owned(), cx);
            return;
        }
        let transcript = self.transcripts.get(thread);
        if transcript.is_some_and(|t| waiting_on_user(t) > 0) {
            self.answer_unattended(root, s, cx);
            return;
        }
        if transcript.is_some_and(Transcript::is_running) {
            if let Some(count) =
                clock.poll_stall(now, &p.file.autopilot, p.task_of(s).map(Task::key))
            {
                if count == 3 {
                    let task = p.task_of(s).map(Task::key);
                    if s.role == Role::Worker && s.task.is_none() {
                        state.retire_after_interrupt = task.clone();
                    }
                    let msg = format!(
                        "{} stalled three times on {}.",
                        s.id,
                        task.as_deref().unwrap_or(&s.id)
                    );
                    self.pilot_notice(root, s, &msg, true);
                }
                // A pending Codex RPC has no turn id until turn/started arrives.
                let old = std::mem::replace(&mut self.view, View::Thread(thread.to_owned()));
                self.stop(cx);
                self.view = old;
            }
            return;
        }
        if let Some(task) = &state.retire_after_interrupt {
            if p.task_of(s).is_some_and(|t| t.key() == *task) {
                // Keep the claim quiescent until #2 retires it under its TaskLock.
                return;
            }
            state.retire_after_interrupt = None;
        }

        let due = clock.wake_due(now, &p.file.autopilot, s.role, has_work);
        if !clock.may_wake(now) || (due.is_none() && !notice && state.requested.is_none()) {
            return;
        }
        let requested = state.requested;
        if !self.backend_ready(&s.backend) {
            self.reconnect_pilot_backend(&s.backend, now, cx);
            return;
        }
        self.project_schedule.pilot.reconnect.remove(&s.backend);
        if transcript.is_none() {
            return;
        }
        let why = due.or(requested).unwrap_or(Wake::Heartbeat);
        if why == Wake::Restarted
            && self
                .project_schedule
                .pilot
                .last_restart
                .is_some_and(|t| now.duration_since(t) < Duration::from_secs(5))
        {
            return;
        }
        self.wake(root, s, why, now, cx);
    }

    fn wake(
        &mut self,
        root: &Path,
        s: &Workstream,
        why: Wake,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        let p = self
            .projects
            .iter()
            .find(|p| p.root.as_path() == root)
            .unwrap();
        let key = (root.to_path_buf(), s.id.clone());
        let thread = s.thread_id.as_ref().unwrap();
        if agents::split_backend(&s.backend).1.is_none() {
            let directory = p.work_dir(&s.id);
            if !approval::contained(root, &directory)
                || !self.threads.iter().any(|meta| {
                    meta.id == *thread
                        && meta.backend == s.backend
                        && approval::contained(&directory, Path::new(&meta.cwd))
                })
            {
                return;
            }
        }
        if !automatic_notice_dispatch_allowed(p, &s.id)
            || !wake_allowed(
                self.project_schedule.last_wake.get(&key).copied(),
                now,
                false,
                false,
                false,
            )
            || p.file
                .streams
                .iter()
                .filter(|s| {
                    !s.retired
                        && s.thread_id.as_ref().is_none_or(|id| {
                            self.loading.contains(id)
                                || self.transcripts.get(id).is_some_and(Transcript::is_running)
                        })
                })
                .count()
                >= 12
        {
            return;
        }
        let mut text = p.continuation(s, why);
        let phase = p.task_of(s).and_then(Task::current_phase);
        for extra in &self.project_schedule.pilot.streams[&key].extra {
            text.push_str("\n\n");
            text.push_str(extra);
        }
        if !self.scheduler_turn_slot(root) {
            return;
        }
        let state = BackgroundDispatch::enter(&mut self.view, &mut self.editing, thread.clone());
        let reactivate = self
            .goals
            .get(thread)
            .is_some_and(|g| g.status != GoalStatus::Active);
        let sent = self.send_turn(Composed::text(text), None, reactivate, cx);
        state.restore(&mut self.view, &mut self.editing);
        self.project_schedule.last_wake.insert(key.clone(), now);
        if sent == NoticeSendResult::Submitted {
            let state = self.project_schedule.pilot.streams.get_mut(&key).unwrap();
            state.extra.clear();
            state.requested = None;
            state.clock.started(now, phase);
            if why == Wake::Restarted {
                self.project_schedule.pilot.last_restart = Some(now);
            }
            self.project_schedule.pending.remove(&key);
        }
    }

    fn pilot_notice(&mut self, root: &Path, s: &Workstream, text: &str, parent: bool) {
        let mut to = vec![project::YOU.to_owned()];
        if parent && let Some(parent) = &s.parent {
            to.push(parent.clone());
        }
        if let Some(p) = self.project_mut(root)
            && let Err(e) = p.post(ATRIUM, to, text)
        {
            log::warn!("could not record unattended notice: {e:#}");
        }
    }
}

#[path = "autopilot_approval.rs"]
mod approval;

impl Workspace {
    pub(in crate::workspace) fn local_pilot_approval(
        &mut self,
        backend: &str,
        request: &atrium_core::ApprovalRequest,
        wire: &serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some((p, s)) = self.stream_of(&request.thread_id) else {
            return;
        };
        if !p.file.autopilot.on
            || s.backend != backend
            || agents::split_backend(backend).1.is_some()
            || s.retired
        {
            return;
        }
        let dir = p.work_dir(&s.id);
        let same_cwd = self
            .threads
            .iter()
            .find(|meta| meta.id == request.thread_id)
            .is_some_and(|meta| {
                dir.canonicalize()
                    .ok()
                    .zip(Path::new(&meta.cwd).canonicalize().ok())
                    .is_some_and(|(dir, cwd)| dir == cwd)
            });
        let permitted = same_cwd
            && approval::contained(&p.root, &dir)
            && self.settings.mode != codex_client::AccessMode::ReadOnly;
        let answer = self
            .transcripts
            .get(&request.thread_id)
            .filter(|_| permitted)
            .map_or(atrium_core::ApprovalDecision::Decline, |t| {
                if backend == CODEX {
                    approval::decision(&dir, request, wire, t)
                } else {
                    approval::acp_decision(&dir, wire)
                }
            });
        let root = p.root.to_path_buf();
        let stream = s.clone();
        self.pilot_answer(&root, &stream, request, answer, cx);
    }

    fn pilot_answer(
        &mut self,
        root: &Path,
        s: &Workstream,
        req: &atrium_core::ApprovalRequest,
        answer: atrium_core::ApprovalDecision,
        cx: &mut Context<Self>,
    ) {
        let result = if agents::is_codex(&s.backend) {
            self.codex_for(&s.backend)
                .map(|client| client.answer_approval(req.request_id.clone(), answer))
        } else {
            self.acp(&s.backend)
                .map(|client| client.answer_permission(req.request_id.clone(), answer))
        };
        match result {
            Some(Ok(())) => {
                if let Some(t) = self.transcripts.get_mut(&req.thread_id) {
                    t.answer_approval(&req.request_id, answer);
                }
                let action = match &req.kind {
                    atrium_core::ApprovalKind::Command { command, cwd, .. } => {
                        format!("{command} (cwd {cwd})")
                    }
                    atrium_core::ApprovalKind::FileChange { grant_root, .. } => format!(
                        "file change {} ({})",
                        req.item_id,
                        grant_root.as_deref().unwrap_or("no grant root")
                    ),
                    atrium_core::ApprovalKind::Tool { title, .. } => title.clone(),
                };
                self.pilot_notice(
                    root,
                    s,
                    &format!("{}: {answer:?} for {action}", s.id),
                    answer == atrium_core::ApprovalDecision::Decline,
                );
                cx.notify();
            }
            Some(Err(e)) => log::warn!("could not answer unattended approval: {e:#}"),
            None => {}
        }
    }

    fn answer_unattended(&mut self, root: &Path, s: &Workstream, cx: &mut Context<Self>) {
        let thread = s.thread_id.as_ref().unwrap();
        let Some(t) = self.transcripts.get(thread) else {
            return;
        };
        let approvals: Vec<_> = t.pending_approvals().cloned().collect();
        let questions: Vec<_> = t.pending_questions().cloned().collect();
        for req in approvals {
            // Only the original local wire request can prove scope. Replayed
            // descriptions and paths on a different host cannot.
            self.pilot_answer(root, s, &req, atrium_core::ApprovalDecision::Decline, cx);
        }
        if !agents::is_codex(&s.backend) {
            return;
        }
        let Some(client) = self.thread_codex(thread) else {
            return;
        };
        const ANSWER: &str = "No one is available to answer; choose the reasonable option, note it in your status file and tell your parent.";
        for req in questions {
            let answers: codex_client::Answers = req
                .questions
                .iter()
                .map(|q| (q.id.clone(), vec![ANSWER.to_owned()]))
                .collect();
            if let Err(e) = client.answer_user_input(req.request_id.clone(), answers.clone()) {
                log::warn!("could not answer unattended question: {e:#}");
                continue;
            }
            if let Some(t) = self.transcripts.get_mut(thread) {
                t.answer_questions(&req.request_id, answers);
            }
            let questions: Vec<_> = req.questions.iter().map(|q| q.question.as_str()).collect();
            self.pilot_notice(
                root,
                s,
                &format!("{} asked {} — {ANSWER}", s.id, questions.join(" / ")),
                true,
            );
            cx.notify();
        }
    }
}
