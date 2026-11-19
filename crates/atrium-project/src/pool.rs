//! A lead's task pool: the lead writes many tasks at once, a few pool
//! workers claim them one after another, and the lead is woken only when the
//! pool runs low. After Paperclip's checkout: a task has one owner at a time,
//! a claim is atomic, and a worker that already holds a task gets it again.
//!
//! ```text
//! .atrium/tasks/<lead>/<id>.md     status: open → running (claimed_by: <worker>) → done | failed
//! ```
//!
//! Workers are separate processes (each agent's `atrium mcp`), so every
//! read-modify-write of task files happens under [`TaskLock`].

use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context as _, Result, bail};

use crate::{
    ATRIUM, DIR, Project, ProjectNotice, Role, Task, TaskStatus, Workstream, with_header,
    with_status,
};

const POOL_RECEIPT: &str = "[Atrium pool receipt] ";

fn receipt_keys(notice: &ProjectNotice, lead: &str) -> Option<Vec<String>> {
    if notice.from != ATRIUM
        || notice.to != [lead]
        || !notice.text.starts_with("[Atrium] Your task pool is ")
    {
        return None;
    }
    let encoded = notice.text.lines().last()?.strip_prefix(POOL_RECEIPT)?;
    let keys: Vec<String> = serde_json::from_str(encoded).ok()?;
    let prefix = format!("{lead}/");
    (!keys.is_empty()
        && keys.iter().all(|key| {
            key.strip_prefix(&prefix)
                .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        }))
    .then_some(keys)
}

/// Held while task files are read and rewritten; released on drop.
pub struct TaskLock {
    path: PathBuf,
}

impl TaskLock {
    /// Waits up to five seconds for `.atrium/tasks/.lock`. A lock older than
    /// thirty seconds was left by a process that died, and is taken over.
    pub fn take(project: &Project) -> Result<Self> {
        let dir = project.dir().join("tasks");
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(".lock");
        for _ in 0..250 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|at| SystemTime::now().duration_since(at).ok())
                        .is_some_and(|age| age > Duration::from_secs(30));
                    if stale {
                        let _ = fs::remove_file(&path);
                    } else {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("locking {}", path.display()));
                }
            }
        }
        bail!("{DIR}/tasks is busy; try again")
    }
}

impl Drop for TaskLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Where a lead's pool stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pool {
    /// Open and claimable now, in the order they will be claimed.
    pub ready: Vec<String>,
    /// Open but waiting for tasks in their `after:`.
    pub waiting: Vec<String>,
    pub running: Vec<String>,
    pub done: usize,
    pub failed: usize,
    /// Pool workers still in service, with the task each holds.
    pub workers: Vec<(String, Option<String>)>,
}

impl Pool {
    pub fn total(&self) -> usize {
        self.ready.len() + self.waiting.len() + self.running.len() + self.done + self.failed
    }

    /// Nothing left to claim or finish.
    pub fn drained(&self) -> bool {
        self.ready.is_empty() && self.waiting.is_empty() && self.running.is_empty()
    }
}

/// What the lead is told when its pool runs low: results since the last
/// report and what is left.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolReport {
    pub lead: String,
    pub text: String,
    /// The finished tasks it covers; mark them reported once delivered.
    pub keys: Vec<String>,
}

impl Project {
    /// Whether `task` is in a pool: owned by a lead and not given its own agent.
    pub fn is_pool_task(&self, task: &Task) -> bool {
        self.stream(&task.owner)
            .is_some_and(|o| o.role == Role::Lead)
            && self.stream_for_task(&task.key()).is_none()
    }

