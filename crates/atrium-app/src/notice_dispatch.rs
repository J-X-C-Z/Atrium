//! Pure helpers used by the real Workspace dispatch hooks, without GPUI.
use crate::project::{
    Project,
    notice_delivery::{NoticeOutcome, NoticePreparation, NoticeTicket},
};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(super) struct NoticeTarget {
    pub(super) root: PathBuf,
    pub(super) stream: String,
    pub(super) thread: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoticeSendResult {
    NoDispatch,
    Empty,
    Submitted,
}

pub(super) struct NoticeDispatch {
    pub(super) root: PathBuf,
    pub(super) stream: String,
    ticket: NoticeTicket,
}

pub(super) struct PreparedText {
    pub(super) text: String,
    pub(super) dispatch: Option<NoticeDispatch>,
    pub(super) held: bool,
}

impl NoticeDispatch {
    pub(super) fn prepare(
        project: &mut Project,
        stream: &str,
        thread: &str,
        original: String,
    ) -> anyhow::Result<PreparedText> {
        let (text, ticket, held) = match project.prepare_notice_delivery(stream, thread)? {
            NoticePreparation::Empty => (original, None, false),
            NoticePreparation::Blocked => (original, None, true),
            NoticePreparation::Ready { text, ticket } => {
                (format!("{text}{original}"), Some(ticket), false)
            }
        };
        Ok(PreparedText {
            text,
            dispatch: ticket.map(|ticket| Self {
                root: project.root.clone(),
                stream: stream.to_owned(),
                ticket,
            }),
            held,
        })
    }

    pub(super) fn prepare_automatic(
        project: &mut Project,
        target: &NoticeTarget,
        thread: &str,
        original: String,
    ) -> anyhow::Result<Result<PreparedText, NoticeSendResult>> {
        if project.root != target.root
            || thread != target.thread
            || !project
                .stream(&target.stream)
                .is_some_and(|s| s.thread_id.as_deref() == Some(thread))
        {
            return Ok(Err(NoticeSendResult::NoDispatch));
        }
        let prepared = Self::prepare(project, &target.stream, thread, original)?;
        Ok(if prepared.dispatch.is_some() {
            Ok(prepared)
        } else if prepared.held {
            Err(NoticeSendResult::NoDispatch)
        } else {
            Err(NoticeSendResult::Empty)
        })
    }

    pub(super) fn settle(
        &self,
        project: &mut Project,
        outcome: NoticeOutcome,
    ) -> anyhow::Result<bool> {
        if project.root != self.root {
            return Ok(false);
        }
        project.settle_notice_delivery(&self.ticket, outcome)
    }
}

pub(super) fn automatic_notice_dispatch_allowed(project: &Project, stream: &str) -> bool {
    !project.notice_delivery_blocked(stream)
}

// Remote errors may follow effects. Without reliable per-attempt evidence,
// even a standard error code stays uncertain; no automatic RPC retry occurs.
pub(super) fn codex_notice_outcome(
    result: &anyhow::Result<String>,
    expected_turn: Option<&str>,
) -> NoticeOutcome {
    match result {
        Ok(id) if !id.trim().is_empty() && expected_turn.is_none_or(|expected| id == expected) => {
            NoticeOutcome::Accepted
        }
        _ => NoticeOutcome::Uncertain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{ATRIUM, Role, Workstream};
    struct Fixture(Project);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("atrium-dispatch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let mut p = Project::open(&root).unwrap();
            p.file.streams.push(Workstream {
                id: "lead".into(),
                backend: "codex".into(),
                thread_id: Some("thread".into()),
                role: Role::Lead,
                ..Default::default()
            });
            p.save().unwrap();
            p.post(ATRIUM, vec!["lead".into()], "first notice").unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0.root).unwrap();
        }
    }

    #[test]
    fn blocked_auto_stops_but_manual_text_has_no_held_notice() {
        let mut f = Fixture::new();
        let prepared =
            NoticeDispatch::prepare(&mut f.0, "lead", "thread", "manual original".into()).unwrap();
        assert!(prepared.text.contains("first notice"));
        assert!(prepared.text.ends_with("manual original"));
        assert!(!automatic_notice_dispatch_allowed(&f.0, "lead"));
        let second =
            NoticeDispatch::prepare(&mut f.0, "lead", "thread", "manual second".into()).unwrap();
        assert_eq!(second.text, "manual second");
        assert!(second.held && second.dispatch.is_none());
        prepared
            .dispatch
            .unwrap()
            .settle(&mut f.0, NoticeOutcome::Uncertain)
            .unwrap();
        assert!(!automatic_notice_dispatch_allowed(&f.0, "lead"));
    }

    #[test]
    fn callback_uses_captured_prefix_and_rejects_other_project_and_lifecycle() {
        let mut f = Fixture::new();
        let dispatch = NoticeDispatch::prepare(&mut f.0, "lead", "thread", "private manual".into())
            .unwrap()
            .dispatch
            .unwrap();
        let mut other = Fixture::new();
        assert!(
            !dispatch
                .settle(&mut other.0, NoticeOutcome::Accepted)
                .unwrap()
        );
        f.0.post(ATRIUM, vec!["lead".into()], "second notice")
            .unwrap();
        assert!(dispatch.settle(&mut f.0, NoticeOutcome::Accepted).unwrap());
        assert_eq!(f.0.unread("lead").len(), 1);
        assert_eq!(f.0.unread("lead")[0].text, "second notice");
        assert!(automatic_notice_dispatch_allowed(&f.0, "lead"));
        let later = NoticeDispatch::prepare(&mut f.0, "lead", "thread", "next".into())
            .unwrap()
            .dispatch
            .unwrap();
        f.0.stream_mut("lead").unwrap().thread_id = Some("replacement".into());
        assert!(!later.settle(&mut f.0, NoticeOutcome::Accepted).unwrap());
        assert!(f.0.notice_delivery_blocked("lead"));
        let disk = std::fs::read_to_string(f.0.root.join(".atrium/project.json")).unwrap();
        assert!(!disk.contains("private manual"));
    }

    #[test]
    fn failed_reservation_keeps_automatic_wake_pending() {
        let mut f = Fixture::new();
        assert!(automatic_notice_dispatch_allowed(&f.0, "lead"));
        let metadata = f.0.root.join(".atrium/project.json");
        let backup = f.0.root.join(".atrium/project.backup");
        std::fs::rename(&metadata, &backup).unwrap();
        std::fs::create_dir(&metadata).unwrap();
        assert!(NoticeDispatch::prepare(&mut f.0, "lead", "thread", "wake".into()).is_err());
        assert!(automatic_notice_dispatch_allowed(&f.0, "lead"));
        assert_eq!(f.0.stream("lead").unwrap().notices_seen, 0);
        std::fs::remove_dir(&metadata).unwrap();
        std::fs::rename(&backup, &metadata).unwrap();
        let prepared = NoticeDispatch::prepare(&mut f.0, "lead", "thread", "wake".into()).unwrap();
        assert!(prepared.dispatch.is_some());
        assert!(!automatic_notice_dispatch_allowed(&f.0, "lead"));
    }

    #[test]
    fn automatic_origin_never_sends_bare_text_from_stale_cache_or_wrong_target() {
        let mut f = Fixture::new();
        let target = NoticeTarget {
            root: f.0.root.clone(),
            stream: "lead".into(),
            thread: "thread".into(),
        };
        // A cached unread notice can disappear before the final disk read.
        std::fs::write(f.0.root.join(".atrium/notices.md"), "").unwrap();
        assert_eq!(f.0.unread("lead").len(), 1);
        assert!(matches!(
            NoticeDispatch::prepare_automatic(&mut f.0, &target, "thread", "carry on".into())
                .unwrap(),
            Err(NoticeSendResult::Empty)
        ));
        assert!(!f.0.notice_delivery_blocked("lead"));
        let mut wrong = target.clone();
        wrong.stream = "someone-else".into();
        assert!(matches!(
            NoticeDispatch::prepare_automatic(&mut f.0, &wrong, "thread", "carry on".into())
                .unwrap(),
            Err(NoticeSendResult::NoDispatch)
        ));
        wrong = target.clone();
        wrong.root = f.0.root.join("another");
        assert!(matches!(
            NoticeDispatch::prepare_automatic(&mut f.0, &wrong, "thread", "carry on".into())
                .unwrap(),
            Err(NoticeSendResult::NoDispatch)
        ));
        assert!(matches!(
            NoticeDispatch::prepare_automatic(
                &mut f.0,
                &target,
                "changed-thread",
                "carry on".into()
            )
            .unwrap(),
            Err(NoticeSendResult::NoDispatch)
        ));
        // Cursor advancement after target capture also consumes no bare RPC.
        f.0.post(ATRIUM, vec!["lead".into()], "new notice").unwrap();
        f.0.mark_read("lead");
        f.0.save().unwrap();
        assert!(matches!(
            NoticeDispatch::prepare_automatic(&mut f.0, &target, "thread", "carry on".into())
                .unwrap(),
            Err(NoticeSendResult::Empty)
        ));
        f.0.post(ATRIUM, vec!["lead".into()], "last notice")
            .unwrap();
        let prepared =
            NoticeDispatch::prepare_automatic(&mut f.0, &target, "thread", "carry on".into())
                .unwrap()
                .unwrap_or_else(|_| panic!("must reserve"));
        assert!(prepared.dispatch.is_some());
        assert!(matches!(
            NoticeDispatch::prepare_automatic(&mut f.0, &target, "thread", "carry on".into())
                .unwrap(),
            Err(NoticeSendResult::NoDispatch)
        ));
        assert_eq!(
            NoticeDispatch::prepare(&mut f.0, "lead", "thread", "manual".into())
                .unwrap()
                .text,
            "manual"
        );
    }

    #[test]
    fn only_nonempty_codex_success_is_acceptance() {
        assert_eq!(
            codex_notice_outcome(&Ok("turn".into()), None),
            NoticeOutcome::Accepted
        );
        assert_eq!(
            codex_notice_outcome(&Ok(" ".into()), None),
            NoticeOutcome::Uncertain
        );
        assert_eq!(
            codex_notice_outcome(&Ok("turn".into()), Some("turn")),
            NoticeOutcome::Accepted
        );
        assert_eq!(
            codex_notice_outcome(&Ok("different".into()), Some("turn")),
            NoticeOutcome::Uncertain
        );
        for error in [
            codex_client::RpcError::Closed,
            codex_client::RpcError::Write("partial".into()),
            codex_client::RpcError::Server {
                code: -32602,
                message: "refused".into(),
                data: None,
            },
        ] {
            assert_eq!(
                codex_notice_outcome(&Err(error.into()), None),
                NoticeOutcome::Uncertain
            );
        }
    }
}
