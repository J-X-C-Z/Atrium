//! One run of a template: where each step stands, its tries and what the
//! checks said, and what the user decided at each stop. Pure state: the
//! runner calls these as agents start and finish and checks come back, and
//! saves the run to `.atrium/run.json` after each change.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{Template, condition_holds};

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
    /// An agent works on it.
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
}

/// One go at a step by an agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub agent: Option<String>,
    /// The agent's conversation, once it has one.
    pub thread: Option<String>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    /// What the agent was told to fix, from failed checks or the user.
    #[serde(default)]
    pub feedback: Vec<String>,
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
    pub id: String,
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
}

impl StepRun {
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
}

/// What [`Run::checked`] did with a step's results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Passed, and stops for the user.
    Review,
    /// Failed; another try starts with the reasons.
    Retry,
    /// Failed in the step it reviews: that step goes again with the
    /// reasons, then this one checks again.
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
            steps.push(StepRun {
                id: step.id.clone(),
                status: if skipped {
                    StepStatus::Skipped
                } else {
                    StepStatus::Pending
                },
                attempts: Vec::new(),
                feedback: Vec::new(),
                budget_from: 0,
            });
        }
        Ok(Self {
            template,
            inputs: values,
            state: RunState::Running,
            steps,
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

    /// The checks step `id` runs for this run's inputs.
    pub fn checks_of(&self, id: &str) -> Vec<crate::Check> {
        self.template
            .step(id)
            .map(|s| {
                s.checks
                    .iter()
                    .filter(|c| condition_holds(c.when.as_deref(), &self.inputs))
                    .map(|c| c.check.clone())
                    .collect()
            })
            .unwrap_or_default()
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

    /// Steps that can start now: pending, with everything they need done
    /// or skipped, while the run runs.
    pub fn ready(&self) -> Vec<String> {
        if self.state != RunState::Running {
            return Vec::new();
        }
        let finished: HashSet<&str> = self
            .steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Done | StepStatus::Skipped))
            .map(|s| s.id.as_str())
            .collect();
        self.steps
            .iter()
            .enumerate()
            .filter(|(ix, s)| {
                s.status == StepStatus::Pending
                    && self
                        .template
                        .needs_of(*ix)
                        .iter()
                        .all(|n| finished.contains(n.as_str()))
            })
            .map(|(_, s)| s.id.clone())
            .collect()
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
            started_ms: now_ms,
            finished_ms: None,
            feedback,
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

    /// The agent's turn on `id` ended; its checks run next.
    pub fn turn_finished(&mut self, id: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(ix, &[StepStatus::Running], "running")?;
        self.steps[ix].status = StepStatus::Checking;
        self.updated_ms = now_ms;
        Ok(())
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
        {
            let step = &mut self.steps[ix];
            if let Some(a) = step.attempts.last_mut() {
                a.finished_ms = Some(now_ms);
                a.checks = results;
            }
        }
        self.updated_ms = now_ms;
        let spec = self.template.steps[ix].clone();
        if passed {
            if spec.gate {
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
        match &spec.fixes {
            Some(target) => {
                let target = target.clone();
                self.send_back(&target, faults, ix)?;
                Ok(Outcome::SentBack { to: target })
            }
            None => {
                let step = &mut self.steps[ix];
                step.feedback = faults;
                step.status = StepStatus::Pending;
                Ok(Outcome::Retry)
            }
        }
    }

    /// Reopens `target` with `faults` to fix, and everything after it that
    /// depends on it, up to and including the reviewer `from`.
    fn send_back(
        &mut self,
        target: &str,
        faults: Vec<String>,
        from: usize,
    ) -> Result<(), RunError> {
        let t = self.ix(target)?;
        let reopened = self.dependents(t);
        for &ix in &reopened {
            if ix == t || ix == from {
                continue;
            }
            if self.steps[ix].status != StepStatus::Skipped {
                self.steps[ix].status = StepStatus::Pending;
            }
        }
        let step = &mut self.steps[t];
        step.feedback.extend(faults);
        step.status = StepStatus::Pending;
        // Fixing is a new round for the target, not a failed try of it.
        step.budget_from = step.attempts.len();
        self.steps[from].status = StepStatus::Pending;
        Ok(())
    }

    /// `ix` and every step that needs it, directly or not.
    fn dependents(&self, ix: usize) -> Vec<usize> {
        let mut out = vec![ix];
        let mut ids: HashSet<String> = [self.steps[ix].id.clone()].into();
        for later in ix + 1..self.steps.len() {
            if self
                .template
                .needs_of(later)
                .iter()
                .any(|n| ids.contains(n))
            {
                ids.insert(self.steps[later].id.clone());
                out.push(later);
            }
        }
        out
    }

    fn finish_if_done(&mut self) {
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

    /// The user looked at a stop and lets the run go on.
    pub fn approve(&mut self, id: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(ix, &[StepStatus::Review], "waiting for you")?;
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
    /// after it that used its work.
    pub fn redo(&mut self, id: &str, note: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        self.expect(
            ix,
            &[StepStatus::Review, StepStatus::Failed, StepStatus::Done],
            "stopped, failed or done",
        )?;
        for later in self.dependents(ix) {
            if self.steps[later].status != StepStatus::Skipped {
                self.steps[later].status = StepStatus::Pending;
            }
        }
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

    /// The user skips an optional step that failed or has not started.
    pub fn skip(&mut self, id: &str, now_ms: i64) -> Result<(), RunError> {
        let ix = self.ix(id)?;
        if !self.template.steps[ix].optional {
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
        for step in &mut self.steps {
            if matches!(step.status, StepStatus::Running | StepStatus::Checking) {
                if let Some(a) = step.attempts.last()
                    && a.finished_ms.is_none()
                {
                    step.feedback = a.feedback.clone();
                    step.attempts.pop();
                }
                step.status = StepStatus::Pending;
                self.updated_ms = now_ms;
            }
        }
    }

    /// Steps done, and steps in all (skipped ones left out).
    pub fn progress(&self) -> (usize, usize) {
        let counted: Vec<_> = self
            .steps
            .iter()
            .filter(|s| s.status != StepStatus::Skipped)
            .collect();
        (
            counted
                .iter()
                .filter(|s| s.status == StepStatus::Done)
                .count(),
            counted.len(),
        )
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
        vec![CheckResult {
            what: "ok".into(),
            passed: true,
            detail: String::new(),
        }]
    }

    fn fail(why: &str) -> Vec<CheckResult> {
        vec![CheckResult {
            what: "paper.pdf exists".into(),
            passed: false,
            detail: why.into(),
        }]
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
        run.approve("plan", 4).unwrap();
        assert_eq!(run.state, RunState::Running);
        assert_eq!(run.ready(), vec!["write"]);
    }

    #[test]
    fn a_failed_try_goes_again_with_its_reasons_until_out_of_tries() {
        let mut run = Run::new(template(), inputs(), &[], 0).unwrap();
        go(&mut run, "plan", pass());
        run.approve("plan", 4).unwrap();
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
        run.approve("plan", 4).unwrap();
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
        run.approve("plan", 4).unwrap();
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
        run.recover(2);
        let plan = run.step("plan").unwrap();
        assert_eq!(plan.status, StepStatus::Pending);
        assert!(plan.attempts.is_empty());
        let saved = Run::from_json(&run.to_json()).unwrap();
        assert_eq!(saved, run);
    }
}
