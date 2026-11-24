//! The team: which models a project's controller can hand work to, the
//! user's notes on each, and whose limit is hit right now.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::SystemTime,
};

use atrium_core::{AgentEvent, ConfigOption, Item};
use atrium_team::{
    Pick, Quotas, Team, schedule,
    watch::{self, Signal, Watch},
};
use gpui::Context;

use super::Workspace;
use crate::{
    agents,
    project::{ATRIUM, Role, TaskStatus},
    settings::ModelChoice,
};

/// A project's own team, with the files' times it was read at.
pub struct ProjectTeam {
    pub team: Team,
    stamp: [Option<SystemTime>; 2],
}

/// What the app keeps about the team while it runs.
#[derive(Default)]
pub struct TeamState {
    /// The app's team file, or the default when there is none.
    pub team: Team,
    /// Why the file could not be read, when it could not.
    pub error: Option<String>,
    pub quotas: Quotas,
    /// Each open project's team: its own file, else the app's.
    pub projects: HashMap<PathBuf, ProjectTeam>,
    /// Who was spent when each project's roster was last posted.
    pub spent: HashMap<PathBuf, BTreeSet<String>>,
    /// One per running lead or worker, by project and stream.
    pub watches: HashMap<(PathBuf, String), Watch>,
    /// The model and effort a started task asked for, by project and
    /// stream, so Codex turns keep the member's choice.
    pub starts: HashMap<(PathBuf, String), ModelChoice>,
}

/// The model and effort to start a placed task's session with, when the
/// member named any.
pub fn start_choice(placed: &Pick) -> Option<ModelChoice> {
    (placed.model.is_some() || placed.effort.is_some()).then(|| ModelChoice {
        model: placed.model.clone(),
        effort: placed.effort.clone(),
    })
}

