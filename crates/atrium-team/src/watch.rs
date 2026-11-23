//! Noticing a session going in circles, from what it does rather than what
//! it says. The app feeds a [`Watch`] one [`Signal`] per finished command,
//! edit and turn of a task's session; when [`Watch::verdict`] says it is
//! stuck, the app tells whoever gave the task ([`notice`]) and leaves the
//! decision to them: a hint, a smaller task, another member, or the user.

use std::collections::VecDeque;

use crate::WatchRules;

/// Something the session did.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// A command finished.
    Command {
        command: String,
        exit_code: Option<i32>,
        output: String,
    },
    /// Files were changed, with the diff text.
    Edit { paths: Vec<String>, diff: String },
    /// A turn ended, having used this many tokens.
    TurnEnded { tokens: u64 },
}

/// Why a task counts as stuck.
#[derive(Clone, Debug, PartialEq)]
pub enum Stuck {
    /// The same error, this many times running.
    SameError { error: String, times: u32 },
    /// The same command or edit, this many times among the last ten.
    Repeating { action: String, times: u32 },
    /// Used more tokens than the task may.
    OverBudget { tokens: u64 },
}

impl Stuck {
    /// One line for a card or a prompt.
    pub fn describe(&self) -> String {
        match self {
            Stuck::SameError { error, times } => {
                format!("the same error {times} times in a row: {error}")
            }
            Stuck::Repeating { action, times } => {
                format!("the same step {times} times: {action}")
            }
            Stuck::OverBudget { tokens } => format!("over its budget, {tokens} tokens used"),
        }
    }
}

/// A task's recent history.
#[derive(Clone, Debug, Default)]
pub struct Watch {
    last_error: Option<String>,
    error_run: u32,
    recent: VecDeque<String>,
    tokens: u64,
    budget_told: bool,
    /// The last few things done, for a hint request.
    trail: VecDeque<String>,
}

const RECENT: usize = 10;
const TRAIL: usize = 8;

impl Watch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, signal: &Signal) {
        match signal {
            Signal::Command {
                command,
                exit_code,
                output,
            } => {
                self.remember(format!("$ {}", one_line(command, 120)));
                if exit_code.is_some_and(|c| c != 0) {
                    let error = fingerprint(output);
                    self.trail_push(format!("  failed: {error}"));
                    if self.last_error.as_deref() == Some(error.as_str()) {
                        self.error_run += 1;
                    } else {
                        self.last_error = Some(error);
                        self.error_run = 1;
                    }
                } else if exit_code.is_some() {
                    self.last_error = None;
                    self.error_run = 0;
                }
            }
            Signal::Edit { paths, diff } => {
                let mut paths = paths.clone();
                paths.sort();
                // Two edits are the same when they change the same files
                // the same way, ignoring blank space.
                let shape: String = diff.split_whitespace().collect::<Vec<_>>().join(" ");
                self.remember(format!("edit {} #{:x}", paths.join(", "), hash(&shape)));
            }
            Signal::TurnEnded { tokens } => self.tokens += tokens,
        }
    }

    fn remember(&mut self, action: String) {
        self.trail_push(action.clone());
        self.recent.push_back(action);
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
    }

    fn trail_push(&mut self, line: String) {
        self.trail.push_back(line);
        if self.trail.len() > TRAIL {
            self.trail.pop_front();
        }
    }

    pub fn verdict(&self, rules: &WatchRules) -> Option<Stuck> {
        if let Some(error) = &self.last_error
            && self.error_run >= rules.same_error
        {
            return Some(Stuck::SameError {
                error: error.clone(),
                times: self.error_run,
            });
        }
        let mut most: Option<(&String, u32)> = None;
        for action in &self.recent {
            let times = self.recent.iter().filter(|a| *a == action).count() as u32;
            if most.is_none_or(|(_, n)| times > n) {
                most = Some((action, times));
            }
        }
        if let Some((action, times)) = most
            && times >= rules.same_step
        {
            return Some(Stuck::Repeating {
                action: action.clone(),
                times,
            });
        }
        if let Some(limit) = rules.task_tokens
            && self.tokens > limit
            && !self.budget_told
        {
            return Some(Stuck::OverBudget {
                tokens: self.tokens,
            });
        }
        None
    }

    /// Starts counting afresh once the parent has been told, so it hears
    /// again only if the session keeps circling. Tokens keep counting, and
    /// a budget already passed is not reported twice.
    pub fn reported(&mut self, rules: &WatchRules) {
        self.last_error = None;
        self.error_run = 0;
        self.recent.clear();
        if rules.task_tokens.is_some_and(|limit| self.tokens > limit) {
            self.budget_told = true;
        }
    }

    /// The last few things done, oldest first, for a hint request.
    pub fn trail(&self) -> Vec<String> {
        self.trail.iter().cloned().collect()
    }

    pub fn tokens(&self) -> u64 {
        self.tokens
    }
}

/// The error a command's output ends with, with numbers, addresses and
/// paths' line numbers blanked so the same failure reads the same.
pub fn fingerprint(output: &str) -> String {
    let line = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| {
            let lower = l.to_lowercase();
            lower.contains("error") || lower.contains("failed") || lower.contains("panicked")
        })
        .or_else(|| output.lines().rev().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    let mut out = String::new();
    let mut digits = false;
    for c in one_line(line, 200).chars() {
        if c.is_ascii_digit() {
            if !digits {
                out.push('#');
            }
            digits = true;
        } else {
            digits = false;
            out.push(c);
        }
    }
    out
}

fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

fn hash(text: &str) -> u64 {
    // FNV-1a: stable across runs, unlike the std hasher.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// What the parent of a stuck task is told. It decides what to do.
pub fn notice(worker: &str, task: &str, why: &Stuck, trail: &[String]) -> String {
    format!(
        "`{worker}` seems to be going in circles on `{task}`: {why}. Its last steps:\n\n\
```\n{trail}\n```\n\n\
You decide what helps: a hint sent to it, a smaller or clearer task, another \
member with `agent:`, or asking the user. It keeps working meanwhile.",
        why = why.describe(),
        trail = trail.join("\n"),
    )
}
