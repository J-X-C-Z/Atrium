//! One run of a template: where each step stands, its tries and what the
//! checks said, and what the user decided at each stop. Pure state: the
//! runner calls these as agents start and finish and checks come back, and
//! saves the run to `.atrium/run.json` after each change.
//!
//! A run's steps are the template's, in order, plus one step per item for a
//! step that repeats (`each`): `<step>:<item>`, placed right after it. The
//! repeating step itself does no work; it is done when all its items are.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{Check, Choice, Step, Template, condition_holds};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Steps start as soon as they are ready.
    Running,
    /// The user paused it: running steps finish, no new ones start.
    Paused,
    /// A step waits on the user: a stop to look, or a step out of tries.
    Waiting,
    Done,
    /// The user stopped it for good.
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    /// An agent works on it; for a repeating step, its items are under way.
    Running,
    /// The agent's turn ended; the checks run.
    Checking,
    /// It passed and is a stop: the user looks before the run goes on.
    Review,
    Done,
    /// Out of tries; the user decides.
    Failed,
    Skipped,
}

/// What one check found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    /// The check in words, such as "paper/main.pdf exists".
    pub what: String,
    pub passed: bool,
    /// Why it failed, for the agent's next try and for the user.
    #[serde(default)]
    pub detail: String,
    /// A reviewer's word on which earlier step is at fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_back: Option<String>,
}

impl CheckResult {
    pub fn pass(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            passed: true,
            detail: String::new(),
            send_back: None,
        }
    }

    pub fn fail(what: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            passed: false,
            detail: detail.into(),
            send_back: None,
        }
    }
}

/// One go at a step by an agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub agent: Option<String>,
    /// The agent's conversation, once it has one.
    pub thread: Option<String>,
    /// The turn started for this attempt, once the agent named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    /// What the agent was told to fix, from failed checks or the user.
    #[serde(default)]
    pub feedback: Vec<String>,
    /// What the agent reported when its turn ended. Later steps' prompts
    /// carry a short form, since every try starts a new conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default)]
    pub checks: Vec<CheckResult>,
}

impl Attempt {
    pub fn passed(&self) -> bool {
        self.finished_ms.is_some() && self.checks.iter().all(|c| c.passed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRun {
    /// The template step's id, or `<step>:<item>` for one item of a
    /// repeating step.
    pub id: String,
    /// The item, for one item of a repeating step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Choice>,
    pub status: StepStatus,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    /// Notes for the next try, gathered since the last one started.
    #[serde(default)]
    pub feedback: Vec<String>,
    /// Tries before this index do not count against the step's allowance:
    /// the user gave it a fresh start.
    #[serde(default)]
    pub budget_from: usize,
    /// For a repeating step: its items have been laid out.
    #[serde(default)]
    pub expanded: bool,
}

impl StepRun {
    fn new(id: String, item: Option<Choice>, status: StepStatus) -> Self {
        Self {
            id,
            item,
            status,
            attempts: Vec::new(),
            feedback: Vec::new(),
            budget_from: 0,
            expanded: false,
        }
    }

    /// The template step this is, or is an item of.
    pub fn spec(&self) -> &str {
        self.id.split(':').next().unwrap_or(&self.id)
    }

    pub fn is_item(&self) -> bool {
        self.item.is_some()
    }

    fn tries(&self) -> usize {
        self.attempts.len().saturating_sub(self.budget_from)
    }

    pub fn last(&self) -> Option<&Attempt> {
        self.attempts.last()
    }

    pub fn thread(&self) -> Option<&str> {
        self.attempts.iter().rev().find_map(|a| a.thread.as_deref())
    }
}

/// What the user decided at a stop: the option picked, and a note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<Choice>,
    #[serde(default)]
    pub note: String,
    pub at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// The template as it was at the start, so later edits do not change
    /// a run under way.
    pub template: Template,
    /// The user's answers by input key; files as paths under `inputs/`,
    /// one per line.
    pub inputs: HashMap<String, String>,
    pub state: RunState,
    pub steps: Vec<StepRun>,
    /// What the user decided at stops, in order.
    #[serde(default)]
    pub decisions: Vec<Decision>,
    /// Lessons the user kept from earlier runs of this template, given to
    /// every step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experience: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("no step {0}")]
    NoStep(String),
    #[error("step {step} is {status:?}, not {expected}")]
    Status {
        step: String,
        status: StepStatus,
        expected: &'static str,
    },
    #[error("{0} is required")]
    MissingInput(String),
    #[error("step {0} cannot be skipped: it is not optional")]
    NotOptional(String),
    #[error("step {0} asks you to pick an option first")]
    ChoiceNeeded(String),
    #[error("step {0} has no items to repeat over")]
    NoItems(String),
}

/// What [`Run::checked`] did with a step's results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Passed, and stops for the user.
    Review,
    /// Failed; another try starts with the reasons.
    Retry,
    /// Failed in a step it reviews: that step goes again with the reasons,
    /// then this one checks again.
    SentBack {
        to: String,
    },
    /// Out of tries: the run waits on the user.
    Failed,
}

impl Run {
    /// A new run. Optional steps in `skip`, and steps whose condition does
    /// not hold for the inputs, are skipped from the start.
    pub fn new(
        template: Template,
        inputs: HashMap<String, String>,
        skip: &[String],
        now_ms: i64,
    ) -> Result<Self, RunError> {
        let mut values = inputs.clone();
        for input in &template.inputs {
            if !values.contains_key(&input.key) {
                let default = input
                    .default
                    .clone()
                    .or_else(|| input.options.first().cloned());
                if let Some(d) = default {
                    values.insert(input.key.clone(), d);
                }
            }
        }
        for input in &template.inputs {
            let shown = condition_holds(input.when.as_deref(), &values);
            let empty = values.get(&input.key).is_none_or(|v| v.trim().is_empty());
            if input.required && shown && empty {
                return Err(RunError::MissingInput(input.label.clone()));
            }
        }
        let mut steps = Vec::new();
        for step in &template.steps {
            if skip.contains(&step.id) && !step.optional {
                return Err(RunError::NotOptional(step.id.clone()));
            }
            let skipped =
                skip.contains(&step.id) || !condition_holds(step.when.as_deref(), &values);
            steps.push(StepRun::new(
                step.id.clone(),
                None,
                if skipped {
                    StepStatus::Skipped
                } else {
                    StepStatus::Pending
                },
            ));
        }
        Ok(Self {
            template,
            inputs: values,
            state: RunState::Running,
            steps,
            decisions: Vec::new(),
            experience: None,
            created_ms: now_ms,
            updated_ms: now_ms,
        })
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    fn ix(&self, id: &str) -> Result<usize, RunError> {
        self.steps
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| RunError::NoStep(id.to_string()))
    }

