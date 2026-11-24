//! Runs scheduled chats: every half minute, each schedule that is due
//! starts a chat in its folder with its agent and prompt.
//!
//! Ported from T3 Code v0.1.11 `apps/server/src/scheduledTasks/
//! ScheduledTaskService.ts` (restart recovery and the one-run-at-a-time
//! guard). The decision logic lives in `schedule.rs`, which is GPUI-free.
//! See `crates/atrium-engine/UPSTREAM.md`, "Ported outside atrium-engine".

use std::path::PathBuf;
use std::time::Duration;

use gpui::Context;

use super::{Notice, Workspace};
use crate::i18n::{t, tf};
use crate::schedule::Schedule;

impl Workspace {
    pub(super) fn start_schedules(&mut self, cx: &mut Context<Self>) {
        self.recover_schedules();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                if this.update(cx, |ws, cx| ws.run_due_schedules(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Reports runs a crash left in flight and gives interval and one-time
    /// chats their next run, once, before the first tick.
    fn recover_schedules(&mut self) {
        let now = jiff::Zoned::now();
        let mut changed = false;
        for schedule in &mut self.settings.schedules {
            let before = schedule.clone();
            schedule.recover_interrupted(&now);
            schedule.ensure_next_run(&now);
            changed |= *schedule != before;
        }
        if changed {
            self.settings.save();
        }
    }

    fn run_due_schedules(&mut self, cx: &mut Context<Self>) {
        let now = jiff::Zoned::now();
        let due: Vec<String> = self
            .settings
            .schedules
            .iter()
            .filter(|s| s.due(&now))
            .map(|s| s.id.clone())
            .collect();
        for id in due {
            self.fire_schedule(&id, &now, cx);
        }
    }

    /// Starts schedule `id`'s chat now, whatever its time. A chat already
    /// dispatching is left alone, so runs never overlap.
    pub fn run_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        let now = jiff::Zoned::now();
        self.fire_schedule(id, &now, cx);
    }

    fn fire_schedule(&mut self, id: &str, now: &jiff::Zoned, cx: &mut Context<Self>) {
        let Some(index) = self.settings.schedules.iter().position(|s| s.id == id) else {
            return;
        };
        if !self.settings.schedules[index].begin_run(now) {
            return;
        }
        // Save before dispatching: a crash during the run is recovered on the
        // next start instead of repeating the occurrence.
        self.settings.save();
        let schedule = self.settings.schedules[index].clone();
        let folder = PathBuf::from(&schedule.folder);
        let error = if !folder.is_dir() {
            let message = tf(
                "Scheduled chat “{name}”: its folder {folder} is gone.",
                &[("name", &schedule.name), ("folder", &schedule.folder)],
            );
            cx.emit(Notice::Error(message.clone()));
            Some(message)
        } else {
            self.start_chat_in(
                schedule.prompt.clone(),
                schedule.backend.clone(),
                folder,
                false,
                cx,
            );
            cx.emit(Notice::Info(tf(
                "Scheduled chat “{name}” started.",
                &[("name", &schedule.name)],
            )));
            None
        };
        if let Some(schedule) = self.settings.schedules.iter_mut().find(|s| s.id == id) {
            schedule.finish_run(error.as_deref());
        }
        self.settings.save();
        cx.notify();
    }

    pub fn add_schedule(&mut self, schedule: Schedule, cx: &mut Context<Self>) {
        self.settings.schedules.push(schedule);
        let now = jiff::Zoned::now();
        if let Some(schedule) = self.settings.schedules.last_mut() {
            schedule.ensure_next_run(&now);
        }
        self.settings.save();
        cx.notify();
    }

    pub fn remove_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        self.settings.schedules.retain(|s| s.id != id);
        self.settings.save();
        cx.notify();
    }

    pub fn toggle_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        let now = jiff::Zoned::now();
        if let Some(s) = self.settings.schedules.iter_mut().find(|s| s.id == id) {
            s.off = !s.off;
            if s.off {
                s.next_run = None;
            } else {
                s.next_run = s.next_after(&now).map(|at| at.timestamp().to_string());
            }
        }
        self.settings.save();
        cx.notify();
    }
}

/// What the list says about a schedule's next run and last result.
pub fn schedule_detail(schedule: &Schedule) -> String {
    let mut parts = Vec::new();
    if schedule.off {
        parts.push(t("Paused").to_string());
    } else if let Some(at) = schedule.next_run.as_deref()
        && let Ok(at) = at.parse::<jiff::Timestamp>()
    {
        let local = at.to_zoned(jiff::tz::TimeZone::system());
        parts.push(tf(
            "Next run {time}",
            &[("time", &local.strftime("%m-%d %H:%M"))],
        ));
    }
    match schedule.last_status {
        crate::schedule::RunStatus::Never => {}
        crate::schedule::RunStatus::Succeeded => parts.push(tf(
            "{n} runs, last succeeded",
            &[("n", &schedule.run_count)],
        )),
        crate::schedule::RunStatus::Failed => {
            let why = schedule.last_error.as_deref().unwrap_or(t("Failed"));
            parts.push(tf(
                "{n} runs, last failed: {error}",
                &[("n", &schedule.run_count), ("error", &why)],
            ));
        }
    }
    parts.join(" · ")
}
