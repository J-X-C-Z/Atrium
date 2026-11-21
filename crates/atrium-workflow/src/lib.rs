//! Workflows: a preset path of steps that agents follow from a brief (a
//! contest problem, say) to a finished result (a paper that meets the
//! contest's rules), checking each step's work, retrying with the reasons
//! when it falls short, and stopping where the user wants to look first.
//!
//! A **template** is a `workflow.toml` file: what the user gives at the
//! start, then the steps. Each step says which agent does it, what to do,
//! what files it must leave, how its work is checked, whether to stop for
//! the user after it, and which earlier step to send back to when its own
//! checks find faults there.
//!
//! A **run** is one use of a template, in a folder of its own:
//!
//! ```text
//! <run>/.atrium/workflow.toml   the template as it was when the run started
//! <run>/.atrium/run.json        the run's state (see [`run::Run`])
//! <run>/inputs/                 the files the user gave (the problem, data)
//! <run>/...                     what the steps make, as the template says
//! ```
//!
//! This crate is the model: parsing and checking templates, the run's state
//! machine, the checks that only read files, and the prompts. Starting
//! agents, waiting for their turns, running commands and asking a reviewer
//! are the runner's (see `docs/workflow-implementation.md`).

pub mod builtin;
pub mod check;
pub mod render;
pub mod run;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// A workflow template, as `workflow.toml` holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// Stable name of the template, such as `mathmodel`.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Bumped by the template's author when steps change meaning.
    #[serde(default = "one")]
    pub version: u32,
    /// What the user gives at the start.
    #[serde(default)]
    pub inputs: Vec<Input>,
    /// Used by steps that leave these out.
    #[serde(default)]
    pub defaults: Defaults,
    /// Text every step's prompt starts with: the run's shared rules.
    #[serde(default)]
    pub preamble: String,
    pub steps: Vec<Step>,
}

fn one() -> u32 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// Referred to in prompts as `{{input.<key>}}`.
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    pub kind: InputKind,
    #[serde(default)]
    pub required: bool,
    /// For `choice`: the options, the first being the default.
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub default: Option<String>,
    /// Never put into a prompt: only checks read it (such as names that
    /// must not appear in an anonymous paper).
    #[serde(default)]
    pub private: bool,
    /// Shown only when another input has this value: `contest=CUMCM`.
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    /// Files copied into `inputs/`.
    Files,
    Text,
    /// Several lines.
    Long,
    Choice,
    Number,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// A backend id, such as `codex` or `claude-code`.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// Tries a step gets before the run stops for the user.
    #[serde(default = "two")]
    pub attempts: u32,
    /// Minutes one try may take before it counts as stuck.
    #[serde(default = "ninety")]
    pub timeout_min: u32,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            agent: None,
            effort: None,
            attempts: 2,
            timeout_min: 90,
        }
    }
}

fn two() -> u32 {
    2
}
fn ninety() -> u32 {
    90
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub title: String,
    /// One line on what the step is for, shown under its title.
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// A skill the agent should use when it has it installed, such as
    /// `mathmodel-contest-suite:2analysis-modeling`.
    #[serde(default)]
    pub skill: Option<String>,
    pub prompt: String,
    /// Steps that must be done first. Left out: the step before this one.
    #[serde(default)]
    pub needs: Option<Vec<String>>,
    /// Files and folders the step must leave, relative to the run folder;
    /// globs allowed. Each is checked to exist and not be empty.
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub checks: Vec<StepCheck>,
    /// Stop for the user once the step passes its checks.
    #[serde(default)]
    pub gate: bool,
    /// The user may skip it at the start; only steps nothing depends on
    /// in a way that needs its files should be optional.
    #[serde(default)]
    pub optional: bool,
    /// When this step's checks fail, send the faults back to that earlier
    /// step to fix, then run this one again. For a step that reviews
    /// another's work.
    #[serde(default)]
    pub fixes: Option<String>,
    /// Shown only when an input has this value: `contest=CUMCM`.
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub timeout_min: Option<u32>,
}

/// A check as a step lists it, perhaps only for some inputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepCheck {
    #[serde(flatten)]
    pub check: Check,
    /// Only when an input has this value: `contest=MCM`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

