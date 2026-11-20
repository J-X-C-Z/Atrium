//! Read-only pull-request status for lead branches.
use crate::{Checks, Project, PullRequest, Role};
use anyhow::{Context, Result, bail, ensure};
use atrium_process::ProcessTree;
use serde::Deserialize;
use std::{
    io::{BufReader, Read, Seek},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const FIELDS: &str = "number,url,state,isDraft,statusCheckRollup";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    number: u64,
    url: String,
    state: String,
    is_draft: bool,
    status_check_rollup: Option<Vec<Check>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Check {
    Run {
        status: String,
        conclusion: Option<String>,
    },
    Context {
        state: String,
    },
}

impl View {
    fn badge(self) -> Result<PullRequest> {
        ensure!(
            self.number > 0
                && (self.url.starts_with("https://") || self.url.starts_with("http://")),
            "invalid PR link"
        );
        let state = match self.state.as_str() {
            "OPEN" | "CLOSED" | "MERGED" if self.is_draft => "draft",
            "OPEN" => "open",
            "CLOSED" => "closed",
            "MERGED" => "merged",
            other => bail!("unexpected PR state {other:?}"),
        };
        let mut checks = Checks::None;
        for check in self.status_check_rollup.into_iter().flatten() {
            let pending = match check {
                Check::Run { status, conclusion } => {
                    if matches!(
                        conclusion.as_deref(),
                        Some("FAILURE" | "CANCELLED" | "TIMED_OUT")
                    ) {
                        checks = Checks::Failing;
                        break;
                    }
                    status != "COMPLETED"
                }
                Check::Context { state } => match state.as_str() {
                    "ERROR" | "FAILURE" => {
                        checks = Checks::Failing;
                        break;
                    }
                    "EXPECTED" | "PENDING" => true,
                    "SUCCESS" => false,
                    other => bail!("unexpected commit status {other:?}"),
                },
            };
            if pending {
                checks = Checks::Pending;
            } else if checks == Checks::None {
                checks = Checks::Passing;
            }
        }
        Ok(PullRequest {
            number: self.number,
            url: self.url,
            state: state.into(),
            checks,
        })
    }
}

pub enum Lookup {
    Badge(Option<PullRequest>),
    Unavailable(&'static str),
}

/// Run off the UI thread. A missing PR clears a badge; an inconclusive lookup leaves it alone.
pub fn lookup(gh: &Path, root: &Path, branch: &str) -> Result<Lookup> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut command = Command::new(gh);
    command
        .current_dir(root)
        .args(["pr", "view", branch, "--json", FIELDS])
        .env_remove("GH_REPO")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut tree = ProcessTree::spawn(&mut command)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !tree.exited()? {
        ensure!(Instant::now() < deadline, "gh pr view timed out");
        thread::sleep(Duration::from_millis(50));
    }
    let status = tree.finish()?;
    if status.success() {
        stdout.rewind()?;
        let view: View = serde_json::from_reader(BufReader::new(stdout))
            .context("invalid gh pr view response")?;
        return Ok(Lookup::Badge(Some(view.badge()?)));
    }
    stderr.rewind()?;
    let mut message = String::new();
    stderr.take(16 * 1024).read_to_string(&mut message)?;
    if status.code() == Some(4) || message.contains("HTTP 401") || message.contains("gh auth login")
    {
        return Ok(Lookup::Unavailable("gh authentication is required"));
    }
    if status.code() == Some(1)
        && let Some(reference) = message
            .trim()
            .strip_prefix("no pull requests found for branch ")
            .and_then(|value| serde_json::from_str::<String>(value).ok())
        && reference.rsplit(':').next() == Some(branch)
    {
        return Ok(Lookup::Badge(None));
    }
    bail!("gh pr view exited with {status}");
}

impl Project {
    /// Apply a result to the live lead, keeping all concurrent project changes.
    pub fn set_pr_badge(
        &mut self,
        id: &str,
        created_at: i64,
        badge: Option<PullRequest>,
    ) -> Result<bool> {
        let Some(stream) = self.stream_mut(id) else {
            return Ok(false);
        };
        if stream.created_at != created_at || stream.role != Role::Lead || stream.pr == badge {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut stream.pr, badge);
        if let Err(error) = self.save() {
            self.stream_mut(id).unwrap().pr = previous;
            return Err(error);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workstream;
    use serde_json::{Value, json};

    fn fixture(state: &str, draft: bool, checks: Value) -> PullRequest {
        serde_json::from_value::<View>(json!({
            "number":42, "url":"https://github.com/example/repo/pull/42", "state":state,
            "isDraft":draft, "statusCheckRollup":checks
        }))
        .unwrap()
        .badge()
        .unwrap()
    }

    #[test]
    fn maps_pr_state_number_and_link() {
        for (state, draft, expected) in [
            ("OPEN", true, "draft"),
            ("OPEN", false, "open"),
            ("MERGED", false, "merged"),
            ("CLOSED", false, "closed"),
        ] {
            let pr = fixture(state, draft, json!([]));
            assert_eq!(
                (pr.number, pr.url.as_str(), pr.state.as_str(), pr.checks),
                (
                    42,
                    "https://github.com/example/repo/pull/42",
                    expected,
                    Checks::None
                )
            );
        }
    }

    #[test]
    fn maps_check_runs_and_commit_statuses_with_failure_first() {
        let passed = json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"});
        let pending = json!({"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""});
        let context =
            |state: &str| json!({"__typename":"StatusContext","context":"ci/build","state":state});
        for failure in ["FAILURE", "CANCELLED", "TIMED_OUT"] {
            let failed = json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":failure});
            for mixed in [json!([pending, failed, passed]), json!([failed, pending])] {
                assert_eq!(fixture("OPEN", false, mixed).checks, Checks::Failing);
            }
        }
        for (rollup, expected) in [
            (json!([context("FAILURE")]), Checks::Failing),
            (json!([context("ERROR")]), Checks::Failing),
            (json!([pending, passed]), Checks::Pending),
            (json!([context("PENDING")]), Checks::Pending),
            (json!([context("EXPECTED")]), Checks::Pending),
            (json!([passed, context("SUCCESS")]), Checks::Passing),
            (json!(null), Checks::None),
        ] {
            assert_eq!(fixture("OPEN", false, rollup).checks, expected);
        }
    }

    #[test]
    fn persists_only_changed_badges_on_the_original_lead() {
        let tmp = tempfile::tempdir().unwrap();
        let mut project = Project::open(tmp.path()).unwrap();
        project.file.streams.push(Workstream {
            id: "web".into(),
            role: Role::Lead,
            created_at: 10,
            ..Default::default()
        });
        let badge = Some(fixture("OPEN", false, json!([])));
        assert!(!project.set_pr_badge("web", 9, badge.clone()).unwrap());
        assert!(project.set_pr_badge("web", 10, badge.clone()).unwrap());
        assert_eq!(
            Project::load(tmp.path()).unwrap().stream("web").unwrap().pr,
            badge
        );
        // Make a save fail, so a spurious write on an unchanged poll is observable.
        let path = project.dir().join("project.json");
        std::fs::rename(&path, project.dir().join("saved")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(!project.set_pr_badge("web", 10, badge.clone()).unwrap());
        assert!(project.set_pr_badge("web", 10, None).is_err());
        assert_eq!(project.stream("web").unwrap().pr, badge);
        std::fs::remove_dir(&path).unwrap();
        assert!(project.set_pr_badge("web", 10, None).unwrap());
        assert!(
            Project::load(tmp.path())
                .unwrap()
                .stream("web")
                .unwrap()
                .pr
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_only_pr_view_and_distinguishes_absence_from_errors() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let gh = tmp.path().join("fake-gh");
        fs::write(&gh, r#"#!/bin/sh
[ "$#" = 5 ] && [ "$1" = pr ] && [ "$2" = view ] && [ "$3" = atrium/web ] && [ "$4" = --json ] && [ "$5" = number,url,state,isDraft,statusCheckRollup ] || exit 7
[ "$GH_PROMPT_DISABLED" = 1 ] || exit 7
cat stdout
cat stderr >&2
exit "$(cat code)"
"#).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        let run = |code: &str, out: &str, err: &str| {
            for (file, value) in [("code", code), ("stdout", out), ("stderr", err)] {
                fs::write(tmp.path().join(file), value).unwrap();
            }
            lookup(&gh, tmp.path(), "atrium/web")
        };
        assert!(
            matches!(run("0",r#"{"number":42,"url":"https://gh.example/o/r/pull/42","state":"OPEN","isDraft":false,"statusCheckRollup":[]}"#,"").unwrap(), Lookup::Badge(Some(pr)) if pr.number==42)
        );
        for branch in ["atrium/web", "octocat:atrium/web"] {
            assert!(matches!(
                run(
                    "1",
                    "",
                    &format!("no pull requests found for branch {branch:?}")
                )
                .unwrap(),
                Lookup::Badge(None)
            ));
        }
        assert!(matches!(run("4", "", "").unwrap(), Lookup::Unavailable(_)));
        assert!(matches!(
            run("1", "", "HTTP 401: Bad credentials").unwrap(),
            Lookup::Unavailable(_)
        ));
        assert!(run("1", "", "network error").is_err());
        assert!(run("0", "truncated", "").is_err());
    }
}
