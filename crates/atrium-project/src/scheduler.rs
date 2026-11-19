//! A stream's unattended clock. The workspace supplies a monotonic time and
//! only dispatches a due wake after checking the live thread and its budget.
use std::{
    fs, io,
    time::{Duration, Instant},
};

use crate::{ATRIUM, Autopilot, Project, Role, YOU, atomic_write, autopilot::Wake};
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct StreamClock {
    due: Option<(Instant, Wake)>,
    last_event: Option<Instant>,
    idle_since: Option<Instant>,
    failures: u32,
    resume_attempt: bool,
    stalled_task: Option<String>,
    stalls: u32,
    interrupting: bool,
    paused: bool,
    phase_at_start: Option<usize>,
}

pub fn retry_delay(attempt: u32) -> Duration {
    minutes((1_u32 << attempt.saturating_sub(1).min(5)).min(30))
}

fn minutes(n: u32) -> Duration {
    Duration::from_secs(u64::from(n) * 60)
}

impl StreamClock {
    pub fn restarted(&mut self, at: Instant) {
        self.due = Some((at, Wake::Restarted));
    }

    pub fn started(&mut self, now: Instant, phase: Option<usize>) {
        self.last_event = Some(now);
        self.due = None;
        self.resume_attempt = false;
        self.idle_since = None;
        self.interrupting = false;
        self.paused = false;
        self.phase_at_start = phase;
    }

    pub fn activity(&mut self, now: Instant) {
        self.last_event = Some(now);
    }

    pub fn completed(
        &mut self,
        now: Instant,
        config: &Autopilot,
        role: Role,
        waiting_on_pool: bool,
        phase: Option<usize>,
    ) {
        self.idle_since = Some(now);
        self.failures = 0;
        if self.finish_interrupt(now) {
            return;
        }
        self.stalls = 0;
        self.paused = false;
        let (delay, why) = match role {
            Role::Controller => (minutes(config.controller_heartbeat_min), Wake::Heartbeat),
            Role::Lead if waiting_on_pool && phase == self.phase_at_start => {
                (minutes(config.lead_heartbeat_min), Wake::Heartbeat)
            }
            _ => (
                Duration::from_secs(config.continue_after_secs.into()),
                Wake::TurnEnded,
            ),
        };
        self.due = Some((now + delay, why));
    }

    fn finish_interrupt(&mut self, now: Instant) -> bool {
        if std::mem::take(&mut self.interrupting) {
            self.due = Some((now, Wake::Stalled));
            true
        } else {
            false
        }
    }

    /// A manual stop is a hold until the next user turn or off/on toggle.
    pub fn interrupted(&mut self, now: Instant) {
        if !self.finish_interrupt(now) {
            self.due = None;
            self.paused = true;
        }
    }

    pub fn failed(&mut self, now: Instant) {
        if self.finish_interrupt(now)
            || (!std::mem::take(&mut self.resume_attempt)
                && matches!(self.due, Some((_, Wake::Retry(_)))))
        {
            return;
        }
        self.failures = self.failures.saturating_add(1);
        self.stalls = 0;
        self.paused = false;
        self.due = Some((now + retry_delay(self.failures), Wake::Retry(self.failures)));
    }

    pub fn wake_due(
        &mut self,
        now: Instant,
        config: &Autopilot,
        role: Role,
        has_work: bool,
    ) -> Option<Wake> {
        if !config.on || self.paused || self.interrupting || !has_work {
            return None;
        }
        let idle = *self.idle_since.get_or_insert(now);
        let (at, why) = match self.due {
            Some(pending) => pending,
            None => match role {
                Role::Controller => (
                    idle + minutes(config.controller_heartbeat_min),
                    Wake::Heartbeat,
                ),
                Role::Lead => (idle + minutes(config.lead_heartbeat_min), Wake::Heartbeat),
                Role::Worker => return None,
            },
        };
        (now >= at).then_some(why)
    }

    pub fn resuming(&mut self) {
        // Keep the retry reason for a successful resume; a failed resume is a fresh attempt.
        self.resume_attempt = true;
    }

    /// New text may expedite a liveness wake, but never a retry or a restart.
    pub fn may_wake(&self, now: Instant) -> bool {
        !self.paused
            && !self.interrupting
            && !matches!(self.due, Some((at, Wake::Retry(_) | Wake::Restarted)) if now < at)
    }

    pub fn poll_stall(
        &mut self,
        now: Instant,
        config: &Autopilot,
        task: Option<String>,
    ) -> Option<u32> {
        let last = self.last_event.get_or_insert(now);
        if !config.on || now.duration_since(*last) < minutes(config.stall_min) {
            return None;
        }
        *last = now;
        // A lost interrupt response can be retried without counting the same turn twice.
        if !self.interrupting {
            if task != self.stalled_task {
                self.stalls = 0;
                self.stalled_task = task;
            }
            self.stalls = self.stalls.saturating_add(1);
            self.interrupting = true;
            return Some(self.stalls);
        }
        Some(0)
    }
}

