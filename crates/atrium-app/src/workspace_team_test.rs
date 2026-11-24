//! End to end: `mock-acp` fails a turn with a usage limit, the member is
//! marked spent from that turn, and the scheduler restarts the task on its
//! stand-in with the autopilot off.
//!
//! Run after building the mock:
//! `cargo build -p acp-client --bin mock-acp`
//! `cargo test -p atrium-app --features workspace-ticker-test -- --ignored`

use super::*;
use crate::project::{Project, Role, Workstream};
use crate::settings::Settings;
use gpui::{AppContext, TestAppContext};
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `mock-acp`, from the Cargo target folder or `ATRIUM_MOCK_ACP`.
fn mock_acp() -> PathBuf {
    if let Ok(path) = std::env::var("ATRIUM_MOCK_ACP") {
        return PathBuf::from(path);
    }
    let exe = format!("mock-acp{}", std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        candidates.push(PathBuf::from(dir).join("debug").join(&exe));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("debug")
            .join(&exe),
    );
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("build mock-acp first: cargo build -p acp-client --bin mock-acp"))
}

const TEAM: &str = "\
on = true

[members.claude]
agent = \"claude\"
stand_ins = [\"gemini\"]

[members.gemini]
agent = \"antigravity\"
";

#[gpui::test(iterations = 1)]
#[ignore = "requires cargo build -p acp-client --bin mock-acp"]
fn a_usage_limit_moves_the_task_to_its_stand_in_without_the_autopilot(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture =
        Fixture(std::env::temp_dir().join(format!("atrium-team-limit-{}", uuid::Uuid::new_v4())));
    fs::create_dir_all(&fixture.0).unwrap();
    let root = fixture.0.clone();
    let mut project = Project::open(&root).unwrap();
    project.file.streams.push(Workstream {
        id: "boss".into(),
        name: "Boss".into(),
        backend: "claude".into(),
        thread_id: Some("boss-session".into()),
        role: Role::Controller,
        ..Default::default()
    });
    // The autopilot stays off: the team alone moves the task.
    assert!(!project.file.autopilot.on);
    let key = project
        .add_task(
            "boss",
            "Review",
            "# Review\nagent: claude\nCheck the build.\n",
        )
        .unwrap();
    project.save().unwrap();
    fs::write(root.join(atrium_team::PROJECT_FILE), TEAM).unwrap();

    let mock = mock_acp();
    let settings = Settings {
        projects: vec![root.clone()],
        quiet_notifications: true,
        no_predictions: true,
        agent_commands: [
            (
                "claude".to_string(),
                format!("\"{}\" --name claude --fail-limit 1", mock.display()),
            ),
            (
                "antigravity".to_string(),
                format!("\"{}\" --name antigravity", mock.display()),
            ),
        ]
        .into(),
        // Keep the test's app state out of the real settings file.
        save_path: Some(fixture.0.join("settings.json")),
        ..Default::default()
    };
    let ws = cx.new(|cx: &mut gpui::Context<Workspace>| {
        let mut ws = Workspace::with_settings(settings);
        ws.init_agents(cx);
        ws.init_projects(cx);
        ws
    });

    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        let project = Project::load(&root).unwrap();
        let current = project
            .file
            .streams
            .iter()
            .find(|s| !s.retired && s.task.as_deref() == Some(key.as_str()));
        let moved = current.is_some_and(|s| s.backend == "antigravity")
            && project
                .notices
                .iter()
                .any(|n| n.text.contains("runs on gemini"));
        if moved {
            break;
        }
        let (agents, team_on, spent, turns) = ws.read_with(cx, |ws, _| {
            (
                ws.agents
                    .iter()
                    .map(|a| {
                        (
                            a.profile.id.clone(),
                            format!("{:?}", a.status),
                            a.client.is_some(),
                        )
                    })
                    .collect::<Vec<_>>(),
                ws.project_team(&root).on,
                ws.team.quotas.spent_until("claude", now()).is_some(),
                ws.projects
                    .iter()
                    .flat_map(|p| p.file.streams.iter())
                    .map(|s| {
                        let t = s.thread_id.as_ref().and_then(|id| ws.transcripts.get(id));
                        (
                            s.id.clone(),
                            s.thread_id.clone(),
                            t.map_or(0, |t| t.len()),
                            t.map(|t| {
                                t.entries()
                                    .iter()
                                    .filter_map(|e| match &e.kind {
                                        atrium_core::EntryKind::Error(error) => Some(error.clone()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        });
        assert!(
            Instant::now() < deadline,
            "task never moved to the stand-in: streams={:?}, notices={:?}, tasks={:?}, agents={:?}, team_on={team_on}, spent={spent}, turns={turns:?}",
            project
                .file
                .streams
                .iter()
                .map(|s| (&s.id, &s.backend, &s.task, s.retired))
                .collect::<Vec<_>>(),
            project.notices.iter().map(|n| &n.text).collect::<Vec<_>>(),
            project
                .tasks
                .iter()
                .map(|t| (&t.owner, &t.id, &t.status, &t.agent))
                .collect::<Vec<_>>(),
            agents
        );
        thread::sleep(Duration::from_millis(20));
    }

    let project = Project::load(&root).unwrap();
    let current = project
        .file
        .streams
        .iter()
        .find(|s| !s.retired && s.task.as_deref() == Some(key.as_str()))
        .expect("the task has a running stream");
    assert_eq!(current.backend, "antigravity");
    // The failed turn's stream stayed behind on the spent backend.
    assert!(
        project
            .file
            .streams
            .iter()
            .any(|s| s.retired && s.backend == "claude")
    );
    assert!(
        project
            .notices
            .iter()
            .any(|n| n.text.contains("claude's limit is reached"))
    );

    // Marked spent from the failed turn, kept in app state for the badge.
    let (spent, badge, stand_in_file) = ws.read_with(cx, |ws, _| {
        (
            ws.team.quotas.spent_until("claude", now()),
            ws.settings.stand_in(&root, &current.id).map(str::to_owned),
            fs::read_to_string(fixture.0.join("settings.json")).unwrap_or_default(),
        )
    });
    assert!(spent.is_some(), "claude should be spent until it resets");
    assert_eq!(badge.as_deref(), Some("gemini"));
    assert!(stand_in_file.contains("gemini"), "{stand_in_file}");
}
