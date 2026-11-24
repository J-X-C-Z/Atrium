//! Workspace scheduling for lead-owned task pools.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use atrium_core::{EntryKind, GoalStatus, Transcript, TurnStatus};
use gpui::Context;

use super::{Composed, Workspace, waiting_on_user};
use crate::{
    agents,
    project::{ATRIUM, Project, Role, TaskStatus, Workstream, autopilot::Wake, wake_allowed},
};

const IDLE_RETIRE: Duration = Duration::from_secs(10 * 60);
const RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct TaskPoolSchedule {
    pub(super) starting: HashSet<(PathBuf, String)>,
    workers: HashMap<(PathBuf, String), WorkerActivity>,
    pub(super) last_start_failure: HashMap<(PathBuf, String), Instant>,
    goal_cleaned: HashSet<(PathBuf, String)>,
    last_cleanup: HashMap<(PathBuf, String), Instant>,
}

struct WorkerActivity {
    idle_since: Instant,
    empty_since: Option<Instant>,
    held: Option<String>,
    last_turn: Option<(usize, TurnStatus)>,
    failures: u8,
}

impl WorkerActivity {
    fn new(now: Instant) -> Self {
        Self {
            idle_since: now,
            empty_since: None,
            held: None,
            last_turn: None,
            failures: 0,
        }
    }

    fn empty_for(&mut self, now: Instant, idle_and_empty: bool) -> Duration {
        if idle_and_empty {
            now.duration_since(*self.empty_since.get_or_insert(now))
        } else {
            self.empty_since = None;
            Duration::ZERO
        }
    }

    fn observe(
        &mut self,
        now: Instant,
        busy: bool,
        held: Option<String>,
        last_turn: Option<(usize, TurnStatus)>,
    ) {
        if self.held != held {
            self.held = held;
            self.failures = 0;
            // A terminal turn observed under another claim is not a failure of this claim.
            self.last_turn = last_turn;
            self.idle_since = now;
        }
        if last_turn.is_some() && self.last_turn != last_turn {
            self.last_turn = last_turn;
            self.idle_since = now;
            if let Some((_, status)) = last_turn {
                self.failures = if self.held.is_some() && status == TurnStatus::Failed {
                    self.failures.saturating_add(1)
                } else {
                    0
                };
            }
        }
        if busy {
            self.idle_since = now;
        }
    }
}

fn terminal_turn(transcript: &Transcript) -> Option<(usize, TurnStatus)> {
    let entry = transcript.entries().last()?;
    let status = if entry.turn_id.is_empty() && matches!(entry.kind, EntryKind::Error(_)) {
        TurnStatus::Failed
    } else {
        transcript.turn_summary(&entry.turn_id)?.status
    };
    Some((transcript.len(), status))
}

pub(super) fn is_pool_worker(stream: &Workstream) -> bool {
    stream.role == Role::Worker && stream.task.is_none() && stream.parent.is_some()
}

fn worker_backend<'a>(project: &'a Project, lead: &'a Workstream) -> &'a str {
    let ready = project.claimable(&lead.id);
    if let Some(backend) = ready.first().and_then(|task| task.agent.as_deref())
        && ready
            .iter()
            .all(|task| task.agent.as_deref() == Some(backend))
    {
        return backend;
    }
    &lead.backend
}

impl Workspace {
    pub(super) fn tick_task_pools(&mut self, root: &PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|project| project.root == *root) else {
            return;
        };
        let now = Instant::now();
        let stale_after = Duration::from_secs(u64::from(project.file.autopilot.stall_min) * 60);
        let leads: Vec<_> = project
            .file
            .streams
            .iter()
            .filter(|s| s.role == Role::Lead && !s.retired)
            .cloned()
            .collect();
        let workers: Vec<_> = project
            .file
            .streams
            .iter()
            .filter(|s| is_pool_worker(s))
            .cloned()
            .collect();
        let orphan_claims: Vec<_> = project
            .tasks
            .iter()
            .filter(|task| {
                task.status == TaskStatus::Running
                    && task
                        .claimed_by
                        .as_deref()
                        .is_some_and(|id| project.stream(id).is_none_or(|worker| worker.retired))
            })
            .map(|task| (task.key(), task.claimed_by.clone().unwrap()))
            .collect();
        for (task, owner) in orphan_claims {
            if let Some(project) = self.project_mut(root) {
                match project.release_if_owner(&task, Some(&owner)) {
                    Ok(()) => cx.notify(),
                    Err(error) => log::warn!("could not release pool claim {task}: {error:#}"),
                }
            }
        }

