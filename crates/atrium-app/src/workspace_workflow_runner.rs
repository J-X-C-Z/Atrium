//! Drives workflow runs: starts ready steps, waits for their turns, runs the
//! checks and saves every state change.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use atrium_core::{EntryKind, Item, Transcript, TurnStatus};
use atrium_runtime::{Delivery, SessionSpec, TurnInput};
use atrium_workflow::{
    Check,
    check::{self, describe},
    render::{self, Lang},
    run::{CheckResult, Outcome, RunState, StepStatus},
    runner,
};
use codex_client::AccessMode;
use gpui::Context;

use super::{
    Notice, WorkflowReview, Workspace,
    workflows::{append_log, load_run, save_run},
};
use crate::agents;

impl Workspace {
    pub(super) fn init_workflow_runner(&mut self, cx: &mut Context<Self>) {
        self.ensure_runtime_clients();
        self.ensure_runtime_events(cx);
        self.refresh_workflows();
        self._workflow_ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                let result = this.update(cx, |workspace, cx| workspace.workflow_tick(cx));
                if result.is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
    }

    fn workflow_tick(&mut self, cx: &mut Context<Self>) {
        self.refresh_workflows();
        let runs = self.workflow_runs.clone();
        for entry in runs {
            let dir = entry.dir;
            let mut run = entry.run;
            if !self.workflow_recovered.contains(&dir) {
                if self.workflow_recover_run(&dir, &mut run, cx) {
                    continue;
                }
                append_log(
                    &dir,
                    serde_json::json!({"at": super::workflows::now_ms(), "event": "recovered"}),
                );
                self.workflow_recovered.insert(dir.clone());
            }
            if !matches!(run.state, RunState::Running | RunState::Waiting) {
                continue;
            }
            if run.state == RunState::Running {
                self.workflow_assign_threads(&dir, &mut run);
                self.workflow_finish_turns(&dir, &mut run, cx);
                self.workflow_expand(&dir, &mut run, cx);
                self.workflow_start_ready(&dir, &mut run, cx);
            }
            self.workflow_poll_review(&dir, &mut run, cx);
        }
        cx.notify();
    }

    /// After a restart, check every cut-off turn with its agent. A turn that
    /// is still running keeps its attempt and is attached again; anything
    /// else follows the existing retry path. Returns true while a check is
    /// still in flight.
    fn workflow_recover_run(
        &mut self,
        dir: &Path,
        run: &mut atrium_workflow::run::Run,
        cx: &mut Context<Self>,
    ) -> bool {
        let now = super::workflows::now_ms();
        let mut pending = false;
        let mut retry = Vec::new();
        for step in run.steps.clone() {
            if step.expanded || !matches!(step.status, StepStatus::Running | StepStatus::Checking) {
                continue;
            }
            if step.status == StepStatus::Running
                && let Some(attempt) = step.last()
                && attempt.finished_ms.is_none()
                && let Some(thread) = attempt.thread.as_deref()
            {
                let backend = attempt.agent.as_deref().unwrap_or("codex");
                if self.workflow_turn_is_live(thread, attempt.turn.as_deref()) {
                    continue;
                }
                let key = (dir.to_path_buf(), step.id.clone());
                if self.workflow_reattaching.contains(&key) {
                    pending = true;
                    continue;
                }
                if crate::agents::is_codex(backend) && self.codex_for(backend).is_some() {
                    self.workflow_reattach_try(
                        dir,
                        &step.id,
                        thread,
                        attempt.turn.clone(),
                        backend,
                        cx,
                    );
                    pending = true;
                    continue;
                }
            }
            retry.push(step.id.clone());
        }
        let mut changed = false;
        for id in retry {
            if run.recover_step(&id, now).is_ok() {
                changed = true;
            }
        }
        if changed {
            let _ = save_run(dir, run);
            append_log(
                dir,
                serde_json::json!({"at": now, "event": "restart_retry"}),
            );
        }
        pending
    }

    fn workflow_turn_is_live(&self, thread: &str, turn: Option<&str>) -> bool {
        self.transcripts
            .get(thread)
            .and_then(Transcript::active_turn)
            .is_some_and(|active| turn.is_none_or(|turn| turn == active))
    }

