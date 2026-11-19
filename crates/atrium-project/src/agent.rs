//! What an agent in a project can do, as calls rather than file edits: the
//! operations behind the `atrium` MCP server (see [`crate::mcp`]).
//!
//! Each call is made *as* one workstream (`me`), checks that its role may do
//! it, and changes only the shared files an agent could edit by hand: task
//! files, `plan.md`, its own status file and `notices.md`. It never writes
//! `project.json`, which belongs to the Atrium app.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::{
    DIR, Milestone, Project, ProjectNotice, Role, Task, TaskStatus, Workstream, first_line,
    scope_label, slug, with_status,
};

/// A task one level down, as an agent asks for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewTask {
    /// The file name; made from the title when empty.
    pub id: String,
    pub title: String,
    /// What to do and what done looks like.
    pub goal: String,
    /// `codex`, `claude`, `deepseek`, `antigravity`; empty for the asker's own.
    pub agent: String,
    pub scope: Vec<String>,
    /// `(phase title, steps)`, in order.
    pub phases: Vec<(String, Vec<String>)>,
    /// Ids of tasks of the same asker that must be done first.
    pub after: Vec<String>,
    /// 1 high, 0 normal, -1 low.
    pub priority: i32,
}

/// What ticking a step changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Ticked {
    pub step: String,
    pub done: bool,
    /// The phase it finished, when it finished one.
    pub finished_phase: Option<String>,
    /// Steps done and total, afterwards.
    pub progress: (usize, usize),
}

