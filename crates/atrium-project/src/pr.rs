//! Read-only pull-request status for lead branches.
//!
//! The observation fields and comment/review shapes are ported from T3 Code
//! `apps/server/src/orchestration-v2/PullRequestSyncReactor.ts` and
//! `apps/server/src/pullRequest/pullRequestSyncKey.ts`, release 0.1.11, MIT
//! (see `crates/atrium-engine/LICENSE-T3`).
use crate::{
    Checks, Project, PullRequest, Role,
    pr_watch::{self, Check, CheckStatus, Mergeability, Remark},
};
use anyhow::{Context, Result, bail, ensure};
use atrium_process::ProcessTree;
use serde::Deserialize;
use serde_json::Value;
use std::{
    io::{BufReader, Read, Seek},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const FIELDS: &str = "number,url,state,isDraft,statusCheckRollup,headRefOid,baseRefName,mergeable,mergeStateStatus,updatedAt,closedAt,mergedAt,author,comments,reviews";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    number: u64,
    url: String,
    state: String,
    is_draft: bool,
    status_check_rollup: Option<Vec<CheckJson>>,
    head_ref_oid: Option<String>,
    base_ref_name: Option<String>,
    mergeable: Option<String>,
    merge_state_status: Option<String>,
    updated_at: Option<String>,
    closed_at: Option<String>,
    merged_at: Option<String>,
    author: Option<Actor>,
    comments: Option<Vec<Comment>>,
    reviews: Option<Vec<Review>>,
}

