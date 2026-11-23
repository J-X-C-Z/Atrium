//! Teams: several models in one project, each used where its notes say it
//! is good, with the scarce ones covered when their limits are hit.
//!
//! Deliberately small. Models change faster than code, and rules written
//! into code end up deciding for a model what it would decide better, so
//! the judgement stays with the models:
//!
//! * the controller is told who is on the team, what each is good at (the
//!   user's own notes) and who is spent right now ([`roster`]), and picks
//!   an agent per task itself, as it already does with `agent:`;
//! * the app only covers for what a model cannot see: a member whose limit
//!   is hit or that cannot start gets its listed stand-in ([`pick`]);
//! * and it notices a session going in circles from what it does (the
//!   same error, the same step, too many tokens) and tells whoever gave
//!   the task, who decides what to do ([`watch`]).
//!
//! No GPUI and no I/O besides [`Team::load`]; the app feeds in events and
//! `now` (see `docs/team-implementation.md`).

pub mod quota;
pub mod watch;

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

pub use quota::Quotas;

/// Where a team file lives in a project; the app's own one is `team.toml`
/// in its config folder, used when a project has none.
pub const PROJECT_FILE: &str = ".atrium/team.toml";

/// One model on the team.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// The backend id the app starts it with: `claude`, `codex`,
    /// `antigravity`, `deepseek`, or `claude@studio` on another device.
    pub agent: String,
    /// A model to ask the backend for, when not its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// What it is good and bad at, in the user's words; the controller
    /// reads this when it hands out work.
    #[serde(default)]
    pub notes: String,
    /// Members to use instead, in order, while this one is spent or
    /// cannot start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stand_ins: Vec<String>,
}

