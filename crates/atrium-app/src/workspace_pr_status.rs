//! One read-only gh query at a time across the workspace.
use super::*;
use crate::project::pr::{self, Lookup};
use gpui::AppContext;

#[derive(Default)]
pub(super) enum Poll {
    #[default]
    Ready,
    Running,
    After(Instant),
    Disabled,
}

impl Poll {
    fn due(&self, now: Instant) -> bool {
        matches!(self, Self::Ready) || matches!(self, Self::After(next) if now >= *next)
    }
}

impl Workspace {
    pub(super) fn tick_pr_badges(&mut self, cx: &mut Context<Self>) {
        self.poll_pr_badges(Instant::now(), || agent_rpc::process::which("gh"), cx);
    }

    fn poll_pr_badges(
        &mut self,
        now: Instant,
        find_gh: impl FnOnce() -> Option<PathBuf> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let mut targets = Vec::new();
        let due = self.project_schedule.pr_poll.due(now);
        for project in &mut self.projects {
            if project.file.worktrees && !due {
                continue;
            }
            let leads: Vec<_> = project
                .file
                .streams
                .iter()
                .filter(|s| s.role == Role::Lead && (project.file.worktrees || s.pr.is_some()))
                .map(|s| (s.id.clone(), s.created_at))
                .collect();
            for (id, created_at) in leads {
                if project.file.worktrees {
                    targets.push((project.root.clone(), id, created_at));
                } else {
                    match project.set_pr_badge(&id, created_at, None) {
                        Ok(true) => cx.notify(),
                        Ok(false) => (),
                        Err(e) => log::warn!("clearing PR badge for {id}: {e:#}"),
                    }
                }
            }
        }
        if targets.is_empty() {
            return;
        }
        self.project_schedule.pr_poll = Poll::Running;
        cx.spawn(async move |this, cx| {
            let gh = cx.background_spawn(async move { find_gh() }).await;
            let mut next = Poll::After(now + Duration::from_secs(120));
            if let Some(gh) = gh {
                for (root, id, created_at) in targets {
                    let query = cx
                        .background_spawn({
                            let gh = gh.clone();
                            let root = root.clone();
                            let branch = Project::branch_of(&id);
                            async move { pr::lookup(&gh, &root, &branch) }
                        })
                        .await;
                    match query {
                        Ok(Lookup::Badge(badge)) => {
                            if this
                                .update(cx, |ws, cx| {
                                    if let Some(p) =
                                        ws.project_mut(&root).filter(|p| p.file.worktrees)
                                    {
                                        match p.set_pr_badge(&id, created_at, badge) {
                                            Ok(true) => cx.notify(),
                                            Ok(false) => (),
                                            Err(e) => log::warn!("saving PR badge for {id}: {e:#}"),
                                        }
                                    }
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(Lookup::Unavailable(reason)) => {
                            log::warn!("PR polling disabled: {reason}");
                            next = Poll::Disabled;
                            break;
                        }
                        Err(e) => {
                            log::warn!("looking up PR for {} in {}: {e:#}", id, root.display())
                        }
                    }
                }
            } else {
                log::warn!("PR polling disabled: gh not found");
                next = Poll::Disabled;
            }
            let _ = this.update(cx, |ws, _| ws.project_schedule.pr_poll = next);
        })
        .detach();
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

    #[gpui::test(iterations = 1)]
    fn polls_only_lead_branches_on_schedule_and_preserves_badges_on_errors(
        cx: &mut TestAppContext,
    ) {
        let tmp = Fixture(std::env::temp_dir().join(format!("atrium-pr-{}", uuid::Uuid::new_v4())));
        let root = tmp.0.join("enabled");
        let other = tmp.0.join("disabled");
        let gh = tmp.0.join("gh");
        let mut projects = Vec::new();
        for folder in [&root, &other] {
            let mut p = Project::open(folder).unwrap();
            p.file.worktrees = folder == &root;
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
            projects.push(p);
        }
        fs::write(&gh,r#"#!/bin/sh
echo "$3" >> calls
if [ -f offline ]; then exit 1; fi
if [ -f unauthenticated ]; then exit 4; fi
echo '{"number":7,"url":"https://github.com/o/r/pull/7","state":"OPEN","isDraft":false,"statusCheckRollup":[]}'
"#).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        let ws = cx.new(|_| {
            let mut ws = Workspace::with_settings(Default::default());
            ws.projects = projects;
            ws
        });
        let start = Instant::now();
        let poll = |cx: &mut TestAppContext, secs| {
            ws.update(cx, |ws, cx| {
                let now = start + Duration::from_secs(secs);
                let gh = gh.clone();
                ws.poll_pr_badges(now, move || Some(gh), cx);
                ws.poll_pr_badges(now, || panic!("overlapping or duplicate poll"), cx);
            });
            cx.run_until_parked();
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
        poll(cx, 119);
        assert_eq!(
            fs::read_to_string(root.join("calls")).unwrap(),
            "atrium/web\n"
        );
        fs::write(root.join("offline"), "").unwrap();
        poll(cx, 120);
        assert!(
            Project::load(&root)
                .unwrap()
                .stream("web")
                .unwrap()
                .pr
                .is_some()
        );
        fs::remove_file(root.join("offline")).unwrap();
        fs::write(root.join("unauthenticated"), "").unwrap();
        poll(cx, 240);
        poll(cx, 480);
        assert_eq!(
            fs::read_to_string(root.join("calls"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        assert!(!other.join("calls").exists());
        ws.update(cx, |ws, _| ws.projects[0].file.worktrees = false);
        poll(cx, 481);
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
    fn missing_gh_disables_later_polls(cx: &mut TestAppContext) {
        let fixture =
            Fixture(std::env::temp_dir().join(format!("atrium-pr-{}", uuid::Uuid::new_v4())));
        let mut p = Project::open(&fixture.0).unwrap();
        p.file.worktrees = true;
        p.file.streams.push(Workstream {
            id: "web".into(),
            role: Role::Lead,
            ..Default::default()
        });
        let ws = cx.new(|_| {
            let mut ws = Workspace::with_settings(Default::default());
            ws.projects.push(p);
            ws
        });
        let now = Instant::now();
        ws.update(cx, |ws, cx| ws.poll_pr_badges(now, || None, cx));
        cx.run_until_parked();
        ws.update(cx, |ws, cx| {
            assert!(matches!(ws.project_schedule.pr_poll, Poll::Disabled));
            ws.poll_pr_badges(
                now + Duration::from_secs(360),
                || panic!("looked up gh again"),
                cx,
            );
        });
        cx.run_until_parked();
    }
}