impl Project {
    /// The project in `root` as it is on disk, without creating or saving
    /// anything: for readers that are not the Atrium app.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(DIR).join("project.json");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("{} is not an Atrium project", root.display()))?;
        let mut project = Self {
            root: root.to_path_buf(),
            file: serde_json::from_str(&text)
                .with_context(|| format!("reading {}", path.display()))?,
            ..Self::default()
        };
        project.reload();
        Ok(project)
    }

    fn me(&self, me: &str) -> Result<&Workstream> {
        self.stream(me)
            .ok_or_else(|| anyhow!("`{me}` is not a workstream of this project"))
    }

    /// Who `me` is and where it stands, for the agent itself.
    pub fn whoami(&self, me: &str) -> Result<String> {
        let own = self.me(me)?;
        let mut out = format!(
            "You are `{}` (\"{}\"), the {} of the project \"{}\".\nScope: {}.\n",
            own.id,
            own.name,
            own.role.label().to_lowercase(),
            self.file.name,
            scope_label(&own.scope),
        );
        if let Some(parent) = own.parent.as_deref().and_then(|id| self.stream(id)) {
            out.push_str(&format!(
                "You report to `{}` (\"{}\", {}).\n",
                parent.id,
                parent.name,
                parent.role.label().to_lowercase()
            ));
        }
        if !own.goal.trim().is_empty() {
            out.push_str(&format!("\nGoal:\n{}\n", own.goal.trim()));
        }
        if let Some(task) = self.task_of(own) {
            out.push_str(&format!(
                "\nYour task: `{}` ({}), status {}.\n",
                task.key(),
                task.rel_path(),
                task.status.word()
            ));
            if !task.phases.is_empty() {
                out.push_str(&roadmap_text(task));
            }
        }
        match own.role.child() {
            Some(child) => {
                let mine: Vec<&Task> = self.tasks.iter().filter(|t| t.owner == own.id).collect();
                out.push_str(&format!(
                    "\nYou may ask for {}s with `create_task`{}.\n",
                    child.label().to_lowercase(),
                    if own.role == Role::Lead {
                        format!(", at most {} running at once", self.file.max_workers.max(1))
                    } else {
                        String::new()
                    }
                ));
                if !mine.is_empty() {
                    out.push_str("Your tasks:\n");
                    for task in mine {
                        out.push_str(&self.task_line(task));
                    }
                }
            }
            None => out.push_str("\nYou cannot start agents of your own.\n"),
        }
        let unread = self.unread(me).len();
        if unread > 0 {
            out.push_str(&format!(
                "\n{unread} notice(s) for you that you have not been handed yet: see `read_notices`.\n"
            ));
        }
        Ok(out)
    }

    /// One line about a task, for lists.
    fn task_line(&self, task: &Task) -> String {
        let mut line = format!("- `{}` {} — {}", task.key(), task.title, task.status.word());
        if let Some(agent) = &task.agent {
            line.push_str(&format!(", agent {agent}"));
        }
        if let Some((done, total)) = task.progress() {
            line.push_str(&format!(", {done}/{total} steps"));
        }
        if let Some(phase) = task.current_phase().map(|i| &task.phases[i]) {
            line.push_str(&format!(", on \"{}\"", phase.title));
        }
        if self.awaits_approval(task) {
            line.push_str(", waiting for the user's approval");
        }
        if let Some(stream) = self.stream_for_task(&task.key()) {
            line.push_str(&format!(", agent `{}`", stream.id));
        }
        if let Some(result) = &task.result {
            line.push_str(&format!("\n  Result: {}", first_line(result)));
        }
        line.push('\n');
        line
    }

    /// The tasks `me` asked for, or every task with `all`.
    pub fn list_tasks(&self, me: &str, all: bool) -> Result<String> {
        self.me(me)?;
        let tasks: Vec<&Task> = self.tasks.iter().filter(|t| all || t.owner == me).collect();
        if tasks.is_empty() {
            return Ok(if all {
                "No tasks in this project yet.".into()
            } else {
                "You have not asked for any tasks.".into()
            });
        }
        Ok(tasks.iter().map(|t| self.task_line(t)).collect())
    }

    /// Writes a task file under `me`; Atrium starts an agent one level down
    /// for it. Returns what will happen, for the agent.
    pub fn create_task(&mut self, me: &str, new: &NewTask) -> Result<String> {
        let own = self.me(me)?.clone();
        let Some(child) = own.role.child() else {
            bail!(
                "Sub-agents cannot start agents. Do the work yourself, or say what is needed in your result."
            );
        };
        if new.title.trim().is_empty() {
            bail!("A task needs a title.");
        }
        if new.goal.trim().is_empty() {
            bail!("A task needs a goal: what to do and what done looks like.");
        }
        if child == Role::Lead && new.phases.iter().all(|(_, steps)| steps.is_empty()) {
            bail!("A lead needs a roadmap: give `phases`, each with a title and a few steps.");
        }
        let key = self.write_task(me, new)?;
        let task = self.task(&key).expect("just written");
        let when = if child == Role::Worker {
            "It goes into your task pool; your sub-agents claim tasks from it one after another."
                .to_string()
        } else if self.awaits_approval(task) {
            "Atrium starts it once the user approves it.".to_string()
        } else {
            "Atrium starts it now.".to_string()
        };
        Ok(format!(
            "Wrote `{}`: a {} \"{}\". {when} You are told when it finishes.",
            task.rel_path(),
            child.label().to_lowercase(),
            task.title,
        ))
    }

    /// Several tasks at once into `me`'s pool (a lead's), in order. All are
    /// checked before any is written.
    pub fn create_tasks(&mut self, me: &str, tasks: &[NewTask]) -> Result<String> {
        if self.me(me)?.role != Role::Lead {
            bail!("Only leads fill a task pool; the controller asks for leads with `create_task`.");
        }
        if tasks.is_empty() {
            bail!("Give at least one task.");
        }
        for (i, new) in tasks.iter().enumerate() {
            if new.title.trim().is_empty() || new.goal.trim().is_empty() {
                bail!(
                    "Task {} needs a title and a goal; nothing was written.",
                    i + 1
                );
            }
        }
        let mut keys = Vec::new();
        for new in tasks {
            keys.push(self.write_task(me, new)?);
        }
        let pool = self.pool(me);
        Ok(format!(
            "Added {} task(s) to your pool: {}. Pool now: {} ready, {} waiting on others, {} in progress. Sub-agents start and claim them on their own (at most {} at once); you are woken when the pool runs low, with their results.",
            keys.len(),
            keys.iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", "),
            pool.ready.len(),
            pool.waiting.len(),
            pool.running.len(),
            self.file.max_workers.max(1),
        ))
    }

    /// Writes one task file under `me`; returns its key.
    fn write_task(&mut self, me: &str, new: &NewTask) -> Result<String> {
        let text = render_task(new);
        let key = if new.id.trim().is_empty() {
            self.add_task(me, &new.title, &text)?
        } else {
            let id = slug(&new.id).ok_or_else(|| anyhow!("`{}` is not a usable id", new.id))?;
            let path = self.dir().join("tasks").join(me).join(format!("{id}.md"));
            if path.exists() {
                bail!("{DIR}/tasks/{me}/{id}.md already exists; pick another id.");
            }
            fs::create_dir_all(path.parent().expect("task dir"))?;
            fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            self.stamp = None;
            self.reload();
            format!("{me}/{id}")
        };
        Ok(key)
    }

    /// Ticks (or unticks) a step of `me`'s own roadmap. `step` is its text,
    /// a unique part of it, or `phase.step` numbered from 1 (`2.1`). A phase
    /// it finishes is posted to `me`'s parent.
    pub fn check_step(&mut self, me: &str, step: &str, done: bool) -> Result<Ticked> {
        let own = self.me(me)?.clone();
        let task = self
            .task_of(&own)
            .cloned()
            .ok_or_else(|| anyhow!("You were not started for a task, so you have no roadmap."))?;
        let _lock = crate::pool::TaskLock::take(self)?;
        self.stamp = None;
        self.reload();
        let task = self
            .task_of(&own)
            .filter(|current| current.key() == task.key())
            .context("Your task changed before the roadmap step could be saved.")?
            .clone();
        let index = find_step(&task, step)?;
        let (phase_ix, _) = locate(&task, index);
        let was_done = task.phases[phase_ix].done();
        let path = self.root.join(task.rel_path());
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        crate::atomic_write(&path, set_step(&text, index, done)?.as_bytes())?;
        self.stamp = None;
        self.reload();
        let task = self.task(&task.key()).expect("still there").clone();
        let phase = &task.phases[phase_ix];
        let finished_phase = (!was_done && phase.done()).then(|| phase.title.clone());
        if let (Some(title), Some(parent)) = (&finished_phase, &own.parent) {
            let phases_done = task.phases.iter().filter(|p| p.done()).count();
            self.post(
                me,
                vec![parent.clone()],
                &format!(
                    "Phase \"{title}\" done ({phases_done}/{} phases of {}).",
                    task.phases.len(),
                    task.rel_path()
                ),
            )?;
        }
        Ok(Ticked {
            step: task.roadmap[index].text.clone(),
            done,
            finished_phase,
            progress: task.progress().unwrap_or_default(),
        })
    }

    /// Ends `me`'s task: writes `result` under `## Result` and sets the
    /// status. Atrium then tells the parent.
    pub fn finish_task(&mut self, me: &str, done: bool, result: &str) -> Result<String> {
        let key = self.task_of(self.me(me)?).map(Task::key);
        let lock = crate::pool::TaskLock::take(self)?;
        self.stamp = None;
        self.reload();
        self.finish_locked(me, key.as_deref(), done, result, &lock)
    }

    /// A keyed MCP retry must never apply the previous result to the worker's next task.
    pub(crate) fn finish_task_for(
        &mut self,
        me: &str,
        key: Option<&str>,
        done: bool,
        result: &str,
    ) -> Result<String> {
        let observed = self.task_of(self.me(me)?).map(Task::key);
        let lock = crate::pool::TaskLock::take(self)?;
        *self = Self::load(&self.root)?;
        self.finish_locked(me, key.or(observed.as_deref()), done, result, &lock)
    }

    fn finish_locked(
        &mut self,
        me: &str,
        key: Option<&str>,
        done: bool,
        result: &str,
        lock: &crate::pool::TaskLock,
    ) -> Result<String> {
        let own = self.me(me)?.clone();
        let key = key.context(
            "You were not started for a task; post a notice to say you are done instead.",
        )?;
        if result.trim().is_empty() {
            bail!("Say what you did, how you checked it and what is left open.");
        }
        let pooled = own.role == Role::Worker && own.task.is_none();
        let task = self.task(key).context("Task no longer exists.")?.clone();
        let status = if done {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        if pooled && task.status.finished() && task.claimed_by.as_deref() == Some(me) {
            anyhow::ensure!(
                task.status == status && task.result.as_deref() == Some(result.trim()),
                "A different result is already recorded for {key}."
            );
            return Ok(match self.task_of(&own) {
                Some(held) => format!(
                    "Result already recorded for `{key}`. Your current task:\n\n{}",
                    task_brief(held)
                ),
                None => format!("Result already recorded for `{key}`. You have no current task."),
            });
        }
        anyhow::ensure!(!pooled || !own.retired, "This worker is retired.");
        anyhow::ensure!(
            self.task_of(&own)
                .is_some_and(|current| current.key() == key),
            "You no longer hold {key}."
        );
        let path = self.root.join(task.rel_path());
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let text = with_status(&with_result(&text, result), status);
        crate::atomic_write(&path, text.as_bytes())?;
        self.stamp = None;
        self.reload();
        if pooled {
            let done = format!("Marked `{}` {}.", task.rel_path(), status.word());
            return Ok(match self.claim_next_locked(me, lock)? {
                Some(next) => format!(
                    "{done} Your next task, claimed for you:\n\n{}\n\nDo it now, then `finish_task` again.",
                    task_brief(&next)
                ),
                None if self.pool_session_full(me) => {
                    format!("{done} Session task limit reached. Stop here for a fresh worker.")
                }
                None if self.worktree_dir(me).join(".git").is_file()
                    && !self.claimable(&task.owner).is_empty() =>
                {
                    format!(
                        "{done} Stop here while Atrium integrates your work and prepares your next checkout."
                    )
                }
                None => format!(
                    "{done} The pool is empty: nothing left to claim. Stop here; you are woken if new tasks arrive."
                ),
            });
        }
        Ok(format!(
            "Marked `{}` {}. Atrium tells {}; you can stop.",
            task.rel_path(),
            status.word(),
            own.parent
                .as_deref()
                .map_or("The user".to_string(), |p| format!("`{p}`"))
        ))
    }

    /// Posts a notice from `me`. `to` holds workstream ids, `all` or `you`
    /// (the user).
    pub fn notify(&mut self, me: &str, to: &[String], text: &str) -> Result<String> {
        self.me(me)?;
        if text.trim().is_empty() {
            bail!("The notice is empty.");
        }
        let to: Vec<String> = to.iter().map(|t| t.trim().to_string()).collect();
        if to.is_empty() {
            bail!("Say who it is for: workstream ids, `all` or `you`.");
        }
        let unknown: Vec<&String> = to
            .iter()
            .filter(|t| !matches!(t.as_str(), "all" | crate::YOU) && self.stream(t).is_none())
            .collect();
        if !unknown.is_empty() {
            let known: Vec<String> = self
                .file
                .streams
                .iter()
                .map(|s| format!("`{}`", s.id))
                .collect();
            bail!(
                "No workstream {}. Known: {}, `all`, `you`.",
                unknown
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                known.join(", ")
            );
        }
        self.post(me, to.clone(), text)?;
        Ok(format!("Posted to {}.", to.join(", ")))
    }

    /// Notices for `me`: those not handed yet, or with `recent` the last 20
    /// addressed to it whether handed or not.
    pub fn notices_for(&self, me: &str, recent: bool) -> Result<String> {
        self.me(me)?;
        let notices: Vec<&ProjectNotice> = if recent {
            let mine: Vec<&ProjectNotice> = self
                .notices
                .iter()
                .filter(|n| n.is_for(me) || n.from == me)
                .collect();
            mine[mine.len().saturating_sub(20)..].to_vec()
        } else {
            self.unread(me)
        };
        if notices.is_empty() {
            return Ok("No new notices.".into());
        }
        Ok(notices
            .iter()
            .map(|n| {
                format!(
                    "- {} · {} → {}: {}\n",
                    n.at,
                    n.from,
                    n.to.join(", "),
                    n.text
                )
            })
            .collect())
    }

    /// Replaces `.atrium/plan.md`; the controller's alone.
    pub fn write_plan(&mut self, me: &str, text: &str) -> Result<String> {
        if self.me(me)?.role != Role::Controller {
            bail!("Only the controller keeps the plan; post it a notice instead.");
        }
        let path = self.dir().join("plan.md");
        fs::write(&path, format!("{}\n", text.trim_end()))
            .with_context(|| format!("writing {}", path.display()))?;
        self.stamp = None;
        self.reload();
        Ok(format!("Wrote {DIR}/plan.md."))
    }

    /// Replaces `me`'s status file.
    pub fn write_status(&mut self, me: &str, text: &str) -> Result<String> {
        self.me(me)?;
        let path = self.dir().join("status").join(format!("{me}.md"));
        fs::create_dir_all(path.parent().expect("status dir"))?;
        fs::write(&path, format!("{}\n", text.trim_end()))
            .with_context(|| format!("writing {}", path.display()))?;
        self.stamp = None;
        self.reload();
        Ok(format!("Wrote {DIR}/status/{me}.md."))
    }
}

