//! The team: which models a project's controller can hand work to, the
//! user's notes on each, and whose limit is hit right now.

use std::path::PathBuf;

use atrium_team::{Quotas, Team};
use gpui::Context;

use super::Workspace;

/// What the app keeps about the team while it runs.
#[derive(Default)]
pub struct TeamState {
    /// The app's team file, or the default when there is none.
    pub team: Team,
    /// Why the file could not be read, when it could not.
    pub error: Option<String>,
    pub quotas: Quotas,
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
        let quotas = std::mem::take(&mut self.team.quotas);
        self.team = TeamState::load();
        self.team.quotas = quotas;
        cx.notify();
    }

    /// A backend's usage limit was hit or cleared: every member it runs
    /// is marked, so their stand-ins take over until the reset.
    pub fn team_limit(&mut self, backend: &str, reached: bool, resets_at: Option<i64>) {
        let now = jiff::Timestamp::now().as_second();
        let names: Vec<String> = self
            .team
            .team
            .members
            .iter()
            .filter(|(_, m)| m.agent == backend)
            .map(|(name, _)| name.clone())
            .collect();
        for name in names {
            if reached {
                self.team.quotas.hit(&name, resets_at, now);
            } else {
                self.team.quotas.cleared(&name);
            }
        }
    }
}
