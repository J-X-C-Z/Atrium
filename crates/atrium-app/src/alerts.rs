//! System notifications: a turn finished, an agent needs you, or a turn
//! failed, in a conversation you are not looking at. Sent through the
//! platform's own tool, so Atrium needs no notification library.

use std::{
    collections::HashMap,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use atrium_core::{AgentEvent, TurnStatus};

/// What happened, worth a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    Finished,
    NeedsApproval,
    NeedsAnswer,
    Failed,
}

impl Alert {
    /// The notification's text, in English (translated by the caller).
    pub fn body(self) -> &'static str {
        match self {
            Alert::Finished => "Finished its turn.",
            Alert::NeedsApproval => "Needs your approval.",
            Alert::NeedsAnswer => "Has a question for you.",
            Alert::Failed => "Its turn failed.",
        }
    }
}

/// The alert `event` calls for, and the thread it is about.
pub fn alert_for(event: &AgentEvent) -> Option<(&str, Alert)> {
    match event {
        AgentEvent::TurnCompleted {
            thread_id, status, ..
        } => match status {
            TurnStatus::Completed => Some((thread_id, Alert::Finished)),
            TurnStatus::Failed => Some((thread_id, Alert::Failed)),
            _ => None,
        },
        AgentEvent::ApprovalRequested(request) => Some((&request.thread_id, Alert::NeedsApproval)),
        AgentEvent::UserInputRequested(request) => Some((&request.thread_id, Alert::NeedsAnswer)),
        _ => None,
    }
}

/// Keeps a busy thread from flooding the notification centre.
#[derive(Default)]
pub struct Alerts {
    last: HashMap<String, Instant>,
    failures: HashMap<String, (String, bool)>,
}

impl Alerts {
    const QUIET: Duration = Duration::from_secs(10);

    /// Alert once on the second distinct failed turn until a turn succeeds.
    pub fn phone_title(
        &mut self,
        thread: &str,
        event: &AgentEvent,
        alert: Alert,
        lead_done: bool,
    ) -> Option<&'static str> {
        match (alert, event) {
            (Alert::NeedsApproval, _) => Some("Approval requested"),
            (Alert::NeedsAnswer, _) => Some("Question from agent"),
            (Alert::Finished, _) => {
                self.failures.remove(thread);
                lead_done.then_some("Lead finished")
            }
            (Alert::Failed, AgentEvent::TurnCompleted { turn_id, .. }) => {
                if let Some((previous, notified)) = self.failures.get_mut(thread) {
                    if previous == turn_id || *notified {
                        return None;
                    }
                    *notified = true;
                    Some("Repeated turn failure")
                } else {
                    self.failures
                        .insert(thread.to_owned(), (turn_id.clone(), false));
                    None
                }
            }
            _ => None,
        }
    }

    /// Whether `thread` may notify now. Something that needs the user always
    /// may; finishing and failing at most once per ten seconds.
    pub fn allow(&mut self, thread: &str, alert: Alert, now: Instant) -> bool {
        let urgent = matches!(alert, Alert::NeedsApproval | Alert::NeedsAnswer);
        let recent = self
            .last
            .get(thread)
            .is_some_and(|at| now.duration_since(*at) < Self::QUIET);
        if recent && !urgent {
            return false;
        }
        self.last.insert(thread.to_string(), now);
        true
    }
}

/// Shows a notification, without waiting and without failing loudly: on a
/// machine without the tool it is only logged.
pub fn show(title: &str, body: &str) {
    let mut command = if cfg!(target_os = "macos") {
        let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut c = Command::new("osascript");
        c.arg("-e").arg(format!(
            "display notification \"{}\" with title \"{}\"",
            quote(body),
            quote(title)
        ));
        c
    } else if cfg!(target_os = "windows") {
        let quote = |s: &str| s.replace('\'', "''");
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; \
             $x = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
             $t = $x.GetElementsByTagName('text'); $t.Item(0).InnerText = '{}'; $t.Item(1).InnerText = '{}'; \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Atrium').Show([Windows.UI.Notifications.ToastNotification]::new($x))",
            quote(title),
            quote(body)
        );
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-Command", &script]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        c
    } else {
        let mut c = Command::new("notify-send");
        c.args(["--app-name=Atrium", title, body]);
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match command.spawn() {
        // Reap it off the UI thread.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(error) => log::info!("notification not shown ({error}): {title}: {body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::{ApprovalKind, ApprovalRequest};

    #[test]
    fn which_events_notify_and_how_often() {
        let done = AgentEvent::TurnCompleted {
            thread_id: "t".into(),
            turn_id: "1".into(),
            status: TurnStatus::Completed,
            error: None,
            duration_ms: None,
        };
        assert_eq!(alert_for(&done), Some(("t", Alert::Finished)));
        let stopped = AgentEvent::TurnCompleted {
            thread_id: "t".into(),
            turn_id: "1".into(),
            status: TurnStatus::Interrupted,
            error: None,
            duration_ms: None,
        };
        assert_eq!(alert_for(&stopped), None, "the user stopped it");
        let ask = AgentEvent::ApprovalRequested(ApprovalRequest {
            request_id: 1.into(),
            thread_id: "t".into(),
            turn_id: "1".into(),
            item_id: "i".into(),
            kind: ApprovalKind::Tool {
                title: "Run".into(),
                detail: None,
            },
        });
        assert_eq!(alert_for(&ask), Some(("t", Alert::NeedsApproval)));

        let mut alerts = Alerts::default();
        let now = Instant::now();
        assert!(alerts.allow("t", Alert::Finished, now));
        assert!(!alerts.allow("t", Alert::Finished, now + Duration::from_secs(3)));
        assert!(alerts.allow("t", Alert::NeedsApproval, now + Duration::from_secs(4)));
        assert!(alerts.allow("u", Alert::Finished, now + Duration::from_secs(4)));
        assert!(alerts.allow("t", Alert::Finished, now + Duration::from_secs(20)));
    }
}
