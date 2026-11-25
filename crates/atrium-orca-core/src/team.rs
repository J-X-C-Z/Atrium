//! `team.*`: the project's team file (`.atrium/team.toml`) in the plugin's
//! camelCase shapes, and `atrium-team`'s pick and roster.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use atrium_team::{Member, Quotas, Skip, Team, WatchRules};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::write_atomic;

/// A member as the UI edits it (`protocol.ts` `Member`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    pub name: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub stand_ins: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchView {
    pub same_error: u32,
    pub same_step: u32,
    #[serde(default)]
    pub task_tokens: Option<u64>,
}

/// A whole team. Fields left out keep their defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamView {
    #[serde(default)]
    pub on: Option<bool>,
    #[serde(default)]
    pub members: Vec<MemberView>,
    #[serde(default)]
    pub watch: Option<WatchView>,
}

impl TeamView {
    pub fn of(team: &Team) -> Self {
        Self {
            on: Some(team.on),
            members: team
                .members
                .iter()
                .map(|(name, m)| MemberView {
                    name: name.clone(),
                    agent: m.agent.clone(),
                    model: m.model.clone(),
                    effort: m.effort.clone(),
                    notes: m.notes.clone(),
                    stand_ins: m.stand_ins.clone(),
                })
                .collect(),
            watch: Some(WatchView {
                same_error: team.watch.same_error,
                same_step: team.watch.same_step,
                task_tokens: team.watch.task_tokens,
            }),
        }
    }

    /// The team, checked the way `Team::parse` checks a file.
    pub fn to_team(&self) -> Result<Team, String> {
        let mut members = BTreeMap::new();
        for m in &self.members {
            let name = m.name.trim().to_string();
            if name.is_empty() {
                return Err("a member has no name".into());
            }
            if m.agent.trim().is_empty() {
                return Err(format!("{name} has no agent"));
            }
            let member = Member {
                agent: m.agent.trim().to_string(),
                model: m.model.clone().filter(|s| !s.trim().is_empty()),
                effort: m.effort.clone().filter(|s| !s.trim().is_empty()),
                notes: m.notes.clone(),
                stand_ins: m.stand_ins.clone(),
            };
            if members.insert(name.clone(), member).is_some() {
                return Err(format!("two members are called {name}"));
            }
        }
        let watch = self.watch.as_ref().map_or_else(WatchRules::default, |w| WatchRules {
            same_error: w.same_error,
            same_step: w.same_step,
            task_tokens: w.task_tokens,
        });
        let team = Team {
            on: self.on.unwrap_or(true),
            members,
            watch,
        };
        // Round-trip through the file format so its own checks apply.
        Team::parse(&team.to_toml()).map_err(|e| e.to_string())
    }
}

fn team_file(root: &Path) -> PathBuf {
    root.join(atrium_team::PROJECT_FILE)
}

