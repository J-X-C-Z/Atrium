use super::*;
use crate::watch::{Signal, Stuck, Watch};

const NOW: i64 = 1_800_000_000;

fn all_ready(_: &str) -> bool {
    true
}

#[test]
fn default_team_round_trips() {
    let team = Team::default();
    let text = team.to_toml();
    assert!(text.contains("[members.claude]"), "{text}");
    assert_eq!(Team::parse(&text).unwrap(), team);
}

#[test]
fn parse_reports_unknown_stand_ins() {
    let mut team = Team::default();
    team.members.get_mut("gpt").unwrap().stand_ins = vec!["nobody".into()];
    assert_eq!(
        Team::parse(&team.to_toml()),
        Err(TeamError::UnknownMember {
            member: "gpt".into(),
            name: "nobody".into()
        })
    );
    assert!(matches!(Team::parse("on = 3"), Err(TeamError::Parse(_))));
}

#[test]
fn a_task_runs_on_the_member_it_names() {
    let team = Team::default();
    let q = Quotas::default();
    let p = pick(&team, "gpt", &q, all_ready, NOW).unwrap();
    assert_eq!(
        (p.member.as_str(), p.agent.as_str(), p.instead_of),
        ("gpt", "codex", None)
    );
    // A backend id works as well as a member name.
    assert_eq!(
        pick(&team, "codex", &q, all_ready, NOW).unwrap().member,
        "gpt"
    );
    // Something not on the team runs as asked.
    let p = pick(&team, "claude@studio", &q, all_ready, NOW).unwrap();
    assert_eq!(p.agent, "claude@studio");
}

#[test]
fn a_spent_member_is_covered_until_it_resets() {
    let team = Team::default();
    let mut q = Quotas::default();
    q.hit("claude", Some(NOW + 600), NOW);
    let p = pick(&team, "claude", &q, all_ready, NOW).unwrap();
    assert_eq!(p.member, "gemini");
    assert_eq!(
        p.instead_of,
        Some(("claude".into(), Skip::Spent(NOW + 600)))
    );
    assert_eq!(
        pick(&team, "claude", &q, all_ready, NOW + 600)
            .unwrap()
            .member,
        "claude"
    );
}

#[test]
fn stand_ins_chain_and_otherwise_wait() {
    let team = Team::default();
    let mut q = Quotas::default();
    // DeepSeek is offline, its stand-in GPT is spent, GPT's stand-in is Claude.
    q.hit("gpt", Some(NOW + 900), NOW);
    let p = pick(&team, "deepseek", &q, |a| a != "deepseek", NOW).unwrap();
    assert_eq!(p.member, "claude");
    assert_eq!(p.instead_of, Some(("deepseek".into(), Skip::Unavailable)));
    // Everyone spent: wait for the first back. Cycles do not loop.
    q.hit("claude", Some(NOW + 300), NOW);
    q.hit("gemini", Some(NOW + 1200), NOW);
    assert_eq!(
        pick(&team, "claude", &q, all_ready, NOW),
        Err(Wait {
            until: Some(NOW + 300)
        })
    );
}

#[test]
fn a_limit_without_a_reset_time_waits_an_hour() {
    let mut q = Quotas::default();
    q.hit("claude", None, NOW);
    assert_eq!(
        q.spent_until("claude", NOW + 1),
        Some(NOW + quota::UNKNOWN_RESET_SECS)
    );
    assert_eq!(
        q.spent_until("claude", NOW + quota::UNKNOWN_RESET_SECS),
        None
    );
    q.hit("claude", Some(NOW + 60), NOW);
    q.cleared("claude");
    assert_eq!(q.spent_until("claude", NOW), None);
    assert!(quota::is_limit_message(
        "You've hit your usage limit. Try again at 5pm."
    ));
    assert!(quota::is_limit_message("HTTP 429 Too Many Requests"));
    assert!(!quota::is_limit_message("error[E0308]: mismatched types"));
}

#[test]
fn the_roster_tells_the_controller_who_is_there() {
    let team = Team::default();
    let mut q = Quotas::default();
    q.hit("claude", Some(NOW + 90 * 60), NOW);
    let text = roster(&team, &q, |a| a != "deepseek", NOW);
    assert!(text.contains("`agent: <name>`"), "{text}");
    assert!(
        text.contains("- `claude` (spent, back in about 90 min): 审美"),
        "{text}"
    );
    assert!(text.contains("- `deepseek` (not available now)"), "{text}");
    assert!(text.contains("- `gpt`: 严谨"), "{text}");
}

fn failing(cmd: &str, out: &str) -> Signal {
    Signal::Command {
        command: cmd.into(),
        exit_code: Some(1),
        output: out.into(),
    }
}

