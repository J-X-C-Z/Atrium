//! One read-only gh query at a time across the workspace, on the cadence and
//! with the watch decisions ported from T3 Code
//! `apps/server/src/orchestration-v2/PullRequestSyncReactor.ts`, kept in
//! `atrium_project::pr_watch` (release 0.1.11, MIT; see
//! `crates/atrium-engine/UPSTREAM.md`).
use super::*;
use crate::project::{
    Checks,
    autopilot::Wake,
    pr::{self, Lookup, Observation},
    pr_watch::{self, SnapshotState, SyncState, Watch},
};
use gpui::AppContext;

/// One sweep each minute, as T3's `PullRequestSyncReactor` runs. Per-pull
/// request cadence and backoff decide what a sweep actually reads.
const SWEEP: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) enum Poll {
    #[default]
    Ready,
    Running,
    After(Instant),
}

impl Poll {
    fn due(&self, now: Instant) -> bool {
        matches!(self, Self::Ready) || matches!(self, Self::After(next) if now >= *next)
    }
}

/// What Atrium remembers between polls for one lead's pull request.
#[derive(Default)]
pub(in crate::workspace) struct Sync {
    pub state: SyncState,
    /// The last read's cadence view.
    pub snapshot: Option<pr_watch::Snapshot>,
    /// What the lead has already heard, so a read only wakes it with news.
    pub watch: Option<Watch>,
    /// T3 stops a watch that only produced comment-only wakes; Atrium has no
    /// unwatch command, so it stops waking until the head moves.
    pub muted: bool,
}

impl Workspace {
    pub(super) fn tick_pr_badges(&mut self, cx: &mut Context<Self>) {
        self.poll_pr_badges(
            Instant::now(),
            pr_watch::now_millis(),
            || agent_rpc::process::which("gh"),
            cx,
        );
    }

    fn poll_pr_badges(
        &mut self,
        now: Instant,
        now_ms: i64,
        find_gh: impl FnOnce() -> Option<PathBuf> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let due = self.project_schedule.pr_poll.due(now);
        let mut targets = Vec::new();
        for project in &mut self.projects {
            let leads: Vec<_> = project
                .file
                .streams
                .iter()
                .filter(|s| s.role == Role::Lead && (project.file.worktrees || s.pr.is_some()))
                .map(|s| (s.id.clone(), s.created_at, s.pr.is_some(), s.retired))
                .collect();
            for (id, created_at, has_pr, retired) in leads {
                if !project.file.worktrees {
                    if has_pr {
                        match project.set_pr_badge(&id, created_at, None) {
                            Ok(true) => cx.notify(),
                            Ok(false) => (),
                            Err(e) => log::warn!("clearing PR badge for {id}: {e:#}"),
                        }
                    }
                    continue;
                }
                if !due {
                    continue;
                }
                let key = (project.root.clone(), id.clone());
                let sync = self.project_schedule.pr_sync.entry(key).or_default();
                if pr_watch::sync_due(&sync.state, sync.snapshot.as_ref(), !retired, now_ms) {
                    targets.push((project.root.clone(), id, created_at));
                }
            }
        }
        if targets.is_empty() {
            if due {
                self.project_schedule.pr_poll = Poll::Ready;
            }
            return;
        }
        self.project_schedule.pr_poll = Poll::Running;
        cx.spawn(async move |this, cx| {
            let gh = cx.background_spawn(async move { find_gh() }).await;
            let Some(gh) = gh else {
                let _ = this.update(cx, |ws, _| {
                    if !ws.project_schedule.pr_gh_missing {
                        log::warn!("PR polling paused: gh not found; retrying every 15 minutes");
                        ws.project_schedule.pr_gh_missing = true;
                    }
                    ws.mark_unavailable(&targets, now_ms);
                    ws.project_schedule.pr_poll = Poll::After(
                        now + Duration::from_millis(pr_watch::SLOW_SYNC_INTERVAL_MS as u64),
                    );
                });
                return;
            };
            let mut next = Poll::After(now + SWEEP);
            for (root, id, created_at) in targets {
                let query = cx
                    .background_spawn({
                        let gh = gh.clone();
                        let root = root.clone();
                        let branch = Project::branch_of(&id);
                        async move { pr::lookup(&gh, &root, &branch) }
                    })
                    .await;
                let key = (root.clone(), id.clone());
                let stop = match query {
                    Ok(Lookup::Badge(Some(observation))) => {
                        if this
                            .update(cx, |ws, cx| {
                                ws.apply_observation(
                                    &root,
                                    &id,
                                    created_at,
                                    observation,
                                    now_ms,
                                    cx,
                                );
                            })
                            .is_err()
                        {
                            return;
                        }
                        false
                    }
                    Ok(Lookup::Badge(None)) => {
                        if this
                            .update(cx, |ws, cx| {
                                ws.apply_no_pr(&root, &id, created_at, now_ms, cx);
                            })
                            .is_err()
                        {
                            return;
                        }
                        false
                    }
                    Ok(Lookup::Unavailable(reason)) => {
                        if this
                            .update(cx, |ws, _| {
                                if !ws.project_schedule.pr_gh_missing {
                                    log::warn!(
                                        "PR polling paused: {reason}; retrying every 15 minutes"
                                    );
                                    ws.project_schedule.pr_gh_missing = true;
                                }
                                ws.project_schedule
                                    .pr_sync
                                    .entry(key.clone())
                                    .or_default()
                                    .state
                                    .missing(now_ms);
                            })
                            .is_err()
                        {
                            return;
                        }
                        true
                    }
                    Err(e) => {
                        log::warn!("looking up PR for {} in {}: {e:#}", id, root.display());
                        if this
                            .update(cx, |ws, _| {
                                ws.project_schedule
                                    .pr_sync
                                    .entry(key.clone())
                                    .or_default()
                                    .state
                                    .failed(now_ms);
                            })
                            .is_err()
                        {
                            return;
                        }
                        false
                    }
                };
                if stop {
                    next = Poll::After(
                        now + Duration::from_millis(pr_watch::SLOW_SYNC_INTERVAL_MS as u64),
                    );
                    break;
                }
            }
            let _ = this.update(cx, |ws, _| ws.project_schedule.pr_poll = next);
        })
        .detach();
    }