    /// The template step a run step is, or is an item of.
    pub fn spec(&self, id: &str) -> Option<&Step> {
        let step = self.step(id)?;
        self.template.step(step.spec())
    }

    fn spec_at(&self, ix: usize) -> &Step {
        self.template
            .step(self.steps[ix].spec())
            .expect("every run step has its template step")
    }

    /// Ids of the template steps `id` needs.
    fn needs(&self, id: &str) -> Vec<String> {
        let spec = id.split(':').next().unwrap_or(id);
        self.template
            .steps
            .iter()
            .position(|s| s.id == spec)
            .map(|t| self.template.needs_of(t))
            .unwrap_or_default()
    }

    /// `{{item.id}}` and `{{item.title}}` of a step's item filled into `text`.
    pub fn fill_item(&self, id: &str, text: &str) -> String {
        match self.step(id).and_then(|s| s.item.as_ref()) {
            Some(item) => text
                .replace("{{item.id}}", &item.id)
                .replace("{{item.title}}", &item.title),
            None => text.to_string(),
        }
    }

    /// The checks step `id` runs for this run's inputs (and item).
    pub fn checks_of(&self, id: &str) -> Vec<Check> {
        let Some(spec) = self.spec(id) else {
            return Vec::new();
        };
        spec.checks
            .iter()
            .filter(|c| condition_holds(c.when.as_deref(), &self.inputs))
            .map(|c| {
                let text = serde_json::to_string(&c.check).unwrap_or_default();
                serde_json::from_str(&self.fill_item(id, &text)).unwrap_or_else(|_| c.check.clone())
            })
            .collect()
    }

    /// The files step `id` must leave, with its item filled in.
    pub fn outputs_of(&self, id: &str) -> Vec<String> {
        self.spec(id)
            .map(|s| s.outputs.iter().map(|o| self.fill_item(id, o)).collect())
            .unwrap_or_default()
    }

    /// The step's title, with its item's.
    pub fn title_of(&self, id: &str) -> String {
        let Some(step) = self.step(id) else {
            return id.to_string();
        };
        let title = self
            .template
            .step(step.spec())
            .map(|s| s.title.clone())
            .unwrap_or_default();
        match &step.item {
            Some(item) => format!("{} · {title}", item.title),
            None => title,
        }
    }

    pub fn step(&self, id: &str) -> Option<&StepRun> {
        self.steps.iter().find(|s| s.id == id)
    }

    fn expect(&self, ix: usize, ok: &[StepStatus], expected: &'static str) -> Result<(), RunError> {
        let s = &self.steps[ix];
        if ok.contains(&s.status) {
            Ok(())
        } else {
            Err(RunError::Status {
                step: s.id.clone(),
                status: s.status,
                expected,
            })
        }
    }

    fn finished_specs(&self) -> HashSet<String> {
        self.steps
            .iter()
            .filter(|s| !s.is_item() && matches!(s.status, StepStatus::Done | StepStatus::Skipped))
            .map(|s| s.id.clone())
            .collect()
    }

    fn needs_met(&self, id: &str, finished: &HashSet<String>) -> bool {
        self.needs(id).iter().all(|n| finished.contains(n))
    }

    /// Steps an agent can start now: pending, with everything they need
    /// done or skipped, while the run runs. A repeating step is never in
    /// it: see [`Run::to_expand`].
    pub fn ready(&self) -> Vec<String> {
        if self.state != RunState::Running {
            return Vec::new();
        }
        let finished = self.finished_specs();
        self.steps
            .iter()
            .filter(|s| {
                let laid_out = !s.is_item() || self.step(s.spec()).is_some_and(|p| p.expanded);
                s.status == StepStatus::Pending
                    && laid_out
                    && !(s.item.is_none() && self.spec_at_id(&s.id).each.is_some())
                    && self.needs_met(&s.id, &finished)
            })
            .map(|s| s.id.clone())
            .collect()
    }