/// A claimed task as a worker is handed it.
pub fn task_brief(task: &Task) -> String {
    let mut out = format!(
        "`{}` ({}): {}\nScope: {}.\n\n{}",
        task.key(),
        task.rel_path(),
        task.title,
        scope_label(&task.scope),
        task.body.trim()
    );
    if !task.phases.is_empty() {
        out.push_str("\n\n");
        out.push_str(&roadmap_text(task));
    }
    out
}

/// A task file with a phased roadmap.
pub fn render_task(new: &NewTask) -> String {
    let mut text = format!("# {}\nstatus: open\n", new.title.trim());
    if !new.agent.trim().is_empty() {
        text.push_str(&format!("agent: {}\n", new.agent.trim().to_lowercase()));
    }
    if !new.scope.is_empty() {
        text.push_str(&format!("scope: {}\n", new.scope.join(", ")));
    }
    if !new.after.is_empty() {
        text.push_str(&format!("after: {}\n", new.after.join(", ")));
    }
    match new.priority {
        p if p > 0 => text.push_str("priority: high\n"),
        p if p < 0 => text.push_str("priority: low\n"),
        _ => {}
    }
    text.push('\n');
    text.push_str(new.goal.trim());
    text.push('\n');
    let phases: Vec<&(String, Vec<String>)> = new
        .phases
        .iter()
        .filter(|(_, steps)| !steps.is_empty())
        .collect();
    if !phases.is_empty() {
        text.push_str("\n## Roadmap\n");
        for (i, (title, steps)) in phases.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let title = title.trim();
            if !title.is_empty() {
                text.push_str(&format!("### {title}\n"));
            }
            for step in steps {
                text.push_str(&format!("- [ ] {}\n", step.trim()));
            }
        }
    }
    text
}