    /// Apply one successful read: store the badge, advance the watch, and
    /// queue the wake the lead has not heard yet (autopilot only, through the
    /// one `wake()` path).
    fn apply_observation(
        &mut self,
        root: &PathBuf,
        id: &str,
        created_at: i64,
        observation: Observation,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        self.project_schedule.pr_gh_missing = false;
        let key = (root.to_owned(), id.to_owned());
        let previous = self
            .project_schedule
            .pr_sync
            .get(&key)
            .and_then(|sync| sync.snapshot.map(|snapshot| snapshot.state));
        if let Some(project) = self.project_mut(root) {
            match project.set_pr_badge(id, created_at, Some(observation.badge.clone())) {
                Ok(true) => cx.notify(),
                Ok(false) => (),
                Err(e) => log::warn!("saving PR badge for {id}: {e:#}"),
            }
        }
        let snapshot = observation.snapshot();
        let ended = previous == Some(SnapshotState::Open)
            && matches!(
                snapshot.state,
                SnapshotState::Merged | SnapshotState::Closed
            );
        let number = observation.badge.number;
        let url = observation.badge.url.clone();
        let base = observation
            .base_branch
            .clone()
            .unwrap_or_else(|| "the base branch".into());
        let mut text = None;
        {
            let sync = self.project_schedule.pr_sync.entry(key).or_default();
            sync.state.succeeded(now_ms);
            sync.snapshot = Some(snapshot);
            let watch = sync
                .watch
                .clone()
                .unwrap_or_else(|| Watch::new(now_ms, observation.badge.checks == Checks::Pending));
            let detail = pr_watch::Detail {
                // GitHub reports statuses and check runs on the head itself,
                // so an empty list after a push is CI that never started.
                checks_always_report: true,
                head_sha: observation.head_sha.clone(),
                checks: observation.checks.clone(),
                mergeability: observation.mergeability,
                // `gh` does not name the viewer; the watch treats the pull
                // request's author as "us", as T3 does without a viewer.
                viewer: None,
                author: observation.author.clone(),
                rebase_required: observation.rebase_required,
                // `gh pr view` does not expose merge-queue removals.
                merge_queue_removal: None,
            };
            let report =
                pr_watch::evaluate(&watch, &detail, observation.remarks.as_deref(), now_ms);
            let head_moved = report.next.head_sha != watch.head_sha;
            if head_moved {
                sync.muted = false;
            }
            let exhausted = report.exhausted;
            let send = !report.changes.is_empty() && !sync.muted;
            // A comment-only wake that hit the limit still tells the agent it
            // stopped watching, then stays quiet until the head moves.
            if exhausted {
                sync.muted = true;
            }
            if send {
                text = Some(pr_watch::watch_message(
                    number,
                    &url,
                    &base,
                    report.next.head_sha.as_deref(),
                    &report,
                ));
            }
            sync.watch = Some(report.next);
        }
        if let Some(text) = text {
            self.queue_autopilot_wake(root, id, Wake::Heartbeat, text);
        }
        if ended {
            self.queue_autopilot_wake(
                root,
                id,
                Wake::Heartbeat,
                pr_watch::watch_end_message(number, &url, snapshot.state, &base),
            );
        }
    }