#[test]
fn the_same_error_three_times_is_stuck() {
    let rules = WatchRules::default();
    let mut w = Watch::new();
    w.feed(&failing(
        "cargo test",
        "...\nerror[E0308]: mismatched types at src/a.rs:10:5",
    ));
    w.feed(&Signal::Edit {
        paths: vec!["src/a.rs".into()],
        diff: "-a\n+b".into(),
    });
    w.feed(&failing(
        "cargo test",
        "error[E0308]: mismatched types at src/a.rs:12:9",
    ));
    assert_eq!(w.verdict(&rules), None);
    w.feed(&Signal::Edit {
        paths: vec!["src/a.rs".into()],
        diff: "-b\n+c".into(),
    });
    w.feed(&failing(
        "cargo test",
        "error[E0308]: mismatched types at src/a.rs:14:1",
    ));
    match w.verdict(&rules) {
        Some(Stuck::SameError { times: 3, error }) => {
            assert!(error.contains("mismatched types"), "{error}")
        }
        other => panic!("{other:?}"),
    }
    // A success in between resets the count.
    let mut w = Watch::new();
    w.feed(&failing("cargo test", "error: x"));
    w.feed(&Signal::Command {
        command: "ls".into(),
        exit_code: Some(0),
        output: String::new(),
    });
    w.feed(&failing("cargo test", "error: x"));
    w.feed(&failing("cargo test", "error: x"));
    assert_eq!(w.verdict(&rules), None);
}

#[test]
fn undoing_and_redoing_the_same_edit_is_stuck() {
    let rules = WatchRules::default();
    let mut w = Watch::new();
    for i in 0..8 {
        let diff = if i % 2 == 0 { "-a\n+b" } else { "-b\n+a" };
        w.feed(&Signal::Edit {
            paths: vec!["x.rs".into()],
            diff: diff.into(),
        });
        if i == 3 {
            assert_eq!(w.verdict(&rules), None);
        }
    }
    assert!(matches!(
        w.verdict(&rules),
        Some(Stuck::Repeating { times: 4, .. })
    ));
}

#[test]
fn over_budget_is_told_once() {
    let rules = WatchRules {
        task_tokens: Some(5000),
        ..Default::default()
    };
    let mut w = Watch::new();
    w.feed(&Signal::TurnEnded { tokens: 4000 });
    assert_eq!(w.verdict(&rules), None);
    w.feed(&Signal::TurnEnded { tokens: 2000 });
    assert_eq!(w.verdict(&rules), Some(Stuck::OverBudget { tokens: 6000 }));
    w.reported(&rules);
    w.feed(&Signal::TurnEnded { tokens: 2000 });
    assert_eq!(w.verdict(&rules), None);
}

#[test]
fn after_a_report_only_new_circling_counts() {
    let rules = WatchRules::default();
    let mut w = Watch::new();
    for _ in 0..3 {
        w.feed(&failing("make", "error: boom"));
    }
    let why = w.verdict(&rules).unwrap();
    let text = watch::notice("w1", "lead/api", &why, &w.trail());
    assert!(
        text.contains("`w1`") && text.contains("$ make") && text.contains("You decide"),
        "{text}"
    );
    w.reported(&rules);
    assert_eq!(w.verdict(&rules), None);
    for _ in 0..3 {
        w.feed(&failing("make", "error: boom"));
    }
    assert!(w.verdict(&rules).is_some());
}

#[test]
fn fingerprints_ignore_numbers() {
    assert_eq!(
        watch::fingerprint(
            "thread 'main' panicked at src/x.rs:41:7:\nnote: run with RUST_BACKTRACE"
        ),
        "thread 'main' panicked at src/x.rs:#:#:"
    );
    assert_eq!(watch::fingerprint("all good\n"), "all good");
}

#[test]
fn each_agent_has_its_own_rules_file() {
    assert_eq!(rules_file("claude"), "CLAUDE.md");
    assert_eq!(rules_file("codex"), "AGENTS.md");
    assert_eq!(rules_file("antigravity"), "GEMINI.md");
    assert_eq!(rules_file("claude@studio"), "CLAUDE.md");
}

#[test]
fn load_prefers_the_project_file() {
    let dir = std::env::temp_dir().join(format!("atrium-team-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".atrium")).unwrap();
    let mut team = Team {
        on: true,
        ..Default::default()
    };
    team.members.get_mut("gpt").unwrap().notes = "mine".into();
    std::fs::write(dir.join(PROJECT_FILE), team.to_toml()).unwrap();
    let loaded = Team::load(Some(&dir), None).unwrap();
    assert!(loaded.on);
    assert_eq!(loaded.members["gpt"].notes, "mine");
    assert_eq!(
        Team::load(Some(&dir.join("missing")), None).unwrap(),
        Team::default()
    );
    std::fs::remove_dir_all(&dir).ok();
}