fn roadmap_text(task: &Task) -> String {
    let current = task.current_phase();
    let mut out = String::from("Roadmap:\n");
    for (i, phase) in task.phases.iter().enumerate() {
        let (done, total) = phase.progress();
        let mark = match current {
            Some(c) if c == i => " ← current",
            _ if phase.done() => " ✓",
            _ => "",
        };
        out.push_str(&format!(
            "{}. {} ({done}/{total}){mark}\n",
            i + 1,
            phase.title
        ));
        for (j, step) in phase.steps.iter().enumerate() {
            out.push_str(&format!(
                "   {}.{} [{}] {}\n",
                i + 1,
                j + 1,
                if step.done { "x" } else { " " },
                step.text
            ));
        }
    }
    out
}

/// Phase and step-in-phase of the `index`th step.
fn locate(task: &Task, index: usize) -> (usize, usize) {
    let mut left = index;
    for (i, phase) in task.phases.iter().enumerate() {
        if left < phase.steps.len() {
            return (i, left);
        }
        left -= phase.steps.len();
    }
    (task.phases.len().saturating_sub(1), 0)
}

/// Which step of the roadmap `query` names.
fn find_step(task: &Task, query: &str) -> Result<usize> {
    let query = query.trim();
    if task.roadmap.is_empty() {
        bail!("Your task has no roadmap.");
    }
    if let Some((p, s)) = query.split_once('.')
        && let (Ok(p), Ok(s)) = (p.trim().parse::<usize>(), s.trim().parse::<usize>())
    {
        let phase = task
            .phases
            .get(p.wrapping_sub(1))
            .ok_or_else(|| anyhow!("There is no phase {p}."))?;
        if s == 0 || s > phase.steps.len() {
            bail!("Phase {p} has {} steps.", phase.steps.len());
        }
        let before: usize = task.phases[..p - 1].iter().map(|ph| ph.steps.len()).sum();
        return Ok(before + s - 1);
    }
    let wanted = query.to_lowercase();
    if let Some(i) = task
        .roadmap
        .iter()
        .position(|m: &Milestone| m.text.to_lowercase() == wanted)
    {
        return Ok(i);
    }
    let hits: Vec<usize> = task
        .roadmap
        .iter()
        .enumerate()
        .filter(|(_, m)| m.text.to_lowercase().contains(&wanted))
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => bail!(
            "No step matches \"{query}\". Steps:\n{}",
            roadmap_text(task)
        ),
        _ => bail!(
            "\"{query}\" matches {} steps; give its number, such as 2.1.",
            hits.len()
        ),
    }
}