        for worker in workers {
            if worker.retired {
                self.complete_retired_pool_goal(root, &worker, now, cx);
                continue;
            }
            let lead = worker.parent.as_deref().expect("pool worker has a lead");
            let Some(project) = self.projects.iter().find(|project| project.root == *root) else {
                return;
            };
            let held = project.task_of(&worker).map(|task| task.key());
            let session_full = project.pool_session_full(&worker.id);
            let pool_empty = project.pool(lead).drained();
            let lead_idle = project.stream(lead).is_none_or(|lead| {
                project
                    .task_of(lead)
                    .is_some_and(|task| task.status.finished())
                    || lead
                        .thread_id
                        .as_ref()
                        .and_then(|thread| self.transcripts.get(thread))
                        .is_some_and(|transcript| {
                            !transcript.is_running() && waiting_on_user(transcript) == 0
                        })
            });
            let key = (root.clone(), worker.id.clone());
            if self.project_schedule.task_pool.starting.contains(&key) {
                continue;
            }
            let thread = worker.thread_id.as_ref();
            let transcript = thread.and_then(|thread| self.transcripts.get(thread));
            let starting = thread.is_some_and(|thread| self.loading.contains(thread));
            let busy =
                starting || transcript.is_some_and(|t| t.is_running() || waiting_on_user(t) > 0);
            let activity = self
                .project_schedule
                .task_pool
                .workers
                .entry(key.clone())
                .or_insert_with(|| WorkerActivity::new(now));
            activity.observe(now, busy, held.clone(), transcript.and_then(terminal_turn));
            let empty_for = activity.empty_for(now, pool_empty && !busy);
            if busy {
                continue;
            }
            let idle = now.duration_since(activity.idle_since);
            let failures = activity.failures;
            if let Some(stalled) = self.stalled_pool_claim(root, &worker.id).map(str::to_owned) {
                self.retire_pool_worker(root, &worker, Some(&stalled), now, cx);
                continue;
            }
            let unattended = project.file.autopilot.on;
            // Only a persisted, unbound worker with no live start can be a crashed launch.
            let retire = thread.is_none()
                || if held.is_some() {
                    failures >= 3 || idle >= stale_after
                } else {
                    session_full || (lead_idle && empty_for >= IDLE_RETIRE)
                };
            if retire {
                self.retire_pool_worker(root, &worker, held.as_deref(), now, cx);
                continue;
            }
            let Some(thread) = thread.filter(|_| transcript.is_some()) else {
                continue;
            };
            // Unattended retries of an existing claim go through the pilot clock.
            if held.is_some() {
                continue;
            }
            if project.notice_delivery_blocked(&worker.id)
                || !self.backend_ready(&worker.backend)
                || self.running_project_streams(root) >= 12
                || !wake_allowed(
                    self.project_schedule.last_wake.get(&key).copied(),
                    now,
                    false,
                    false,
                    false,
                )
                || (failures > 0 && idle < RETRY_DELAY)
            {
                continue;
            }
            if held.is_none() && project.claimable(lead).is_empty() {
                continue;
            }
            let Some(project) = self.project_mut(root) else {
                return;
            };
            match project.claim_next(&worker.id) {
                Ok(Some(_)) => {
                    if unattended {
                        self.queue_autopilot_wake(root, &worker.id, Wake::TurnEnded, String::new());
                        continue;
                    }
                    let message = project.continuation(&worker, Wake::TurnEnded);
                    // Seed the current claim before sending, so a fast failed turn counts on it.
                    let held = project.task_of(&worker).map(|task| task.key());
                    let terminal = self.transcripts.get(thread).and_then(terminal_turn);
                    if let Some(activity) = self.project_schedule.task_pool.workers.get_mut(&key) {
                        activity.observe(now, true, held, terminal);
                    }
                    self.project_schedule.last_wake.insert(key, now);
                    self.send_to_thread(thread.clone(), Composed::text(message), cx);
                }
                Ok(None) => {}
                Err(error) => log::warn!("could not claim pool work for {}: {error:#}", worker.id),
            }
        }