/// One way to tell whether a step did its job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// The files exist and are not empty (a glob may match several; one is
    /// enough unless `all`).
    Exists {
        path: String,
        #[serde(default)]
        min: Option<usize>,
    },
    /// The file holds a match of `pattern` (a regular expression).
    Contains {
        path: String,
        pattern: String,
        #[serde(default)]
        message: Option<String>,
    },
    /// No file matching `path` holds a match of `pattern`, nor any value of
    /// the private input `input` (one per line).
    Absent {
        path: String,
        #[serde(default)]
        pattern: Option<String>,
        #[serde(default)]
        input: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
    /// A command run in the run folder exits with 0.
    Command {
        run: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default = "five")]
        timeout_min: u32,
        #[serde(default)]
        message: Option<String>,
    },
    /// The PDF has at most (and at least) so many pages.
    PdfPages {
        path: String,
        #[serde(default)]
        max: Option<u32>,
        #[serde(default)]
        min: Option<u32>,
    },
    /// A reviewing agent reads the files against `rubric` and must answer
    /// that it passes; its reasons go back on failure.
    Review {
        rubric: String,
        #[serde(default)]
        files: Vec<String>,
        #[serde(default)]
        agent: Option<String>,
    },
}

fn five() -> u32 {
    5
}

impl Check {
    /// Whether this crate can run it, reading files only.
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            Check::Exists { .. } | Check::Contains { .. } | Check::Absent { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("the template cannot be read: {0}")]
    Parse(String),
    #[error("the template has no steps")]
    NoSteps,
    #[error("two steps are called {0}")]
    DuplicateStep(String),
    #[error("two inputs are called {0}")]
    DuplicateInput(String),
    #[error("step {step} needs {missing}, which is not a step before it")]
    UnknownNeed { step: String, missing: String },
    #[error("step {step} fixes {target}, which is not a step before it")]
    UnknownFix { step: String, target: String },
    #[error("step {0} has an empty prompt")]
    EmptyPrompt(String),
    #[error("{0} is not a valid id: use letters, digits, - and _")]
    BadId(String),
    #[error("step {step}: {why}")]
    BadCheck { step: String, why: String },
    #[error("{what} refers to {key}, which is not an input")]
    UnknownInput { what: String, key: String },
    #[error("choice input {0} has no options")]
    NoOptions(String),
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Template {
    pub fn parse(text: &str) -> Result<Self, TemplateError> {
        let template: Template =
            toml::from_str(text).map_err(|e| TemplateError::Parse(e.to_string()))?;
        template.validate()?;
        Ok(template)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// The steps a step needs, with the default (the step before it).
    pub fn needs_of(&self, ix: usize) -> Vec<String> {
        match &self.steps[ix].needs {
            Some(needs) => needs.clone(),
            None if ix > 0 => vec![self.steps[ix - 1].id.clone()],
            None => Vec::new(),
        }
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn attempts_of(&self, step: &Step) -> u32 {
        step.attempts.unwrap_or(self.defaults.attempts).max(1)
    }

    pub fn agent_of(&self, step: &Step) -> Option<String> {
        step.agent.clone().or_else(|| self.defaults.agent.clone())
    }

    pub fn validate(&self) -> Result<(), TemplateError> {
        if !valid_id(&self.id) {
            return Err(TemplateError::BadId(self.id.clone()));
        }
        if self.steps.is_empty() {
            return Err(TemplateError::NoSteps);
        }
        let mut inputs = HashSet::new();
        for input in &self.inputs {
            if !valid_id(&input.key) {
                return Err(TemplateError::BadId(input.key.clone()));
            }
            if !inputs.insert(input.key.as_str()) {
                return Err(TemplateError::DuplicateInput(input.key.clone()));
            }
            if input.kind == InputKind::Choice && input.options.is_empty() {
                return Err(TemplateError::NoOptions(input.key.clone()));
            }
        }
        let condition_key = |when: &Option<String>| -> Option<String> {
            when.as_ref()
                .map(|w| w.split('=').next().unwrap_or("").trim().to_string())
        };
        for input in &self.inputs {
            if let Some(key) = condition_key(&input.when)
                && !inputs.contains(key.as_str())
            {
                return Err(TemplateError::UnknownInput {
                    what: input.key.clone(),
                    key,
                });
            }
        }
        // Steps may only need, or fix, steps before them: no cycles by
        // construction, and the order on screen is the order of work.
        let mut before: HashSet<&str> = HashSet::new();
        for (ix, step) in self.steps.iter().enumerate() {
            if !valid_id(&step.id) {
                return Err(TemplateError::BadId(step.id.clone()));
            }
            if before.contains(step.id.as_str()) {
                return Err(TemplateError::DuplicateStep(step.id.clone()));
            }
            if step.prompt.trim().is_empty() {
                return Err(TemplateError::EmptyPrompt(step.id.clone()));
            }
            for need in self.needs_of(ix) {
                if !before.contains(need.as_str()) {
                    return Err(TemplateError::UnknownNeed {
                        step: step.id.clone(),
                        missing: need,
                    });
                }
            }
            if let Some(target) = &step.fixes
                && !before.contains(target.as_str())
            {
                return Err(TemplateError::UnknownFix {
                    step: step.id.clone(),
                    target: target.clone(),
                });
            }
            if let Some(key) = condition_key(&step.when)
                && !inputs.contains(key.as_str())
            {
                return Err(TemplateError::UnknownInput {
                    what: step.id.clone(),
                    key,
                });
            }
            for StepCheck { check, when } in &step.checks {
                if let Some(key) = condition_key(when)
                    && !inputs.contains(key.as_str())
                {
                    return Err(TemplateError::UnknownInput {
                        what: step.id.clone(),
                        key,
                    });
                }
                match check {
                    Check::Contains { pattern, .. }
                    | Check::Absent {
                        pattern: Some(pattern),
                        ..
                    } => {
                        regex::Regex::new(pattern).map_err(|e| TemplateError::BadCheck {
                            step: step.id.clone(),
                            why: e.to_string(),
                        })?;
                    }
                    _ => {}
                }
                if let Check::Absent {
                    input: Some(key), ..
                } = check
                    && !inputs.contains(key.as_str())
                {
                    return Err(TemplateError::UnknownInput {
                        what: step.id.clone(),
                        key: key.clone(),
                    });
                }
            }
            before.insert(step.id.as_str());
        }
        Ok(())
    }
}

/// Whether a `when` condition (`key=value`, or `key!=value`) holds for the
/// values given; no condition always holds.
pub fn condition_holds(when: Option<&str>, values: &HashMap<String, String>) -> bool {
    let Some(when) = when else { return true };
    if let Some((key, value)) = when.split_once("!=") {
        return values.get(key.trim()).map(String::as_str) != Some(value.trim());
    }
    match when.split_once('=') {
        Some((key, value)) => values.get(key.trim()).map(String::as_str) == Some(value.trim()),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = r#"
id = "essay"
name = "Essay"

[[inputs]]
key = "topic"
label = "Topic"
kind = "text"
required = true

[[steps]]
id = "outline"
title = "Outline"
prompt = "Outline an essay on {{input.topic}}."
outputs = ["outline.md"]
gate = true

[[steps]]
id = "draft"
title = "Draft"
prompt = "Write it."
outputs = ["essay.md"]
checks = [{ kind = "contains", path = "essay.md", pattern = "(?i)conclusion" }]

[[steps]]
id = "review"
title = "Review"
prompt = "Review it."
fixes = "draft"
checks = [{ kind = "review", rubric = "Clear and complete." }]
"#;

    #[test]
    fn a_template_reads_and_steps_follow_each_other() {
        let t = Template::parse(SMALL).unwrap();
        assert_eq!(t.steps.len(), 3);
        assert_eq!(t.needs_of(0), Vec::<String>::new());
        assert_eq!(t.needs_of(2), vec!["draft".to_string()]);
        assert_eq!(t.attempts_of(&t.steps[1]), 2);
        let again = Template::parse(&t.to_toml()).unwrap();
        assert_eq!(again, t);
    }

    #[test]
    fn needing_or_fixing_a_later_step_is_refused() {
        let bad = SMALL.replace("fixes = \"draft\"", "fixes = \"later\"");
        assert!(matches!(
            Template::parse(&bad),
            Err(TemplateError::UnknownFix { .. })
        ));
        let bad = SMALL.replace(
            "title = \"Draft\"",
            "title = \"Draft\"\nneeds = [\"review\"]",
        );
        assert!(matches!(
            Template::parse(&bad),
            Err(TemplateError::UnknownNeed { .. })
        ));
        let bad = SMALL.replace("pattern = \"(?i)conclusion\"", "pattern = \"(\"");
        assert!(matches!(
            Template::parse(&bad),
            Err(TemplateError::BadCheck { .. })
        ));
    }

    #[test]
    fn conditions_read_both_ways() {
        let values: HashMap<String, String> = [("contest".to_string(), "CUMCM".to_string())].into();
        assert!(condition_holds(Some("contest=CUMCM"), &values));
        assert!(!condition_holds(Some("contest=MCM"), &values));
        assert!(condition_holds(Some("contest!=MCM"), &values));
        assert!(condition_holds(None, &values));
    }
}