    /// `lead`'s open tasks whose `after:` are all done, in claim order:
    /// priority, then id.
    pub fn claimable(&self, lead: &str) -> Vec<&Task> {
        let mut out: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.owner == lead && t.status == TaskStatus::Open && self.is_pool_task(t))
            .filter(|t| t.claimed_by.is_none())
            .filter(|t| self.after_done(t))
            .collect();
        out.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
        out
    }

    fn after_done(&self, task: &Task) -> bool {
        task.after.iter().all(|id| {
            self.task(&format!("{}/{id}", task.owner))
                .is_none_or(|t| t.status == TaskStatus::Done)
        })
    }

    /// The pool workers of `lead` still in service.
    pub fn pool_workers<'a>(&'a self, lead: &'a str) -> impl Iterator<Item = &'a Workstream> + 'a {
        self.file.streams.iter().filter(move |s| {
            s.role == Role::Worker
                && s.parent.as_deref() == Some(lead)
                && s.task.is_none()
                && !s.retired
        })
    }

    pub fn pool(&self, lead: &str) -> Pool {
        let ready: Vec<String> = self.claimable(lead).iter().map(|t| t.key()).collect();
        let mut pool = Pool {
            ready,
            ..Pool::default()
        };
        for task in self
            .tasks
            .iter()
            .filter(|t| t.owner == lead && self.is_pool_task(t))
        {
            match task.status {
                TaskStatus::Open if !pool.ready.contains(&task.key()) => {
                    pool.waiting.push(task.key())
                }
                TaskStatus::Open => {}
                TaskStatus::Running => pool.running.push(task.key()),
                TaskStatus::Done => pool.done += 1,
                TaskStatus::Failed => pool.failed += 1,
            }
        }
        pool.workers = self
            .pool_workers(lead)
            .map(|w| (w.id.clone(), self.task_of(w).map(Task::key)))
            .collect();
        pool
    }

    /// How many more pool workers `lead` should get now: one per claimable
    /// task, up to `max_workers` in service.
    pub fn workers_needed(&self, lead: &str) -> usize {
        let serving = self.pool_workers(lead).count();
        let idle = self
            .pool_workers(lead)
            .filter(|w| self.task_of(w).is_none())
            .count();
        let wanted = self.claimable(lead).len().saturating_sub(idle);
        wanted.min(self.file.max_workers.max(1).saturating_sub(serving))
    }

    /// A new pool worker for `lead`: no task of its own; it claims them.
    pub fn pool_worker(&self, lead: &str, backend: &str) -> Option<Workstream> {
        let owner = self.stream(lead)?;
        if owner.role != Role::Lead {
            return None;
        }
        let n = self
            .file
            .streams
            .iter()
            .filter(|s| s.role == Role::Worker && s.parent.as_deref() == Some(lead))
            .count()
            + 1;
        Some(Workstream {
            id: self.new_id(&format!("{lead}-w{n}")),
            name: format!("{} · {n}", owner.name),
            backend: backend.to_string(),
            goal: format!("Work through the task pool of {}.", owner.name),
            scope: owner.scope.clone(),
            notices_seen: self.notices.len(),
            created_at: jiff::Timestamp::now().as_second(),
            role: Role::Worker,
            parent: Some(lead.to_string()),
            ..Workstream::default()
        })
    }

    /// Stops a session between tasks, including the automatic claim in MCP finish_task.
    pub fn pool_session_full(&self, worker: &str) -> bool {
        let limit = self.file.autopilot.tasks_per_session;
        limit > 0
            && self
                .tasks
                .iter()
                .filter(|t| t.claimed_by.as_deref() == Some(worker) && t.status.finished())
                .take(limit)
                .count()
                == limit
    }

    /// Persist the worker before claiming, and only construct a kickoff with a real claim.
    pub fn prepare_pool_start(&mut self, worker: &Workstream) -> Result<Option<String>> {
        anyhow::ensure!(self.stream(&worker.id).is_none(), "worker already exists");
        self.file.streams.push(worker.clone());
        if let Err(error) = self.save() {
            self.file.streams.pop();
            return Err(error);
        }
        Ok(self.claim_next(&worker.id)?.map(|_| self.kickoff(worker)))
    }

    /// Claims the next task of `me`'s lead for `me`, atomically. A worker
    /// that already holds a task gets that one back. `None` when the pool
    /// has nothing to claim.
    pub fn claim_next(&mut self, me: &str) -> Result<Option<Task>> {
        let lock = TaskLock::take(self)?;
        self.stamp = None;
        self.reload();
        self.claim_next_locked(me, &lock)
    }

    // MCP processes do not own project.json. Resolve their identity after locking,
    // including retirement/removal since the tool call loaded the project.
    pub(crate) fn claim_from_disk(&mut self, me: &str) -> Result<Option<Task>> {
        let lock = TaskLock::take(self)?;
        *self = Self::load(&self.root)?;
        self.claim_next_locked(me, &lock)
    }

    pub(crate) fn claim_next_locked(&mut self, me: &str, _lock: &TaskLock) -> Result<Option<Task>> {
        let own = self
            .stream(me)
            .with_context(|| format!("`{me}` is not a workstream of this project"))?;
        if own.retired || own.role != Role::Worker || own.task.is_some() {
            bail!("Only active pool sub-agents claim tasks.");
        }
        let lead = own
            .parent
            .as_deref()
            .context("a pool sub-agent needs a lead")?;
        if let Some(held) = self.task_of(own) {
            return Ok(Some(held.clone()));
        }
        if self.pool_session_full(me) {
            return Ok(None);
        }
        let Some(task) = self.claimable(lead).first().map(|t| (*t).clone()) else {
            return Ok(None);
        };
        if !self.worktree_allows_claim(me, &task)? {
            return Ok(None);
        }
        let path = self.root.join(task.rel_path());
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let text = with_header(
            &with_status(&text, TaskStatus::Running),
            "claimed_by",
            Some(me),
        );
        crate::atomic_write(&path, text.as_bytes())?;
        self.stamp = None;
        self.reload();
        Ok(self.task(&task.key()).cloned())
    }

    /// Puts a claimed task back in the pool: its worker died or was retired
    /// before finishing it.
    pub fn release(&mut self, key: &str) -> Result<()> {
        self.release_if_owner(key, None)
    }

    /// A stale scheduler snapshot cannot release a claim since handed to another worker.
    pub fn release_if_owner(&mut self, key: &str, owner: Option<&str>) -> Result<()> {
        let lock = TaskLock::take(self)?;
        self.stamp = None;
        self.reload();
        self.release_locked(key, owner, &lock)
    }

    /// Recheck the observed claim under the same lock used by MCP to finish and claim.
    /// Save retirement before release so restart can recover either side of the write.
    pub fn retire_pool_worker(&mut self, id: &str, expected: Option<&str>) -> Result<bool> {
        let lock = TaskLock::take(self)?;
        self.stamp = None;
        self.reload();
        let worker = self.stream(id).context("missing pool worker")?;
        anyhow::ensure!(
            worker.role == Role::Worker && worker.task.is_none() && worker.parent.is_some(),
            "not a pool worker"
        );
        let held = self.task_of(worker).map(Task::key);
        if held.as_deref() != expected {
            return Ok(false);
        }
        if !worker.retired {
            self.stream_mut(id).unwrap().retired = true;
            if let Err(error) = self.save() {
                self.stream_mut(id).unwrap().retired = false;
                return Err(error);
            }
        }
        if let Some(key) = held {
            self.release_locked(&key, Some(id), &lock)?;
        }
        Ok(true)
    }

    fn release_locked(&mut self, key: &str, owner: Option<&str>, _lock: &TaskLock) -> Result<()> {
        let Some(task) = self.task(key) else {
            return Ok(());
        };
        if task.status != TaskStatus::Running
            || owner.is_some_and(|id| task.claimed_by.as_deref() != Some(id))
        {
            return Ok(());
        }
        let path = self.root.join(task.rel_path());
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let text = with_header(&with_status(&text, TaskStatus::Open), "claimed_by", None);
        crate::atomic_write(&path, text.as_bytes())?;
        self.stamp = None;
        self.reload();
        Ok(())
    }

    /// Queue a digest once and acknowledge its task keys after delivery.
    pub fn queue_pool_report(&mut self, lead: &str) -> anyhow::Result<bool> {
        // Only accepted deliveries advance notices_seen. A queued receipt suppresses republication
        // even after a restart or a save failure between delivery and bookkeeping.
        let seen = self.stream(lead).map_or(0, |s| s.notices_seen);
        let previous = self.file.reported_tasks.len();
        let mut queued = Vec::new();
        for (i, notice) in self.notices.iter().enumerate() {
            if let Some(keys) = receipt_keys(notice, lead) {
                if i >= seen {
                    queued.extend(keys);
                } else {
                    for key in keys {
                        if !self.file.reported_tasks.contains(&key) {
                            self.file.reported_tasks.push(key);
                        }
                    }
                }
            }
        }
        if previous != self.file.reported_tasks.len()
            && let Err(error) = self.save()
        {
            self.file.reported_tasks.truncate(previous);
            return Err(error);
        }
        let pending = !queued.is_empty();
        let report = self.pool_report_excluding(lead, &queued);
        if let Some(report) = report {
            let receipt = serde_json::to_string(&report.keys)?;
            self.post(
                ATRIUM,
                vec![report.lead],
                &format!("{}\n\n{POOL_RECEIPT}{receipt}", report.text),
            )?;
            return Ok(true);
        }
        Ok(pending)
    }

    /// Finished pool tasks of `lead` its lead has not been told about.
    pub fn unreported_pool(&self, lead: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.owner == lead && t.status.finished() && self.is_pool_task(t))
            .filter(|t| !self.file.reported_tasks.contains(&t.key()))
            .collect()
    }

    /// What to wake `lead` with, if anything: when the claimable tasks fall
    /// to `refill_at` (one per worker by default) while results wait, or when
    /// a task failed. `None` while the pool is busy and healthy.
    pub fn pool_report(&self, lead: &str) -> Option<PoolReport> {
        self.pool_report_excluding(lead, &[])
    }

    fn pool_report_excluding(&self, lead: &str, queued: &[String]) -> Option<PoolReport> {
        let mut finished = self.unreported_pool(lead);
        finished.retain(|t| !queued.contains(&t.key()));
        if finished.is_empty() {
            return None;
        }
        let pool = self.pool(lead);
        let threshold = match self.file.autopilot.refill_at {
            0 => self.file.max_workers.max(1),
            n => n,
        };
        let low = pool.ready.len() < threshold;
        let failed = finished.iter().any(|t| t.status == TaskStatus::Failed);
        if !low && !failed {
            return None;
        }
        let mut text = if pool.drained() {
            format!(
                "[Atrium] Your task pool is empty: all {} task(s) finished.",
                pool.total()
            )
        } else {
            format!(
                "[Atrium] Your task pool is running low: {} ready, {} waiting on others, {} in progress.",
                pool.ready.len(),
                pool.waiting.len(),
                pool.running.len()
            )
        };
        text.push_str("\n\nFinished since the last report:\n");
        for task in &finished {
            text.push_str(&format!(
                "- `{}` {} — {}: {}\n",
                task.key(),
                task.title,
                task.status.word(),
                task.result
                    .as_deref()
                    .map(crate::first_line)
                    .unwrap_or("no result written")
            ));
        }
        text.push_str(
            "\nReview what you need to (results are under `## Result` in each file; check the changes), tick your roadmap, then add the next batch of tasks with `create_tasks` so the sub-agents keep going. If this phase needs no more, move on; if your goal is done, `finish_task`.",
        );
        Some(PoolReport {
            lead: lead.to_string(),
            text,
            keys: finished.iter().map(|t| t.key()).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::NewTask;

    fn project(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("atrium-pool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        project.file.max_workers = 2;
        project.file.streams.push(Workstream {
            id: "web".into(),
            name: "Web".into(),
            role: Role::Lead,
            scope: vec!["web/**".into()],
            ..Default::default()
        });
        project.save().unwrap();
        project
    }

    fn task(title: &str) -> NewTask {
        NewTask {
            title: title.into(),
            goal: format!("Do {title}."),
            ..Default::default()
        }
    }

    #[test]
    fn workers_claim_one_task_each_in_order_and_go_on_to_the_next() {
        let mut p = project("claim");
        let said = p
            .create_tasks(
                "web",
                &[
                    task("Button"),
                    NewTask {
                        after: vec!["button".into()],
                        ..task("Form")
                    },
                    NewTask {
                        priority: 1,
                        ..task("Zeta urgent")
                    },
                ],
            )
            .unwrap();
        assert!(said.contains("3 task"), "{said}");
        assert_eq!(p.workers_needed("web"), 2, "two ready, two slots");
        let w1 = p.pool_worker("web", "codex").unwrap();
        p.file.streams.push(w1.clone());
        let w2 = p.pool_worker("web", "codex").unwrap();
        assert_ne!(w1.id, w2.id);
        p.file.streams.push(w2.clone());
        p.save().unwrap();
        assert_eq!(p.workers_needed("web"), 0);

        // Priority first; the form waits for the button.
        let a = p.claim_next(&w1.id).unwrap().unwrap();
        assert_eq!(a.id, "zeta-urgent");
        assert_eq!(a.claimed_by.as_deref(), Some(w1.id.as_str()));
        assert_eq!(
            p.claim_next(&w1.id).unwrap().unwrap().id,
            "zeta-urgent",
            "a worker holding a task gets it again"
        );
        let b = p.claim_next(&w2.id).unwrap().unwrap();
        assert_eq!(b.id, "button");
        assert_eq!(p.pool("web").waiting, vec!["web/form"]);

        // Finishing hands over the next task in the same call.
        let said = p.finish_task(&w2.id, true, "Button done.").unwrap();
        assert!(said.contains("web/form"), "{said}");
        assert_eq!(
            p.task("web/form").unwrap().claimed_by.as_deref(),
            Some(w2.id.as_str())
        );
        let said = p.finish_task(&w1.id, true, "Urgent done.").unwrap();
        assert!(said.contains("pool is empty"), "{said}");

        // A worker that dies hands its task back.
        p.release("web/form").unwrap();
        let form = p.task("web/form").unwrap();
        assert_eq!(
            (form.status, form.claimed_by.clone()),
            (TaskStatus::Open, None)
        );
        assert_eq!(p.claim_next(&w1.id).unwrap().unwrap().id, "form");
        let _ = fs::remove_dir_all(&p.root);
    }

    #[test]
    fn the_lead_is_woken_when_the_pool_runs_low_with_the_results() {
        let mut p = project("refill");
        p.create_tasks("web", &[task("A"), task("B"), task("C"), task("D")])
            .unwrap();
        let w = p.pool_worker("web", "claude").unwrap();
        p.file.streams.push(w.clone());
        p.save().unwrap();
        assert_eq!(p.pool_report("web"), None, "nothing finished yet");
        p.claim_next(&w.id).unwrap();
        p.finish_task(&w.id, true, "A is in.").unwrap();
        // Two ready (C, D), one running (B): not low yet with two workers.
        assert_eq!(p.pool_report("web"), None);
        p.finish_task(&w.id, true, "B is in.").unwrap();
        let report = p.pool_report("web").unwrap();
        assert!(
            report.text.contains("running low: 1 ready"),
            "{}",
            report.text
        );
        assert!(report.text.contains("web/a") && report.text.contains("A is in."));
        assert_eq!(report.keys, vec!["web/a", "web/b"]);
        p.file.reported_tasks.extend(report.keys);
        assert_eq!(p.pool_report("web"), None, "reported once");
        // A failure is reported at once.
        p.finish_task(&w.id, false, "C is blocked on the API.")
            .unwrap();
        assert!(
            p.pool_report("web")
                .unwrap()
                .text
                .contains("failed: C is blocked")
        );
        let _ = fs::remove_dir_all(&p.root);
    }

    #[test]
    fn digest_is_published_once_and_recovered_before_delivery() {
        use crate::notice_delivery::{NoticeOutcome, NoticePreparation};
        let mut project = project("digest-recovery");
        project.bind_thread("web", "lead-session").unwrap();
        let root = project.root.clone();
        project
            .create_task(
                "web",
                &NewTask {
                    title: "Layout".into(),
                    goal: "Build the layout.".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let key = "web/layout".to_owned();
        project.set_task_status(&key, TaskStatus::Failed).unwrap();

        assert!(project.queue_pool_report("web").unwrap());
        assert!(project.file.reported_tasks.is_empty());
        let mut project = Project::load(&root).unwrap();
        assert!(project.queue_pool_report("web").unwrap());
        assert_eq!(project.notices.len(), 1);
        let NoticePreparation::Ready { ticket, text } = project
            .prepare_notice_delivery("web", "lead-session")
            .unwrap()
        else {
            panic!("expected the queued digest");
        };
        assert!(text.contains("web/layout"));
        assert!(
            project
                .settle_notice_delivery(&ticket, NoticeOutcome::Accepted)
                .unwrap()
        );

        let metadata = project.dir().join("project.json");
        let backup = project.dir().join("project-backup.json");
        fs::rename(&metadata, &backup).unwrap();
        fs::create_dir(&metadata).unwrap();
        assert!(project.queue_pool_report("web").is_err());
        assert!(project.file.reported_tasks.is_empty());
        fs::remove_dir(&metadata).unwrap();
        fs::rename(&backup, &metadata).unwrap();
        let mut project = Project::load(&root).unwrap();
        assert!(!project.queue_pool_report("web").unwrap());
        assert_eq!(project.file.reported_tasks, [key]);
        assert_eq!(project.notices.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_pool_start_releases_before_replacement_and_stale_calls_cannot_take_it() {
        let mut p = project("failed-start");
        p.create_tasks("web", &[task("A"), task("B")]).unwrap();
        let first = p.pool_worker("web", "codex").unwrap();
        let kickoff = p.prepare_pool_start(&first).unwrap().unwrap();
        assert!(kickoff.contains("Your first task, already claimed for you:"));
        assert!(kickoff.contains("`web/a`"));
        let mut stale = Project::load(&p.root).unwrap();
        assert_eq!(stale.task_of(&first).unwrap().key(), "web/a");
        assert_eq!(
            p.fail_start(&first.id, "session refused")
                .unwrap()
                .as_deref(),
            Some("web")
        );
        assert!(p.fail_start(&first.id, "same failure").unwrap().is_none());
        assert_eq!(p.notices.len(), 1);
        assert!(p.notices[0].text.contains("session refused"));
        assert_eq!(p.task("web/a").unwrap().status, TaskStatus::Open);
        let mut p = Project::load(&p.root).unwrap();
        assert!(p.stream(&first.id).unwrap().retired);
        let next = p.pool_worker("web", "claude").unwrap();
        assert_ne!(next.id, first.id);
        assert!(
            p.prepare_pool_start(&next)
                .unwrap()
                .unwrap()
                .contains("`web/a`")
        );
        stale.release_if_owner("web/a", Some(&first.id)).unwrap();
        assert!(stale.claim_from_disk(&first.id).is_err());
        assert!(
            stale
                .finish_task_for(&first.id, Some("web/a"), true, "late")
                .is_err()
        );
        assert_eq!(stale.task_of(&next).unwrap().key(), "web/a");
        fs::remove_dir_all(p.root).unwrap();
    }

    #[test]
    fn six_tasks_two_workers_one_low_water_digest_and_keyed_mcp_replay() {
        use crate::mcp::{Target, handle};
        use serde_json::json;
        let mut p = project("six");
        p.file.autopilot.tasks_per_session = 2;
        p.save().unwrap();
        p.create_tasks("web", &["A", "B", "C", "D", "E", "F"].map(task))
            .unwrap();
        let a = p.pool_worker("web", "claude").unwrap();
        p.prepare_pool_start(&a).unwrap().unwrap();
        let b = p.pool_worker("web", "claude").unwrap();
        p.prepare_pool_start(&b).unwrap().unwrap();
        assert_eq!(p.workers_needed("web"), 0);
        let target = Target {
            root: p.root.clone(),
            stream: a.id.clone(),
        };
        let finish = json!({"id":1,"method":"tools/call","params":{"name":"finish_task",
            "arguments":{"task":"web/a","status":"done","result":"A checked"}}});
        for _ in 0..2 {
            let reply = handle(&target, &finish).unwrap();
            assert!(
                reply["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("`web/c`"),
                "{reply}"
            );
        }
        p = Project::load(&p.root).unwrap();
        assert!(!p.retire_pool_worker(&a.id, Some("web/a")).unwrap());
        assert!(!p.queue_pool_report("web").unwrap());
        p.finish_task(&b.id, true, "B checked").unwrap();
        assert!(!p.queue_pool_report("web").unwrap()); // two still ready
        assert!(
            p.finish_task(&a.id, true, "C checked")
                .unwrap()
                .contains("limit reached")
        );
        assert!(p.claim_next(&a.id).unwrap().is_none());
        assert!(p.retire_pool_worker(&a.id, None).unwrap());
        let c = p.pool_worker("web", "claude").unwrap();
        assert!(
            p.prepare_pool_start(&c)
                .unwrap()
                .unwrap()
                .contains("`web/e`")
        );
        assert_eq!(p.pool("web").ready, ["web/f"]);
        for _ in 0..2 {
            assert!(p.queue_pool_report("web").unwrap());
        }
        assert_eq!(p.notices.len(), 1);
        for result in ["A checked", "B checked", "C checked"] {
            assert!(p.notices[0].text.contains(result));
        }
        p.finish_task(&b.id, true, "D checked").unwrap();
        assert!(
            p.finish_task(&c.id, true, "E checked")
                .unwrap()
                .contains("`web/f`")
        );
        p.finish_task(&c.id, true, "F checked").unwrap();
        let final_pool = Project::load(&p.root).unwrap().pool("web");
        assert!(final_pool.drained());
        assert_eq!(final_pool.done, 6);
        fs::remove_dir_all(p.root).unwrap();
    }

    #[test]
    fn concurrent_workers_never_claim_the_same_task() {
        let mut p = project("race");
        p.file.autopilot.tasks_per_session = 0;
        let tasks: Vec<NewTask> = (0..12).map(|i| task(&format!("T{i:02}"))).collect();
        p.create_tasks("web", &tasks).unwrap();
        let mut ids = Vec::new();
        for _ in 0..6 {
            let w = p.pool_worker("web", "codex").unwrap();
            ids.push(w.id.clone());
            p.file.streams.push(w);
        }
        p.save().unwrap();
        let root = p.root.clone();
        let handles: Vec<_> = ids
            .into_iter()
            .map(|id| {
                let root = root.clone();
                thread::spawn(move || {
                    let mut mine = Vec::new();
                    let mut p = Project::load(&root).unwrap();
                    while let Some(t) = p.claim_next(&id).unwrap() {
                        mine.push(t.key());
                        // This also claims the next one, which the loop gets back.
                        p.finish_task(&id, true, "ok").unwrap();
                    }
                    mine
                })
            })
            .collect();
        let mut all: Vec<String> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let p = Project::load(&root).unwrap();
        assert!(p.tasks.iter().all(|t| t.status == TaskStatus::Done));
        all.sort();
        let before = all.len();
        assert_eq!(before, 12);
        all.dedup();
        assert_eq!(before, all.len(), "no task was claimed twice");
        let _ = fs::remove_dir_all(&root);
    }
}