#[derive(Deserialize)]
struct Actor {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    id: Value,
    author: Option<Actor>,
    body: String,
    created_at: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Review {
    id: Value,
    author: Option<Actor>,
    body: String,
    state: Option<String>,
    submitted_at: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
enum CheckJson {
    Run {
        name: Option<String>,
        status: String,
        conclusion: Option<String>,
        details_url: Option<String>,
        is_required: Option<bool>,
    },
    Context {
        context: String,
        state: String,
        target_url: Option<String>,
    },
}

impl CheckJson {
    fn observation(self) -> Result<Check> {
        Ok(match self {
            CheckJson::Run {
                name,
                status,
                conclusion,
                details_url,
                is_required,
            } => {
                let status = match (status.as_str(), conclusion.as_deref()) {
                    (_, Some("FAILURE" | "TIMED_OUT")) => CheckStatus::Failure,
                    (_, Some("CANCELLED")) => CheckStatus::Cancelled,
                    ("COMPLETED", Some("ACTION_REQUIRED")) => CheckStatus::ActionRequired,
                    ("COMPLETED", Some("SKIPPED" | "NEUTRAL")) => CheckStatus::Skipped,
                    ("COMPLETED", _) => CheckStatus::Success,
                    _ => CheckStatus::Pending,
                };
                Check {
                    name: name.unwrap_or_else(|| "check".into()),
                    status,
                    url: details_url,
                    required: is_required.unwrap_or(false),
                    placeholder: false,
                }
            }
            CheckJson::Context {
                context,
                state,
                target_url,
            } => {
                let status = match state.as_str() {
                    "ERROR" | "FAILURE" => CheckStatus::Failure,
                    "EXPECTED" | "PENDING" => CheckStatus::Pending,
                    "SUCCESS" => CheckStatus::Success,
                    other => bail!("unexpected commit status {other:?}"),
                };
                Check {
                    name: context,
                    status,
                    url: target_url,
                    required: false,
                    placeholder: false,
                }
            }
        })
    }
}

/// One poll's full result: the badge Atrium stores on the lead, plus what
/// [`crate::pr_watch`] needs to decide whether it is news.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub badge: PullRequest,
    pub head_sha: Option<String>,
    /// The branch the pull request targets, for the wake's prose.
    pub base_branch: Option<String>,
    pub mergeability: Mergeability,
    /// The head is behind its base (`mergeStateStatus == "BEHIND"`). `None`
    /// while the host is still computing, so a known answer is not cleared.
    pub rebase_required: Option<bool>,
    pub checks: Vec<Check>,
    /// `None` when the activity read was not whole, so the watch's watermark
    /// does not advance.
    pub remarks: Option<Vec<Remark>>,
    pub author: Option<String>,
    pub updated_at: Option<i64>,
    pub closed_at: Option<i64>,
    pub merged_at: Option<i64>,
}

impl Observation {
    /// The cadence view of this read.
    pub fn snapshot(&self) -> pr_watch::Snapshot {
        pr_watch::Snapshot {
            state: match self.badge.state.as_str() {
                "merged" => pr_watch::SnapshotState::Merged,
                "closed" => pr_watch::SnapshotState::Closed,
                _ => pr_watch::SnapshotState::Open,
            },
            updated_at: self.updated_at,
            merged_at: self.merged_at,
            closed_at: self.closed_at,
        }
    }
}

impl View {
    fn observation(self) -> Result<Observation> {
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
        let checks: Vec<Check> = self
            .status_check_rollup
            .into_iter()
            .flatten()
            .map(CheckJson::observation)
            .collect::<Result<_>>()?;
        let mut summary = Checks::None;
        for check in &checks {
            if check.status.failed() {
                summary = Checks::Failing;
                break;
            }
            if check.status == CheckStatus::Pending {
                summary = Checks::Pending;
            } else if summary == Checks::None {
                summary = Checks::Passing;
            }
        }
        let remarks = self.comments.and_then(|comments| {
            let mut remarks: Vec<Remark> = comments
                .into_iter()
                .map(|comment| Remark {
                    id: id_string(comment.id),
                    author: comment.author.map(|author| author.login),
                    body: comment.body,
                    created_at: comment.created_at.as_deref().map(pr_watch::at).unwrap_or(0),
                    edited_at: None,
                    url: comment.url,
                    path: None,
                    review_state: None,
                })
                .collect();
            let reviews = self.reviews?;
            remarks.extend(reviews.into_iter().map(|review| {
                Remark {
                    id: id_string(review.id),
                    author: review.author.map(|author| author.login),
                    body: review.body,
                    created_at: review
                        .submitted_at
                        .as_deref()
                        .map(pr_watch::at)
                        .unwrap_or(0),
                    edited_at: None,
                    url: review.url,
                    path: None,
                    review_state: review
                        .state
                        .map(|state| state.to_lowercase().replace('_', "-")),
                }
            }));
            remarks.sort_by_key(|remark| remark.created_at);
            Some(remarks)
        });
        Ok(Observation {
            badge: PullRequest {
                number: self.number,
                url: self.url,
                state: state.into(),
                checks: summary,
            },
            head_sha: self.head_ref_oid.filter(|sha| !sha.is_empty()),
            base_branch: self.base_ref_name,
            mergeability: match self.mergeable.as_deref() {
                Some(value) if value.eq_ignore_ascii_case("MERGEABLE") => Mergeability::Mergeable,
                Some(value) if value.eq_ignore_ascii_case("CONFLICTING") => {
                    Mergeability::Conflicting
                }
                _ => Mergeability::Unknown,
            },
            rebase_required: match self.merge_state_status.as_deref() {
                None => None,
                Some(status) if status.eq_ignore_ascii_case("UNKNOWN") => None,
                Some(status) => Some(status.eq_ignore_ascii_case("BEHIND")),
            },
            checks,
            remarks,
            author: self.author.map(|author| author.login),
            updated_at: self.updated_at.as_deref().map(pr_watch::at),
            closed_at: self.closed_at.as_deref().map(pr_watch::at),
            merged_at: self.merged_at.as_deref().map(pr_watch::at),
        })
    }
}

fn id_string(id: Value) -> String {
    match id {
        Value::String(id) => id,
        id => id.to_string(),
    }
}

// The observation is a one-shot poll result, not something stored, so the
// larger variant costs nothing in practice.
#[allow(clippy::large_enum_variant)]
pub enum Lookup {
    Badge(Option<Observation>),
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
        return Ok(Lookup::Badge(Some(view.observation()?)));
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