impl Member {
    pub fn new(agent: &str, notes: &str, stand_ins: &[&str]) -> Self {
        Self {
            agent: agent.into(),
            model: None,
            effort: None,
            notes: notes.into(),
            stand_ins: stand_ins.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// When a session counts as going in circles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WatchRules {
    /// The same error this many times in a row.
    pub same_error: u32,
    /// The same command or edit this many times among the last ten.
    pub same_step: u32,
    /// Tokens one task may use before its parent hears; none for no limit.
    pub task_tokens: Option<u64>,
}

impl Default for WatchRules {
    fn default() -> Self {
        Self {
            same_error: 3,
            same_step: 4,
            task_tokens: Some(2_000_000),
        }
    }
}

/// Who is on the team.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Team {
    /// Off: no roster in the controller's brief, no stand-ins.
    pub on: bool,
    /// Members by a short name (`claude`, `gpt`, `gemini`, `deepseek`).
    pub members: BTreeMap<String, Member>,
    pub watch: WatchRules,
}

impl Default for Team {
    /// A starting point for Claude, ChatGPT and Gemini subscriptions and a
    /// DeepSeek API key. The notes are the user's to rewrite as models change.
    fn default() -> Self {
        let members = [
            (
                "claude",
                Member::new(
                    "claude",
                    "审美和前端最强，适合当主控、做规划和界面。额度最少：留给规划、页面和需要判断的事。",
                    &["gemini"],
                ),
            ),
            (
                "gpt",
                Member::new(
                    "codex",
                    "严谨细致：后端、算法、修 bug、代码审查、浏览器操作。汇报比较难读，审美一般。",
                    &["claude"],
                ),
            ),
            (
                "gemini",
                Member::new(
                    "antigravity",
                    "沟通顺畅、有创意：第二方案、文案、头脑风暴。容易粗心，事实要核对。",
                    &["claude"],
                ),
            ),
            (
                "deepseek",
                Member::new(
                    "deepseek",
                    "API 按量付费、几乎不限量，但慢、容易打转：只给边界清楚、能用命令验收的活。",
                    &["gpt"],
                ),
            ),
        ]
        .map(|(name, m)| (name.to_string(), m))
        .into();
        Self {
            on: false,
            members,
            watch: WatchRules::default(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum TeamError {
    Parse(String),
    /// A member's stand-ins name someone who is not a member.
    UnknownMember {
        member: String,
        name: String,
    },
}

impl std::fmt::Display for TeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamError::Parse(e) => write!(f, "team.toml: {e}"),
            TeamError::UnknownMember { member, name } => {
                write!(
                    f,
                    "team.toml: '{member}' stands in with '{name}', who is not a member"
                )
            }
        }
    }
}

impl std::error::Error for TeamError {}

impl Team {
    pub fn parse(text: &str) -> Result<Self, TeamError> {
        let team: Team = toml::from_str(text).map_err(|e| TeamError::Parse(e.message().into()))?;
        for (member, m) in &team.members {
            if let Some(name) = m.stand_ins.iter().find(|n| !team.members.contains_key(*n)) {
                return Err(TeamError::UnknownMember {
                    member: member.clone(),
                    name: name.clone(),
                });
            }
        }
        Ok(team)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// The project's team file, else `fallback` (the app's), else the default.
    pub fn load(project: Option<&Path>, fallback: Option<&Path>) -> Result<Self, TeamError> {
        let files = project
            .map(|root| root.join(PROJECT_FILE))
            .into_iter()
            .chain(fallback.map(Path::to_path_buf));
        for file in files {
            if let Ok(text) = std::fs::read_to_string(&file) {
                return Team::parse(&text);
            }
        }
        Ok(Team::default())
    }

    /// The member a task's `agent:` means: a member name (`gpt`) or a
    /// member's backend id (`codex`).
    pub fn member_for(&self, wanted: &str) -> Option<&str> {
        let wanted = wanted.trim().to_lowercase();
        self.members
            .get_key_value(&wanted)
            .map(|(k, _)| k.as_str())
            .or_else(|| {
                self.members
                    .iter()
                    .find(|(_, m)| m.agent == wanted)
                    .map(|(k, _)| k.as_str())
            })
    }
}

/// Who runs a task.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// The member's name on the team.
    pub member: String,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Standing in for this member, and why.
    pub instead_of: Option<(String, Skip)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Skip {
    /// Its limit was hit; usable again at this time (Unix seconds).
    Spent(i64),
    /// The app cannot start it now (not installed, device offline).
    Unavailable,
}

/// Nobody can take the work now.
#[derive(Clone, Debug, PartialEq)]
pub struct Wait {
    /// When the first spent member is back, if one is.
    pub until: Option<i64>,
}

/// Who runs a task that asked for `wanted`: that member when it can
/// start and is not spent, else the first of its stand-ins (and theirs)
/// that can. `wanted` not on the team runs as asked, unchanged.
pub fn pick(
    team: &Team,
    wanted: &str,
    quotas: &Quotas,
    ready: impl Fn(&str) -> bool,
    now: i64,
) -> Result<Pick, Wait> {
    let Some(first) = team.member_for(wanted) else {
        return Ok(Pick {
            member: wanted.to_string(),
            agent: wanted.to_string(),
            model: None,
            effort: None,
            instead_of: None,
        });
    };
    let mut queue = vec![first.to_string()];
    let mut seen = Vec::new();
    let mut first_skip = None;
    let mut until: Option<i64> = None;
    while !queue.is_empty() {
        let name = queue.remove(0);
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let Some(m) = team.members.get(&name) else {
            continue;
        };
        let skip = if !ready(&m.agent) {
            Some(Skip::Unavailable)
        } else {
            quotas.spent_until(&name, now).map(Skip::Spent)
        };
        match skip {
            None => {
                return Ok(Pick {
                    member: name,
                    agent: m.agent.clone(),
                    model: m.model.clone(),
                    effort: m.effort.clone(),
                    instead_of: first_skip.map(|s| (first.to_string(), s)),
                });
            }
            Some(skip) => {
                if let Skip::Spent(at) = skip {
                    until = Some(until.map_or(at, |u| u.min(at)));
                }
                first_skip.get_or_insert(skip);
                queue.extend(m.stand_ins.iter().cloned());
            }
        }
    }
    Err(Wait { until })
}

/// What the controller is told about the team at kickoff and whenever
/// someone's limit is hit or resets: one line per member.
pub fn roster(team: &Team, quotas: &Quotas, ready: impl Fn(&str) -> bool, now: i64) -> String {
    let mut out = String::from(
        "## Your team\n\n\
Choose who does each task by writing `agent: <name>` in its task file. These are \
the user's notes on each; use your own judgement too. A member marked spent is \
replaced by its stand-in until it is back.\n\n",
    );
    for (name, m) in &team.members {
        let state = if !ready(&m.agent) {
            " (not available now)".to_string()
        } else if let Some(at) = quotas.spent_until(name, now) {
            format!(" (spent, back in about {} min)", (at - now + 59) / 60)
        } else {
            String::new()
        };
        let notes = if m.notes.is_empty() {
            String::new()
        } else {
            format!(": {}", m.notes)
        };
        out.push_str(&format!("- `{name}`{state}{notes}\n"));
    }
    out
}

/// The rules file an agent reads by itself in a repository, so each
/// model's habits are corrected in a file of its own.
pub fn rules_file(agent: &str) -> &'static str {
    match agent.split('@').next().unwrap_or(agent) {
        "claude" => "CLAUDE.md",
        "antigravity" | "gemini" => "GEMINI.md",
        _ => "AGENTS.md",
    }
}

#[cfg(test)]
mod tests;