/// The project's team, the fallback file's, or the default; with why the
/// file did not read, if it did not.
pub fn load(root: Option<&Path>, fallback: Option<&Path>) -> (Team, Option<String>) {
    match Team::load(root, fallback) {
        Ok(team) => (team, None),
        Err(error) => (Team::default(), Some(error.to_string())),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadArgs {
    root: PathBuf,
    /// The app-wide team file used when the project has none.
    #[serde(default)]
    fallback: Option<PathBuf>,
}

/// `team.read {root, fallback?}` → `{on, members, watch, errors, path, exists}`.
pub fn read(args: ReadArgs) -> Result<Value, String> {
    let path = team_file(&args.root);
    let (team, error) = load(Some(&args.root), args.fallback.as_deref());
    let view = TeamView::of(&team);
    Ok(json!({
        "on": team.on,
        "members": view.members,
        "watch": view.watch,
        "errors": error.map(|message| vec![json!({ "message": message })]).unwrap_or_default(),
        "path": path,
        "exists": path.is_file(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteArgs {
    root: PathBuf,
    #[serde(default)]
    team: Option<TeamView>,
    /// `team.save` in `protocol.ts` sends only the members.
    #[serde(default)]
    members: Option<Vec<MemberView>>,
}

/// `team.write {root, team | members}` → `{errors}`; nothing is written
/// when there are errors.
pub fn write(args: WriteArgs) -> Result<Value, String> {
    let mut view = match args.team {
        Some(team) => team,
        None => {
            let (team, _) = load(Some(&args.root), None);
            TeamView::of(&team)
        }
    };
    if let Some(members) = args.members {
        view.members = members;
    }
    let team = match view.to_team() {
        Ok(team) => team,
        Err(message) => return Ok(json!({ "errors": [{ "message": message }] })),
    };
    write_atomic(&team_file(&args.root), team.to_toml().as_bytes()).map_err(|e| format!("{e:#}"))?;
    Ok(json!({ "errors": [] }))
}

/// The team an op works on: given inline, or read from a project.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSource {
    #[serde(default)]
    team: Option<TeamView>,
    #[serde(default)]
    root: Option<PathBuf>,
    /// Spent members: name → Unix seconds when usable again.
    #[serde(default)]
    quotas: BTreeMap<String, i64>,
    /// Backends that can start now; left out, every backend can.
    #[serde(default)]
    ready: Option<Vec<String>>,
    /// Unix seconds.
    #[serde(default)]
    now: Option<i64>,
}

impl TeamSource {
    fn team(&self) -> Result<Team, String> {
        match (&self.team, &self.root) {
            (Some(view), _) => view.to_team(),
            (None, Some(root)) => Ok(load(Some(root), None).0),
            (None, None) => Ok(Team::default()),
        }
    }

    fn quotas(&self) -> Quotas {
        Quotas {
            spent: self.quotas.clone(),
        }
    }

    fn now(&self) -> i64 {
        self.now
            .unwrap_or_else(|| jiff::Timestamp::now().as_second())
    }
}

/// Whether `backend` is in `ready` (all are when it is `None`).
pub fn is_ready(ready: &Option<Vec<String>>, backend: &str) -> bool {
    ready
        .as_ref()
        .is_none_or(|list| list.iter().any(|r| r == backend))
}

pub fn pick_json(pick: &atrium_team::Pick) -> Value {
    let instead = pick.instead_of.as_ref().map(|(member, skip)| match skip {
        Skip::Spent(until) => json!({ "member": member, "reason": "spent", "until": until }),
        Skip::Unavailable => json!({ "member": member, "reason": "unavailable" }),
    });
    json!({
        "member": pick.member,
        "agent": pick.agent,
        "model": pick.model,
        "effort": pick.effort,
        "insteadOf": instead,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickArgs {
    #[serde(flatten)]
    source: TeamSource,
    /// A member name or a backend id, as a task's `agent:` says.
    wanted: String,
}

/// `team.pick {team | root, wanted, quotas, ready, now}` → `{pick}` or `{wait: {until}}`.
pub fn pick(args: PickArgs) -> Result<Value, String> {
    let team = args.source.team()?;
    let ready = args.source.ready.clone();
    match atrium_team::pick(
        &team,
        &args.wanted,
        &args.source.quotas(),
        |b| is_ready(&ready, b),
        args.source.now(),
    ) {
        Ok(pick) => Ok(json!({ "pick": pick_json(&pick) })),
        Err(wait) => Ok(json!({ "wait": { "until": wait.until } })),
    }
}

/// `team.roster {team | root, quotas, ready, now}` → `{text}`.
pub fn roster(args: TeamSource) -> Result<Value, String> {
    let team = args.team()?;
    let ready = args.ready.clone();
    let text = atrium_team::roster(&team, &args.quotas(), |b| is_ready(&ready, b), args.now());
    Ok(json!({ "text": text }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<MemberView> {
        serde_json::from_value(json!([
            {"name": "claude", "agent": "claude", "notes": "plans", "standIns": ["gpt"]},
            {"name": "gpt", "agent": "codex", "model": "gpt-5"}
        ]))
        .unwrap()
    }

    #[test]
    fn a_team_is_written_read_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let fresh = read(ReadArgs { root: root.clone(), fallback: None }).unwrap();
        assert_eq!(fresh["exists"], false);
        assert_eq!(fresh["members"].as_array().unwrap().len(), 4, "the default team");

        let out = write(WriteArgs { root: root.clone(), team: None, members: Some(members()) }).unwrap();
        assert_eq!(out["errors"], json!([]));
        let back = read(ReadArgs { root: root.clone(), fallback: None }).unwrap();
        assert_eq!(back["exists"], true);
        assert_eq!(back["members"][0]["standIns"], json!(["gpt"]));
        assert_eq!(back["members"][1]["model"], "gpt-5");
        // The app reads the same file.
        let team = Team::load(Some(&root), None).unwrap();
        assert_eq!(team.members["gpt"].agent, "codex");

        let mut bad = members();
        bad[0].stand_ins = vec!["nobody".into()];
        let out = write(WriteArgs { root: root.clone(), team: None, members: Some(bad) }).unwrap();
        assert!(out["errors"][0]["message"].as_str().unwrap().contains("nobody"));
        assert_eq!(Team::load(Some(&root), None).unwrap().members.len(), 2, "unchanged");
    }

    #[test]
    fn a_spent_member_is_covered_by_its_stand_in() {
        let team = TeamView { on: Some(true), members: members(), watch: None };
        let args = |quotas: BTreeMap<String, i64>| PickArgs {
            source: TeamSource {
                team: Some(team.clone()),
                root: None,
                quotas,
                ready: None,
                now: Some(100),
            },
            wanted: "claude".into(),
        };
        let first = pick(args(BTreeMap::new())).unwrap();
        assert_eq!(first["pick"]["member"], "claude");
        let covered = pick(args([("claude".to_string(), 500)].into())).unwrap();
        assert_eq!(covered["pick"]["member"], "gpt");
        assert_eq!(covered["pick"]["insteadOf"]["reason"], "spent");
        assert_eq!(covered["pick"]["insteadOf"]["until"], 500);
        let both = pick(args([("claude".to_string(), 500), ("gpt".to_string(), 300)].into())).unwrap();
        assert_eq!(both["wait"]["until"], 300);

        let text = roster(TeamSource {
            team: Some(team),
            root: None,
            quotas: [("gpt".to_string(), 400)].into(),
            ready: None,
            now: Some(100),
        })
        .unwrap();
        assert!(text["text"].as_str().unwrap().contains("`gpt` (spent, back in about 5 min)"));
    }
}
