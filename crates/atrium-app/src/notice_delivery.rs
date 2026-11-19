//! Pure state for at-most-one automatic attempt while delivery is uncertain.
//! Workspace owns RPC dispatch; Project commits this journal with the cursor.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeOutcome {
    Accepted,
    Rejected,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeResolution {
    MarkReceived,
    RetryNotices,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NoticeTicket {
    attempt: String,
    pub(super) stream: String,
    pub(super) created_at: i64,
    pub(super) backend: String,
    pub(super) thread: String,
    pub(super) from: usize,
    pub(super) through: usize,
    pub(super) prefix_sha256: [u8; 32],
}

impl NoticeTicket {
    pub(super) fn new(
        stream: &super::Workstream,
        thread: &str,
        through: usize,
        prefix_sha256: [u8; 32],
    ) -> Self {
        Self {
            attempt: uuid::Uuid::new_v4().to_string(),
            stream: stream.id.clone(),
            created_at: stream.created_at,
            backend: stream.backend.clone(),
            thread: thread.to_owned(),
            from: stream.notices_seen,
            through,
            prefix_sha256,
        }
    }

    pub(super) fn is_for(&self, stream: &super::Workstream) -> bool {
        self.stream == stream.id
            && self.created_at == stream.created_at
            && self.backend == stream.backend
            && stream.thread_id.as_deref() == Some(self.thread.as_str())
    }
}

#[derive(Debug)]
pub(crate) enum NoticePreparation {
    Empty,
    Blocked,
    Ready { ticket: NoticeTicket, text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Reserved,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Delivery {
    pub(super) ticket: NoticeTicket,
    pub(super) phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Commit {
    Advance(usize),
    Release,
    Hold,
}
impl Delivery {
    pub(super) fn reserved(ticket: NoticeTicket) -> Self {
        Self {
            ticket,
            phase: Phase::Reserved,
        }
    }

    pub(super) fn settle(&self, outcome: NoticeOutcome, matches_prefix: bool) -> Option<Commit> {
        if self.phase != Phase::Reserved {
            return None;
        }
        Some(match outcome {
            NoticeOutcome::Accepted if matches_prefix => Commit::Advance(self.ticket.through),
            NoticeOutcome::Rejected => Commit::Release,
            NoticeOutcome::Accepted | NoticeOutcome::Uncertain => Commit::Hold,
        })
    }

    pub(super) fn resolve(
        &self,
        resolution: NoticeResolution,
        matches_prefix: bool,
    ) -> Option<Commit> {
        if self.phase != Phase::Uncertain {
            return None;
        }
        match resolution {
            NoticeResolution::MarkReceived if matches_prefix => {
                Some(Commit::Advance(self.ticket.through))
            }
            NoticeResolution::MarkReceived => None,
            NoticeResolution::RetryNotices => Some(Commit::Release),
        }
    }
}