/// `text` with the `index`th roadmap step marked `done`, read the way
/// [`Task::parse`] counts steps.
fn set_step(text: &str, index: usize, done: bool) -> Result<String> {
    let mut in_roadmap = false;
    let mut n = 0;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for line in &mut lines {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            in_roadmap = matches!(
                heading.trim().to_lowercase().as_str(),
                "roadmap" | "milestones"
            );
            continue;
        }
        if !in_roadmap || trimmed.starts_with("### ") {
            continue;
        }
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        if item.trim().is_empty() {
            continue;
        }
        if n == index {
            let rest = item
                .strip_prefix("[ ]")
                .or_else(|| item.strip_prefix("[x]"))
                .or_else(|| item.strip_prefix("[X]"))
                .unwrap_or(item)
                .trim();
            let indent = &line[..line.len() - line.trim_start().len()];
            *line = format!("{indent}- [{}] {rest}", if done { "x" } else { " " });
            let mut out = lines.join("\n");
            out.push('\n');
            return Ok(out);
        }
        n += 1;
    }
    bail!("The roadmap changed on disk; read it again.")
}

/// `text` with its `## Result` section replaced by `result`.
fn with_result(text: &str, result: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if let Some(heading) = line.trim().strip_prefix("## ") {
            skipping = matches!(heading.trim().to_lowercase().as_str(), "result" | "report");
            if skipping {
                continue;
            }
        }
        if !skipping {
            out.push(line);
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    format!("{}\n\n## Result\n{}\n", out.join("\n"), result.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> Project {
        let root = std::env::temp_dir().join(format!("atrium-agent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        project.file.streams.push(Workstream {
            id: "boss".into(),
            name: "Controller".into(),
            role: Role::Controller,
            ..Default::default()
        });
        project.save().unwrap();
        project
    }

    #[test]
    fn a_controller_hands_a_lead_a_phased_roadmap_and_the_lead_ticks_it() {
        let mut project = temp_project("roadmap");
        let said = project
            .create_task(
                "boss",
                &NewTask {
                    title: "Web app".into(),
                    goal: "Build the web front end.".into(),
                    agent: "Claude".into(),
                    scope: vec!["web/**".into()],
                    phases: vec![
                        ("Skeleton".into(), vec!["Routes".into(), "Layout".into()]),
                        ("Editing".into(), vec!["Timeline".into()]),
                    ],
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(said.contains("starts it now"), "{said}");
        let task = project.task("boss/web-app").unwrap().clone();
        assert_eq!(task.agent.as_deref(), Some("claude"));
        assert_eq!(task.phases.len(), 2);

        // The scheduler starts the lead.
        let lead = project.stream_from_task(&task, "claude").unwrap();
        project.file.streams.push(lead.clone());
        project.save().unwrap();
        let mut project = Project::load(&project.root).unwrap();

        assert!(project.whoami(&lead.id).unwrap().contains("1.2 [ ] Layout"));
        let ticked = project.check_step(&lead.id, "routes", true).unwrap();
        assert_eq!(ticked.finished_phase, None);
        assert_eq!(ticked.progress, (1, 3));
        let ticked = project.check_step(&lead.id, "1.2", true).unwrap();
        assert_eq!(ticked.finished_phase.as_deref(), Some("Skeleton"));
        let last = project.notices.last().unwrap();
        assert_eq!(
            (last.from.as_str(), last.to.clone()),
            (lead.id.as_str(), vec!["boss".to_string()])
        );
        assert!(last.text.contains("\"Skeleton\" done (1/2"));
        // Unticking does not post again.
        let before = project.notices.len();
        project.check_step(&lead.id, "Layout", false).unwrap();
        assert_eq!(project.notices.len(), before);
        assert!(project.check_step(&lead.id, "3.1", true).is_err());

        let said = project
            .finish_task(&lead.id, true, "Done: routes and layout.")
            .unwrap();
        assert!(said.contains("`boss`"), "{said}");
        let task = project.task("boss/web-app").unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(task.result.as_deref(), Some("Done: routes and layout."));
        // Finishing again replaces the result rather than adding another.
        project
            .finish_task(&lead.id, false, "Actually blocked.")
            .unwrap();
        let task = project.task("boss/web-app").unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.result.as_deref(), Some("Actually blocked."));
        let _ = fs::remove_dir_all(&project.root);
    }

    #[test]
    fn roles_limit_what_an_agent_may_do() {
        let mut project = temp_project("roles");
        project.file.streams.push(Workstream {
            id: "web".into(),
            role: Role::Lead,
            parent: Some("boss".into()),
            ..Default::default()
        });
        project.file.streams.push(Workstream {
            id: "web-a".into(),
            role: Role::Worker,
            parent: Some("web".into()),
            ..Default::default()
        });
        project.save().unwrap();
        let worker_task = NewTask {
            title: "More".into(),
            goal: "x".into(),
            ..Default::default()
        };
        assert!(project.create_task("web-a", &worker_task).is_err());
        // A lead without a roadmap is refused; a worker task needs none.
        assert!(project.create_task("boss", &worker_task).is_err());
        assert!(project.create_task("web", &worker_task).is_ok());
        assert!(project.write_plan("web", "# Plan").is_err());
        assert!(project.write_plan("boss", "# Plan").is_ok());
        assert_eq!(project.plan.trim(), "# Plan");
        assert!(project.notify("web", &["nobody".into()], "hi").is_err());
        assert!(project.notify("web", &["boss".into()], "hi").is_ok());
        assert!(
            project
                .notices_for("boss", false)
                .unwrap()
                .contains("web → boss: hi")
        );
        assert!(project.whoami("ghost").is_err());
        // Nothing here wrote project.json.
        let file: crate::ProjectFile =
            serde_json::from_str(&fs::read_to_string(project.dir().join("project.json")).unwrap())
                .unwrap();
        assert_eq!(file.streams.len(), 3);
        let _ = fs::remove_dir_all(&project.root);
    }
}