    fn workflow_reattach_try(
        &mut self,
        dir: &Path,
        step: &str,
        thread: &str,
        turn: Option<String>,
        backend: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.codex_for(backend) else {
            return;
        };
        let key = (dir.to_path_buf(), step.to_string());
        if !self.workflow_reattaching.insert(key.clone()) {
            return;
        }
        let dir = dir.to_path_buf();
        let step = step.to_string();
        let thread = thread.to_string();
        let backend = backend.to_string();
        cx.spawn(async move |this, cx| {
            let resumed = client.thread_resume(&thread).await;
            let live = resumed.as_ref().is_ok_and(|(_, turns)| {
                turns.iter().any(|(id, status, _)| {
                    *status == TurnStatus::InProgress
                        && turn.as_deref().is_none_or(|turn| turn == id)
                })
            });
            let _ = this.update(cx, |workspace, cx| {
                workspace.workflow_reattaching.remove(&key);
                match resumed {
                    Ok((mut meta, turns)) if live => {
                        meta.backend = backend;
                        workspace.upsert_thread(meta);
                        workspace
                            .transcripts
                            .insert(thread.clone(), Transcript::from_history(turns));
                        workspace.loaded_threads.insert(thread.clone());
                        workspace
                            .turn_started
                            .insert(thread.clone(), std::time::Instant::now());
                        append_log(
                            &dir,
                            serde_json::json!({
                                "at": super::workflows::now_ms(),
                                "event": "reattached",
                                "step": step,
                                "thread": thread,
                                "turn": turn
                            }),
                        );
                    }
                    _ => {
                        if let Some(mut run) = load_run(&dir)
                            && run.recover_step(&step, super::workflows::now_ms()).is_ok()
                        {
                            let _ = save_run(&dir, &run);
                            append_log(
                                &dir,
                                serde_json::json!({
                                    "at": super::workflows::now_ms(),
                                    "event": "restart_retry",
                                    "step": step,
                                    "thread": thread
                                }),
                            );
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn workflow_assign_threads(&mut self, dir: &Path, run: &mut atrium_workflow::run::Run) {
        let assigned: HashSet<String> = run
            .steps
            .iter()
            .flat_map(|step| step.attempts.iter().filter_map(|a| a.thread.clone()))
            .collect();
        let mut changes = Vec::new();
        for step in &run.steps {
            if step.status != StepStatus::Running || step.thread().is_some() {
                continue;
            }
            let Some(attempt) = step.last() else {
                continue;
            };
            let backend = attempt.agent.as_deref().unwrap_or(&self.draft_backend);
            let found = self
                .threads
                .iter()
                .filter(|thread| !assigned.contains(&thread.id))
                .filter(|thread| agents::split_backend(&thread.backend).0 == backend)
                .filter(|thread| Path::new(&thread.cwd) == dir)
                .filter(|thread| {
                    thread.created_at.saturating_mul(1000) + 1000 >= attempt.started_ms
                })
                .max_by_key(|thread| thread.created_at)
                .map(|thread| thread.id.clone());
            if let Some(thread) = found {
                changes.push((step.id.clone(), thread));
            }
        }
        let changed = !changes.is_empty();
        for (step, thread) in changes {
            let _ = run.set_thread(&step, thread);
        }
        if changed {
            let _ = save_run(dir, run);
        }
    }

    fn workflow_finish_turns(
        &mut self,
        dir: &Path,
        run: &mut atrium_workflow::run::Run,
        cx: &mut Context<Self>,
    ) {
        let running: Vec<(String, String)> = run
            .steps
            .iter()
            .filter(|step| step.status == StepStatus::Running)
            .filter_map(|step| {
                step.thread()
                    .map(|thread| (step.id.clone(), thread.to_string()))
            })
            .collect();
        for (step, thread) in running {
            // Checks finishing a step save the run from disk; start from
            // that, or this save would undo them.
            if let Some(fresh) = load_run(dir) {
                *run = fresh;
            }
            let Some(transcript) = self.transcripts.get(&thread) else {
                continue;
            };
            if transcript.is_running()
                || transcript.pending_approvals().next().is_some()
                || transcript.pending_questions().next().is_some()
            {
                continue;
            }
            let stopped = transcript
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match &entry.kind {
                    EntryKind::Error(message) => Some(message.clone()),
                    _ => None,
                });
            if let Err(error) = run.turn_finished(&step, super::workflows::now_ms()) {
                log::warn!("workflow {} turn: {error}", dir.display());
                continue;
            }
            let _ = save_run(dir, run);
            append_log(
                dir,
                serde_json::json!({"at": super::workflows::now_ms(), "event": "turn_finished", "step": step}),
            );
            if let Some(error) = stopped {
                self.workflow_complete_checked(
                    dir,
                    &step,
                    vec![CheckResult {
                        what: format!("the agent finished {step}"),
                        passed: false,
                        detail: format!("the agent stopped: {error}"),
                        send_back: None,
                    }],
                    cx,
                );
            } else {
                self.workflow_finish_checks(dir, &step, cx);
            }
        }
        if let Some(fresh) = load_run(dir) {
            *run = fresh;
        }
    }

    /// Lays out the items of repeating steps whose list is now written.
    fn workflow_expand(
        &mut self,
        dir: &Path,
        run: &mut atrium_workflow::run::Run,
        cx: &mut Context<Self>,
    ) {
        for (id, from) in run.to_expand() {
            let text = std::fs::read_to_string(dir.join(&from)).unwrap_or_default();
            let items = atrium_workflow::read_choices(&text);
            let count = items.len();
            let now = super::workflows::now_ms();
            let result = run.expand(&id, items, now);
            let _ = save_run(dir, run);
            append_log(
                dir,
                serde_json::json!({"at": now, "event": "expanded", "step": id, "items": count}),
            );
            if result.is_err() {
                cx.emit(Notice::Error(crate::i18n::tf(
                    "{step} has nothing to go through: {file} lists no items.",
                    &[("step", &run.title_of(&id)), ("file", &from)],
                )));
            }
        }
    }

    fn workflow_start_ready(
        &mut self,
        dir: &Path,
        run: &mut atrium_workflow::run::Run,
        cx: &mut Context<Self>,
    ) {
        let running = run
            .steps
            .iter()
            .filter(|step| step.status == StepStatus::Running)
            .count();
        if running >= 2 {
            return;
        }
        let ready = run.ready();
        for id in ready.into_iter().take(2 - running) {
            let Some(spec) = run.spec(&id).cloned() else {
                continue;
            };
            let backend = run
                .template
                .agent_of(&spec)
                .or_else(|| self.settings.default_agent.clone())
                .unwrap_or_else(|| self.draft_backend.clone());
            if !self.backend_ready(&backend) {
                cx.emit(Notice::Error(crate::i18n::tf(
                    "{agent} is not installed or not running; {step} cannot start.",
                    &[
                        ("agent", &agents::backend_label(&backend)),
                        ("step", &spec.title),
                    ],
                )));
                continue;
            }
            let now = super::workflows::now_ms();
            if run.begin(&id, Some(backend.clone()), now).is_err() {
                continue;
            }
            let Some(prompt) = render::prompt(
                run,
                &id,
                agents::split_backend(&backend).0 == "claude",
                if crate::i18n::chinese() {
                    Lang::Zh
                } else {
                    Lang::En
                },
            ) else {
                continue;
            };
            let _ = save_run(dir, run);
            append_log(
                dir,
                serde_json::json!({
                    "at": now,
                    "event": "step_started",
                    "step": id,
                    "backend": backend
                }),
            );
            self.workflow_start_chat(
                prompt,
                backend,
                dir.to_path_buf(),
                spec.effort,
                id.clone(),
                cx,
            );
        }
    }

    fn workflow_start_chat(
        &mut self,
        text: String,
        backend: String,
        cwd: PathBuf,
        effort: Option<String>,
        step: String,
        cx: &mut Context<Self>,
    ) {
        if !agents::is_codex(&backend) {
            self.start_chat_in(text, backend, cwd, false, cx);
            return;
        }
        if self.codex_for(&backend).is_none() {
            return;
        }
        self.ensure_runtime_clients();
        let title: String = text.chars().take(60).collect();
        let runtime = self.runtime.clone();
        let send_runtime = runtime.clone();
        let sink = self.runtime_sink(&backend);
        let dir = cwd.clone();
        let spec = SessionSpec {
            agent: backend.clone(),
            model: self.settings.model.clone(),
            effort,
            service_tier: None,
            folder: cwd,
            worktree: None,
            device: agents::split_backend(&backend).1.map(str::to_owned),
            mcp_servers: self.user_mcp(&backend),
            first_message: None,
            access_mode: Some(
                crate::runtime_adapters::access_mode_name(AccessMode::Auto).to_string(),
            ),
        };
        let named = self.codex_for(&backend);
        cx.spawn(async move |this, cx| {
            let started = crate::runtime_adapters::start_session_async(runtime, spec, sink).await;
            let thread = match started {
                Ok(thread) => thread,
                Err(error) => {
                    let _ = this.update(cx, |ws, cx| {
                        let step = ws
                            .workflow_runs
                            .iter()
                            .find(|entry| entry.dir == dir)
                            .and_then(|entry| {
                                entry.run.steps.iter().find(|step| {
                                    step.status == StepStatus::Running && step.thread().is_none()
                                })
                            })
                            .map(|step| step.id.clone());
                        if let Some(step) = step {
                            ws.workflow_fail_start(&dir, &step, format!("{error}"), cx);
                        }
                    });
                    return;
                }
            };
            if let Some(client) = &named {
                let _ = client.thread_set_name(&thread, &title).await;
            }
            let prepared = this.update(cx, |ws, _| {
                let now = jiff::Timestamp::now().as_second();
                ws.upsert_thread(atrium_core::ThreadMeta {
                    id: thread.clone(),
                    backend: backend.clone(),
                    name: Some(title.clone()),
                    preview: title.clone(),
                    cwd: dir.display().to_string(),
                    model: None,
                    created_at: now,
                    updated_at: now,
                });
                let mut transcript = Transcript::new();
                transcript.push_local_user(text.clone(), Vec::new());
                transcript.begin_pending_turn();
                ws.transcripts.insert(thread.clone(), transcript);
                ws.loaded_threads.insert(thread.clone());
                ws.turn_started
                    .insert(thread.clone(), std::time::Instant::now());
            });
            if prepared.is_err() {
                return;
            }
            let _ = this.update(cx, |_, _| {
                if let Some(mut run) = load_run(&dir)
                    && run.set_thread(&step, thread.clone()).is_ok()
                {
                    let _ = save_run(&dir, &run);
                }
            });
            let call_thread = thread.clone();
            let sent = crate::runtime_adapters::send_turn_async(
                send_runtime,
                call_thread,
                TurnInput::text(text),
                Delivery::Start,
            )
            .await;
            match sent {
                Ok(turn) => {
                    let _ = this.update(cx, |_, _| {
                        if let Some(mut run) = load_run(&dir)
                            && run.set_turn(&step, turn.clone()).is_ok()
                        {
                            let _ = save_run(&dir, &run);
                        }
                    });
                }
                Err(error) => {
                    let _ = this.update(cx, |ws, cx| {
                        if let Some(transcript) = ws.transcripts.get_mut(&thread) {
                            transcript.fail_pending_turn(format!("Could not send: {error}"));
                        }
                        ws.turn_started.remove(&thread);
                        let step = ws
                            .workflow_runs
                            .iter()
                            .find(|entry| entry.dir == dir)
                            .and_then(|entry| {
                                entry
                                    .run
                                    .steps
                                    .iter()
                                    .find(|step| step.thread() == Some(&thread))
                            })
                            .map(|step| step.id.clone());
                        if let Some(step) = step {
                            ws.workflow_fail_start(&dir, &step, format!("{error}"), cx);
                        }
                    });
                }
            }
        })
        .detach();
    }

    fn workflow_fail_start(
        &mut self,
        dir: &Path,
        step: &str,
        error: String,
        cx: &mut Context<Self>,
    ) {
        let Some(mut run) = load_run(dir) else { return };
        if run
            .step(step)
            .is_some_and(|s| s.status == StepStatus::Running)
        {
            let _ = run.turn_finished(step, super::workflows::now_ms());
        }
        let _ = run.checked(
            step,
            vec![CheckResult {
                what: format!("the agent starts {step}"),
                passed: false,
                detail: format!("the agent stopped: {error}"),
                send_back: None,
            }],
            super::workflows::now_ms(),
        );
        let _ = save_run(dir, &run);
        append_log(
            dir,
            serde_json::json!({
                "at": super::workflows::now_ms(),
                "event": "step_failed_to_start",
                "step": step,
                "error": error
            }),
        );
        cx.emit(Notice::Error(crate::i18n::tf(
            "{step} could not start: {error}",
            &[("step", &step), ("error", &error)],
        )));
    }

    fn workflow_finish_checks(&mut self, dir: &Path, step: &str, cx: &mut Context<Self>) {
        let key = (dir.to_path_buf(), step.to_string());
        if !self.workflow_checking.insert(key) {
            return;
        }
        let Some(run) = load_run(dir) else {
            self.workflow_checking
                .remove(&(dir.to_path_buf(), step.to_string()));
            return;
        };
        let lang = if crate::i18n::chinese() {
            Lang::Zh
        } else {
            Lang::En
        };
        let results = check::local_results(dir, &run, step, lang);
        self.workflow_check_from(dir, step, results, 0, cx);
    }

    fn workflow_check_from(
        &mut self,
        dir: &Path,
        step: &str,
        results: Vec<CheckResult>,
        start: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = load_run(dir) else {
            self.workflow_checking
                .remove(&(dir.to_path_buf(), step.to_string()));
            return;
        };
        let checks = run.checks_of(step);
        let Some(ix) = (start..checks.len()).find(|ix| !checks[*ix].is_local()) else {
            self.workflow_complete_checked(dir, step, results, cx);
            return;
        };
        let check = checks[ix].clone();
        let check_what = describe(
            &check,
            if crate::i18n::chinese() {
                Lang::Zh
            } else {
                Lang::En
            },
        );
        match check {
            Check::Command {
                run: command,
                cwd,
                timeout_min,
                ..
            } => {
                let dir = dir.to_path_buf();
                let step = step.to_string();
                cx.spawn(async move |this, cx| {
                    let work = dir.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            runner::command(&work, &command, cwd.as_deref(), timeout_min)
                        })
                        .await;
                    let _ = this.update(cx, |ws, cx| {
                        let mut results = results;
                        results.push(CheckResult {
                            what: check_what,
                            ..result
                        });
                        ws.workflow_check_from(&dir, &step, results, ix + 1, cx);
                    });
                })
                .detach();
            }
            Check::PdfPages { path, max, min } => {
                let mut results = results;
                results.push(CheckResult {
                    what: check_what,
                    ..runner::pdf_pages(dir, &path, max, min)
                });
                self.workflow_check_from(dir, step, results, ix + 1, cx);
            }
            Check::Review {
                rubric,
                files,
                agent,
            } => {
                let backend = agent
                    .or_else(|| run.spec(step).and_then(|s| run.template.agent_of(s)))
                    .or_else(|| self.settings.default_agent.clone())
                    .unwrap_or_else(|| self.draft_backend.clone());
                if !self.backend_ready(&backend) {
                    let mut results = results;
                    results.push(CheckResult {
                        what: check_what,
                        passed: false,
                        detail: format!("{} is not running", agents::backend_label(&backend)),
                        send_back: None,
                    });
                    self.workflow_check_from(dir, step, results, ix + 1, cx);
                    return;
                }
                let prompt = runner::review_prompt(&rubric, &files, &fixes_of(&run, step));
                let started_ms = super::workflows::now_ms();
                self.start_chat_in(prompt, backend, dir.to_path_buf(), false, cx);
                self.workflow_reviews.insert(
                    (dir.to_path_buf(), step.to_string()),
                    WorkflowReview {
                        started_ms,
                        results,
                        next: ix + 1,
                        thread: None,
                        retried: false,
                    },
                );
            }
            _ => self.workflow_check_from(dir, step, results, ix + 1, cx),
        }
    }

    fn workflow_poll_review(
        &mut self,
        dir: &Path,
        run: &mut atrium_workflow::run::Run,
        cx: &mut Context<Self>,
    ) {
        let pending: Vec<String> = self
            .workflow_reviews
            .keys()
            .filter(|(path, _)| path == dir)
            .map(|(_, step)| step.clone())
            .filter(|step| {
                run.step(step)
                    .is_some_and(|s| s.status == StepStatus::Checking)
            })
            .collect();
        let assigned: HashSet<String> = run
            .steps
            .iter()
            .flat_map(|step| step.attempts.iter().filter_map(|a| a.thread.clone()))
            .collect();
        for step in pending {
            let key = (dir.to_path_buf(), step.clone());
            let Some(review) = self.workflow_reviews.get_mut(&key) else {
                continue;
            };
            if review.thread.is_none() {
                review.thread = self
                    .threads
                    .iter()
                    .filter(|thread| !assigned.contains(&thread.id))
                    .filter(|thread| Path::new(&thread.cwd) == dir)
                    .filter(|thread| {
                        thread.created_at.saturating_mul(1000) + 1000 >= review.started_ms
                    })
                    .max_by_key(|thread| thread.created_at)
                    .map(|thread| thread.id.clone());
                continue;
            }
            let Some(thread) = review.thread.clone() else {
                continue;
            };
            let Some(transcript) = self.transcripts.get(&thread) else {
                continue;
            };
            if transcript.is_running() {
                continue;
            }
            let answer = transcript
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match &entry.kind {
                    EntryKind::Item(Item::Agent { text, .. }) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let verdict = runner::review_verdict(&answer);
            if verdict.is_none() && !review.retried {
                review.retried = true;
                review.thread = None;
                review.started_ms = super::workflows::now_ms();
                let check = run
                    .checks_of(&step)
                    .into_iter()
                    .find(|check| matches!(check, Check::Review { .. }));
                if let Some(Check::Review {
                    rubric,
                    files,
                    agent,
                }) = check
                {
                    let backend = agent
                        .or_else(|| run.spec(&step).and_then(|s| run.template.agent_of(s)))
                        .or_else(|| self.settings.default_agent.clone())
                        .unwrap_or_else(|| self.draft_backend.clone());
                    let prompt = runner::review_prompt(&rubric, &files, &fixes_of(run, &step));
                    self.start_chat_in(prompt, backend, dir.to_path_buf(), false, cx);
                }
                continue;
            }
            let mut review = self.workflow_reviews.remove(&key).unwrap();
            let lang = if crate::i18n::chinese() {
                Lang::Zh
            } else {
                Lang::En
            };
            let check = run
                .checks_of(&step)
                .into_iter()
                .find(|check| matches!(check, Check::Review { .. }));
            let what = check
                .as_ref()
                .map(|check| describe(check, lang))
                .unwrap_or_else(|| "review passes".into());
            review.results.push(match verdict {
                Some(verdict) => CheckResult {
                    what,
                    passed: verdict.pass,
                    detail: verdict.reasons.join("; "),
                    send_back: verdict.step.filter(|s| !s.is_empty()),
                },
                None => CheckResult {
                    what,
                    passed: false,
                    detail: "the reviewer did not give a verdict".into(),
                    send_back: None,
                },
            });
            self.workflow_check_from(dir, &step, review.results, review.next, cx);
        }
    }

    fn workflow_complete_checked(
        &mut self,
        dir: &Path,
        step: &str,
        results: Vec<CheckResult>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut run) = load_run(dir) else {
            self.workflow_checking
                .remove(&(dir.to_path_buf(), step.to_string()));
            return;
        };
        let outcome = run.checked(step, results.clone(), super::workflows::now_ms());
        if let Ok(outcome) = &outcome {
            let _ = save_run(dir, &run);
            append_log(
                dir,
                serde_json::json!({
                    "at": super::workflows::now_ms(),
                    "event": "checked",
                    "step": step,
                    "outcome": format!("{outcome:?}"),
                    "results": results
                }),
            );
            if matches!(outcome, Outcome::Review | Outcome::Failed) {
                cx.emit(Notice::Info(crate::i18n::tf(
                    "{step} needs your review in {run}.",
                    &[("step", &step), ("run", &dir.display())],
                )));
            }
        } else if let Err(error) = outcome {
            cx.emit(Notice::Error(error.to_string()));
        }
        self.workflow_checking
            .remove(&(dir.to_path_buf(), step.to_string()));
    }

    pub(super) fn workflow_interrupt_thread(&mut self, thread: &str, cx: &mut Context<Self>) {
        if self.sync_is_remote(thread) {
            self.sync_remote_interrupt(thread, cx);
            return;
        }
        self.ensure_runtime_clients();
        if self.runtime.has_session(thread) {
            let runtime = self.runtime.clone();
            let id = thread.to_string();
            cx.background_executor()
                .spawn(async move {
                    let _ = crate::runtime_adapters::interrupt_turn_async(runtime, id).await;
                })
                .detach();
            return;
        }
        if !crate::agents::is_codex(self.backend_of(thread)) {
            self.acp_stop(thread);
            return;
        }
        let turn = self
            .transcripts
            .get(thread)
            .and_then(Transcript::active_turn)
            .filter(|turn| !turn.is_empty())
            .map(str::to_owned);
        let (Some(turn), Some(client)) = (turn, self.thread_codex(thread)) else {
            return;
        };
        let id = thread.to_string();
        cx.spawn(async move |_, _| {
            let _ = client.turn_interrupt(&id, &turn).await;
        })
        .detach();
    }
}

#[cfg(all(test, unix, feature = "workspace-ticker-test"))]
#[path = "workspace_workflow_runner_test.rs"]
mod tests;

/// The earlier steps a reviewer of `step` may send it back to.
fn fixes_of(run: &atrium_workflow::run::Run, step: &str) -> Vec<String> {
    run.spec(step).map(|s| s.fixes.clone()).unwrap_or_default()
}
