//! Keeping a project going without the user, after Paperclip's heartbeats
//! and liveness recovery: who still has work, and what to tell an agent when
//! Atrium wakes it to continue. *When* to wake (timers, stall detection,
//! retries, restarts) is the scheduler's: see `docs/autopilot-implementation.md`.

use crate::{Project, Role, TaskStatus, Workstream, first_line};

/// Why Atrium wakes an agent that nobody messaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Its turn ended with work left (liveness recovery).
    TurnEnded,
    /// The periodic check-in of an idle agent with work left.
    Heartbeat,
    /// Its turn produced nothing for too long and was stopped.
    Stalled,
    /// Its last turn failed (rate limit, overload, crash); this is attempt `n`.
    Retry(u32),
    /// Atrium restarted, or the agent's process did, and its session was resumed.
    Restarted,
}

impl Wake {
    /// The reason as the scheduler logs it and the UI shows it.
    pub fn label(self) -> &'static str {
        match self {
            Wake::TurnEnded => "continue",
            Wake::Heartbeat => "heartbeat",
            Wake::Stalled => "stalled",
            Wake::Retry(_) => "retry",
            Wake::Restarted => "restarted",
        }
    }
}

impl Project {
    /// Whether `stream` still has something to do on its own, so it should
    /// not just sit idle: a lead with an unfinished task or roadmap, a pool
    /// worker with a claimable task, a controller with leads still working.
    pub fn has_work(&self, stream: &Workstream) -> bool {
        if stream.retired {
            return false;
        }
        let task_open = self.task_of(stream).is_some_and(|t| !t.status.finished());
        match stream.role {
            Role::Controller => {
                self.file.streams.iter().any(|s| {
                    s.role == Role::Lead
                        && s.parent.as_deref() == Some(stream.id.as_str())
                        && self.task_of(s).is_some_and(|t| !t.status.finished())
                }) || self.startable().iter().any(|t| t.owner == stream.id)
            }
            Role::Lead => match stream.task {
                Some(_) => task_open,
                // A lead the user added has no task: its goal never ends by
                // itself, so it has work while its pool does.
                None => !self.pool(&stream.id).drained(),
            },
            Role::Worker if stream.task.is_none() => {
                task_open
                    || stream
                        .parent
                        .as_deref()
                        .is_some_and(|lead| !self.claimable(lead).is_empty())
            }
            Role::Worker => task_open,
        }
    }