    /// A read found no pull request for the branch: clear the badge and look
    /// again on Atrium's two-minute discovery cadence.
    fn apply_no_pr(
        &mut self,
        root: &PathBuf,
        id: &str,
        created_at: i64,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        self.project_schedule.pr_gh_missing = false;
        if let Some(project) = self.project_mut(root) {
            match project.set_pr_badge(id, created_at, None) {
                Ok(true) => cx.notify(),
                Ok(false) => (),
                Err(e) => log::warn!("clearing PR badge for {id}: {e:#}"),
            }
        }
        let sync = self
            .project_schedule
            .pr_sync
            .entry((root.to_owned(), id.to_owned()))
            .or_default();
        sync.state.saw_no_pr(now_ms);
        sync.snapshot = None;
        sync.watch = None;
        sync.muted = false;
    }

    /// Mark every target due in this sweep unavailable, so a host outage
    /// backs them all off together.
    fn mark_unavailable(&mut self, targets: &[(PathBuf, String, i64)], now_ms: i64) {
        for (root, id, _) in targets {
            self.project_schedule
                .pr_sync
                .entry((root.clone(), id.clone()))
                .or_default()
                .state
                .missing(now_ms);
        }
    }
}

#[cfg(all(test, unix, feature = "workspace-ticker-test"))]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{fs, os::unix::fs::PermissionsExt};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const BASE: &str = "2026-10-02T12:00:00Z";

    fn view(checks: &str, head: &str, state: &str) -> String {
        format!(
            r#"{{"number":7,"url":"https://github.com/o/r/pull/7","state":"{state}","isDraft":false,"headRefOid":"{head}","baseRefName":"main","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","updatedAt":"{BASE}","closedAt":null,"mergedAt":null,"author":{{"login":"agent"}},"comments":[],"reviews":[],"statusCheckRollup":{checks}}}"#
        )
    }

    fn gh_script(fixture: &Path) -> PathBuf {
        fs::create_dir_all(fixture).unwrap();
        let gh = fixture.join("gh");
        fs::write(
            &gh,
            r#"#!/bin/sh
echo "$3" >> calls
if [ -f offline ]; then exit 1; fi
if [ -f unauthenticated ]; then exit 4; fi
cat view.json
"#,
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        gh
    }

    fn project(root: &Path, worktrees: bool, autopilot: bool) -> Project {
        let mut p = Project::open(root).unwrap();
        p.file.worktrees = worktrees;
        p.file.autopilot.on = autopilot;
        for (id, role) in [
            ("web", Role::Lead),
            ("worker", Role::Worker),
            ("boss", Role::Controller),
        ] {
            p.file.streams.push(Workstream {
                id: id.into(),
                role,
                ..Default::default()
            });
        }
        p.save().unwrap();
        p
    }

    #[gpui::test(iterations = 1)]
    fn polls_on_the_ported_cadence_and_preserves_badges_on_errors(cx: &mut TestAppContext) {
        let tmp = Fixture(std::env::temp_dir().join(format!("atrium-pr-{}", uuid::Uuid::new_v4())));
        let root = tmp.0.join("enabled");
        let other = tmp.0.join("disabled");
        let gh = gh_script(&tmp.0);
        let projects = vec![project(&root, true, false), project(&other, false, false)];
        fs::write(root.join("view.json"), view("[]", "aaaa", "OPEN")).unwrap();
        let ws = cx.new(|_| {
            let mut ws = Workspace::with_settings(Default::default());
            ws.projects = projects;
            ws
        });
        let start = Instant::now();
        let base = pr_watch::at(BASE);
        let poll = |cx: &mut TestAppContext, secs: u64| {
            ws.update(cx, |ws, cx| {
                let now = start + Duration::from_secs(secs);
                let now_ms = base + secs as i64 * 1000;
                let gh = gh.clone();
                ws.poll_pr_badges(now, now_ms, move || Some(gh), cx);
                ws.poll_pr_badges(now, now_ms, || panic!("overlapping or duplicate poll"), cx);
            });
            cx.run_until_parked();
        };
        let calls = || {
            fs::read_to_string(root.join("calls"))
                .unwrap_or_default()
                .lines()
                .count()
        };
        poll(cx, 0);
        assert_eq!(
            Project::load(&root)
                .unwrap()
                .stream("web")
                .unwrap()
                .pr
                .as_ref()
                .unwrap()
                .number,
            7
        );
        assert_eq!(calls(), 1);
        poll(cx, 59);
        assert_eq!(calls(), 1, "a sweep runs once a minute");
        poll(cx, 60);
        assert_eq!(calls(), 2);

        fs::write(root.join("offline"), "").unwrap();
        poll(cx, 120);
        assert_eq!(calls(), 3);
        assert!(
            Project::load(&root)
                .unwrap()
                .stream("web")
                .unwrap()
                .pr
                .is_some(),
            "a failed read leaves the badge alone"
        );
        poll(cx, 179);
        assert_eq!(calls(), 3, "the first failure backs off a minute");
        poll(cx, 180);
        assert_eq!(calls(), 4);
        fs::remove_file(root.join("offline")).unwrap();
        poll(cx, 240);
        assert_eq!(calls(), 4, "the second failure backs off two minutes");
        poll(cx, 300);
        assert_eq!(calls(), 5);

        fs::write(root.join("unauthenticated"), "").unwrap();
        poll(cx, 360);
        assert_eq!(calls(), 6);
        poll(cx, 720);
        assert_eq!(calls(), 6, "an unavailable host waits the slow cadence");
        poll(cx, 1260);
        assert_eq!(calls(), 7);
        assert!(!other.join("calls").exists());
        ws.update(cx, |ws, _| ws.projects[0].file.worktrees = false);
        poll(cx, 1261);
        assert!(
            Project::load(&root)
                .unwrap()
                .stream("web")
                .unwrap()
                .pr
                .is_none()
        );
    }

    #[gpui::test(iterations = 1)]
    fn a_missing_gh_pauses_and_retries_on_the_slow_cadence(cx: &mut TestAppContext) {
        let fixture =
            Fixture(std::env::temp_dir().join(format!("atrium-pr-{}", uuid::Uuid::new_v4())));
        let p = project(&fixture.0, true, false);
        let ws = cx.new(|_| {
            let mut ws = Workspace::with_settings(Default::default());
            ws.projects.push(p);
            ws
        });
        let start = Instant::now();
        let base = pr_watch::at(BASE);
        ws.update(cx, |ws, cx| ws.poll_pr_badges(start, base, || None, cx));
        cx.run_until_parked();
        ws.update(cx, |ws, cx| {
            assert!(matches!(ws.project_schedule.pr_poll, Poll::After(_)));
            ws.poll_pr_badges(
                start + Duration::from_secs(899),
                base + 899 * 1000,
                || panic!("looked up gh before the slow cadence"),
                cx,
            );
        });
        cx.run_until_parked();
        ws.update(cx, |ws, cx| {
            ws.poll_pr_badges(
                start + Duration::from_secs(900),
                base + 900 * 1000,
                || None,
                cx,
            );
        });
        cx.run_until_parked();
        ws.update(cx, |ws, _| {
            assert!(matches!(ws.project_schedule.pr_poll, Poll::After(_)));
        });
    }

    #[gpui::test(iterations = 1)]
    fn wakes_the_lead_once_per_failing_commit_and_keeps_the_watch(cx: &mut TestAppContext) {
        let fixture =
            Fixture(std::env::temp_dir().join(format!("atrium-pr-{}", uuid::Uuid::new_v4())));
        let root = fixture.0.clone();
        let gh = gh_script(&fixture.0);
        fs::write(
            root.join("view.json"),
            view(
                r#"[{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}]"#,
                "aaaa",
                "OPEN",
            ),
        )
        .unwrap();
        let p = project(&root, true, true);
        let ws = cx.new(|_| {
            let mut ws = Workspace::with_settings(Default::default());
            ws.projects.push(p);
            ws
        });
        let start = Instant::now();
        let base = pr_watch::at(BASE);
        let poll = |cx: &mut TestAppContext, secs: u64| {
            ws.update(cx, |ws, cx| {
                let gh = gh.clone();
                ws.poll_pr_badges(
                    start + Duration::from_secs(secs),
                    base + secs as i64 * 1000,
                    move || Some(gh),
                    cx,
                );
            });
            cx.run_until_parked();
        };
        poll(cx, 0);
        // The first read baselines the passing head, as a fresh T3 watch does.
        ws.update(cx, |ws, _| {
            let sync = &ws.project_schedule.pr_sync[&(root.clone(), "web".into())];
            assert!(sync.watch.as_ref().unwrap().passed);
            assert!(!ws.test_requested(&root, "web"));
        });

        fs::write(
            root.join("view.json"),
            view(
                r#"[{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}]"#,
                "aaaa",
                "OPEN",
            ),
        )
        .unwrap();
        poll(cx, 60);
        ws.update(cx, |ws, _| {
            assert_eq!(
                Project::load(&root)
                    .unwrap()
                    .stream("web")
                    .unwrap()
                    .pr
                    .clone()
                    .unwrap()
                    .checks,
                Checks::Failing
            );
            let sync = &ws.project_schedule.pr_sync[&(root.clone(), "web".into())];
            assert_eq!(sync.watch.as_ref().unwrap().failed_checks, ["test"]);
            assert!(ws.test_requested(&root, "web"), "the lead is woken");
            ws.test_clear_requested(&root, "web");
        });
        // The same failing commit is not news twice.
        poll(cx, 120);
        ws.update(cx, |ws, _| assert!(!ws.test_requested(&root, "web")));
        // A new head with the same failure is news again.
        fs::write(
            root.join("view.json"),
            view(
                r#"[{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}]"#,
                "bbbb",
                "OPEN",
            ),
        )
        .unwrap();
        poll(cx, 180);
        ws.update(cx, |ws, _| assert!(ws.test_requested(&root, "web")));
    }

    #[gpui::test(iterations = 1)]
    fn a_merged_pull_request_wakes_once_and_stops_polling(cx: &mut TestAppContext) {
        let fixture =
            Fixture(std::env::temp_dir().join(format!("atrium-pr-{}", uuid::Uuid::new_v4())));
        let root = fixture.0.clone();
        let gh = gh_script(&fixture.0);
        fs::write(root.join("view.json"), view("[]", "aaaa", "OPEN")).unwrap();
        let p = project(&root, true, true);
        let ws = cx.new(|_| {
            let mut ws = Workspace::with_settings(Default::default());
            ws.projects.push(p);
            ws
        });
        let start = Instant::now();
        let base = pr_watch::at(BASE);
        let poll = |cx: &mut TestAppContext, secs: u64| {
            ws.update(cx, |ws, cx| {
                let gh = gh.clone();
                ws.poll_pr_badges(
                    start + Duration::from_secs(secs),
                    base + secs as i64 * 1000,
                    move || Some(gh),
                    cx,
                );
            });
            cx.run_until_parked();
        };
        poll(cx, 0);
        fs::write(
            root.join("view.json"),
            view("[]", "aaaa", "MERGED")
                .replace("\"mergedAt\":null", "\"mergedAt\":\"2026-10-02T12:01:00Z\""),
        )
        .unwrap();
        poll(cx, 60);
        ws.update(cx, |ws, _| {
            assert!(ws.test_requested(&root, "web"));
            ws.test_clear_requested(&root, "web");
            assert_eq!(
                ws.project_schedule.pr_sync[&(root.clone(), "web".into())]
                    .snapshot
                    .unwrap()
                    .state,
                SnapshotState::Merged
            );
        });
        let calls = fs::read_to_string(root.join("calls"))
            .unwrap()
            .lines()
            .count();
        poll(cx, 180);
        assert_eq!(
            fs::read_to_string(root.join("calls"))
                .unwrap()
                .lines()
                .count(),
            calls,
            "a merged pull request is final"
        );
    }
}