/// The ACP config options to change so a member's `model` and `effort`
/// apply, by category: ACP calls them `model` and `thought_level`.
pub fn option_updates(
    options: &[ConfigOption],
    model: Option<&str>,
    effort: Option<&str>,
) -> Vec<(String, String)> {
    options
        .iter()
        .filter_map(|option| {
            let value = match option.category.as_deref() {
                Some("model") => model,
                Some("thought_level") => effort,
                _ => None,
            }?;
            (!value.is_empty()).then(|| (option.id.clone(), value.to_string()))
        })
        .collect()
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub(super) fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// A time of day for a notice, in the user's zone.
fn clock(at: i64) -> String {
    jiff::Timestamp::from_second(at)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// The app's own team file, used by projects without `.atrium/team.toml`.
pub fn team_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Atrium").join("team.toml"))
}

impl TeamState {
    pub fn load() -> Self {
        match Team::load(None, team_path().as_deref()) {
            Ok(team) => Self {
                team,
                ..Default::default()
            },
            Err(error) => Self {
                error: Some(error.to_string()),
                ..Default::default()
            },
        }
    }
}

impl Workspace {
    /// Saves the team file after an edit in settings.
    pub fn save_team(&mut self, team: Team, cx: &mut Context<Self>) {
        if let Some(path) = team_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(error) = std::fs::write(&path, team.to_toml()) {
                log::warn!("team.toml: {error}");
            }
        }
        self.team.team = team;
        self.team.error = None;
        cx.notify();
    }

    /// Reads the team file again, after it was edited by hand.
    pub fn reload_team(&mut self, cx: &mut Context<Self>) {
        let fresh = TeamState::load();
        self.team.team = fresh.team;
        self.team.error = fresh.error;
        self.team.projects.clear();
        cx.notify();
    }

    /// The team a project runs with: its own file, else the app's.
    pub fn project_team(&self, root: &Path) -> &Team {
        self.team
            .projects
            .get(root)
            .map_or(&self.team.team, |p| &p.team)
    }

    /// Reads a project's team again when its file or the app's changed.
    pub(super) fn refresh_project_team(&mut self, root: &Path) {
        let stamp = [
            mtime(&root.join(atrium_team::PROJECT_FILE)),
            team_path().and_then(|p| mtime(&p)),
        ];
        if self
            .team
            .projects
            .get(root)
            .is_some_and(|p| p.stamp == stamp)
        {
            return;
        }
        let team = match Team::load(Some(root), team_path().as_deref()) {
            Ok(team) => team,
            Err(error) => {
                log::warn!("{}: {error}", root.display());
                self.team.team.clone()
            }
        };
        self.team
            .projects
            .insert(root.to_path_buf(), ProjectTeam { team, stamp });
    }

    /// Where a task that asked for `wanted` starts now.
    pub(super) fn place_task(
        &self,
        root: &Path,
        wanted: &str,
    ) -> Result<atrium_team::Pick, atrium_team::Wait> {
        schedule::place(
            self.project_team(root),
            wanted,
            &self.team.quotas,
            |b| self.backend_ready(b),
            now(),
        )
    }

    /// What the team adds to a stream's kickoff; empty with the team off.
    pub(super) fn team_kickoff_extras(&self, root: &Path, role: Role, backend: &str) -> String {
        schedule::kickoff_extras(
            self.project_team(root),
            &self.team.quotas,
            backend,
            role != Role::Worker,
            |b| self.backend_ready(b),
            now(),
        )
    }

    /// The notice to a task's owner when it starts on a stand-in.
    pub(super) fn stand_in_notice(task: &str, placed: &atrium_team::Pick) -> Option<String> {
        schedule::stand_in_notice(task, placed, clock)
    }

    /// Tells the controller and leads the new roster when a member became
    /// spent or came back since the last one.
    pub(super) fn tick_roster(&mut self, root: &Path) {
        let now = now();
        let current = schedule::spent_set(self.project_team(root), &self.team.quotas, now);
        let Some(before) = self.team.spent.insert(root.to_path_buf(), current) else {
            return;
        };
        let Some(text) = schedule::roster_change(
            self.project_team(root),
            &self.team.quotas,
            &before,
            |b| self.backend_ready(b),
            now,
        ) else {
            return;
        };
        let Some(project) = self.projects.iter().find(|p| p.root == root) else {
            return;
        };
        let to: Vec<String> = project
            .file
            .streams
            .iter()
            .filter(|s| !s.retired && s.role != Role::Worker)
            .map(|s| s.id.clone())
            .collect();
        if to.is_empty() {
            return;
        }
        if let Some(project) = self.projects.iter_mut().find(|p| p.root == root)
            && let Err(error) = project.post(ATRIUM, to.clone(), &text)
        {
            log::warn!("could not post the roster: {error:#}");
            return;
        }
        for id in to {
            self.queue_wake(root, id);
        }
    }

    /// Forgets watches and stand-in marks of streams that are gone or done.
    pub(super) fn prune_team_marks(&mut self, root: &Path) {
        let on = self.project_team(root).on;
        let Some(project) = self.projects.iter().find(|p| p.root == root) else {
            return;
        };
        let live = |id: &str| {
            project.stream(id).is_some_and(|s| {
                !s.retired && project.task_of(s).is_none_or(|t| !t.status.finished())
            })
        };
        self.team
            .watches
            .retain(|(r, id), _| r != root || (on && live(id)));
        self.team.starts.retain(|(r, id), _| r != root || live(id));
        let mut forgot = false;
        if let Some(streams) = self.settings.team_stand_ins.get_mut(root) {
            let before = streams.len();
            streams.retain(|id, _| live(id));
            forgot = streams.len() != before;
            if streams.is_empty() {
                self.settings.team_stand_ins.remove(root);
            }
        }
        if forgot {
            self.settings.save();
        }
    }

    /// A backend's usage limit was hit or cleared: every member it runs
    /// is marked, so their stand-ins take over until the reset. The app's
    /// team and each open project's own team count, since a limit is per
    /// account and a project may name its members differently.
    pub fn team_limit(&mut self, backend: &str, reached: bool, resets_at: Option<i64>) {
        let now = jiff::Timestamp::now().as_second();
        let mut names: BTreeSet<String> = self
            .team
            .team
            .members
            .iter()
            .filter(|(_, m)| m.agent == backend)
            .map(|(name, _)| name.clone())
            .collect();
        for project in self.team.projects.values() {
            names.extend(
                project
                    .team
                    .members
                    .iter()
                    .filter(|(_, m)| m.agent == backend)
                    .map(|(name, _)| name.clone()),
            );
        }
        let names: Vec<String> = names.into_iter().collect();
        for name in names {
            if reached {
                self.team.quotas.hit(&name, resets_at, now);
            } else {
                self.team.quotas.cleared(&name);
            }
        }
    }
}

