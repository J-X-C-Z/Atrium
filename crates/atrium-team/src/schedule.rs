//! What the scheduler does with a team, as plain functions: where a task
//! starts, what the planners are told, and when the roster changed. The
//! app calls these; they hold no state and do no I/O.

use std::collections::BTreeSet;

use crate::{Pick, Quotas, Skip, Team, Wait, pick, roster, rules_file};

/// Where a task that asked for `wanted` starts. With the team off it
/// starts exactly where it asked, whatever `ready` says.
pub fn place(
    team: &Team,
    wanted: &str,
    quotas: &Quotas,
    ready: impl Fn(&str) -> bool,
    now: i64,
) -> Result<Pick, Wait> {
    if !team.on {
        return Ok(Pick {
            member: wanted.to_string(),
            agent: wanted.to_string(),
            model: None,
            effort: None,
            instead_of: None,
        });
    }
    pick(team, wanted, quotas, ready, now)
}

/// The one notice to a task's owner when its task starts on a stand-in.
/// `at` says when a spent member is back, in the user's local time.
pub fn stand_in_notice(task: &str, placed: &Pick, at: impl Fn(i64) -> String) -> Option<String> {
    let (member, why) = placed.instead_of.as_ref()?;
    let why = match why {
        Skip::Spent(until) => format!("{member}'s limit is reached until {}", at(*until)),
        Skip::Unavailable => format!("{member} is not available now"),
    };
    Some(format!("`{task}` runs on {}: {why}.", placed.member))
}

/// What the team adds to a kickoff: nothing when it is off; otherwise the
/// lesson line for everyone, and the roster for the controller and leads.
/// The result is empty or starts with a blank line.
pub fn kickoff_extras(
    team: &Team,
    quotas: &Quotas,
    backend: &str,
    planner: bool,
    ready: impl Fn(&str) -> bool,
    now: i64,
) -> String {
    if !team.on {
        return String::new();
    }
    let mut out = String::new();
    if planner {
        out.push_str("\n\n");
        out.push_str(roster(team, quotas, ready, now).trim_end());
    }
    out.push_str(&format!(
        "\n\nLessons from the user's corrections go in {}; prefer turning each into a test or check.",
        rules_file(backend)
    ));
    out
}

/// The members spent at `now`.
pub fn spent_set(team: &Team, quotas: &Quotas, now: i64) -> BTreeSet<String> {
    team.members
        .keys()
        .filter(|name| quotas.spent_until(name, now).is_some())
        .cloned()
        .collect()
}

/// The new roster to post when who is spent has changed since `before`;
/// the caller keeps `spent_set` as the next `before`.
pub fn roster_change(
    team: &Team,
    quotas: &Quotas,
    before: &BTreeSet<String>,
    ready: impl Fn(&str) -> bool,
    now: i64,
) -> Option<String> {
    if !team.on || spent_set(team, quotas, now) == *before {
        return None;
    }
    Some(roster(team, quotas, ready, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Team {
        Team {
            on: true,
            ..Team::default()
        }
    }

    #[test]
    fn a_spent_member_is_replaced_and_the_owner_hears_once() {
        let team = on();
        let mut quotas = Quotas::default();
        quotas.hit("claude", Some(5_000), 1_000);
        let placed = place(&team, "claude", &quotas, |_| true, 1_000).unwrap();
        assert_eq!(placed.agent, "antigravity");
        let text = stand_in_notice("api", &placed, |at| format!("t{at}")).unwrap();
        assert_eq!(
            text,
            "`api` runs on gemini: claude's limit is reached until t5000."
        );
        // Not replaced: no notice.
        let placed = place(&team, "gpt", &quotas, |_| true, 1_000).unwrap();
        assert!(stand_in_notice("api", &placed, |_| String::new()).is_none());
    }

    #[test]
    fn everyone_spent_waits_until_the_first_is_back() {
        let team = on();
        let mut quotas = Quotas::default();
        quotas.hit("claude", Some(9_000), 1_000);
        quotas.hit("gemini", Some(7_000), 1_000);
        let wait = place(&team, "claude", &quotas, |_| true, 1_000).unwrap_err();
        assert_eq!(wait.until, Some(7_000));
        // At that time the task starts again.
        assert!(place(&team, "claude", &quotas, |_| true, 7_000).is_ok());
    }

    #[test]
    fn team_off_places_as_asked() {
        let mut quotas = Quotas::default();
        quotas.hit("claude", Some(5_000), 1_000);
        let placed = place(&Team::default(), "claude", &quotas, |_| false, 1_000).unwrap();
        assert_eq!(placed.agent, "claude");
        assert!(placed.instead_of.is_none());
    }

    #[test]
    fn team_off_adds_nothing_to_a_kickoff() {
        let quotas = Quotas::default();
        assert_eq!(
            kickoff_extras(&Team::default(), &quotas, "claude", true, |_| true, 0),
            ""
        );
    }

    #[test]
    fn planners_get_the_roster_and_workers_only_the_lesson() {
        let team = on();
        let quotas = Quotas::default();
        let planner = kickoff_extras(&team, &quotas, "claude", true, |_| true, 0);
        assert!(planner.contains("## Your team"));
        assert!(planner.ends_with("go in CLAUDE.md; prefer turning each into a test or check."));
        let worker = kickoff_extras(&team, &quotas, "codex", false, |_| true, 0);
        assert!(!worker.contains("Your team"));
        assert!(worker.contains("go in AGENTS.md;"));
    }

    #[test]
    fn the_roster_is_posted_only_when_who_is_spent_changes() {
        let team = on();
        let mut quotas = Quotas::default();
        let before = spent_set(&team, &quotas, 0);
        assert!(roster_change(&team, &quotas, &before, |_| true, 0).is_none());
        quotas.hit("claude", Some(100), 0);
        let text = roster_change(&team, &quotas, &before, |_| true, 0).unwrap();
        assert!(text.contains("spent"));
        let after = spent_set(&team, &quotas, 0);
        assert!(roster_change(&team, &quotas, &after, |_| true, 0).is_none());
        // Back again.
        assert!(roster_change(&team, &quotas, &after, |_| true, 200).is_some());
    }
}