    fn spec_at_id(&self, id: &str) -> &Step {
        let ix = self.steps.iter().position(|s| s.id == id).unwrap_or(0);
        self.spec_at(ix)
    }

    /// Repeating steps whose needs are met and whose items are not laid
    /// out yet: the runner reads each one's list and calls [`Run::expand`].
    pub fn to_expand(&self) -> Vec<(String, String)> {
        if self.state != RunState::Running {
            return Vec::new();
        }
        let finished = self.finished_specs();
        self.steps
            .iter()
            .filter(|s| s.item.is_none() && s.status == StepStatus::Pending && !s.expanded)
            .filter_map(|s| {
                let each = self.spec_at_id(&s.id).each.as_ref()?;
                self.needs_met(&s.id, &finished)
                    .then(|| (s.id.clone(), each.from.clone()))
            })
            .collect()
    }

    /// Lays out one step per item after repeating step `id`. Items it had
    /// before are kept (with their tries) when their id is still listed.
    pub fn expand(&mut self, id: &str, items: Vec<Choice>, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        let max = self.spec_at(ix).each.as_ref().map_or(8, |e| e.max);
        let items: Vec<Choice> = items.into_iter().take(max).collect();
        if items.is_empty() {
            let from = self
                .spec_at(ix)
                .each
                .as_ref()
                .map(|e| e.from.clone())
                .unwrap_or_default();
            self.steps[ix].status = StepStatus::Failed;
            self.steps[ix].attempts.push(Attempt {
                agent: None,
                thread: None,
                turn: None,
                started_ms: now_ms,
                finished_ms: Some(now_ms),
                feedback: Vec::new(),
                answer: None,
                checks: vec![CheckResult::fail(from, "no items listed")],
            });
            self.state = RunState::Waiting;
            return Err(RunError::NoItems(id.to_string()));
        }
        let prefix = format!("{id}:");
        let old: Vec<StepRun> = self
            .steps
            .iter()
            .filter(|s| s.id.starts_with(&prefix))
            .cloned()
            .collect();
        self.steps.retain(|s| !s.id.starts_with(&prefix));
        let at = self.ix(id)? + 1;
        let children: Vec<StepRun> = items
            .into_iter()
            .map(|item| {
                let child_id = format!("{id}:{}", item.id);
                match old.iter().find(|o| o.id == child_id) {
                    Some(kept) => StepRun {
                        item: Some(item),
                        ..kept.clone()
                    },
                    None => StepRun::new(child_id, Some(item), StepStatus::Pending),
                }
            })
            .collect();
        self.steps.splice(at..at, children);
        let parent = &mut self.steps[at - 1];
        parent.expanded = true;
        parent.status = StepStatus::Running;
        self.updated_ms = now_ms;
        self.roll_up();
        Ok(())
    }

    /// The items of repeating step `id`.
    pub fn items_of(&self, id: &str) -> Vec<&StepRun> {
        let prefix = format!("{id}:");
        self.steps
            .iter()
            .filter(|s| s.id.starts_with(&prefix))
            .collect()
    }

    /// A repeating step is done when all its items are.
    fn roll_up(&mut self) {
        let parents: Vec<String> = self
            .steps
            .iter()
            .filter(|s| s.expanded && s.item.is_none())
            .map(|s| s.id.clone())
            .collect();
        for parent in parents {
            let items: Vec<StepStatus> = self.items_of(&parent).iter().map(|s| s.status).collect();
            let ix = self.steps.iter().position(|s| s.id == parent).unwrap();
            if self.steps[ix].status == StepStatus::Skipped {
                continue;
            }
            self.steps[ix].status = if items
                .iter()
                .all(|s| matches!(s, StepStatus::Done | StepStatus::Skipped))
            {
                StepStatus::Done
            } else {
                StepStatus::Running
            };
        }
    }

    /// An agent starts on `id`; the notes gathered for it go with this try.
    pub fn begin(&mut self, id: &str, agent: Option<String>, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(ix, &[StepStatus::Pending], "pending")?;
        let step = &mut self.steps[ix];
        let feedback = std::mem::take(&mut step.feedback);
        step.attempts.push(Attempt {
            agent,
            thread: None,
            turn: None,
            started_ms: now_ms,
            finished_ms: None,
            feedback,
            answer: None,
            checks: Vec::new(),
        });
        step.status = StepStatus::Running;
        self.updated_ms = now_ms;
        Ok(())
    }

    pub fn set_thread(&mut self, id: &str, thread: String) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        if let Some(a) = self.steps[ix].attempts.last_mut() {
            a.thread = Some(thread);
        }
        Ok(())
    }