#[derive(Serialize, Deserialize)]
struct Day {
    date: String,
    turns: u32,
}

/// Persist the reservation before submitting the RPC. A crash between the two
/// may consume one slot but cannot dispatch a turn beyond the limit.
impl Project {
    pub fn reserve_scheduler_turn(&mut self, date: &str) -> anyhow::Result<bool> {
        if !self.file.autopilot.on {
            return Ok(true);
        }
        let _lock = crate::pool::TaskLock::take(self)?;
        let path = self.dir().join("autopilot-turns.json");
        let used = match fs::read(&path) {
            Ok(bytes) => {
                let day: Day = serde_json::from_slice(&bytes)?;
                if day.date == date { day.turns } else { 0 }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        let limit = self.file.autopilot.daily_turns;
        if limit > 0 && used >= limit {
            let text = format!(
                "Daily unattended turn limit reached ({date}). Scheduling resumes at local midnight."
            );
            if !self
                .notices
                .iter()
                .any(|n| n.from == ATRIUM && n.text == text)
            {
                self.post(ATRIUM, vec![YOU.into()], &text)?;
            }
            return Ok(false);
        }
        let day = Day {
            date: date.to_owned(),
            turns: used.saturating_add(1),
        };
        atomic_write(&path, &serde_json::to_vec(&day)?)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_cancel_and_pool_phase_selects_heartbeat() {
        let c = Autopilot {
            on: true,
            ..Default::default()
        };
        let t = Instant::now();
        let mut s = StreamClock::default();
        s.started(t, Some(0));
        s.completed(t, &c, Role::Lead, true, Some(1));
        let due = t + Duration::from_secs(c.continue_after_secs.into());
        assert_eq!(
            s.wake_due(due - Duration::from_secs(1), &c, Role::Lead, true),
            None
        );
        assert_eq!(s.wake_due(due, &c, Role::Lead, false), None);
        assert_eq!(s.wake_due(due, &c, Role::Lead, true), Some(Wake::TurnEnded));
        assert_eq!(
            s.wake_due(due, &Autopilot::default(), Role::Lead, true),
            None
        );
        s.started(due, Some(1));
        assert_eq!(s.wake_due(due, &c, Role::Lead, true), None);
        s.completed(due, &c, Role::Lead, true, Some(1));
        assert_eq!(
            s.wake_due(due + minutes(c.lead_heartbeat_min), &c, Role::Lead, true),
            Some(Wake::Heartbeat)
        );
        s.completed(due, &c, Role::Controller, false, None);
        assert_eq!(
            s.wake_due(
                due + minutes(c.controller_heartbeat_min),
                &c,
                Role::Controller,
                true
            ),
            Some(Wake::Heartbeat)
        );
    }

    #[test]
    fn retry_stall_restart_and_user_stop() {
        let c = Autopilot {
            on: true,
            ..Default::default()
        };
        let mut now = Instant::now();
        let mut s = StreamClock::default();
        s.restarted(now + Duration::from_secs(5));
        assert!(!s.may_wake(now));
        now += Duration::from_secs(5);
        assert_eq!(
            s.wake_due(now, &c, Role::Worker, true),
            Some(Wake::Restarted)
        );
        for (i, delay) in [1, 2, 4, 8, 16, 30, 30].into_iter().enumerate() {
            s.started(now, None);
            s.failed(now);
            s.failed(now);
            now += minutes(delay);
            assert!(!s.may_wake(now - Duration::from_secs(1)));
            assert_eq!(
                s.wake_due(now, &c, Role::Worker, true),
                Some(Wake::Retry(i as u32 + 1))
            );
        }
        for (task, n) in [("a", 1), ("a", 2), ("a", 3), ("b", 1)] {
            s.started(now, None);
            now += minutes(1);
            s.activity(now);
            assert_eq!(s.poll_stall(now, &c, Some(task.into())), None);
            now += minutes(c.stall_min);
            assert_eq!(s.poll_stall(now, &c, Some(task.into())), Some(n));
            s.interrupted(now);
            assert_eq!(s.wake_due(now, &c, Role::Worker, true), Some(Wake::Stalled));
        }
        s.started(now, None);
        s.interrupted(now);
        assert!(!s.may_wake(now));
        assert_eq!(s.wake_due(now + minutes(200), &c, Role::Lead, true), None);
    }

    #[test]
    fn daily_limit_survives_reopen_and_resets_at_midnight() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&path).unwrap();
        let mut p = Project::open(&path).unwrap();
        p.file.autopilot.on = true;
        p.file.autopilot.daily_turns = 1;
        p.save().unwrap();
        assert!(p.reserve_scheduler_turn("Monday").unwrap());
        let mut p = Project::open(&path).unwrap();
        for _ in 0..2 {
            assert!(!p.reserve_scheduler_turn("Monday").unwrap());
        }
        assert_eq!(p.notices.iter().filter(|n| n.from == ATRIUM).count(), 1);
        assert!(p.reserve_scheduler_turn("Tuesday").unwrap());
        fs::remove_dir_all(path).unwrap();
    }
}
