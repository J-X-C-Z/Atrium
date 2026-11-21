//! Runs scheduled chats: every half minute, each schedule that is due
//! starts a chat in its folder with its agent and prompt.

use std::path::PathBuf;
use std::time::Duration;

use gpui::Context;

use super::{Notice, Workspace};
use crate::i18n::tf;
use crate::schedule::Schedule;

impl Workspace {
    pub(super) fn start_schedules(&mut self, cx: &mut Context<Self>) {
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

    fn run_due_schedules(&mut self, cx: &mut Context<Self>) {
        let now = jiff::Zoned::now().datetime();
        let due: Vec<String> = self
            .settings
            .schedules
            .iter()
            .filter(|s| s.due(now))
            .map(|s| s.id.clone())
            .collect();
        for id in due {
            self.run_schedule(&id, cx);
        }
    }

    /// Starts schedule `id`'s chat now, whatever its time.
    pub fn run_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(s) = self.settings.schedules.iter_mut().find(|s| s.id == id) else {
            return;
        };
        s.last_run = Some(jiff::Zoned::now().date().to_string());
        let s = s.clone();
        self.settings.save();
        let folder = PathBuf::from(&s.folder);
        if !folder.is_dir() {
            cx.emit(Notice::Error(tf(
                "Scheduled chat “{name}”: its folder {folder} is gone.",
                &[("name", &s.name), ("folder", &s.folder)],
            )));
            return;
        }
        self.start_chat_in(s.prompt.clone(), s.backend.clone(), folder, false, cx);
        cx.emit(Notice::Info(tf(
            "Scheduled chat “{name}” started.",
            &[("name", &s.name)],
        )));
        cx.notify();
    }

    pub fn add_schedule(&mut self, schedule: Schedule, cx: &mut Context<Self>) {
        self.settings.schedules.push(schedule);
        self.settings.save();
        cx.notify();
    }

    pub fn remove_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        self.settings.schedules.retain(|s| s.id != id);
        self.settings.save();
        cx.notify();
    }

    pub fn toggle_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(s) = self.settings.schedules.iter_mut().find(|s| s.id == id) {
            s.off = !s.off;
        }
        self.settings.save();
        cx.notify();
    }
}