    pub fn set_turn(&mut self, id: &str, turn: String) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        if let Some(a) = self.steps[ix].attempts.last_mut() {
            a.turn = Some(turn);
        }
        Ok(())
    }

    /// Keeps what the agent reported at the end of its turn. An empty
    /// report clears it.
    pub fn set_answer(&mut self, id: &str, answer: impl Into<String>) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        let answer = answer.into();
        if let Some(attempt) = self.steps[ix].attempts.last_mut() {
            attempt.answer = (!answer.trim().is_empty()).then_some(answer);
        }
        Ok(())
    }

    /// The last answer a step's agent gave.
    pub fn answer_of(&self, id: &str) -> Option<&str> {
        self.step(id)?
            .attempts
            .iter()
            .rev()
            .find_map(|attempt| attempt.answer.as_deref())
    }

    /// Finished steps before `id` and what they reported, in order. A later
    /// step's prompt carries these because each try is a new conversation.
    pub fn answers_before(&self, id: &str) -> Vec<(String, String)> {
        let Some(pos) = self.steps.iter().position(|step| step.id == id) else {
            return Vec::new();
        };
        self.steps[..pos]
            .iter()
            .filter(|step| step.status == StepStatus::Done)
            .filter_map(|step| {
                step.attempts
                    .iter()
                    .rev()
                    .find_map(|attempt| attempt.answer.as_ref())
                    .map(|answer| (self.title_of(&step.id), answer.clone()))
            })
            .collect()
    }

    /// The agent's turn on `id` ended; its checks run next.
    pub fn turn_finished(&mut self, id: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(ix, &[StepStatus::Running], "running")?;
        self.steps[ix].status = StepStatus::Checking;
        self.updated_ms = now_ms;
        Ok(())
    }

    /// Whether a step stops for the user once it passes.
    fn stops(&self, ix: usize) -> bool {
        let spec = self.spec_at(ix);
        spec.gate || spec.decide.is_some()
    }

    /// The checks on `id` came back.
    pub fn checked(
        &mut self,
        id: &str,
        results: Vec<CheckResult>,
        now_ms: i64,
    ) -> Result<Outcome, RunError> {
        let ix = self.ix(id)?;
        self.expect(ix, &[StepStatus::Checking], "checking")?;
        let passed = results.iter().all(|r| r.passed);
        let faults: Vec<String> = results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| {
                if r.detail.is_empty() {
                    r.what.clone()
                } else {
                    format!("{}: {}", r.what, r.detail)
                }
            })
            .collect();
        let named: Option<String> = results
            .iter()
            .filter(|r| !r.passed)
            .find_map(|r| r.send_back.clone());
        {
            let step = &mut self.steps[ix];
            if let Some(a) = step.attempts.last_mut() {
                a.finished_ms = Some(now_ms);
                a.checks = results;
            }
        }
        self.updated_ms = now_ms;
        let spec = self.spec_at(ix).clone();
        if passed {
            if self.stops(ix) {
                self.steps[ix].status = StepStatus::Review;
                self.state = RunState::Waiting;
                return Ok(Outcome::Review);
            }
            self.steps[ix].status = StepStatus::Done;
            self.finish_if_done();
            return Ok(Outcome::Done);
        }
        let allowance = self.template.attempts_of(&spec) as usize;
        if self.steps[ix].tries() >= allowance {
            self.steps[ix].status = StepStatus::Failed;
            self.state = RunState::Waiting;
            return Ok(Outcome::Failed);
        }
        if spec.fixes.is_empty() {
            let step = &mut self.steps[ix];
            step.feedback = faults;
            step.status = StepStatus::Pending;
            return Ok(Outcome::Retry);
        }
        let target = named
            .filter(|n| {
                let named_spec = n.split(':').next().unwrap_or(n);
                spec.fixes.iter().any(|f| f == named_spec) && self.step(n).is_some()
            })
            .unwrap_or_else(|| spec.fixes[0].clone());
        self.send_back(&target, faults, ix)?;
        Ok(Outcome::SentBack { to: target })
    }

    /// Reopens `target` with `faults` to fix, and every step that depends
    /// on it, down to and including the reviewer `from`. A repeating step's
    /// items each get the faults.
    fn send_back(
        &mut self,
        target: &str,
        faults: Vec<String>,
        from: usize,
    ) -> Result<(), RunError> {
        let from_id = self.steps[from].id.clone();
        let reopened = self.reopen(target);
        let receivers: Vec<usize> = {
            let items: Vec<usize> = self
                .steps
                .iter()
                .enumerate()
                .filter(|(_, s)| s.id.starts_with(&format!("{target}:")))
                .map(|(i, _)| i)
                .collect();
            if items.is_empty() {
                vec![self.ix(target)?]
            } else {
                items
            }
        };
        for i in receivers {
            let step = &mut self.steps[i];
            step.feedback.extend(faults.iter().cloned());
            // Fixing is a new round for the target, not a failed try of it.
            step.budget_from = step.attempts.len();
        }
        if !reopened.contains(&from_id) {
            let f = self.ix(&from_id)?;
            self.steps[f].status = StepStatus::Pending;
        }
        Ok(())
    }

    /// Sets `id` and everything that depends on it, directly or not, back
    /// to pending. A repeating step reopened lays its items out again from
    /// its list (keeping the history of items still listed); one item
    /// reopened goes again on its own. Returns the ids reopened.
    fn reopen(&mut self, id: &str) -> Vec<String> {
        let spec = id.split(':').next().unwrap_or(id).to_string();
        let item_only = id.contains(':');
        let mut affected: HashSet<String> = [spec.clone()].into();
        loop {
            let before = affected.len();
            for (ix, t) in self.template.steps.iter().enumerate() {
                if !affected.contains(&t.id)
                    && self
                        .template
                        .needs_of(ix)
                        .iter()
                        .any(|n| affected.contains(n))
                {
                    affected.insert(t.id.clone());
                }
            }
            if affected.len() == before {
                break;
            }
        }
        if item_only {
            affected.remove(&spec);
        }
        let mut out = Vec::new();
        for s in &mut self.steps {
            if s.status == StepStatus::Skipped {
                continue;
            }
            if s.id == id || affected.contains(s.spec()) {
                s.status = StepStatus::Pending;
                s.expanded = false;
                out.push(s.id.clone());
            }
        }
        if item_only && let Some(p) = self.steps.iter_mut().find(|s| s.id == spec) {
            // The repeating step waits on the item again.
            p.status = StepStatus::Running;
        }
        out
    }

    fn finish_if_done(&mut self) {
        self.roll_up();
        if self
            .steps
            .iter()
            .all(|s| matches!(s.status, StepStatus::Done | StepStatus::Skipped))
        {
            self.state = RunState::Done;
        } else if self.state == RunState::Waiting && !self.anything_waits() {
            self.state = RunState::Running;
        }
    }

    fn anything_waits(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s.status, StepStatus::Review | StepStatus::Failed))
    }

    /// The options a deciding step offers, as the runner or the UI read
    /// them from its options file.
    pub fn options_file(&self, id: &str) -> Option<String> {
        self.spec(id)
            .and_then(|s| s.decide.as_ref())
            .map(|d| self.fill_item(id, &d.options))
    }

    /// The user looked at a stop and lets the run go on, with the option
    /// they picked (required where the step offers options) and a note,
    /// which every later step's prompt carries.
    pub fn approve(
        &mut self,
        id: &str,
        choice: Option<Choice>,
        note: &str,
        now_ms: i64,
    ) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(ix, &[StepStatus::Review], "waiting for you")?;
        if self.spec_at(ix).decide.is_some() && choice.is_none() {
            return Err(RunError::ChoiceNeeded(id.to_string()));
        }
        if choice.is_some() || !note.trim().is_empty() {
            self.decisions.retain(|d| d.step != id);
            self.decisions.push(Decision {
                step: id.to_string(),
                choice,
                note: note.trim().to_string(),
                at_ms: now_ms,
            });
        }
        self.steps[ix].status = StepStatus::Done;
        self.updated_ms = now_ms;
        if self.state == RunState::Waiting && !self.anything_waits() {
            self.state = RunState::Running;
        }
        self.finish_if_done();
        Ok(())
    }

    /// The user asks for changes to a step that stopped, failed, or is
    /// already done: it goes again with `note`, and so does every step
    /// after it that used its work. A decision made at it is undone.
    pub fn redo(&mut self, id: &str, note: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(
            ix,
            &[StepStatus::Review, StepStatus::Failed, StepStatus::Done],
            "stopped, failed or done",
        )?;
        let reopened = self.reopen(id);
        self.decisions.retain(|d| !reopened.contains(&d.step));
        let ix = self.ix(id)?;
        let step = &mut self.steps[ix];
        if !note.trim().is_empty() {
            step.feedback.push(note.trim().to_string());
        }
        step.budget_from = step.attempts.len();
        self.updated_ms = now_ms;
        if matches!(self.state, RunState::Waiting | RunState::Done) && !self.anything_waits() {
            self.state = RunState::Running;
        }
        Ok(())
    }

    /// The user skips an optional step (or one item of a repeating step)
    /// that failed or has not started.
    pub fn skip(&mut self, id: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        if !self.spec_at(ix).optional && !self.steps[ix].is_item() {
            return Err(RunError::NotOptional(id.to_string()));
        }
        self.expect(
            ix,
            &[StepStatus::Pending, StepStatus::Failed],
            "pending or failed",
        )?;
        self.steps[ix].status = StepStatus::Skipped;
        self.updated_ms = now_ms;
        if self.state == RunState::Waiting && !self.anything_waits() {
            self.state = RunState::Running;
        }
        self.finish_if_done();
        Ok(())
    }

    pub fn pause(&mut self, now_ms: i64) {
        if self.state == RunState::Running {
            self.state = RunState::Paused;
            self.updated_ms = now_ms;
        }
    }

    pub fn resume(&mut self, now_ms: i64) {
        if self.state == RunState::Paused {
            self.state = if self.anything_waits() {
                RunState::Waiting
            } else {
                RunState::Running
            };
            self.updated_ms = now_ms;
        }
    }

    pub fn stop(&mut self, now_ms: i64) {
        if self.state != RunState::Done {
            self.state = RunState::Stopped;
            self.updated_ms = now_ms;
        }
    }

    /// After Atrium restarts: steps cut off mid-turn or mid-check start
    /// their try again, without it counting against them.
    pub fn recover(&mut self, now_ms: i64) {
        let ids: Vec<String> = self
            .steps
            .iter()
            .filter(|step| {
                !step.expanded && matches!(step.status, StepStatus::Running | StepStatus::Checking)
            })
            .map(|step| step.id.clone())
            .collect();
        for id in ids {
            let _ = self.recover_step(&id, now_ms);
        }
    }

    /// Forget one interrupted try and make its step ready again. The try
    /// does not count against the allowance.
    pub fn recover_step(&mut self, id: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(ix, &[StepStatus::Running, StepStatus::Checking], "running")?;
        let step = &mut self.steps[ix];
        if let Some(a) = step.attempts.last()
            && a.finished_ms.is_none()
        {
            step.feedback = a.feedback.clone();
            step.attempts.pop();
        }
        step.status = StepStatus::Pending;
        self.updated_ms = now_ms;
        Ok(())
    }

    /// Steps done, and steps in all: items count, the repeating steps that
    /// hold them and skipped steps do not.
    pub fn progress(&self) -> (usize, usize) {
        let counted: Vec<_> = self
            .steps
            .iter()
            .filter(|s| s.status != StepStatus::Skipped && !s.expanded)
            .collect();
        (
            counted
                .iter()
                .filter(|s| s.status == StepStatus::Done)
                .count(),
            counted.len(),
        )
    }

    /// Decisions made at steps before `id`, for its prompt.
    pub fn decisions_before(&self, id: &str) -> Vec<&Decision> {
        let Some(pos) = self.steps.iter().position(|s| s.id == id) else {
            return Vec::new();
        };
        let before: HashSet<&str> = self.steps[..pos].iter().map(|s| s.id.as_str()).collect();
        self.decisions
            .iter()
            .filter(|d| before.contains(d.step.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Template;

    fn template() -> Template {
        Template::parse(
            r#"
id = "t"
name = "T"
[[inputs]]
key = "topic"
label = "Topic"
kind = "text"
required = true
[[inputs]]
key = "lang"
label = "Language"
kind = "choice"
options = ["zh", "en"]
[[steps]]
id = "plan"
title = "Plan"
prompt = "p"
gate = true
[[steps]]
id = "figures"
title = "Figures"
prompt = "f"
optional = true
[[steps]]
id = "write"
title = "Write"
prompt = "w"
needs = ["plan"]
[[steps]]
id = "abstract_en"
title = "English abstract"
prompt = "a"
needs = ["write"]
when = "lang=en"
[[steps]]
id = "verify"
title = "Verify"
prompt = "v"
needs = ["write"]
fixes = "write"
attempts = 3
"#,
        )
        .unwrap()
    }

    fn pass() -> Vec<CheckResult> {
        vec![CheckResult::pass("ok")]
    }

    fn fail(why: &str) -> Vec<CheckResult> {
        vec![CheckResult::fail("paper.pdf exists", why)]
    }

    fn go(run: &mut Run, id: &str, results: Vec<CheckResult>) -> Outcome {
        run.begin(id, None, 1).unwrap();
        run.turn_finished(id, 2).unwrap();
        run.checked(id, results, 3).unwrap()
    }

    fn inputs() -> HashMap<String, String> {
        [("topic".to_string(), "x".to_string())].into()
    }

    #[test]
    fn required_inputs_and_conditions_shape_the_run() {
        assert!(matches!(
            Run::new(template(), HashMap::new(), &[], 0),
            Err(RunError::MissingInput(_))
        ));
        let run = Run::new(template(), inputs(), &[], 0).unwrap();
        assert_eq!(
            run.inputs["lang"], "zh",
            "a choice defaults to its first option"
        );
        assert_eq!(run.step("abstract_en").unwrap().status, StepStatus::Skipped);
        assert!(matches!(
            Run::new(template(), inputs(), &["write".into()], 0),
            Err(RunError::NotOptional(_))
        ));
    }

    #[test]
    fn a_stop_waits_for_the_user_then_steps_follow_their_needs() {
        let mut run = Run::new(template(), inputs(), &["figures".into()], 0).unwrap();
        assert_eq!(run.ready(), vec!["plan"]);
        assert_eq!(go(&mut run, "plan", pass()), Outcome::Review);
        assert_eq!(run.state, RunState::Waiting);
        assert!(run.ready().is_empty());
        run.approve("plan", None, "", 4).unwrap();
        assert_eq!(run.state, RunState::Running);
        assert_eq!(run.ready(), vec!["write"]);
    }

    #[test]
    fn a_failed_try_goes_again_with_its_reasons_until_out_of_tries() {
        let mut run = Run::new(template(), inputs(), &[], 0).unwrap();
        go(&mut run, "plan", pass());
        run.approve("plan", None, "", 4).unwrap();
        assert_eq!(go(&mut run, "write", fail("missing")), Outcome::Retry);
        assert_eq!(
            run.step("write").unwrap().feedback,
            vec!["paper.pdf exists: missing"]
        );
        run.begin("write", None, 5).unwrap();
        assert_eq!(
            run.step("write").unwrap().last().unwrap().feedback,
            vec!["paper.pdf exists: missing"],
            "the reasons go with the next try"
        );
        run.turn_finished("write", 6).unwrap();
        assert_eq!(
            run.checked("write", fail("still"), 7).unwrap(),
            Outcome::Failed
        );
        assert_eq!(run.state, RunState::Waiting);
        run.redo("write", "use the template in paper/", 8).unwrap();
        assert_eq!(run.state, RunState::Running);
        assert_eq!(go(&mut run, "write", pass()), Outcome::Done);
    }

    #[test]
    fn a_reviewer_sends_faults_back_to_the_step_it_reviews() {
        let mut run = Run::new(template(), inputs(), &["figures".into()], 0).unwrap();
        go(&mut run, "plan", pass());
        run.approve("plan", None, "", 4).unwrap();
        go(&mut run, "write", pass());
        assert_eq!(
            go(&mut run, "verify", fail("page 31")),
            Outcome::SentBack { to: "write".into() }
        );
        assert_eq!(run.step("write").unwrap().status, StepStatus::Pending);
        assert_eq!(run.step("verify").unwrap().status, StepStatus::Pending);
        assert_eq!(run.ready(), vec!["write"], "the writer fixes first");
        go(&mut run, "write", pass());
        assert_eq!(go(&mut run, "verify", pass()), Outcome::Done);
        assert_eq!(run.state, RunState::Done);
        assert_eq!(run.progress(), (3, 3));
    }

    #[test]
    fn redoing_a_done_step_reopens_what_used_it() {
        let mut run = Run::new(template(), inputs(), &["figures".into()], 0).unwrap();
        go(&mut run, "plan", pass());
        run.approve("plan", None, "", 4).unwrap();
        go(&mut run, "write", pass());
        go(&mut run, "verify", pass());
        assert_eq!(run.state, RunState::Done);
        run.redo("plan", "use a queueing model", 9).unwrap();
        assert_eq!(run.state, RunState::Running);
        for id in ["plan", "write", "verify"] {
            assert_eq!(run.step(id).unwrap().status, StepStatus::Pending, "{id}");
        }
        assert_eq!(
            run.step("plan").unwrap().feedback,
            vec!["use a queueing model"]
        );
    }

    #[test]
    fn a_restart_retries_cut_off_steps_without_using_a_try() {
        let mut run = Run::new(template(), inputs(), &[], 0).unwrap();
        run.begin("plan", None, 1).unwrap();
        run.set_thread("plan", "th-1".into()).unwrap();
        run.set_turn("plan", "turn-1".into()).unwrap();
        let saved = Run::from_json(&run.to_json()).unwrap();
        assert_eq!(
            saved.step("plan").unwrap().last().unwrap().turn.as_deref(),
            Some("turn-1")
        );
        run.recover(2);
        let plan = run.step("plan").unwrap();
        assert_eq!(plan.status, StepStatus::Pending);
        assert!(plan.attempts.is_empty());
        let saved = Run::from_json(&run.to_json()).unwrap();
        assert_eq!(saved, run);
    }

    #[test]
    fn recovering_one_step_leaves_another_running_try_alone() {
        let mut run = Run::new(template(), inputs(), &[], 0).unwrap();
        run.begin("plan", None, 1).unwrap();
        run.set_thread("plan", "th-plan".into()).unwrap();
        run.set_turn("plan", "turn-plan".into()).unwrap();
        run.begin("write", None, 2).unwrap();
        run.recover_step("write", 3).unwrap();
        let plan = run.step("plan").unwrap();
        assert_eq!(plan.status, StepStatus::Running);
        assert_eq!(plan.last().unwrap().turn.as_deref(), Some("turn-plan"));
    }

    #[test]
    fn a_finished_step_keeps_its_answer_for_later_steps() {
        let mut run = Run::new(template(), inputs(), &[], 0).unwrap();
        go(&mut run, "plan", pass());
        run.set_answer("plan", "A queueing model with two servers.")
            .unwrap();
        assert_eq!(
            run.answer_of("plan"),
            Some("A queueing model with two servers.")
        );
        run.approve("plan", None, "", 4).unwrap();
        assert_eq!(
            run.answers_before("write"),
            vec![(
                "Plan".to_string(),
                "A queueing model with two servers.".to_string()
            )]
        );
        assert!(
            run.answers_before("plan").is_empty(),
            "a step does not report to itself"
        );
        let saved = Run::from_json(&run.to_json()).unwrap();
        assert_eq!(saved, run, "the answer survives a save");

        run.set_answer("plan", "   ").unwrap();
        assert_eq!(run.answer_of("plan"), None, "an empty answer clears it");
    }

    fn contest() -> Template {
        Template::parse(
            r#"
id = "c"
name = "C"
[[steps]]
id = "analysis"
title = "Analysis"
prompt = "a"
outputs = ["reports/subproblems.json"]
[[steps]]
id = "choose"
title = "Choose models"
prompt = "c"
decide = { options = "reports/options.json", question = "Which route?" }
[[steps]]
id = "solve"
title = "Solve"
prompt = "Solve {{item.title}}."
outputs = ["results/{{item.id}}/summary.md"]
each = { from = "reports/subproblems.json" }
[[steps]]
id = "write"
title = "Write"
prompt = "w"
[[steps]]
id = "verify"
title = "Verify"
prompt = "v"
fixes = ["analysis", "solve", "write"]
attempts = 3
"#,
        )
        .unwrap()
    }

    fn items() -> Vec<Choice> {
        crate::read_choices(r#"[{"id":"Q1","title":"问题一"},{"id":"Q2","title":"问题二"}]"#)
    }

    fn through_choice(run: &mut Run) {
        go(run, "analysis", pass());
        assert_eq!(go(run, "choose", pass()), Outcome::Review);
        assert!(matches!(
            run.approve("choose", None, "", 4),
            Err(RunError::ChoiceNeeded(_))
        ));
        let pick = Choice {
            id: "B".into(),
            title: "排队论 + 仿真".into(),
            summary: String::new(),
        };
        run.approve("choose", Some(pick), "先做问题一", 4).unwrap();
    }

    #[test]
    fn a_choice_is_required_and_reaches_later_steps() {
        let mut run = Run::new(contest(), HashMap::new(), &[], 0).unwrap();
        through_choice(&mut run);
        let later = run.decisions_before("write");
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].choice.as_ref().unwrap().title, "排队论 + 仿真");
        assert_eq!(later[0].note, "先做问题一");
        assert!(run.decisions_before("choose").is_empty(), "not its own");
        run.redo("choose", "", 5).unwrap();
        assert!(
            run.decisions.is_empty(),
            "redoing a stop undoes its decision"
        );
    }

    #[test]
    fn a_repeating_step_runs_once_per_item_and_the_rest_wait() {
        let mut run = Run::new(contest(), HashMap::new(), &[], 0).unwrap();
        through_choice(&mut run);
        assert!(
            run.ready().is_empty(),
            "the repeating step itself does no work"
        );
        assert_eq!(
            run.to_expand(),
            vec![("solve".to_string(), "reports/subproblems.json".to_string())]
        );
        run.expand("solve", items(), 5).unwrap();
        assert_eq!(
            run.ready(),
            vec!["solve:Q1", "solve:Q2"],
            "items run side by side"
        );
        assert_eq!(run.title_of("solve:Q2"), "问题二 · Solve");
        assert_eq!(run.outputs_of("solve:Q1"), vec!["results/Q1/summary.md"]);
        assert_eq!(
            run.progress(),
            (2, 6),
            "items count, the repeating step does not"
        );
        go(&mut run, "solve:Q1", pass());
        assert!(
            !run.ready().contains(&"write".to_string()),
            "write waits for every item"
        );
        go(&mut run, "solve:Q2", pass());
        assert_eq!(run.step("solve").unwrap().status, StepStatus::Done);
        assert_eq!(run.ready(), vec!["write"]);
    }

    #[test]
    fn an_empty_list_stops_for_the_user() {
        let mut run = Run::new(contest(), HashMap::new(), &[], 0).unwrap();
        through_choice(&mut run);
        assert!(matches!(
            run.expand("solve", vec![], 5),
            Err(RunError::NoItems(_))
        ));
        assert_eq!(run.step("solve").unwrap().status, StepStatus::Failed);
        assert_eq!(run.state, RunState::Waiting);
    }

    #[test]
    fn a_reviewer_sends_back_to_the_step_it_names() {
        let mut run = Run::new(contest(), HashMap::new(), &[], 0).unwrap();
        through_choice(&mut run);
        run.expand("solve", items(), 5).unwrap();
        go(&mut run, "solve:Q1", pass());
        go(&mut run, "solve:Q2", pass());
        go(&mut run, "write", pass());
        run.begin("verify", None, 6).unwrap();
        run.turn_finished("verify", 7).unwrap();
        let mut fault = CheckResult::fail("问题二的结果和论文不一致", "Q2 的目标值写错");
        fault.send_back = Some("solve:Q2".into());
        assert_eq!(
            run.checked("verify", vec![fault], 8).unwrap(),
            Outcome::SentBack {
                to: "solve:Q2".into()
            }
        );
        assert_eq!(
            run.step("solve:Q1").unwrap().status,
            StepStatus::Done,
            "Q1 stands"
        );
        assert_eq!(run.step("solve:Q2").unwrap().status, StepStatus::Pending);
        assert_eq!(
            run.step("write").unwrap().status,
            StepStatus::Pending,
            "what used Q2 goes again"
        );
        assert_eq!(run.ready(), vec!["solve:Q2"]);
        assert!(run.step("solve:Q2").unwrap().feedback[0].contains("目标值"));
        go(&mut run, "solve:Q2", pass());
        go(&mut run, "write", pass());
        assert_eq!(go(&mut run, "verify", pass()), Outcome::Done);
        assert_eq!(run.state, RunState::Done);
    }

    #[test]
    fn going_back_before_a_repeating_step_lays_its_items_out_again() {
        let mut run = Run::new(contest(), HashMap::new(), &[], 0).unwrap();
        through_choice(&mut run);
        run.expand("solve", items(), 5).unwrap();
        go(&mut run, "solve:Q1", pass());
        run.redo("analysis", "第三问漏了", 6).unwrap();
        assert!(!run.step("solve").unwrap().expanded);
        assert!(run.ready().contains(&"analysis".to_string()));
        assert!(
            !run.ready().iter().any(|s| s.starts_with("solve:")),
            "items wait for the new list"
        );
        let three = crate::read_choices(r#"["问题一","问题二","问题三"]"#);
        go(&mut run, "analysis", pass());
        go(&mut run, "choose", pass());
        run.approve("choose", Some(three[0].clone()), "", 7)
            .unwrap();
        run.expand(
            "solve",
            crate::read_choices(r#"[{"id":"Q1","title":"问题一"},{"id":"Q2","title":"问题二"},{"id":"Q3","title":"问题三"}]"#),
            8,
        )
        .unwrap();
        assert_eq!(run.items_of("solve").len(), 3);
        assert_eq!(
            run.step("solve:Q1").unwrap().attempts.len(),
            1,
            "Q1 keeps its history"
        );
        let saved = Run::from_json(&run.to_json()).unwrap();
        assert_eq!(saved, run);
    }
}