    /// What Atrium tells `stream` when it wakes it to keep going.
    pub fn continuation(&self, stream: &Workstream, why: Wake) -> String {
        let opening = match why {
            Wake::TurnEnded => "Your turn ended but your work is not finished. Continue.".to_string(),
            Wake::Heartbeat => "Check-in: you still have unfinished work. Continue where you left off.".to_string(),
            Wake::Stalled => "Your last turn stopped producing output and was interrupted. Check what state you left things in (files, running commands), then continue.".to_string(),
            Wake::Retry(n) => format!("Your last turn failed (attempt {n}). Continue where you left off; if the same step keeps failing, take another route and say so in a notice."),
            Wake::Restarted => "Atrium restarted and resumed your session. Re-read your task file and status file, then continue.".to_string(),
        };
        let mut text = format!("[Atrium] {opening}\n");
        match stream.role {
            Role::Controller => {
                let leads: Vec<String> = self
                    .file
                    .streams
                    .iter()
                    .filter(|s| {
                        s.role == Role::Lead && s.parent.as_deref() == Some(stream.id.as_str())
                    })
                    .map(|s| {
                        let state = match self.task_of(s) {
                            Some(t) => match (t.status, t.current_phase()) {
                                (TaskStatus::Done, _) => "done".to_string(),
                                (TaskStatus::Failed, _) => "failed".to_string(),
                                (_, Some(i)) => format!(
                                    "phase {}/{} \"{}\"",
                                    i + 1,
                                    t.phases.len(),
                                    t.phases[i].title
                                ),
                                _ => t.status.word().to_string(),
                            },
                            None => "no task".to_string(),
                        };
                        format!("- `{}`: {state}", s.id)
                    })
                    .collect();
                text.push_str("\nLeads:\n");
                text.push_str(&leads.join("\n"));
                text.push_str("\n\nCheck progress against the plan; re-align leads with notices if needed; then stop. Do not do their work.");
            }
            Role::Lead => {
                if let Some(task) = self.task_of(stream)
                    && let Some(i) = task.current_phase()
                {
                    let phase = &task.phases[i];
                    let (done, total) = phase.progress();
                    text.push_str(&format!(
                        "\nYou are on phase {}/{} \"{}\" ({done}/{total} steps).",
                        i + 1,
                        task.phases.len(),
                        phase.title
                    ));
                    if let Some(step) = phase.steps.iter().find(|m| !m.done) {
                        text.push_str(&format!(" Next step: {}.", step.text));
                    }
                }
                let pool = self.pool(&stream.id);
                text.push_str(&format!(
                    "\nYour pool: {} ready, {} waiting, {} in progress, {} done, {} failed.",
                    pool.ready.len(),
                    pool.waiting.len(),
                    pool.running.len(),
                    pool.done,
                    pool.failed
                ));
                if pool.drained() {
                    text.push_str(" It is empty: add the next batch with `create_tasks`, or do the step yourself.");
                }
            }
            Role::Worker => match self.task_of(stream) {
                Some(task) => text.push_str(&format!(
                    "\nYou hold `{}` ({}): {}. Finish it with `finish_task`.",
                    task.key(),
                    task.rel_path(),
                    first_line(&task.title)
                )),
                None => text.push_str("\nYou hold no task: call `claim_next`."),
            },
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::NewTask;
    use std::fs;

    #[test]
    fn who_has_work_and_what_they_are_told() {
        let root = std::env::temp_dir().join(format!("atrium-autopilot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut p = Project::open(&root).unwrap();
        p.file.streams.push(Workstream {
            id: "boss".into(),
            role: Role::Controller,
            ..Default::default()
        });
        p.save().unwrap();
        let boss = p.stream("boss").unwrap().clone();
        assert!(!p.has_work(&boss), "nothing asked for yet");
        p.create_task(
            "boss",
            &NewTask {
                title: "Web".into(),
                goal: "Build it.".into(),
                phases: vec![("Shell".into(), vec!["Layout".into(), "Routes".into()])],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(p.has_work(&boss), "a lead to start");
        let task = p.task("boss/web").unwrap().clone();
        let lead = p.stream_from_task(&task, "codex").unwrap();
        p.file.streams.push(lead.clone());
        p.set_task_status("boss/web", TaskStatus::Running).unwrap();
        p.check_step("web", "Layout", true).unwrap();
        assert!(p.has_work(&lead));
        let text = p.continuation(&lead, Wake::TurnEnded);
        assert!(
            text.contains("phase 1/1 \"Shell\" (1/2 steps)") && text.contains("Next step: Routes"),
            "{text}"
        );
        assert!(
            p.continuation(&boss, Wake::Heartbeat)
                .contains("`web`: phase 1/1")
        );

        // A pool worker has work while there is something to claim.
        p.create_tasks(
            "web",
            &[NewTask {
                title: "A".into(),
                goal: "a".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        let worker = p.pool_worker("web", "codex").unwrap();
        p.file.streams.push(worker.clone());
        p.save().unwrap();
        assert!(p.has_work(&worker));
        assert!(
            p.continuation(&worker, Wake::Retry(2))
                .contains("call `claim_next`")
        );
        p.claim_next(&worker.id).unwrap();
        assert!(
            p.continuation(&worker, Wake::Stalled)
                .contains("You hold `web/a`")
        );
        p.finish_task(&worker.id, true, "done").unwrap();
        assert!(!p.has_work(&worker), "the pool is empty");

        // Unattended projects say so in every kickoff.
        assert!(!p.kickoff(&lead).contains("runs unattended"));
        p.file.autopilot.on = true;
        assert!(p.kickoff(&lead).contains("runs unattended"));
        let _ = fs::remove_dir_all(&root);
    }
}