impl Workspace {
    /// Feeds a running lead's or worker's watch from what it finished; when
    /// it is going in circles, its parent is told. The stream itself is
    /// never interrupted or messaged: the parent decides.
    pub(super) fn team_observe(&mut self, event: &AgentEvent, root: &Path, stream: &str) {
        let rules = {
            let team = self.project_team(root);
            if !team.on {
                return;
            }
            team.watch.clone()
        };
        let Some(project) = self.projects.iter().find(|p| p.root == root) else {
            return;
        };
        let Some(s) = project.stream(stream) else {
            return;
        };
        let Some(parent) = s.parent.clone().filter(|_| s.role != Role::Controller) else {
            return;
        };
        if s.retired {
            return;
        }
        let name = s.name.clone();
        let task = project
            .task_of(s)
            .map_or_else(|| s.name.clone(), |t| t.id.clone());
        let signal = match event {
            AgentEvent::ItemCompleted {
                item:
                    Item::Command {
                        command,
                        exit_code,
                        output,
                        ..
                    },
                ..
            } => Signal::Command {
                command: command.clone(),
                exit_code: *exit_code,
                output: output.clone(),
            },
            AgentEvent::ItemCompleted {
                item: Item::FileChange { changes, .. },
                ..
            } => Signal::Edit {
                paths: changes.iter().map(|c| c.path.clone()).collect(),
                diff: changes
                    .iter()
                    .map(|c| c.diff.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
            AgentEvent::TurnCompleted { thread_id, .. } => Signal::TurnEnded {
                tokens: self
                    .transcripts
                    .get(thread_id)
                    .and_then(|t| t.usage())
                    .map_or(0, |u| u.last.total),
            },
            _ => return,
        };
        let watch = self
            .team
            .watches
            .entry((root.to_path_buf(), stream.to_string()))
            .or_default();
        watch.feed(&signal);
        let Some(stuck) = watch.verdict(&rules) else {
            return;
        };
        let text = watch::notice(&name, &task, &stuck, &watch.trail());
        watch.reported(&rules);
        if let Some(project) = self.projects.iter_mut().find(|p| p.root == root)
            && let Err(error) = project.post(ATRIUM, vec![parent.clone()], &text)
        {
            log::warn!("could not post a watch notice: {error:#}");
        }
        self.queue_wake(root, parent);
    }

    /// An ACP agent's turn failed. If it says a usage limit was hit, the
    /// member is marked spent. Whenever the team is on, the stream's task
    /// goes back to open when a stand-in can take it, so the scheduler's
    /// normal task start places it again through `pick`; the autopilot is
    /// not needed for this.
    pub(super) fn team_turn_failed(&mut self, root: &Path, stream: &str, error: &str) {
        let Some(project) = self.projects.iter().find(|p| p.root == root) else {
            return;
        };
        let Some(s) = project.stream(stream) else {
            return;
        };
        if agents::is_codex(&s.backend)
            || !self.project_team(root).on
            || !atrium_team::quota::is_limit_message(error)
        {
            return;
        }
        let backend = s.backend.clone();
        let target = project
            .task_of(s)
            .filter(|t| {
                !s.retired
                    && s.parent.is_some()
                    && s.task.as_deref() == Some(t.key().as_str())
                    && !t.status.finished()
            })
            .map(|t| {
                let owner = project.stream(&t.owner).map(|o| o.backend.clone());
                (t.key(), t.agent.clone().or(owner))
            });
        self.team_limit(&backend, true, None);
        let Some((key, Some(wanted))) = target else {
            return;
        };
        if !schedule::limit_handback(
            self.project_team(root),
            &wanted,
            &backend,
            &self.team.quotas,
            |b| self.backend_ready(b),
            now(),
        ) {
            return;
        }
        if let Some(project) = self.projects.iter_mut().find(|p| p.root == root) {
            if let Some(s) = project.stream_mut(stream) {
                s.retired = true;
                s.task = None;
            }
            let result = project
                .save()
                .and_then(|()| project.set_task_status(&key, TaskStatus::Open));
            if let Err(error) = result {
                log::warn!("could not hand {key} back: {error:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::ConfigChoice;

    fn option(id: &str, category: Option<&str>, current: &str) -> ConfigOption {
        ConfigOption {
            id: id.into(),
            name: id.into(),
            category: category.map(str::to_owned),
            current: current.into(),
            choices: vec![ConfigChoice {
                value: current.into(),
                name: current.into(),
                description: None,
                group: None,
            }],
        }
    }

    #[test]
    fn a_members_model_and_effort_reach_the_options_their_categories_name() {
        let options = [
            option("mode", Some("mode"), "default"),
            option("model", Some("model"), "default"),
            option("reasoning_effort", Some("thought_level"), "high"),
        ];
        assert_eq!(
            option_updates(&options, Some("sonnet"), Some("low")),
            vec![
                ("model".to_string(), "sonnet".to_string()),
                ("reasoning_effort".to_string(), "low".to_string()),
            ]
        );
        // Only what the member set changes.
        assert_eq!(
            option_updates(&options, Some("sonnet"), None),
            vec![("model".to_string(), "sonnet".to_string())]
        );
        assert!(option_updates(&options, None, None).is_empty());
        // Categories the member does not name stay as they are.
        let unknown = [option("weird", Some("something_else"), "x")];
        assert!(option_updates(&unknown, Some("sonnet"), Some("low")).is_empty());
    }

    #[test]
    fn only_a_member_that_names_model_or_effort_overrides_the_defaults() {
        let placed = |model: Option<&str>, effort: Option<&str>| Pick {
            member: "gemini".into(),
            agent: "antigravity".into(),
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            instead_of: None,
        };
        assert_eq!(
            start_choice(&placed(Some("flash"), Some("low"))),
            Some(ModelChoice {
                model: Some("flash".into()),
                effort: Some("low".into()),
            })
        );
        assert_eq!(start_choice(&placed(None, None)), None);
    }
}

#[cfg(all(test, unix, feature = "workspace-ticker-test"))]
#[path = "workspace_team_test.rs"]
mod ticker_tests;
