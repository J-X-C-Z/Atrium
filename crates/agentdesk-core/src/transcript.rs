use std::collections::HashMap;

use crate::{
    event::{AgentEvent, ApprovalDecision, ApprovalRequest, UserInputRequest},
    model::{Item, ItemStatus, PlanStep, TokenUsage, TurnStatus},
};

/// One row of a conversation as the UI draws it.
#[derive(Clone, Debug, PartialEq)]
pub enum EntryKind {
    Item(Item),
    Approval {
        request: ApprovalRequest,
        /// Set once the user answered; `resolved` once the backend let go of it.
        answer: Option<ApprovalDecision>,
        resolved: bool,
    },
    /// The agent asks the user questions and waits for answers.
    Question {
        request: UserInputRequest,
        /// The answers given, once given: `(question id, answers)`.
        answers: Option<Vec<(String, Vec<String>)>>,
        resolved: bool,
    },
    Plan {
        explanation: Option<String>,
        steps: Vec<PlanStep>,
    },
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub turn_id: String,
    pub kind: EntryKind,
    /// More may still arrive for this entry.
    pub live: bool,
}

/// A conversation folded from [`AgentEvent`]s into a flat list of rows.
///
/// Flat on purpose: a virtualized list draws row `ix` without walking turns,
/// and an item is found by id in O(1) when a delta lands.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    /// Key (item id, `plan:<turn>`, `approval:<request>`) to row.
    index: HashMap<String, usize>,
    active_turn: Option<String>,
    usage: Option<TokenUsage>,
    /// Bumped on every change, so views can tell cheaply whether to redraw.
    version: u64,
    local_seq: u64,
    /// A transient notice, such as a retry in progress.
    notice: Option<String>,
    /// Rows that came from stored history rather than live events.
    seeded: usize,
    /// How each finished turn ended.
    turns: HashMap<String, TurnSummary>,
}

/// How a turn ended, for the footer under its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnSummary {
    pub status: TurnStatus,
    pub duration_ms: Option<u64>,
}