    fn fixture(state: &str, draft: bool, checks: Value) -> Observation {
        serde_json::from_value::<View>(json!({
            "number":42, "url":"https://github.com/example/repo/pull/42", "state":state,
            "isDraft":draft, "statusCheckRollup":checks
        }))
        .unwrap()
        .observation()
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
            let observation = fixture(state, draft, json!([]));
            let pr = observation.badge;
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
        let passed = json!({"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS"});
        let pending =
            json!({"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":""});
        let context =
            |state: &str| json!({"__typename":"StatusContext","context":"ci/build","state":state});
        for failure in ["FAILURE", "CANCELLED", "TIMED_OUT"] {
            let failed = json!({"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":failure});
            for mixed in [json!([pending, failed, passed]), json!([failed, pending])] {
                assert_eq!(fixture("OPEN", false, mixed).badge.checks, Checks::Failing);
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
            assert_eq!(fixture("OPEN", false, rollup).badge.checks, expected);
        }
    }

    #[test]
    fn maps_watch_inputs_head_mergeability_and_required_checks() {
        let observation = serde_json::from_value::<View>(json!({
            "number": 42,
            "url": "https://github.com/example/repo/pull/42",
            "state": "OPEN",
            "isDraft": false,
            "headRefOid": "abcdef123456",
            "mergeable": "CONFLICTING",
            "mergeStateStatus": "BEHIND",
            "updatedAt": "2026-10-02T12:00:00Z",
            "closedAt": null,
            "mergedAt": null,
            "author": {"login": "octocat"},
            "statusCheckRollup": [
                {"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS","isRequired":true,"detailsUrl":"https://github.com/o/r/actions/runs/7"},
                {"__typename":"StatusContext","context":"ci/build","state":"SUCCESS","targetUrl":"https://ci.example/build"},
                {"__typename":"CheckRun","name":"gated","status":"COMPLETED","conclusion":"ACTION_REQUIRED"}
            ]
        }))
        .unwrap()
        .observation()
        .unwrap();
        assert_eq!(observation.head_sha.as_deref(), Some("abcdef123456"));
        assert_eq!(observation.mergeability, Mergeability::Conflicting);
        assert_eq!(observation.rebase_required, Some(true));
        assert_eq!(observation.author.as_deref(), Some("octocat"));
        assert_eq!(
            observation.updated_at,
            Some(pr_watch::at("2026-10-02T12:00:00Z"))
        );
        assert_eq!(
            observation.badge.checks,
            Checks::Passing,
            "action-required is not a failure, as before"
        );
        assert!(observation.checks[0].required);
        assert_eq!(observation.checks[0].status, CheckStatus::Success);
        assert_eq!(
            observation.checks[0].url.as_deref(),
            Some("https://github.com/o/r/actions/runs/7")
        );
        assert_eq!(observation.checks[2].status, CheckStatus::ActionRequired);
    }

    #[test]
    fn maps_comments_and_reviews_into_remarks_once_the_activity_read_is_whole() {
        let view = || {
            serde_json::from_value::<View>(json!({
                "number": 42,
                "url": "https://github.com/example/repo/pull/42",
                "state": "OPEN",
                "isDraft": false,
                "statusCheckRollup": [],
                "comments": [
                    {"id":"IC_1","author":{"login":"reviewer"},"body":"Please rename.","createdAt":"2026-10-02T12:06:00Z","url":"https://github.com/o/r/pull/42#issuecomment-1"}
                ],
                "reviews": [
                    {"id":"PRR_1","author":{"login":"maintainer"},"body":"","state":"CHANGES_REQUESTED","submittedAt":"2026-10-02T12:07:00Z","url":"https://github.com/o/r/pull/42#pullrequestreview-1"}
                ]
            }))
            .unwrap()
        };
        let observation = view().observation().unwrap();
        let remarks = observation.remarks.unwrap();
        assert_eq!(remarks.len(), 2);
        assert_eq!(remarks[0].id, "IC_1");
        assert_eq!(remarks[0].author.as_deref(), Some("reviewer"));
        assert_eq!(
            remarks[1].review_state.as_deref(),
            Some("changes-requested")
        );
        assert_eq!(remarks[1].created_at, pr_watch::at("2026-10-02T12:07:00Z"));

        // Without one of the two activity fields, the read is not whole and
        // the watch watermark must not advance.
        let mut incomplete = json!({
            "number": 42,
            "url": "https://github.com/example/repo/pull/42",
            "state": "OPEN",
            "isDraft": false,
            "statusCheckRollup": [],
            "comments": []
        });
        assert!(
            serde_json::from_value::<View>(incomplete.clone())
                .unwrap()
                .observation()
                .unwrap()
                .remarks
                .is_none()
        );
        incomplete["reviews"] = json!([]);
        assert!(
            serde_json::from_value::<View>(incomplete)
                .unwrap()
                .observation()
                .unwrap()
                .remarks
                .is_some()
        );
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
        let badge = Some(fixture("OPEN", false, json!([])).badge);
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
[ "$#" = 5 ] && [ "$1" = pr ] && [ "$2" = view ] && [ "$3" = atrium/web ] && [ "$4" = --json ] && [ "$5" = number,url,state,isDraft,statusCheckRollup,headRefOid,baseRefName,mergeable,mergeStateStatus,updatedAt,closedAt,mergedAt,author,comments,reviews ] || exit 7
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
        assert!(matches!(
            run("0",r#"{"number":42,"url":"https://gh.example/o/r/pull/42","state":"OPEN","isDraft":false,"statusCheckRollup":[],"comments":[],"reviews":[]}"#,"").unwrap(),
            Lookup::Badge(Some(observation)) if observation.badge.number==42
        ));
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