        for lead in leads {
            self.start_pool_workers(root, &lead, now, cx);
            if let Some(project) = self.project_mut(root) {
                match project.queue_pool_report(&lead.id) {
                    Ok(true) => {
                        self.project_schedule
                            .pending
                            .insert((root.clone(), lead.id));
                    }
                    Ok(false) => {}
                    Err(error) => {
                        log::warn!("could not record pool report for {}: {error:#}", lead.id)
                    }
                }
            }
        }
    }

    pub(super) fn send_stream_kickoff(
        &mut self,
        root: &PathBuf,
        id: &str,
        thread: String,
        kickoff: String,
        cx: &mut Context<Self>,
    ) {
        let project = self.projects.iter().find(|p| p.root == *root);
        if let Some((project, worker)) = project.and_then(|p| Some((p, p.stream(id)?)))
            && is_pool_worker(worker)
        {
            let now = Instant::now();
            let key = (root.clone(), id.to_owned());
            let mut activity = WorkerActivity::new(now);
            activity.observe(now, true, project.task_of(worker).map(|t| t.key()), None);
            self.project_schedule
                .task_pool
                .workers
                .insert(key.clone(), activity);
            self.project_schedule.last_wake.insert(key, now);
        }
        self.send_to_thread(thread, Composed::text(kickoff), cx);
    }

    pub(super) fn running_project_streams(&self, root: &PathBuf) -> usize {
        self.projects
            .iter()
            .find(|p| p.root == *root)
            .map_or(0, |p| {
                p.file
                    .streams
                    .iter()
                    .filter(|s| {
                        if self
                            .project_schedule
                            .task_pool
                            .starting
                            .contains(&(root.clone(), s.id.clone()))
                        {
                            return true;
                        }
                        match &s.thread_id {
                            None => !s.retired,
                            Some(thread) => self.transcripts.get(thread).map_or_else(
                                || !s.retired && self.backend_ready(&s.backend),
                                Transcript::is_running,
                            ),
                        }
                    })
                    .count()
            })
    }

    fn start_pool_workers(
        &mut self,
        root: &PathBuf,
        lead: &Workstream,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        let key = (root.clone(), lead.id.clone());
        if self
            .project_schedule
            .task_pool
            .last_start_failure
            .get(&key)
            .is_some_and(|last| now.duration_since(*last) < RETRY_DELAY)
        {
            return;
        }
        let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
            return;
        };
        let wanted = worker_backend(project, lead).to_owned();
        let needed = project.workers_needed(&lead.id);
        for _ in 0..needed {
            if self.running_project_streams(root) >= 12 {
                break;
            }
            // Pool workers are placed like leads: a spent member's pool
            // starts its stand-in instead, and with nobody free it waits.
            let Ok(placed) = self.place_task(root, &wanted) else {
                break;
            };
            let backend = placed.agent.clone();
            if !self.backend_ready(&backend) {
                break;
            }
            let Some(project) = self.projects.iter().find(|p| p.root == *root) else {
                return;
            };
            if project.workers_needed(&lead.id) == 0 {
                break;
            }
            let Some(worker) = project.pool_worker(&lead.id, &backend) else {
                break;
            };
            if self
                .project_schedule
                .task_pool
                .starting
                .contains(&(root.clone(), worker.id.clone()))
            {
                break;
            }
            let worker_key = (root.clone(), worker.id.clone());
            if !self.scheduler_turn_slot(root) {
                break;
            }
            let worker_id = worker.id.clone();
            let choice = crate::workspace::team::start_choice(&placed);
            self.start_stream(root, worker, choice, cx);
            // The badge lives in app state so it survives a restart.
            let previous = self.settings.stand_in(root, &worker_id).map(str::to_owned);
            let current = placed.instead_of.as_ref().map(|_| placed.member.clone());
            if previous != current {
                self.settings
                    .set_stand_in(root, &worker_id, current.as_deref());
                self.settings.save();
            }
            if let Some(text) = Self::stand_in_notice(&worker_id, &placed) {
                if let Some(project) = self.project_mut(root)
                    && let Err(error) = project.post(ATRIUM, vec![lead.id.clone()], &text)
                {
                    log::warn!("could not post a pool stand-in notice: {error:#}");
                }
                self.queue_wake(root, lead.id.clone());
            }
            if !self
                .project_schedule
                .task_pool
                .starting
                .contains(&worker_key)
            {
                self.project_schedule
                    .task_pool
                    .last_start_failure
                    .insert(key, now);
                break;
            }
        }
    }

    fn retire_pool_worker(
        &mut self,
        root: &PathBuf,
        worker: &Workstream,
        held: Option<&str>,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project_mut(root) else {
            return;
        };
        match project.retire_pool_worker(&worker.id, held) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                log::warn!("could not retire pool worker {}: {error:#}", worker.id);
                return;
            }
        }
        self.project_schedule
            .task_pool
            .workers
            .remove(&(root.clone(), worker.id.clone()));
        self.complete_retired_pool_goal(root, worker, now, cx);
        cx.notify();
    }

    fn complete_retired_pool_goal(
        &mut self,
        root: &Path,
        worker: &Workstream,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = worker.thread_id.clone() else {
            return;
        };
        if !agents::is_codex(&worker.backend)
            || self.loading.contains(&thread)
            || self
                .transcripts
                .get(&thread)
                .is_some_and(|t| t.is_running() || waiting_on_user(t) > 0)
        {
            return;
        }
        let Some(client) = self.codex_for(&worker.backend) else {
            return;
        };
        let key = (root.to_path_buf(), worker.id.clone());
        let schedule = &mut self.project_schedule.task_pool;
        if schedule.goal_cleaned.contains(&key)
            || schedule
                .last_cleanup
                .get(&key)
                .is_some_and(|last| now.duration_since(*last) < RETRY_DELAY)
            || !self.project_schedule.cleaning.insert(key.clone())
        {
            return;
        }
        schedule.last_cleanup.insert(key.clone(), now);
        cx.spawn(async move |this, cx| {
            let result = match client.goal_get(&thread).await {
                Ok(Some(goal)) if goal.status != GoalStatus::Complete => {
                    client
                        .goal_set(&thread, None, Some(GoalStatus::Complete), None)
                        .await
                }
                result => result,
            };
            let _ = this.update(cx, |ws, cx| {
                ws.project_schedule.cleaning.remove(&key);
                match result {
                    Ok(goal) => {
                        ws.project_schedule.task_pool.goal_cleaned.insert(key);
                        if let Some(goal) = goal {
                            ws.goals.insert(thread, goal);
                        } else {
                            ws.goals.remove(&thread);
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        log::warn!("could not complete retired pool worker goal: {error:#}")
                    }
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_retirement_needs_ten_uninterrupted_empty_minutes() {
        let now = Instant::now();
        let mut activity = WorkerActivity::new(now);
        assert_eq!(activity.empty_for(now, true), Duration::ZERO);
        assert!(activity.empty_for(now + IDLE_RETIRE - Duration::from_secs(1), true) < IDLE_RETIRE);
        assert_eq!(activity.empty_for(now + IDLE_RETIRE, false), Duration::ZERO);
        assert_eq!(activity.empty_for(now + IDLE_RETIRE, true), Duration::ZERO);
        assert_eq!(activity.empty_for(now + IDLE_RETIRE * 2, true), IDLE_RETIRE);
    }

    #[test]
    fn failure_streak_counts_distinct_turns_for_one_claim() {
        let now = Instant::now();
        let mut activity = WorkerActivity::new(now);
        activity.observe(now, false, Some("web/a".into()), None);
        for turn in 1..=3 {
            let failed = Some((turn, TurnStatus::Failed));
            activity.observe(now, false, Some("web/a".into()), failed);
            activity.observe(now, false, Some("web/a".into()), failed);
            assert_eq!(usize::from(activity.failures), turn);
        }
        activity.observe(
            now,
            false,
            Some("web/b".into()),
            Some((3, TurnStatus::Failed)),
        );
        assert_eq!(activity.failures, 0);
    }
}

#[cfg(all(test, unix, feature = "workspace-ticker-test"))]
#[path = "workspace_project_pool_test.rs"]
mod ticker_tests;