const LOCAL_PREFIX: &str = "local:";

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    /// A transcript rebuilt from stored history: `(turn id, status, items)` oldest first.
    pub fn from_history(turns: impl IntoIterator<Item = (String, TurnStatus, Vec<Item>)>) -> Self {
        let mut transcript = Self::new();
        for (turn_id, status, items) in turns {
            for item in items {
                transcript.upsert_item(&turn_id, item, false);
            }
            if status == TurnStatus::InProgress {
                transcript.active_turn = Some(turn_id);
            } else {
                transcript.turns.insert(
                    turn_id,
                    TurnSummary {
                        status,
                        duration_ms: None,
                    },
                );
            }
        }
        transcript.seeded = transcript.entries.len();
        transcript
    }

    /// How many leading rows came from stored history. Views animate only the rest.
    pub fn seeded(&self) -> usize {
        self.seeded
    }

    /// How `turn_id` ended, once it has.
    pub fn turn_summary(&self, turn_id: &str) -> Option<TurnSummary> {
        self.turns.get(turn_id).copied()
    }

    /// Whether row `ix` is the last row of its turn and that turn is over.
    pub fn ends_turn(&self, ix: usize) -> bool {
        let Some(entry) = self.entries.get(ix) else {
            return false;
        };
        if entry.turn_id.is_empty() || !self.turns.contains_key(&entry.turn_id) {
            return false;
        }
        self.entries
            .get(ix + 1)
            .is_none_or(|next| next.turn_id != entry.turn_id)
    }

    /// The user's message (text and attachments) that started `turn_id`.
    pub fn turn_prompt(&self, turn_id: &str) -> Option<(String, Vec<String>)> {
        self.entries.iter().find_map(|entry| match &entry.kind {
            EntryKind::Item(Item::User {
                text, attachments, ..
            }) if entry.turn_id == turn_id => Some((text.clone(), attachments.clone())),
            _ => None,
        })
    }

    /// Everything the agent said in `turn_id`, for copying.
    pub fn turn_answer(&self, turn_id: &str) -> String {
        self.entries
            .iter()
            .filter(|entry| entry.turn_id == turn_id)
            .filter_map(|entry| match &entry.kind {
                EntryKind::Item(Item::Agent { text, .. }) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn usage(&self) -> Option<TokenUsage> {
        self.usage
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The turn that is running, if any.
    pub fn active_turn(&self) -> Option<&str> {
        self.active_turn.as_deref()
    }

    pub fn is_running(&self) -> bool {
        self.active_turn.is_some()
    }

    /// Approvals still waiting on the user.
    pub fn pending_approvals(&self) -> impl Iterator<Item = &ApprovalRequest> {
        self.entries.iter().filter_map(|entry| match &entry.kind {
            EntryKind::Approval {
                request,
                answer: None,
                resolved: false,
            } => Some(request),
            _ => None,
        })
    }

    /// Shows the user's message at once, before the backend echoes it.
    /// The echo replaces this row instead of adding a second one.
    pub fn push_local_user(&mut self, text: impl Into<String>, attachments: Vec<String>) {
        self.local_seq += 1;
        let id = format!("{LOCAL_PREFIX}{}", self.local_seq);
        self.push(
            String::new(),
            id.clone(),
            EntryKind::Item(Item::User {
                id,
                text: text.into(),
                attachments,
            }),
            false,
        );
    }

    /// Marks the optimistic turn as started before the backend confirms it,
    /// so the composer turns into a stop button at once.
    pub fn begin_pending_turn(&mut self) {
        if self.active_turn.is_none() {
            self.active_turn = Some(String::new());
            self.bump();
        }
    }

    /// Undoes `begin_pending_turn` when sending failed.
    pub fn fail_pending_turn(&mut self, message: impl Into<String>) {
        if self.active_turn.as_deref() == Some("") {
            self.active_turn = None;
        }
        self.entries.push(Entry {
            turn_id: String::new(),
            kind: EntryKind::Error(message.into()),
            live: false,
        });
        self.bump();
    }

    /// Questions still waiting on the user.
    pub fn pending_questions(&self) -> impl Iterator<Item = &UserInputRequest> {
        self.entries.iter().filter_map(|entry| match &entry.kind {
            EntryKind::Question {
                request,
                answers: None,
                resolved: false,
            } => Some(request),
            _ => None,
        })
    }

    /// Records the user's answers to the agent's questions.
    pub fn answer_questions(
        &mut self,
        request_id: &serde_json::Value,
        given: Vec<(String, Vec<String>)>,
    ) {
        let key = approval_key(request_id);
        if let Some(&ix) = self.index.get(&key)
            && let EntryKind::Question { answers, .. } = &mut self.entries[ix].kind
        {
            *answers = Some(given);
            self.entries[ix].live = false;
            self.bump();
        }
    }

    /// Records the user's answer to an approval.
    pub fn answer_approval(&mut self, request_id: &serde_json::Value, decision: ApprovalDecision) {
        let key = approval_key(request_id);
        if let Some(&ix) = self.index.get(&key)
            && let EntryKind::Approval { answer, .. } = &mut self.entries[ix].kind
        {
            *answer = Some(decision);
            self.entries[ix].live = false;
            self.bump();
        }
    }

    /// Folds one event in. Returns whether anything changed.
    pub fn apply(&mut self, event: &AgentEvent) -> bool {
        let before = self.version;
        match event {
            AgentEvent::TurnStarted { turn_id, .. } => {
                self.active_turn = Some(turn_id.clone());
                self.notice = None;
                // Rows added while the turn was pending belong to it.
                for entry in &mut self.entries {
                    if entry.turn_id.is_empty() {
                        entry.turn_id = turn_id.clone();
                    }
                }
                self.bump();
            }
            AgentEvent::TurnCompleted {
                turn_id,
                status,
                error,
                duration_ms,
                ..
            } => {
                self.turns.insert(
                    turn_id.clone(),
                    TurnSummary {
                        status: *status,
                        duration_ms: *duration_ms,
                    },
                );
                if self
                    .active_turn
                    .as_deref()
                    .is_some_and(|active| active == turn_id || active.is_empty())
                {
                    self.active_turn = None;
                }
                self.notice = None;
                // Nothing in a finished turn is live any more.
                for entry in &mut self.entries {
                    if entry.turn_id == *turn_id && entry.live {
                        entry.live = false;
                        if let EntryKind::Approval { resolved, .. }
                        | EntryKind::Question { resolved, .. } = &mut entry.kind
                        {
                            *resolved = true;
                        }
                    }
                }
                match (status, error) {
                    (_, Some(message)) => self.push_error(turn_id, message.clone()),
                    (TurnStatus::Failed, None) => {
                        self.push_error(turn_id, "The turn failed.".into())
                    }
                    _ => {}
                }
                self.bump();
            }
            AgentEvent::ItemStarted { turn_id, item, .. } => {
                let live = !matches!(item, Item::User { .. });
                self.upsert_item(turn_id, item.clone(), live);
            }
            AgentEvent::ItemCompleted { turn_id, item, .. } => {
                self.upsert_item(turn_id, item.clone(), false);
            }
            AgentEvent::AgentMessageDelta {
                turn_id,
                item_id,
                delta,
                ..
            } => {
                let ix = self.ensure(turn_id, item_id, || Item::Agent {
                    id: item_id.clone(),
                    text: String::new(),
                });
                if let EntryKind::Item(Item::Agent { text, .. }) = &mut self.entries[ix].kind {
                    text.push_str(delta);
                    self.bump();
                }
            }
            AgentEvent::ReasoningDelta {
                turn_id,
                item_id,
                delta,
                ..
            } => {
                let ix = self.ensure(turn_id, item_id, || Item::Reasoning {
                    id: item_id.clone(),
                    text: String::new(),
                });
                if let EntryKind::Item(Item::Reasoning { text, .. }) = &mut self.entries[ix].kind {
                    text.push_str(delta);
                    self.bump();
                }
            }
            AgentEvent::CommandOutputDelta { item_id, delta, .. } => {
                if let Some(&ix) = self.index.get(item_id)
                    && let EntryKind::Item(Item::Command { output, .. }) =
                        &mut self.entries[ix].kind
                {
                    output.push_str(delta);
                    self.bump();
                }
            }
            AgentEvent::PlanUpdated {
                turn_id,
                explanation,
                steps,
                ..
            } => {
                let kind = EntryKind::Plan {
                    explanation: explanation.clone(),
                    steps: steps.clone(),
                };
                let key = format!("plan:{turn_id}");
                match self.index.get(&key) {
                    Some(&ix) => {
                        self.entries[ix].kind = kind;
                        self.bump();
                    }
                    None => self.push(turn_id.clone(), key, kind, true),
                }
            }
            AgentEvent::TokenUsage { usage, .. } => {
                self.usage = Some(*usage);
                self.bump();
            }
            AgentEvent::ApprovalRequested(request) => {
                let key = approval_key(&request.request_id);
                if !self.index.contains_key(&key) {
                    self.push(
                        request.turn_id.clone(),
                        key,
                        EntryKind::Approval {
                            request: request.clone(),
                            answer: None,
                            resolved: false,
                        },
                        true,
                    );
                }
            }
            AgentEvent::UserInputRequested(request) => {
                let key = approval_key(&request.request_id);
                if !self.index.contains_key(&key) {
                    self.push(
                        request.turn_id.clone(),
                        key,
                        EntryKind::Question {
                            request: request.clone(),
                            answers: None,
                            resolved: false,
                        },
                        true,
                    );
                }
            }
            AgentEvent::ApprovalResolved { request_id, .. } => {
                if let Some(&ix) = self.index.get(&approval_key(request_id))
                    && let EntryKind::Approval { resolved, .. }
                    | EntryKind::Question { resolved, .. } = &mut self.entries[ix].kind
                {
                    *resolved = true;
                    self.entries[ix].live = false;
                    self.bump();
                }
            }
            AgentEvent::Error {
                message,
                will_retry,
                ..
            } => {
                if *will_retry {
                    self.notice = Some(message.clone());
                    self.bump();
                } else {
                    let turn = self.active_turn.clone().unwrap_or_default();
                    self.push_error(&turn, message.clone());
                }
            }
            AgentEvent::Disconnected { reason } => {
                if self.active_turn.take().is_some() {
                    for entry in &mut self.entries {
                        entry.live = false;
                    }
                    self.push_error("", format!("The agent stopped: {reason}"));
                }
                self.bump();
            }
            AgentEvent::ThreadStarted(_)
            | AgentEvent::ThreadNameUpdated { .. }
            | AgentEvent::PredictionUpdated { .. }
            | AgentEvent::GoalUpdated { .. }
            | AgentEvent::RateLimitsUpdated(_)
            | AgentEvent::AccountChanged
            | AgentEvent::LoginCompleted { .. }
            | AgentEvent::Warning(_) => {}
        }
        self.version != before
    }

    fn bump(&mut self) {
        self.version += 1;
    }

    fn push(&mut self, turn_id: String, key: String, kind: EntryKind, live: bool) {
        self.index.insert(key, self.entries.len());
        self.entries.push(Entry {
            turn_id,
            kind,
            live,
        });
        self.bump();
    }

    fn push_error(&mut self, turn_id: &str, message: String) {
        self.entries.push(Entry {
            turn_id: turn_id.to_string(),
            kind: EntryKind::Error(message),
            live: false,
        });
        self.bump();
    }

    /// The row for `item_id`, made from `make` if it does not exist yet.
    fn ensure(&mut self, turn_id: &str, item_id: &str, make: impl FnOnce() -> Item) -> usize {
        if let Some(&ix) = self.index.get(item_id) {
            return ix;
        }
        self.push(
            turn_id.to_string(),
            item_id.to_string(),
            EntryKind::Item(make()),
            true,
        );
        self.entries.len() - 1
    }

    fn upsert_item(&mut self, turn_id: &str, mut item: Item, live: bool) {
        let id = item.id().to_string();
        if let Some(&ix) = self.index.get(&id) {
            let entry = &mut self.entries[ix];
            // A completed item may arrive without the text its deltas built up.
            merge_streamed(&entry.kind, &mut item);
            entry.kind = EntryKind::Item(item);
            entry.live = live;
            self.bump();
            return;
        }
        // The backend's echo of a message we already show.
        if let Item::User { text, .. } = &item
            && let Some(ix) = self.local_user_row(text)
        {
            let old = match &self.entries[ix].kind {
                EntryKind::Item(old) => old.id().to_string(),
                _ => unreachable!("local rows are user items"),
            };
            self.index.remove(&old);
            self.index.insert(id, ix);
            let entry = &mut self.entries[ix];
            entry.turn_id = turn_id.to_string();
            entry.kind = EntryKind::Item(item);
            entry.live = false;
            self.bump();
            return;
        }
        self.push(turn_id.to_string(), id, EntryKind::Item(item), live);
    }

    /// The oldest optimistic user row, preferring one with the same text.
    fn local_user_row(&self, text: &str) -> Option<usize> {
        let local = |entry: &Entry| match &entry.kind {
            EntryKind::Item(Item::User { id, text, .. }) if id.starts_with(LOCAL_PREFIX) => {
                Some(text.clone())
            }
            _ => None,
        };
        let candidates: Vec<(usize, String)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(ix, entry)| local(entry).map(|t| (ix, t)))
            .collect();
        candidates
            .iter()
            .find(|(_, t)| t.trim() == text.trim())
            .or(candidates.first())
            .map(|(ix, _)| *ix)
    }
}

fn approval_key(request_id: &serde_json::Value) -> String {
    format!("approval:{request_id}")
}

/// Keeps text streamed into `old` when `new` arrives empty, and fills in
/// command output the same way.
fn merge_streamed(old: &EntryKind, new: &mut Item) {
    let EntryKind::Item(old) = old else { return };
    match (old, new) {
        (Item::Agent { text: was, .. }, Item::Agent { text, .. })
        | (Item::Reasoning { text: was, .. }, Item::Reasoning { text, .. })
            if text.is_empty() =>
        {
            *text = was.clone();
        }
        (Item::Command { output: was, .. }, Item::Command { output, .. }) if output.is_empty() => {
            *output = was.clone();
        }
        _ => {}
    }
}

impl ItemStatus {
    pub fn is_done(self) -> bool {
        !matches!(self, ItemStatus::InProgress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ApprovalKind;

    fn started(turn: &str, item: Item) -> AgentEvent {
        AgentEvent::ItemStarted {
            thread_id: "t".into(),
            turn_id: turn.into(),
            item,
        }
    }

    fn completed(turn: &str, item: Item) -> AgentEvent {
        AgentEvent::ItemCompleted {
            thread_id: "t".into(),
            turn_id: turn.into(),
            item,
        }
    }

    fn delta(turn: &str, item: &str, text: &str) -> AgentEvent {
        AgentEvent::AgentMessageDelta {
            thread_id: "t".into(),
            turn_id: turn.into(),
            item_id: item.into(),
            delta: text.into(),
        }
    }

    #[test]
    fn a_streamed_answer_builds_up_then_settles() {
        let mut t = Transcript::new();
        t.push_local_user("hi", vec![]);
        t.begin_pending_turn();
        assert!(t.is_running());
        t.apply(&AgentEvent::TurnStarted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
        });
        t.apply(&started(
            "u1",
            Item::User {
                id: "m1".into(),
                text: "hi".into(),
                attachments: vec![],
            },
        ));
        assert_eq!(t.len(), 1, "the echo replaces the optimistic row");
        t.apply(&delta("u1", "a1", "Hel"));
        t.apply(&delta("u1", "a1", "lo"));
        assert!(t.entries()[1].live);
        t.apply(&completed(
            "u1",
            Item::Agent {
                id: "a1".into(),
                text: String::new(),
            },
        ));
        match &t.entries()[1].kind {
            EntryKind::Item(Item::Agent { text, .. }) => assert_eq!(text, "Hello"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(!t.entries()[1].live);
        t.apply(&AgentEvent::TurnCompleted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
            status: TurnStatus::Completed,
            error: None,
            duration_ms: None,
        });
        assert!(!t.is_running());
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn command_output_streams_into_its_card() {
        let mut t = Transcript::new();
        let cmd = |status, output: &str| Item::Command {
            id: "c1".into(),
            command: "ls".into(),
            cwd: "/".into(),
            status,
            output: output.into(),
            exit_code: None,
            duration_ms: None,
        };
        t.apply(&started("u1", cmd(ItemStatus::InProgress, "")));
        t.apply(&AgentEvent::CommandOutputDelta {
            thread_id: "t".into(),
            turn_id: "u1".into(),
            item_id: "c1".into(),
            delta: "a\nb\n".into(),
        });
        t.apply(&completed("u1", cmd(ItemStatus::Completed, "")));
        match &t.entries()[0].kind {
            EntryKind::Item(Item::Command { output, status, .. }) => {
                assert_eq!(output, "a\nb\n");
                assert_eq!(*status, ItemStatus::Completed);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn approvals_wait_until_answered_and_end_with_the_turn() {
        let mut t = Transcript::new();
        let request = ApprovalRequest {
            request_id: serde_json::json!(7),
            thread_id: "t".into(),
            turn_id: "u1".into(),
            item_id: "c1".into(),
            kind: ApprovalKind::Command {
                command: "rm -rf build".into(),
                cwd: "/".into(),
                reason: None,
            },
        };
        t.apply(&AgentEvent::ApprovalRequested(request.clone()));
        t.apply(&AgentEvent::ApprovalRequested(request));
        assert_eq!(
            t.pending_approvals().count(),
            1,
            "a repeated request is one row"
        );
        t.answer_approval(&serde_json::json!(7), ApprovalDecision::Accept);
        assert_eq!(t.pending_approvals().count(), 0);
    }

    #[test]
    fn questions_wait_for_answers_then_resolve() {
        let mut t = Transcript::new();
        let request = UserInputRequest {
            request_id: serde_json::json!("q1"),
            thread_id: "t".into(),
            turn_id: "u1".into(),
            item_id: "i".into(),
            questions: vec![crate::model::Question {
                id: "db".into(),
                header: "Database".into(),
                question: "Which database?".into(),
                options: vec![("Postgres".into(), "".into())],
                allow_other: true,
                secret: false,
            }],
        };
        t.apply(&AgentEvent::UserInputRequested(request));
        assert_eq!(t.pending_questions().count(), 1);
        t.answer_questions(
            &serde_json::json!("q1"),
            vec![("db".into(), vec!["Postgres".into()])],
        );
        assert_eq!(t.pending_questions().count(), 0);
        t.apply(&AgentEvent::ApprovalResolved {
            thread_id: "t".into(),
            request_id: serde_json::json!("q1"),
        });
        assert!(matches!(
            &t.entries()[0].kind,
            EntryKind::Question { resolved: true, .. }
        ));
    }

    #[test]
    fn rate_limit_updates_merge_sparsely() {
        use crate::model::{RateLimits, RateWindow};
        let mut limits = RateLimits {
            primary: Some(RateWindow {
                used_percent: 10.,
                window_mins: Some(300),
                resets_at: None,
            }),
            plan: Some("pro".into()),
            ..Default::default()
        };
        limits.merge(RateLimits {
            secondary: Some(RateWindow {
                used_percent: 40.,
                window_mins: Some(10080),
                resets_at: None,
            }),
            ..Default::default()
        });
        assert_eq!(limits.plan.as_deref(), Some("pro"));
        assert_eq!(limits.highest_used(), Some(40.));
        assert_eq!(limits.primary.unwrap().label(), "5-hour limit");
        assert_eq!(crate::model::plan_label("prolite"), "Pro Lite");
    }

    #[test]
    fn a_failed_turn_says_why_and_stops_running() {
        let mut t = Transcript::new();
        t.apply(&AgentEvent::TurnStarted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
        });
        t.apply(&delta("u1", "a1", "partial"));
        t.apply(&AgentEvent::TurnCompleted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
            status: TurnStatus::Failed,
            error: Some("quota exceeded".into()),
            duration_ms: None,
        });
        assert!(!t.is_running());
        assert!(t.entries().iter().all(|e| !e.live));
        assert!(
            matches!(&t.entries().last().unwrap().kind, EntryKind::Error(m) if m == "quota exceeded")
        );
    }

    #[test]
    fn a_dead_backend_ends_the_running_turn() {
        let mut t = Transcript::new();
        t.apply(&AgentEvent::TurnStarted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
        });
        t.apply(&AgentEvent::Disconnected {
            reason: "exit 1".into(),
        });
        assert!(!t.is_running());
    }

    #[test]
    fn history_rebuilds_rows_in_order() {
        let t = Transcript::from_history([(
            "u1".to_string(),
            TurnStatus::Completed,
            vec![
                Item::User {
                    id: "m1".into(),
                    text: "q".into(),
                    attachments: vec![],
                },
                Item::Agent {
                    id: "a1".into(),
                    text: "a".into(),
                },
            ],
        )]);
        assert_eq!(t.len(), 2);
        assert!(!t.is_running());
    }
}
