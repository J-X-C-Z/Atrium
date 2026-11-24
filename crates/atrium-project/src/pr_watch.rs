//! Ported from T3 Code `apps/server/src/orchestration-v2/pullRequestWatch.ts`
//! (watch decisions and wake text) and `apps/server/src/orchestration-v2/PullRequestSyncReactor.ts`
//! (sync cadence, backoff and missing-host handling), release 0.1.11, MIT; see
//! `crates/atrium-engine/LICENSE-T3` and `crates/atrium-engine/UPSTREAM.md`.
//!
//! Atrium's existing `gh` poll (`pr.rs`, `workspace_pr_status.rs`) keeps its own
//! shape: one read per lead branch, the badge on the stream. This module adds
//! the decision layer that poll was missing: when a read is due, what counts
//! as news for the lead, and what to tell it. Everything here takes `now`
//! (epoch milliseconds) as an argument and is GPUI-free, so it is unit tested.

/// A watcher that only sees comment after comment stops after this many
/// consecutive comment-only wakes. Check, conflict or push news resets it.
pub const WATCH_WAKE_LIMIT: u32 = 10;
/// How long a head may go without any check before the agent is told none
/// reported.
pub const NO_CHECKS_MS: i64 = 15 * 60_000;

/// Cadence from `PullRequestSyncReactor`: an open pull request read this long
/// after its last update is "quiet" and read on the slower cadence.
pub const QUIET_AFTER_MS: i64 = 30 * 60_000;
/// Quiet open pull requests are read this often.
pub const QUIET_SYNC_INTERVAL_MS: i64 = 5 * 60_000;
/// Anything else still worth reading (a recently closed pull request, a
/// host that answered "not found") is read this often.
pub const SLOW_SYNC_INTERVAL_MS: i64 = 15 * 60_000;
/// A closed pull request older than this is rarely reopened, so it is no
/// longer swept.
pub const CLOSED_FINAL_AFTER_MS: i64 = 24 * 60 * 60_000;
/// A link whose host read failed waits this long, then twice as long after
/// each further failure (capped at [`SLOW_SYNC_INTERVAL_MS`]).
pub const FAILED_READ_BACKOFF_MS: i64 = 60_000;
/// A lead branch with no pull request yet is checked this often. T3 links
/// pull requests explicitly; Atrium discovers them by branch, so it keeps
/// the brief's two-minute discovery cadence.
pub const NO_PR_SYNC_INTERVAL_MS: i64 = 2 * 60_000;

const LISTED_ITEMS: usize = 10;
const SNIPPET_LENGTH: usize = 200;

/// Parses the ISO timestamps `gh` prints. Unparseable values become `0`,
/// which sorts before every real timestamp, like `Date.parse`'s `NaN` does
/// not — every caller here only compares against parsed values, so the
/// difference is confined to malformed host output.
pub fn at(iso: &str) -> i64 {
    iso.parse::<jiff::Timestamp>()
        .map(|t| t.as_millisecond())
        .unwrap_or(0)
}

/// Wall-clock now, for callers that do not inject a clock.
pub fn now_millis() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Baseline {
    /// Record what is already there (checks, conflicts) without reporting it.
    #[default]
    All,
    /// A previous read could not show the checks; keep the baseline for the
    /// next one.
    Checks,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    Pending,
    ActionRequired,
    Success,
    Failure,
    /// `skipped` and `neutral` collapse here: the watch only distinguishes
    /// pending, failed and done, and both are done.
    Skipped,
    Cancelled,
}

impl CheckStatus {
    pub(crate) fn failed(self) -> bool {
        matches!(self, CheckStatus::Failure | CheckStatus::Cancelled)
    }
}

/// One check on the head, shaped like T3's `PullRequestCheck`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub url: Option<String>,
    pub required: bool,
    /// Not a check: it stands for ones the host could not list, such as
    /// workflow runs awaiting approval when that read fails.
    pub placeholder: bool,
}

impl Check {
    pub fn new(name: impl Into<String>, status: CheckStatus) -> Self {
        Self {
            name: name.into(),
            status,
            url: None,
            required: false,
            placeholder: false,
        }
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn placeholder(mut self) -> Self {
        self.placeholder = true;
        self
    }
}

/// One comment or review that can wake the agent, shaped like T3's
/// `PullRequestComment`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remark {
    pub id: String,
    /// Present on comments and reviews; `None` when the host omits the author.
    pub author: Option<String>,
    pub body: String,
    pub created_at: i64,
    pub edited_at: Option<i64>,
    pub url: Option<String>,
    pub path: Option<String>,
    pub review_state: Option<String>,
}

impl Remark {
    fn active_at(&self) -> i64 {
        self.edited_at.unwrap_or(self.created_at)
    }
}

/// A merge-queue removal the host reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeQueueRemoval {
    pub id: String,
    pub removed_at: i64,
    pub reason: Option<String>,
}

/// One read of a pull request, in the terms the watch decides on.
#[derive(Clone, Debug, Default)]
pub struct Detail {
    /// Whether an empty check list means CI never started (GitHub), rather
    /// than a read that saw nothing.
    pub checks_always_report: bool,
    pub head_sha: Option<String>,
    pub checks: Vec<Check>,
    pub mergeability: Mergeability,
    /// The authenticated user, when the host names it.
    pub viewer: Option<String>,
    /// The pull request's author.
    pub author: Option<String>,
    pub rebase_required: Option<bool>,
    pub merge_queue_removal: Option<MergeQueueRemoval>,
}

/// What the agent last heard about a watched pull request. The fields and
/// their defaults are T3's `ThreadPullRequestWatch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watch {
    pub started_at: i64,
    pub head_sha: Option<String>,
    pub failed_checks: Vec<String>,
    pub passed: bool,
    pub passed_checks: Vec<String>,
    /// Checks still pending when required ones passed early, owed one wake
    /// once all settle.
    pub owed_checks: Vec<String>,
    /// Approval holds keyed by workflow run URL, or check name when no run
    /// URL is known.
    pub awaiting_approval: Vec<String>,
    /// The agent was told checks await approval on this head, so later ones
    /// stay quiet.
    pub approval_announced: bool,
    pub remarks_through: i64,
    /// Reported remarks at `remarks_through`, so late arrivals at that
    /// timestamp still count.
    pub remark_ids: Vec<String>,
    pub conflicting: bool,
    pub rebase_required: Option<bool>,
    /// Last removal seen. `known == false` means the pull request has not
    /// been read since this field existed; `known && id.is_none()` is an
    /// empty read.
    pub removal_known: bool,
    pub merge_queue_removal_id: Option<String>,
    /// Consecutive comment-only wakes, limited against bot reply loops.
    pub wakes: u32,
    /// What is already on the pull request is recorded, not reported.
    pub awaiting_baseline: Option<Baseline>,
    /// When the head was first read without any check, until a check shows up
    /// or the agent is told none did.
    pub checks_awaited_since: Option<i64>,
    /// Whether the host was still running checks on the head at the last
    /// read. Absent until the first read.
    pub checks_pending: Option<bool>,
}

impl Watch {
    /// A fresh watch. `checks_pending` seeds T3's rule that CI already
    /// running when the watch starts may fail before the first read, and that
    /// failure is news.
    pub fn new(now: i64, checks_pending: bool) -> Self {
        Self {
            started_at: now,
            head_sha: None,
            failed_checks: Vec::new(),
            passed: false,
            passed_checks: Vec::new(),
            owed_checks: Vec::new(),
            awaiting_approval: Vec::new(),
            approval_announced: false,
            remarks_through: now,
            remark_ids: Vec::new(),
            conflicting: false,
            rebase_required: None,
            removal_known: false,
            merge_queue_removal_id: None,
            wakes: 0,
            awaiting_baseline: Some(Baseline::All),
            checks_awaited_since: None,
            checks_pending: checks_pending.then_some(true),
        }
    }

    fn same_answer(&self, other: &Watch) -> bool {
        self.failed_checks.join("\n") == other.failed_checks.join("\n")
            && self.passed == other.passed
            && self.passed_checks.join("\n") == other.passed_checks.join("\n")
            && self.owed_checks.join("\n") == other.owed_checks.join("\n")
            && self.awaiting_approval.join("\n") == other.awaiting_approval.join("\n")
            && self.approval_announced == other.approval_announced
            && self.remarks_through == other.remarks_through
            && self.remark_ids.join("\n") == other.remark_ids.join("\n")
            && self.conflicting == other.conflicting
            && self.rebase_required == other.rebase_required
            && self.removal_known == other.removal_known
            && self.merge_queue_removal_id == other.merge_queue_removal_id
            && self.wakes == other.wakes
            && self.awaiting_baseline == other.awaiting_baseline
            && self.checks_awaited_since == other.checks_awaited_since
            && self.checks_pending == other.checks_pending
    }
}

/// Something the agent has not heard yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchChange {
    ChecksFailed { failed: Vec<Check> },
    ChecksPassed { count: usize, required: bool },
    ChecksAwaitingApproval { checks: Vec<Check> },
    Remarks { remarks: Vec<Remark> },
    NoChecks { minutes: i64 },
    Conflicting,
    NeedsRebase,
    MergeQueueRemoved { reason: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchReport {
    pub changes: Vec<WatchChange>,
    pub next: Watch,
    pub exhausted: bool,
}

/// Decides what is new since `watch`, and the watch to save afterwards.
/// Direct port of T3's `evaluatePullRequestWatch`.
pub fn evaluate(
    watch: &Watch,
    detail: &Detail,
    remarks: Option<&[Remark]>,
    now: i64,
) -> WatchReport {
    let mut changes = Vec::new();
    // A read without a head says nothing about pushes, so it keeps the last
    // known one.
    let head_sha = detail.head_sha.clone().or_else(|| watch.head_sha.clone());
    let head_moved = watch.head_sha.is_some() && head_sha != watch.head_sha;
    let checks_baseline = watch.awaiting_baseline.is_some() && !head_moved;

    // Hosts can return no checks on read failure; preserve prior state.
    let mut failed_checks = if head_moved {
        Vec::new()
    } else {
        watch.failed_checks.clone()
    };
    let mut passed = if head_moved { false } else { watch.passed };
    let mut passed_checks = if head_moved {
        Vec::new()
    } else {
        watch.passed_checks.clone()
    };
    let mut owed_checks = if head_moved {
        Vec::new()
    } else {
        watch.owed_checks.clone()
    };
    let mut awaiting_approval = if head_moved {
        Vec::new()
    } else {
        watch.awaiting_approval.clone()
    };
    let mut approval_announced = if head_moved {
        false
    } else {
        watch.approval_announced
    };
    // A watch with no answer (saved before the field, or since a push) waits
    // only while the clock below runs.
    let mut checks_pending = if head_moved {
        false
    } else {
        watch
            .checks_pending
            .unwrap_or(watch.checks_awaited_since.is_some())
    };
    // An unavailable approval read cannot establish success or that no checks
    // exist.
    let unread = detail.checks.iter().any(|check| check.placeholder);
    let checks: Vec<&Check> = detail
        .checks
        .iter()
        .filter(|check| !check.placeholder)
        .collect();
    let failed: Vec<&Check> = checks
        .iter()
        .copied()
        .filter(|check| check.status.failed())
        .collect();
    let awaiting: Vec<&Check> = checks
        .iter()
        .copied()
        .filter(|check| check.status == CheckStatus::ActionRequired)
        .collect();
    if !checks.is_empty() {
        // A run awaiting approval waits on a person, like a review, so only
        // running checks count. An unfinished baseline read keeps the seed
        // below for the next one.
        checks_pending = checks
            .iter()
            .any(|check| check.status == CheckStatus::Pending)
            || (checks_baseline && unread && watch.checks_pending == Some(true));
        let newly_failed: Vec<Check> = failed
            .iter()
            .filter(|check| !failed_checks.contains(&check.name))
            .map(|check| (*check).clone())
            .collect();
        // The baseline hides failures from before the watch, but CI running
        // at the start can fail before the first read.
        if !newly_failed.is_empty() && (!checks_baseline || watch.checks_pending == Some(true)) {
            changes.push(WatchChange::ChecksFailed {
                failed: newly_failed,
            });
        }
        if !approval_announced && !awaiting.is_empty() {
            if !checks_baseline {
                changes.push(WatchChange::ChecksAwaitingApproval {
                    checks: awaiting.iter().map(|check| (*check).clone()).collect(),
                });
            }
            approval_announced = true;
        }
        failed_checks = failed.iter().map(|check| check.name.clone()).collect();

        // Approval reads can omit gated runs. Keep each hold until its jobs
        // finish.
        let mut awaiting_now: Vec<String> = Vec::new();
        for check in &awaiting {
            let key = approval_key(check);
            if !awaiting_now.contains(&key) {
                awaiting_now.push(key);
            }
        }
        let name_unfinished = |name: &str| {
            let mut matching = checks.iter().filter(|check| check.name == name);
            match matching.next() {
                None => true,
                Some(_) => checks.iter().any(|check| {
                    check.name == name
                        && matches!(
                            check.status,
                            CheckStatus::Pending | CheckStatus::ActionRequired
                        )
                }),
            }
        };
        // GitHub deduplicates jobs across runs, so a visible pending job may
        // hide one in an awaited run. A pending check without a URL could be
        // such a job; one with another host's URL cannot.
        let jobs_pending = checks.iter().any(|check| {
            check.status == CheckStatus::Pending
                && (check.url.is_none() || check.url.as_deref().and_then(workflow_run_of).is_some())
        });
        let finished: Vec<Option<String>> = if jobs_pending {
            Vec::new()
        } else {
            checks
                .iter()
                .filter(|check| {
                    !matches!(
                        check.status,
                        CheckStatus::ActionRequired | CheckStatus::Pending
                    )
                })
                .map(|check| check.url.as_deref().and_then(workflow_run_of))
                .collect()
        };
        let carried: Vec<String> = awaiting_approval
            .iter()
            .filter_map(|key| {
                if awaiting_now.contains(key) {
                    return None;
                }
                let finished_run = finished
                    .iter()
                    .any(|run| run.as_deref() == Some(key.as_str()));
                let keep = if workflow_run_of(key).as_deref() == Some(key.as_str()) {
                    !finished_run
                } else {
                    name_unfinished(key)
                };
                keep.then(|| key.clone())
            })
            .collect();
        awaiting_approval = awaiting_now.clone();
        awaiting_approval.extend(carried);
    }
    let required: Vec<&Check> = checks
        .iter()
        .copied()
        .filter(|check| check.required)
        .collect();
    let gate: Vec<&Check> = if required.is_empty() {
        checks.clone()
    } else {
        required.clone()
    };
    let passed_now = awaiting_approval.is_empty()
        && gate
            .iter()
            .all(|check| check.status != CheckStatus::Pending && !check.status.failed());
    if !checks.is_empty() && !(unread && passed_now) {
        let gate_names: Vec<String> = gate.iter().map(|check| check.name.clone()).collect();
        let told: Vec<String> = if passed && passed_checks.is_empty() {
            gate_names.clone()
        } else {
            passed_checks.clone()
        };
        // Required checks can finish between sweeps; detect new names. With no
        // required checks, ignore advisory gate growth.
        let gate_grew = !required.is_empty() && gate_names.iter().any(|name| !told.contains(name));
        // Other checks still pending when required ones passed are owed one
        // wake when all settle. Only those are owed, so later advisory churn
        // never wakes. One turned required is gate news.
        let owed: Vec<String> = if passed_now && passed {
            checks
                .iter()
                .filter(|check| !check.required && owed_checks.contains(&check.name))
                .map(|check| check.name.clone())
                .collect()
        } else {
            Vec::new()
        };
        let settled = failed.is_empty()
            && awaiting_approval.is_empty()
            && checks
                .iter()
                .all(|check| check.status != CheckStatus::Pending);
        owed_checks = owed.clone();
        if passed_now && (!passed || gate_grew) {
            if !checks_baseline {
                changes.push(WatchChange::ChecksPassed {
                    count: gate.len(),
                    required: !required.is_empty(),
                });
            }
            owed_checks = if required.is_empty() {
                Vec::new()
            } else {
                checks
                    .iter()
                    .filter(|check| !check.required && check.status == CheckStatus::Pending)
                    .map(|check| check.name.clone())
                    .collect()
            };
        } else if settled && !owed.is_empty() {
            changes.push(WatchChange::ChecksPassed {
                count: checks.len(),
                required: false,
            });
            owed_checks = Vec::new();
        }
        passed = passed_now;
        passed_checks = if passed_now { gate_names } else { Vec::new() };
    }

    // Only a head never read with checks waits for them; a legacy watch's
    // clock starts at its next push. CI can register late on any host, so
    // every host waits, but running out of time is news only where a host
    // always reports checks.
    let awaited_before = if head_moved || watch.awaiting_baseline == Some(Baseline::All) {
        Some(now)
    } else {
        watch.checks_awaited_since
    };
    let mut checks_awaited_since = if !checks.is_empty() {
        None
    } else {
        awaited_before
    };
    let awaited_ms = checks_awaited_since.map_or(0, |since| now - since);
    if !unread && awaited_ms >= NO_CHECKS_MS {
        if detail.checks_always_report {
            changes.push(WatchChange::NoChecks {
                minutes: (awaited_ms as f64 / 60_000.0).round() as i64,
            });
        }
        checks_awaited_since = None;
        checks_pending = false;
    } else if checks_awaited_since.is_some() {
        checks_pending = true;
    }

    let own = detail
        .viewer
        .as_deref()
        .or(detail.author.as_deref())
        .map(|login| login.to_lowercase());
    let through = watch.remarks_through;
    // Hosts can stamp remarks to the second, so remarks at the boundary time
    // are told apart by ID.
    let fresh: Vec<Remark> = remarks
        .unwrap_or_default()
        .iter()
        .filter(|remark| {
            let at = remark.active_at();
            (at > through || (at == through && !watch.remark_ids.contains(&remark.id)))
                && remark.author.as_deref().map(|login| login.to_lowercase()) != own
        })
        .cloned()
        .collect();
    if !fresh.is_empty() {
        changes.push(WatchChange::Remarks {
            remarks: fresh.clone(),
        });
    }
    let latest = fresh.iter().map(Remark::active_at).fold(through, i64::max);
    let at_latest: Vec<&Remark> = fresh
        .iter()
        .filter(|remark| remark.active_at() == latest)
        .collect();
    let remarks_through = if latest == through {
        watch.remarks_through
    } else {
        at_latest[0].active_at()
    };
    let mut remark_ids = if latest == through {
        watch.remark_ids.clone()
    } else {
        Vec::new()
    };
    remark_ids.extend(at_latest.iter().map(|remark| remark.id.clone()));

    // Like a failed check, a conflict or rebase a push did not resolve is
    // news for the new head.
    let told_conflict = !head_moved && watch.conflicting;
    if watch.awaiting_baseline != Some(Baseline::All)
        && detail.mergeability == Mergeability::Conflicting
        && !told_conflict
    {
        changes.push(WatchChange::Conflicting);
    }
    let told_rebase = !head_moved && watch.rebase_required == Some(true);
    if watch.awaiting_baseline != Some(Baseline::All)
        && detail.rebase_required == Some(true)
        && !told_rebase
    {
        changes.push(WatchChange::NeedsRebase);
    }
    let rebase_required = detail.rebase_required.or(watch.rebase_required);
    // "unknown" is a host still computing after a push; only a clean answer
    // clears a conflict.
    let conflicting = match detail.mergeability {
        Mergeability::Unknown => told_conflict,
        Mergeability::Conflicting => true,
        Mergeability::Mergeable => false,
    };

    // The queue's checks run on a commit of its own, so its head can stay
    // green after the queue drops it. After the first read, IDs avoid
    // comparing the server clock with the host clock. The initial read uses
    // whole seconds to exclude historical removals.
    if let Some(removal) = &detail.merge_queue_removal {
        // Merging also records a removal, up to a second before the pull
        // request reads as merged.
        let changed = removal.reason.as_deref() != Some("merged")
            && (!watch.removal_known
                || watch.merge_queue_removal_id.as_deref() != Some(removal.id.as_str()));
        let initial_after_start = watched_since(watch.started_at) <= removal.removed_at;
        if changed && (watch.removal_known || initial_after_start) {
            changes.push(WatchChange::MergeQueueRemoved {
                reason: removal.reason.clone(),
            });
        }
    }
    // Null records an empty baseline; historical IDs are saved without
    // waking the watch.
    let merge_queue_removal_id = detail
        .merge_queue_removal
        .as_ref()
        .map(|removal| removal.id.clone())
        .or_else(|| watch.merge_queue_removal_id.clone());

    let comments_only = !changes.is_empty()
        && changes
            .iter()
            .all(|change| matches!(change, WatchChange::Remarks { .. }));
    let progress = head_moved || (!changes.is_empty() && !comments_only);
    let wakes = if progress { 0 } else { watch.wakes } + u32::from(comments_only);
    let next = Watch {
        started_at: watch.started_at,
        head_sha,
        failed_checks,
        passed,
        passed_checks,
        owed_checks,
        awaiting_approval,
        approval_announced,
        remarks_through,
        remark_ids,
        conflicting,
        rebase_required,
        removal_known: true,
        merge_queue_removal_id,
        wakes,
        // Only a read that cannot be trusted to show the checks leaves the
        // baseline to the next one.
        awaiting_baseline: (checks_baseline
            && (unread || (detail.head_sha.is_none() && checks.is_empty())))
        .then_some(Baseline::Checks),
        checks_awaited_since,
        checks_pending: Some(checks_pending),
    };
    WatchReport {
        changes,
        next,
        exhausted: comments_only && wakes >= WATCH_WAKE_LIMIT,
    }
}

/// The start of the watch rounded down to a whole second, so the host's
/// second-granular timestamps compare cleanly.
fn watched_since(started_at: i64) -> i64 {
    (started_at / 1000) * 1000
}

/// A workflow run URL ends at its run number; a job URL extends it. `None`
/// for a check without a URL, or with another host's URL.
fn workflow_run_of(url: &str) -> Option<String> {
    let at = url.find("/actions/runs/")?;
    let rest = &url[at + "/actions/runs/".len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0).then(|| url[..at + "/actions/runs/".len() + digits].to_owned())
}

// A GitHub Actions job's URL extends its workflow run's URL; their names need
// not match.
fn approval_key(check: &Check) -> String {
    check
        .url
        .as_deref()
        .and_then(workflow_run_of)
        .unwrap_or_else(|| check.name.clone())
}

/// Whether `next` differs from `watch` in anything the next read decides on.
/// Used to keep the in-memory watch map from growing on every poll.
pub fn same_watch(left: &Watch, right: &Watch) -> bool {
    left.same_answer(right)
}

/// Whether a lead's pull request is due for a read. Port of T3's `isDue`:
/// a failed read backs off, an explicit request reads at once, a retired
/// link is not read, a final snapshot is not read, quiet open pull requests
/// wait for the quiet cadence, and everything else waits for the slow one.
pub fn sync_due(state: &SyncState, snapshot: Option<&Snapshot>, active: bool, now: i64) -> bool {
    if state.retry_at.is_some_and(|retry| retry > now) {
        return false;
    }
    if state.requested {
        return true;
    }
    if !active {
        return false;
    }
    if state.no_pr {
        return since_last(state.last_synced, now) >= NO_PR_SYNC_INTERVAL_MS;
    }
    if state.missing {
        return since_last(state.last_synced, now) >= SLOW_SYNC_INTERVAL_MS;
    }
    let Some(snapshot) = snapshot else {
        return true;
    };
    let final_snapshot = final_snapshot(snapshot, now);
    if final_snapshot {
        return false;
    }
    match snapshot.state {
        SnapshotState::Open => {
            let quiet = snapshot
                .updated_at
                .is_some_and(|updated| now - updated >= QUIET_AFTER_MS);
            !quiet || since_last(state.last_synced, now) >= QUIET_SYNC_INTERVAL_MS
        }
        SnapshotState::Closed | SnapshotState::Merged => {
            since_last(state.last_synced, now) >= SLOW_SYNC_INTERVAL_MS
        }
    }
}

fn final_snapshot(snapshot: &Snapshot, now: i64) -> bool {
    match snapshot.state {
        SnapshotState::Merged => snapshot.merged_at.is_some(),
        SnapshotState::Closed => snapshot
            .closed_at
            .is_some_and(|closed| now - closed >= CLOSED_FINAL_AFTER_MS),
        SnapshotState::Open => false,
    }
}

fn since_last(last: Option<i64>, now: i64) -> i64 {
    last.map_or(i64::MAX, |last| now - last)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotState {
    Open,
    Closed,
    Merged,
}

/// The parts of a stored badge the cadence looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub state: SnapshotState,
    pub updated_at: Option<i64>,
    pub merged_at: Option<i64>,
    pub closed_at: Option<i64>,
}

/// Per-pull-request read state, from T3's `PullRequestSyncReactor`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncState {
    pub last_synced: Option<i64>,
    pub retry_at: Option<i64>,
    pub backoff: i64,
    /// The host answered not-found, or this server cannot read it (no CLI,
    /// signed out); read on the slow cadence until a read succeeds.
    pub missing: bool,
    /// The branch has no pull request yet; look again on the discovery
    /// cadence.
    pub no_pr: bool,
    /// A refresh was asked for and has not been consumed.
    pub requested: bool,
}

impl SyncState {
    /// The host answered: reset backoff and missing, consume a request.
    pub fn succeeded(&mut self, now: i64) {
        self.last_synced = Some(now);
        self.retry_at = None;
        self.backoff = 0;
        self.missing = false;
        self.no_pr = false;
        self.requested = false;
    }

    /// The host answered "no pull request for this branch".
    pub fn saw_no_pr(&mut self, now: i64) {
        self.last_synced = Some(now);
        self.retry_at = None;
        self.backoff = 0;
        self.missing = false;
        self.no_pr = true;
        self.requested = false;
    }

    /// A failed read waits a sweep, then twice as long after each further
    /// failure, capped at the slow cadence.
    pub fn failed(&mut self, now: i64) {
        self.backoff = if self.backoff == 0 {
            FAILED_READ_BACKOFF_MS
        } else {
            (self.backoff * 2).min(SLOW_SYNC_INTERVAL_MS)
        };
        self.retry_at = Some(now + self.backoff);
    }

    /// Not found, or unreadable here: consumes the request and starts the
    /// slow cadence.
    pub fn missing(&mut self, now: i64) {
        self.missing = true;
        self.last_synced = Some(now);
        self.requested = false;
    }

    /// Ask for a read on the next sweep.
    pub fn request(&mut self) {
        self.requested = true;
    }
}

/// The renderable summary of one change, matching T3's `changeLines`.
fn change_lines(change: &WatchChange, base_branch: &str, commit: &str) -> Vec<String> {
    match change {
        WatchChange::ChecksFailed { failed } => {
            let mut lines = vec![format!("- Checks failed{commit}:")];
            lines.extend(listed(failed, |check| {
                let state = if check.status == CheckStatus::Failure {
                    String::new()
                } else {
                    format!(" ({})", status_word(check.status))
                };
                let url = check
                    .url
                    .as_deref()
                    .map(|url| format!(" {url}"))
                    .unwrap_or_default();
                format!("  - {}{state}{url}", check.name)
            }));
            lines
        }
        WatchChange::ChecksAwaitingApproval { checks } => {
            let mut lines = vec![format!(
                "- Checks are waiting for approval or action from a maintainer{commit}:"
            )];
            lines.extend(listed(checks, |check| {
                let url = check
                    .url
                    .as_deref()
                    .map(|url| format!(" {url}"))
                    .unwrap_or_default();
                format!("  - {}{url}", check.name)
            }));
            lines
        }
        WatchChange::ChecksPassed { count, required } => vec![format!(
            "- All {count} {}{} passed{commit}.",
            if *required { "required " } else { "" },
            if *count == 1 { "check" } else { "checks" },
        )],
        WatchChange::Remarks { remarks } => {
            let mut lines = vec![format!(
                "- {} new {}:",
                remarks.len(),
                if remarks.len() == 1 {
                    "comment"
                } else {
                    "comments"
                }
            )];
            lines.extend(listed(remarks, |remark| {
                let where_ = remark
                    .path
                    .as_deref()
                    .map(|path| format!(" on {path}"))
                    .unwrap_or_default();
                let body = snippet(&remark.body);
                let said = if body.is_empty() {
                    remark
                        .review_state
                        .as_deref()
                        .unwrap_or("reviewed")
                        .to_owned()
                } else {
                    format!("\"{body}\"")
                };
                let url = remark
                    .url
                    .as_deref()
                    .map(|url| format!(" {url}"))
                    .unwrap_or_default();
                format!(
                    "  - {}{where_}: {said}{url}",
                    remark.author.as_deref().unwrap_or("someone")
                )
            }));
            lines
        }
        WatchChange::NoChecks { minutes } => vec![format!(
            "- No checks have reported{commit} after {minutes} minutes. None may be configured to run on this change."
        )],
        WatchChange::NeedsRebase => {
            vec![format!(
                "- The branch requires a rebase onto {base_branch}."
            )]
        }
        WatchChange::Conflicting => {
            vec![format!("- The branch now conflicts with {base_branch}.")]
        }
        WatchChange::MergeQueueRemoved { reason } => vec![format!(
            "- It was removed from the merge queue without merging{}.",
            reason
                .as_deref()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        )],
    }
}

fn status_word(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pending => "pending",
        CheckStatus::ActionRequired => "action-required",
        CheckStatus::Success => "success",
        CheckStatus::Failure => "failure",
        CheckStatus::Skipped => "skipped",
        CheckStatus::Cancelled => "cancelled",
    }
}

fn listed<T>(items: &[T], line: impl Fn(&T) -> String) -> Vec<String> {
    let mut lines: Vec<String> = items.iter().take(LISTED_ITEMS).map(line).collect();
    if items.len() > LISTED_ITEMS {
        lines.push(format!("  - and {} more", items.len() - LISTED_ITEMS));
    }
    lines
}

/// Collapses a comment to one line and truncates it, without splitting a
/// character in half. (T3 also strips HTML tags; that is left out here so the
/// port needs no HTML parser — see `docs/pr-preview-implementation.md`.)
fn snippet(body: &str) -> String {
    let without_comments = strip_html_comments(body);
    let collapsed: String = without_comments
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.encode_utf16().count() <= SNIPPET_LENGTH {
        return collapsed;
    }
    // T3 works in UTF-16 units; keep a whole character, and drop an astral
    // one that would end exactly at the cut.
    let mut chars: Vec<char> = collapsed.chars().collect();
    let mut end = SNIPPET_LENGTH - 3;
    let units = |slice: &[char]| slice.iter().map(|c| c.len_utf16()).sum::<usize>();
    while end > 0 && units(&chars[..end]) > SNIPPET_LENGTH - 3 {
        end -= 1;
    }
    if end > 0 && chars[end - 1] as u32 > 0xFFFF {
        end -= 1;
    }
    chars.truncate(end);
    format!("{}...", chars.into_iter().collect::<String>())
}

fn strip_html_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start + 4..].find("-->") {
            Some(end) => rest = &rest[start + 4 + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

const WATCHING_CLOSING: &str = "Look into each item and act on it as your task requires. This pull request stays watched, so do not poll it yourself. End your turn when you are done, and Atrium wakes you on the next change.";

/// The text Atrium sends a lead when its pull request changed. Port of T3's
/// `pullRequestWatchMessage` text (the notification record is Atrium's own).
pub fn watch_message(
    number: u64,
    url: &str,
    base_branch: &str,
    head_sha: Option<&str>,
    report: &WatchReport,
) -> String {
    let commit = head_sha
        .map(|sha| format!(" on {}", &sha[..sha.len().min(7)]))
        .unwrap_or_default();
    let mut lines = vec![format!(
        "Update on pull request #{number} ({url}), which Atrium is watching for you:"
    )];
    for change in &report.changes {
        lines.extend(change_lines(change, base_branch, &commit));
    }
    lines.push(String::new());
    lines.push(if report.exhausted {
        format!(
            "Atrium stopped watching after {WATCH_WAKE_LIMIT} comment-only updates in a row. Re-check the pull request yourself."
        )
    } else {
        WATCHING_CLOSING.to_owned()
    });
    lines.join("\n")
}

/// The text Atrium sends when a watched pull request ends. Port of T3's
/// `pullRequestWatchEndMessage`.
pub fn watch_end_message(
    number: u64,
    url: &str,
    state: SnapshotState,
    base_branch: &str,
) -> String {
    if state == SnapshotState::Merged {
        format!(
            "Pull request #{number} ({url}) was merged into {base_branch}, so Atrium stopped watching it."
        )
    } else {
        format!(
            "Pull request #{number} ({url}) was closed without merging, so Atrium stopped watching it. If it is reopened, Atrium watches it again."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STARTED: &str = "2026-10-02T12:00:00.000Z";

    fn watch() -> Watch {
        Watch {
            started_at: at(STARTED),
            head_sha: None,
            failed_checks: Vec::new(),
            passed: false,
            passed_checks: Vec::new(),
            owed_checks: Vec::new(),
            awaiting_approval: Vec::new(),
            approval_announced: false,
            remarks_through: at(STARTED),
            remark_ids: Vec::new(),
            conflicting: false,
            rebase_required: None,
            removal_known: false,
            merge_queue_removal_id: None,
            wakes: 0,
            awaiting_baseline: None,
            checks_awaited_since: None,
            checks_pending: None,
        }
    }

    fn run(id: u64) -> String {
        format!("https://github.com/o/r/actions/runs/{id}")
    }

    fn check(name: &str, status: CheckStatus) -> Check {
        Check::new(name, status).with_url(format!("https://ci.example/{name}"))
    }

    fn capabilities(checks_always_report: bool) -> bool {
        checks_always_report
    }

    fn detail() -> Detail {
        Detail {
            checks_always_report: capabilities(true),
            head_sha: Some("aaaaaaaaaa".into()),
            checks: vec![
                check("lint", CheckStatus::Success),
                check("test", CheckStatus::Pending),
            ],
            mergeability: Mergeability::Mergeable,
            viewer: Some("agent-user".into()),
            author: Some("agent-user".into()),
            rebase_required: None,
            merge_queue_removal: None,
        }
    }

    fn remark(login: &str, created_at: &str) -> Remark {
        remark_with(login, created_at, "Please rename this.")
    }

    fn remark_with(login: &str, created_at: &str, body: &str) -> Remark {
        Remark {
            id: format!("{login}-{created_at}"),
            author: Some(login.into()),
            body: body.into(),
            created_at: at(created_at),
            edited_at: None,
            url: Some(format!("https://github.com/o/r/pull/1#{login}")),
            path: Some("src/index.ts".into()),
            review_state: None,
        }
    }

    fn evaluate_at(w: &Watch, d: &Detail, remarks: Option<&[Remark]>) -> WatchReport {
        evaluate(w, d, remarks, 0)
    }

    fn kinds(report: &WatchReport) -> Vec<&'static str> {
        report
            .changes
            .iter()
            .map(|change| match change {
                WatchChange::ChecksFailed { .. } => "checks-failed",
                WatchChange::ChecksPassed { .. } => "checks-passed",
                WatchChange::ChecksAwaitingApproval { .. } => "checks-awaiting-approval",
                WatchChange::Remarks { .. } => "remarks",
                WatchChange::NoChecks { .. } => "no-checks",
                WatchChange::Conflicting => "conflicting",
                WatchChange::NeedsRebase => "needs-rebase",
                WatchChange::MergeQueueRemoved { .. } => "merge-queue-removed",
            })
            .collect()
    }

    const NO_REMARKS: Option<&[Remark]> = Some(&[]);

    #[test]
    fn reports_a_required_rebase_once_with_pending_or_success_ci() {
        for status in [CheckStatus::Pending, CheckStatus::Success] {
            let blocked = Detail {
                mergeability: Mergeability::Unknown,
                rebase_required: Some(true),
                checks: vec![check("test", status)],
                ..detail()
            };
            let first = evaluate_at(&watch(), &blocked, NO_REMARKS);
            assert!(kinds(&first).contains(&"needs-rebase"));
            assert!(
                evaluate_at(&first.next, &blocked, NO_REMARKS)
                    .changes
                    .is_empty()
            );
            let unknown = evaluate_at(
                &first.next,
                &Detail {
                    rebase_required: None,
                    ..blocked.clone()
                },
                NO_REMARKS,
            );
            assert!(
                evaluate_at(&unknown.next, &blocked, NO_REMARKS)
                    .changes
                    .is_empty()
            );
            let clean = evaluate_at(
                &first.next,
                &Detail {
                    rebase_required: Some(false),
                    ..blocked.clone()
                },
                NO_REMARKS,
            );
            assert_eq!(
                kinds(&evaluate_at(&clean.next, &blocked, NO_REMARKS)),
                ["needs-rebase"]
            );
            let pushed = evaluate_at(
                &first.next,
                &Detail {
                    head_sha: Some("bbbbbbbbbb".into()),
                    ..blocked
                },
                NO_REMARKS,
            );
            assert!(kinds(&pushed).contains(&"needs-rebase"));
        }
    }

    #[test]
    fn baselines_already_passed_checks() {
        for required in [false, true] {
            let green = Detail {
                checks: vec![{
                    let mut check = check("test", CheckStatus::Success);
                    check.required = required;
                    check
                }],
                ..detail()
            };
            let baseline = evaluate_at(
                &Watch {
                    awaiting_baseline: Some(Baseline::All),
                    ..watch()
                },
                &green,
                NO_REMARKS,
            );
            assert!(baseline.changes.is_empty());
            assert_eq!(baseline.next.head_sha, green.head_sha);
            assert!(baseline.next.passed);
            assert_eq!(baseline.next.passed_checks, ["test"]);
            assert!(!baseline.exhausted);
            assert!(
                evaluate_at(&baseline.next, &green, NO_REMARKS)
                    .changes
                    .is_empty()
            );

            let pending = evaluate_at(
                &baseline.next,
                &Detail {
                    checks: vec![{
                        let mut check = check("test", CheckStatus::Pending);
                        check.required = required;
                        check
                    }],
                    ..detail()
                },
                NO_REMARKS,
            );
            assert!(pending.changes.is_empty());
            let failed = evaluate_at(
                &pending.next,
                &Detail {
                    checks: vec![{
                        let mut check = check("test", CheckStatus::Failure);
                        check.required = required;
                        check
                    }],
                    ..detail()
                },
                NO_REMARKS,
            );
            assert!(matches!(
                failed.changes.first(),
                Some(WatchChange::ChecksFailed { .. })
            ));
            let passed = evaluate_at(&failed.next, &green, NO_REMARKS);
            assert_eq!(
                passed.changes,
                [WatchChange::ChecksPassed { count: 1, required }]
            );
            assert_eq!(
                evaluate_at(
                    &passed.next,
                    &Detail {
                        head_sha: Some("bbbbbbbbbb".into()),
                        ..green.clone()
                    },
                    NO_REMARKS
                )
                .changes,
                [WatchChange::ChecksPassed { count: 1, required }]
            );
        }
    }

    #[test]
    fn baselines_existing_failures_and_conflicts_then_reports_new_ones() {
        let broken = Detail {
            checks: vec![
                check("lint", CheckStatus::Failure),
                check("test", CheckStatus::Pending),
            ],
            mergeability: Mergeability::Conflicting,
            rebase_required: Some(true),
            ..detail()
        };
        let baseline = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                ..watch()
            },
            &broken,
            NO_REMARKS,
        );
        assert!(baseline.changes.is_empty());
        assert_eq!(baseline.next.failed_checks, ["lint"]);
        assert!(!baseline.next.passed);
        assert!(baseline.next.conflicting);
        assert_eq!(baseline.next.rebase_required, Some(true));
        assert!(
            evaluate_at(&baseline.next, &broken, NO_REMARKS)
                .changes
                .is_empty()
        );

        let failed = evaluate_at(
            &baseline.next,
            &Detail {
                checks: vec![
                    check("lint", CheckStatus::Failure),
                    check("test", CheckStatus::Failure),
                ],
                ..broken.clone()
            },
            NO_REMARKS,
        );
        assert_eq!(
            failed.changes,
            [WatchChange::ChecksFailed {
                failed: vec![check("test", CheckStatus::Failure)]
            }]
        );
        let clean = evaluate_at(
            &failed.next,
            &Detail {
                rebase_required: Some(false),
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(clean.changes.is_empty());
        assert_eq!(
            kinds(&evaluate_at(&clean.next, &broken, NO_REMARKS)),
            ["checks-failed", "conflicting", "needs-rebase"]
        );
    }

    #[test]
    fn reports_failures_on_the_first_read_only_when_ci_was_pending_at_the_start() {
        let red = Detail {
            checks: vec![
                check("lint", CheckStatus::Failure),
                check("test", CheckStatus::Pending),
            ],
            ..detail()
        };
        let pending = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                checks_pending: Some(true),
                ..watch()
            },
            &red,
            NO_REMARKS,
        );
        assert_eq!(
            pending.changes,
            [WatchChange::ChecksFailed {
                failed: vec![check("lint", CheckStatus::Failure)]
            }]
        );
        assert!(
            evaluate_at(
                &pending.next,
                &Detail {
                    checks: vec![
                        check("lint", CheckStatus::Failure),
                        check("test", CheckStatus::Success),
                    ],
                    ..detail()
                },
                NO_REMARKS
            )
            .changes
            .is_empty()
        );
        assert!(
            evaluate_at(
                &Watch {
                    awaiting_baseline: Some(Baseline::All),
                    checks_pending: Some(false),
                    ..watch()
                },
                &red,
                NO_REMARKS
            )
            .changes
            .is_empty()
        );

        // A read the host could not finish keeps the baseline and must keep
        // the seed with it.
        let placeholder =
            Check::new("Workflow approval status", CheckStatus::Pending).placeholder();
        let incomplete = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                checks_pending: Some(true),
                ..watch()
            },
            &Detail {
                checks: vec![check("lint", CheckStatus::Success), placeholder],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(incomplete.changes.is_empty());
        assert_eq!(
            evaluate_at(
                &incomplete.next,
                &Detail {
                    checks: vec![
                        check("lint", CheckStatus::Success),
                        check("gated", CheckStatus::Failure),
                    ],
                    ..detail()
                },
                NO_REMARKS
            )
            .changes,
            [WatchChange::ChecksFailed {
                failed: vec![check("gated", CheckStatus::Failure)]
            }]
        );
    }

    #[test]
    fn baselines_checks_already_awaiting_approval() {
        let gated = Detail {
            checks: vec![check("CI", CheckStatus::ActionRequired).with_url(run(1))],
            ..detail()
        };
        let baseline = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                ..watch()
            },
            &gated,
            NO_REMARKS,
        );
        assert!(baseline.changes.is_empty());
        assert!(
            evaluate_at(&baseline.next, &gated, NO_REMARKS)
                .changes
                .is_empty()
        );
    }

    #[test]
    fn reports_remarks_after_the_watch_started_while_baselining_checks_and_conflicts() {
        let green = Detail {
            checks: vec![check("test", CheckStatus::Success)],
            mergeability: Mergeability::Conflicting,
            ..detail()
        };
        let comments = [
            remark("reviewer", "2026-10-02T11:00:00Z"),
            remark("reviewer", "2026-10-02T12:06:00Z"),
        ];
        let baseline = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                ..watch()
            },
            &green,
            Some(&comments),
        );
        assert_eq!(
            baseline.changes,
            [WatchChange::Remarks {
                remarks: vec![comments[1].clone()]
            }]
        );
        assert!(baseline.next.passed);
        assert!(baseline.next.conflicting);
        assert_eq!(baseline.next.wakes, 1);
        assert!(
            evaluate_at(&baseline.next, &green, Some(&comments))
                .changes
                .is_empty()
        );
    }

    #[test]
    fn reports_a_failure_once_the_head_sha_appears_after_a_baseline_without_one() {
        let waiting = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                ..watch()
            },
            &Detail {
                head_sha: None,
                checks: vec![check("test", CheckStatus::Pending)],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(waiting.changes.is_empty());
        assert_eq!(
            evaluate_at(
                &waiting.next,
                &Detail {
                    checks: vec![check("test", CheckStatus::Failure)],
                    ..detail()
                },
                NO_REMARKS
            )
            .changes,
            [WatchChange::ChecksFailed {
                failed: vec![check("test", CheckStatus::Failure)]
            }]
        );
    }

    #[test]
    fn keeps_a_baseline_taken_without_a_head_sha_once_the_sha_appears() {
        let green = Detail {
            checks: vec![check("test", CheckStatus::Success)],
            ..detail()
        };
        let baseline = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                ..watch()
            },
            &Detail {
                head_sha: None,
                ..green.clone()
            },
            NO_REMARKS,
        );
        let known = evaluate_at(&baseline.next, &green, NO_REMARKS);
        assert!(known.changes.is_empty());
        assert_eq!(known.next.head_sha, green.head_sha);
        let unknown = evaluate_at(
            &known.next,
            &Detail {
                head_sha: None,
                ..green.clone()
            },
            NO_REMARKS,
        );
        assert_eq!(unknown.next.head_sha, green.head_sha);
        assert_eq!(
            evaluate_at(
                &unknown.next,
                &Detail {
                    head_sha: Some("bbbbbbbbbb".into()),
                    ..green
                },
                NO_REMARKS
            )
            .changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: false
            }]
        );
    }

    #[test]
    fn reports_checks_that_appear_after_a_first_read_without_any() {
        for (status, expected) in [
            (
                CheckStatus::Success,
                WatchChange::ChecksPassed {
                    count: 1,
                    required: false,
                },
            ),
            (
                CheckStatus::Failure,
                WatchChange::ChecksFailed {
                    failed: vec![check("test", CheckStatus::Failure)],
                },
            ),
        ] {
            let empty = evaluate_at(
                &Watch {
                    awaiting_baseline: Some(Baseline::All),
                    ..watch()
                },
                &Detail {
                    checks: Vec::new(),
                    ..detail()
                },
                NO_REMARKS,
            );
            assert!(empty.changes.is_empty());
            assert_eq!(empty.next.awaiting_baseline, None);
            assert_eq!(empty.next.checks_pending, Some(true));
            assert_eq!(
                evaluate_at(
                    &empty.next,
                    &Detail {
                        checks: vec![check("test", status)],
                        ..detail()
                    },
                    NO_REMARKS
                )
                .changes,
                [expected]
            );
        }
    }

    #[test]
    fn keeps_the_check_baseline_through_a_read_whose_approval_lookup_failed() {
        let bot = check("bot", CheckStatus::Success);
        let unavailable =
            Check::new("Workflow approval status", CheckStatus::Pending).placeholder();
        let partial = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                ..watch()
            },
            &Detail {
                checks: vec![bot.clone(), unavailable],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(partial.changes.is_empty());
        assert_eq!(partial.next.awaiting_baseline, Some(Baseline::Checks));
        let approval = check("CI", CheckStatus::ActionRequired).with_url(run(1));
        for checks in [vec![bot.clone()], vec![bot.clone(), approval]] {
            assert!(
                evaluate_at(&partial.next, &Detail { checks, ..detail() }, NO_REMARKS)
                    .changes
                    .is_empty()
            );
        }
    }

    #[test]
    fn reports_pending_advisory_checks_finishing_after_a_required_check_baseline() {
        let initial = Detail {
            checks: vec![
                check("test", CheckStatus::Success).required(),
                check("bot", CheckStatus::Pending),
            ],
            ..detail()
        };
        let baseline = evaluate_at(
            &Watch {
                awaiting_baseline: Some(Baseline::All),
                ..watch()
            },
            &initial,
            NO_REMARKS,
        );
        assert!(baseline.changes.is_empty());
        let settled = evaluate_at(
            &baseline.next,
            &Detail {
                checks: vec![
                    initial.checks[0].clone(),
                    check("bot", CheckStatus::Success),
                ],
                ..detail()
            },
            NO_REMARKS,
        );
        assert_eq!(
            settled.changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: false
            }]
        );
        assert!(
            evaluate_at(
                &settled.next,
                &Detail {
                    checks: vec![
                        initial.checks[0].clone(),
                        check("bot", CheckStatus::Success),
                    ],
                    ..detail()
                },
                NO_REMARKS
            )
            .changes
            .is_empty()
        );
    }

    #[test]
    fn waits_for_workflow_approval_once_through_reads_that_miss_it_until_the_run_finishes() {
        let bot = check("bot", CheckStatus::Success);
        let approval = check("CI", CheckStatus::ActionRequired).with_url(run(1));
        let gated = Detail {
            checks: vec![bot.clone(), approval.clone()],
            ..detail()
        };
        let first = evaluate_at(&watch(), &gated, NO_REMARKS);
        assert_eq!(
            first.changes,
            [WatchChange::ChecksAwaitingApproval {
                checks: vec![approval.clone()]
            }]
        );

        // GitHub's approval read can answer empty while the rest of the
        // checks read fine.
        let missing = evaluate_at(
            &first.next,
            &Detail {
                checks: vec![bot.clone()],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(missing.changes.is_empty());
        assert!(!missing.next.passed);
        assert!(
            evaluate_at(&missing.next, &gated, NO_REMARKS)
                .changes
                .is_empty()
        );
        let later = check("Preview", CheckStatus::ActionRequired).with_url(run(2));
        assert!(
            evaluate_at(
                &missing.next,
                &Detail {
                    checks: vec![bot.clone(), approval.clone(), later],
                    ..detail()
                },
                NO_REMARKS
            )
            .changes
            .is_empty()
        );
        let unnamed = Check::new("Docs", CheckStatus::ActionRequired);
        let named = Detail {
            checks: vec![bot.clone(), unnamed.clone()],
            ..detail()
        };
        let alone = evaluate_at(&watch(), &named, NO_REMARKS);
        assert_eq!(
            alone.changes,
            [WatchChange::ChecksAwaitingApproval {
                checks: vec![unnamed.clone()]
            }]
        );
        let placeholder =
            Check::new("Workflow approval status", CheckStatus::ActionRequired).placeholder();
        let unlisted = evaluate_at(
            &alone.next,
            &Detail {
                checks: vec![bot.clone(), placeholder],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(unlisted.changes.is_empty());
        assert!(!unlisted.next.passed);
        assert!(
            evaluate_at(&unlisted.next, &named, NO_REMARKS)
                .changes
                .is_empty()
        );
        let dropped = evaluate_at(
            &alone.next,
            &Detail {
                checks: vec![bot.clone()],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(dropped.changes.is_empty());
        assert!(!dropped.next.passed);
        assert_eq!(dropped.next.awaiting_approval, vec![unnamed.name.clone()]);
        assert!(
            evaluate_at(&dropped.next, &named, NO_REMARKS)
                .changes
                .is_empty()
        );
        let named_pending = evaluate_at(
            &dropped.next,
            &Detail {
                checks: vec![
                    bot.clone().required(),
                    Check {
                        status: CheckStatus::Pending,
                        ..unnamed.clone()
                    },
                ],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(named_pending.changes.is_empty());
        assert!(!named_pending.next.passed);
        let named_done = Detail {
            checks: vec![
                bot.clone(),
                Check {
                    status: CheckStatus::Success,
                    ..unnamed.clone()
                },
            ],
            ..detail()
        };
        let named_finished = evaluate_at(&named_pending.next, &named_done, NO_REMARKS);
        assert_eq!(
            named_finished.changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: false
            }]
        );
        assert!(named_finished.next.awaiting_approval.is_empty());
        assert!(
            evaluate_at(&named_finished.next, &named_done, NO_REMARKS)
                .changes
                .is_empty()
        );
        let pushed = Detail {
            head_sha: Some("bbbbbbbbbb".into()),
            checks: vec![bot.clone()],
            ..detail()
        };
        assert_eq!(
            evaluate_at(&missing.next, &pushed, NO_REMARKS).changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: false
            }]
        );

        // Approved, the run's jobs carry their own names under the run's URL.
        let job = check("lint", CheckStatus::Success).with_url(format!("{}/job/7", run(1)));
        let pending = check("test", CheckStatus::Pending).with_url(format!("{}/job/8", run(1)));
        let required = bot.clone().required();
        let running = evaluate_at(
            &missing.next,
            &Detail {
                checks: vec![required.clone(), job.clone(), pending.clone()],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(running.changes.is_empty());
        assert!(!running.next.passed);
        assert_eq!(running.next.awaiting_approval, [run(1)]);
        let overlap = evaluate_at(
            &running.next,
            &Detail {
                checks: vec![
                    required.clone(),
                    job.clone(),
                    check("test", CheckStatus::Pending).with_url(format!("{}/job/9", run(2))),
                ],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(overlap.changes.is_empty());
        assert!(!overlap.next.passed);
        assert_eq!(overlap.next.awaiting_approval, [run(1)]);
        let unlinked = evaluate_at(
            &overlap.next,
            &Detail {
                checks: vec![
                    required.clone(),
                    job.clone(),
                    Check::new("test", CheckStatus::Pending),
                ],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(unlinked.changes.is_empty());
        assert!(!unlinked.next.passed);
        assert_eq!(unlinked.next.awaiting_approval, [run(1)]);
        let advisory = evaluate_at(
            &unlinked.next,
            &Detail {
                checks: vec![
                    required.clone(),
                    job.clone(),
                    check("test", CheckStatus::Pending),
                ],
                ..detail()
            },
            NO_REMARKS,
        );
        assert_eq!(
            advisory.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: true
            }]
        );
        assert!(advisory.next.awaiting_approval.is_empty());
        let completed = Detail {
            checks: vec![
                required.clone(),
                job.clone(),
                check("test", CheckStatus::Success),
            ],
            ..detail()
        };
        let finished = evaluate_at(&unlinked.next, &completed, NO_REMARKS);
        assert_eq!(
            finished.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: true
            }]
        );
        assert!(finished.next.awaiting_approval.is_empty());
        assert!(
            evaluate_at(&finished.next, &completed, NO_REMARKS)
                .changes
                .is_empty()
        );

        let done = evaluate_at(
            &missing.next,
            &Detail {
                checks: vec![bot.clone(), job],
                ..detail()
            },
            NO_REMARKS,
        );
        assert_eq!(
            done.changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: false
            }]
        );
    }

    #[test]
    fn does_not_pass_or_fail_while_the_host_cannot_tell_whether_workflows_await_approval() {
        let bot = check("bot", CheckStatus::Success);
        let unknown =
            Check::new("Workflow approval status", CheckStatus::ActionRequired).placeholder();
        let unread = evaluate_at(
            &watch(),
            &Detail {
                checks: vec![bot.clone(), unknown.clone()],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(unread.changes.is_empty());
        assert!(!unread.next.passed);
        let read = evaluate_at(
            &unread.next,
            &Detail {
                checks: vec![bot.clone()],
                ..detail()
            },
            NO_REMARKS,
        );
        assert_eq!(
            read.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: false
            }]
        );
        let blocked = evaluate_at(
            &read.next,
            &Detail {
                checks: vec![
                    Check {
                        status: CheckStatus::Pending,
                        ..bot.clone()
                    },
                    unknown,
                ],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(!blocked.next.passed);
        assert_eq!(
            evaluate_at(
                &blocked.next,
                &Detail {
                    checks: vec![bot],
                    ..detail()
                },
                NO_REMARKS
            )
            .changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: false
            }]
        );
    }

    #[test]
    fn reports_each_failure_at_once_even_while_another_check_never_finishes() {
        let bot = check("CodeRabbit", CheckStatus::Pending);
        let first = Detail {
            checks: vec![
                check("lint", CheckStatus::Failure),
                check("test", CheckStatus::Pending),
                bot.clone(),
            ],
            ..detail()
        };
        let lint = evaluate_at(&watch(), &first, NO_REMARKS);
        assert_eq!(
            lint.changes,
            [WatchChange::ChecksFailed {
                failed: vec![check("lint", CheckStatus::Failure)]
            }]
        );
        assert!(
            evaluate_at(&lint.next, &first, NO_REMARKS)
                .changes
                .is_empty()
        );

        let second = Detail {
            checks: vec![
                check("lint", CheckStatus::Failure),
                check("test", CheckStatus::Failure),
                bot,
            ],
            ..detail()
        };
        let test = evaluate_at(&lint.next, &second, NO_REMARKS);
        assert_eq!(
            test.changes,
            [WatchChange::ChecksFailed {
                failed: vec![check("test", CheckStatus::Failure)]
            }]
        );

        let rerun = evaluate_at(&test.next, &first, NO_REMARKS);
        assert_eq!(
            evaluate_at(&rerun.next, &second, NO_REMARKS).changes.len(),
            1
        );

        let pushed = Detail {
            head_sha: Some("bbbbbbbbbb".into()),
            ..second.clone()
        };
        assert_eq!(
            evaluate_at(&test.next, &pushed, NO_REMARKS).changes.len(),
            1
        );

        let approval = check("CI", CheckStatus::ActionRequired).with_url(run(1));
        let gated = Detail {
            checks: vec![approval.clone(), check("lint", CheckStatus::Failure)],
            ..detail()
        };
        assert_eq!(
            evaluate_at(&watch(), &gated, NO_REMARKS).changes,
            [
                WatchChange::ChecksFailed {
                    failed: vec![check("lint", CheckStatus::Failure)]
                },
                WatchChange::ChecksAwaitingApproval {
                    checks: vec![approval]
                }
            ]
        );
    }

    #[test]
    fn reports_passed_once_the_required_checks_pass_whatever_the_others_do() {
        let green = Detail {
            checks: vec![
                check("test", CheckStatus::Success).required(),
                check("lint", CheckStatus::Success).required(),
                check("bot", CheckStatus::Pending),
            ],
            ..detail()
        };
        let passed = evaluate_at(&watch(), &green, NO_REMARKS);
        assert_eq!(
            passed.changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: true
            }]
        );
        assert!(
            evaluate_at(&passed.next, &green, NO_REMARKS)
                .changes
                .is_empty()
        );

        let plain = Detail {
            checks: vec![
                check("test", CheckStatus::Success),
                check("bot", CheckStatus::Pending),
            ],
            ..detail()
        };
        assert!(evaluate_at(&watch(), &plain, NO_REMARKS).changes.is_empty());
    }

    #[test]
    fn reports_passed_again_when_a_required_check_shows_up_already_passed() {
        let tests = Detail {
            checks: vec![
                check("Tests", CheckStatus::Success).required(),
                check("Smoke Tests", CheckStatus::Pending),
            ],
            ..detail()
        };
        let first = evaluate_at(&watch(), &tests, NO_REMARKS);
        assert_eq!(
            first.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: true
            }]
        );

        let gated = Detail {
            checks: vec![
                check("Tests", CheckStatus::Success).required(),
                check("Smoke Tests", CheckStatus::Success),
                check("Smoke Tests Gate", CheckStatus::Success).required(),
            ],
            ..detail()
        };
        let second = evaluate_at(&first.next, &gated, NO_REMARKS);
        assert_eq!(
            second.changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: true
            }]
        );
        assert!(
            evaluate_at(&second.next, &gated, NO_REMARKS)
                .changes
                .is_empty()
        );

        let pending = Detail {
            checks: vec![
                check("Tests", CheckStatus::Success).required(),
                check("Smoke Tests Gate", CheckStatus::Pending).required(),
            ],
            ..detail()
        };
        let waiting = evaluate_at(&first.next, &pending, NO_REMARKS);
        assert!(waiting.changes.is_empty());
        assert_eq!(
            evaluate_at(&waiting.next, &gated, NO_REMARKS).changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: true
            }]
        );
    }

    #[test]
    fn wakes_again_when_the_other_checks_finish_after_the_required_ones_passed() {
        let early = Detail {
            checks: vec![
                check("test", CheckStatus::Success).required(),
                check("e2e", CheckStatus::Pending),
            ],
            ..detail()
        };
        let first = evaluate_at(&watch(), &early, NO_REMARKS);
        assert_eq!(
            first.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: true
            }]
        );

        let done = Detail {
            checks: vec![
                check("test", CheckStatus::Success).required(),
                check("e2e", CheckStatus::Success),
            ],
            ..detail()
        };
        let final_report = evaluate_at(&first.next, &done, NO_REMARKS);
        assert_eq!(kinds(&final_report), ["checks-passed"]);
        assert!(
            evaluate_at(&final_report.next, &done, NO_REMARKS)
                .changes
                .is_empty()
        );
        // A later advisory job running and passing is not more news.
        let bot_pending = Detail {
            checks: [
                done.checks.clone(),
                vec![check("bot", CheckStatus::Pending)],
            ]
            .concat(),
            ..detail()
        };
        let churn = evaluate_at(&final_report.next, &bot_pending, NO_REMARKS);
        assert!(churn.changes.is_empty());
        let bot_done = Detail {
            checks: [
                done.checks.clone(),
                vec![check("bot", CheckStatus::Success)],
            ]
            .concat(),
            ..detail()
        };
        assert!(
            evaluate_at(&churn.next, &bot_done, NO_REMARKS)
                .changes
                .is_empty()
        );

        // A pending check that turns required on the next read still finishes
        // with one wake.
        let promoted = Detail {
            checks: vec![
                check("test", CheckStatus::Success).required(),
                check("e2e", CheckStatus::Success).required(),
            ],
            ..detail()
        };
        let gated = evaluate_at(&first.next, &promoted, NO_REMARKS);
        assert_eq!(
            gated.changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: true
            }]
        );
        assert!(
            evaluate_at(&gated.next, &promoted, NO_REMARKS)
                .changes
                .is_empty()
        );

        // Nothing else was pending when the required ones passed, so nothing
        // more is owed.
        let green = evaluate_at(&watch(), &done, NO_REMARKS);
        assert_eq!(
            green.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: true
            }]
        );
        assert!(
            evaluate_at(&green.next, &bot_done, NO_REMARKS)
                .changes
                .is_empty()
        );
    }

    #[test]
    fn reports_an_owed_non_required_failure_once_after_the_required_notice() {
        let required = check("test", CheckStatus::Success).required();
        let early = Detail {
            checks: vec![required.clone(), check("e2e", CheckStatus::Pending)],
            ..detail()
        };
        let first = evaluate_at(&watch(), &early, NO_REMARKS);
        assert_eq!(
            first.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: true
            }]
        );
        let failed = Detail {
            checks: vec![required.clone(), check("e2e", CheckStatus::Failure)],
            ..detail()
        };
        let failure = evaluate_at(&first.next, &failed, NO_REMARKS);
        assert_eq!(
            failure.changes,
            [WatchChange::ChecksFailed {
                failed: vec![check("e2e", CheckStatus::Failure)]
            }]
        );
        assert!(
            evaluate_at(&failure.next, &failed, NO_REMARKS)
                .changes
                .is_empty()
        );
        let recovered = Detail {
            checks: vec![required, check("e2e", CheckStatus::Success)],
            ..detail()
        };
        let final_report = evaluate_at(&failure.next, &recovered, NO_REMARKS);
        assert_eq!(
            final_report.changes,
            [WatchChange::ChecksPassed {
                count: 2,
                required: false
            }]
        );
        assert!(
            evaluate_at(&final_report.next, &recovered, NO_REMARKS)
                .changes
                .is_empty()
        );
    }

    #[test]
    fn clears_old_head_owed_checks_when_a_new_head_is_pushed() {
        let required = check("test", CheckStatus::Success).required();
        let early = Detail {
            checks: vec![required.clone(), check("e2e", CheckStatus::Pending)],
            ..detail()
        };
        let first = evaluate_at(&watch(), &early, NO_REMARKS);
        assert_eq!(first.next.owed_checks, ["e2e"]);
        let unread_head = Detail {
            head_sha: Some("bbbbbbbbbb".into()),
            checks: Vec::new(),
            ..detail()
        };
        let unread = evaluate_at(&first.next, &unread_head, NO_REMARKS);
        assert!(unread.changes.is_empty());
        assert!(unread.next.owed_checks.is_empty());
        let pushed = Detail {
            checks: vec![required],
            ..unread_head.clone()
        };
        let new_head = evaluate_at(&unread.next, &pushed, NO_REMARKS);
        assert_eq!(
            new_head.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: true
            }]
        );
        assert!(new_head.next.owed_checks.is_empty());
        let late_result = Detail {
            checks: vec![
                check("test", CheckStatus::Success).required(),
                check("e2e", CheckStatus::Success),
            ],
            ..pushed
        };
        let final_report = evaluate_at(&new_head.next, &late_result, NO_REMARKS);
        assert!(final_report.changes.is_empty());
        assert!(
            evaluate_at(&final_report.next, &late_result, NO_REMARKS)
                .changes
                .is_empty()
        );
    }

    #[test]
    fn does_not_report_passed_again_for_a_new_passed_check_where_none_is_required() {
        let first = evaluate_at(
            &watch(),
            &Detail {
                checks: vec![check("test", CheckStatus::Success)],
                ..detail()
            },
            NO_REMARKS,
        );
        assert_eq!(
            first.changes,
            [WatchChange::ChecksPassed {
                count: 1,
                required: false
            }]
        );
        let both = Detail {
            checks: vec![
                check("test", CheckStatus::Success),
                check("lint", CheckStatus::Success),
            ],
            ..detail()
        };
        assert!(
            evaluate_at(&first.next, &both, NO_REMARKS)
                .changes
                .is_empty()
        );
    }

    #[test]
    fn does_not_wake_a_watch_saved_before_passed_checks_were_recorded() {
        let green = Detail {
            checks: vec![check("test", CheckStatus::Success).required()],
            ..detail()
        };
        let told = Watch {
            head_sha: Some("aaaaaaaaaa".into()),
            passed: true,
            ..watch()
        };
        let saved = evaluate_at(&told, &green, NO_REMARKS);
        assert!(saved.changes.is_empty());
        assert_eq!(saved.next.passed_checks, ["test"]);
    }

    #[test]
    fn keeps_remarks_for_a_later_pass_when_the_conversation_was_not_read_whole() {
        let comments = [remark("reviewer", "2026-10-02T12:06:00Z")];
        let partial = evaluate_at(&watch(), &detail(), None);
        assert!(partial.changes.is_empty());
        assert!(matches!(
            evaluate_at(&partial.next, &detail(), Some(&comments))
                .changes
                .first(),
            Some(WatchChange::Remarks { .. })
        ));
    }

    #[test]
    fn reports_a_remark_that_shows_up_late_with_the_same_time_as_a_reported_one() {
        let first = remark("reviewer", "2026-10-02T12:06:00Z");
        let late = Remark {
            id: "late".into(),
            ..remark("bot", "2026-10-02T12:06:00Z")
        };
        let reported = evaluate_at(&watch(), &detail(), Some(std::slice::from_ref(&first)));
        let again = evaluate_at(
            &reported.next,
            &detail(),
            Some(&[first.clone(), late.clone()]),
        );
        assert_eq!(
            again.changes,
            [WatchChange::Remarks {
                remarks: vec![late.clone()]
            }]
        );
        assert_eq!(again.next.remark_ids, vec![first.id, "late".into()]);
    }

    #[test]
    fn reports_edits_after_the_watermark_once_and_counts_them_toward_the_wake_limit() {
        let old = remark("greptile[bot]", "2026-10-02T11:00:00Z");
        let watching = Watch {
            head_sha: Some("aaaaaaaaaa".into()),
            remark_ids: vec![old.id.clone()],
            wakes: WATCH_WAKE_LIMIT - 1,
            ..watch()
        };
        assert!(
            evaluate_at(&watching, &detail(), Some(std::slice::from_ref(&old)))
                .changes
                .is_empty()
        );
        let edited = Remark {
            edited_at: Some(at("2026-10-02T12:06:00Z")),
            ..old.clone()
        };
        let report = evaluate_at(&watching, &detail(), Some(std::slice::from_ref(&edited)));
        assert_eq!(
            report.changes,
            [WatchChange::Remarks {
                remarks: vec![edited.clone()]
            }]
        );
        assert_eq!(report.next.remarks_through, edited.edited_at.unwrap());
        assert_eq!(report.next.remark_ids, vec![old.id.clone()]);
        assert!(report.exhausted);
        assert!(
            evaluate_at(&report.next, &detail(), Some(std::slice::from_ref(&edited)))
                .changes
                .is_empty()
        );
        let late = Remark {
            id: "late".into(),
            ..edited.clone()
        };
        assert_eq!(
            evaluate_at(&report.next, &detail(), Some(&[edited, late.clone()])).changes,
            [WatchChange::Remarks {
                remarks: vec![late]
            }]
        );
    }

    #[test]
    fn wakes_once_per_edit_of_a_remark_seen_at_the_watermark_never_for_the_agents_own() {
        let seen = remark("ci-bot", "2026-10-10T19:52:40.000Z");
        let watching = Watch {
            head_sha: Some("aaaaaaaaaa".into()),
            remarks_through: seen.created_at,
            remark_ids: vec![seen.id.clone()],
            wakes: 2,
            ..watch()
        };
        assert!(
            evaluate_at(&watching, &detail(), Some(std::slice::from_ref(&seen)))
                .changes
                .is_empty()
        );
        let edited = Remark {
            edited_at: Some(at("2026-10-10T20:04:50.000Z")),
            ..seen.clone()
        };
        let first = evaluate_at(&watching, &detail(), Some(std::slice::from_ref(&edited)));
        assert_eq!(
            first.changes,
            [WatchChange::Remarks {
                remarks: vec![edited.clone()]
            }]
        );
        assert_eq!(first.next.remarks_through, edited.edited_at.unwrap());
        assert!(
            evaluate_at(&first.next, &detail(), Some(std::slice::from_ref(&edited)))
                .changes
                .is_empty()
        );
        let again = Remark {
            edited_at: Some(at("2026-10-10T20:05:50.000Z")),
            ..seen.clone()
        };
        let second = evaluate_at(&first.next, &detail(), Some(std::slice::from_ref(&again)));
        assert_eq!(
            second.changes,
            [WatchChange::Remarks {
                remarks: vec![again.clone()]
            }]
        );
        assert!(
            evaluate_at(&second.next, &detail(), Some(&[again]))
                .changes
                .is_empty()
        );
        let own = Remark {
            author: Some("agent-user".into()),
            ..edited
        };
        assert!(
            evaluate_at(&watching, &detail(), Some(&[own]))
                .changes
                .is_empty()
        );
    }

    #[test]
    fn does_not_treat_a_failed_check_read_as_a_rerun() {
        let failed = Detail {
            checks: vec![check("lint", CheckStatus::Failure)],
            ..detail()
        };
        let reported = evaluate_at(&watch(), &failed, NO_REMARKS);
        assert_eq!(reported.changes.len(), 1);
        let unreadable = evaluate_at(
            &reported.next,
            &Detail {
                checks: Vec::new(),
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(
            evaluate_at(&unreadable.next, &failed, NO_REMARKS)
                .changes
                .is_empty()
        );
    }

    #[test]
    fn wakes_for_the_pull_requests_author_when_the_agent_is_someone_else() {
        let contributor = Detail {
            author: Some("contributor".into()),
            ..detail()
        };
        let reply = remark("contributor", "2026-10-02T12:06:00Z");
        assert_eq!(
            evaluate_at(&watch(), &contributor, Some(std::slice::from_ref(&reply))).changes,
            [WatchChange::Remarks {
                remarks: vec![reply.clone()]
            }]
        );
        let no_viewer = Detail {
            viewer: None,
            author: Some("contributor".into()),
            ..detail()
        };
        assert!(
            evaluate_at(&watch(), &no_viewer, Some(&[reply]))
                .changes
                .is_empty()
        );
    }

    #[test]
    fn reports_remarks_from_others_once_and_never_the_agents_own() {
        let comments = [
            remark_with(
                "agent-user",
                "2026-10-02T12:05:00Z",
                "Fixed in the latest push.",
            ),
            remark("macroscope-app[bot]", "2026-10-02T12:06:00Z"),
            remark_with("reviewer", "2026-10-02T11:00:00Z", "Older than the watch."),
        ];
        let report = evaluate_at(&watch(), &detail(), Some(&comments));
        assert_eq!(
            report.changes,
            [WatchChange::Remarks {
                remarks: vec![comments[1].clone()]
            }]
        );
        assert_eq!(report.next.remarks_through, at("2026-10-02T12:06:00Z"));
        assert!(
            evaluate_at(&report.next, &detail(), Some(&comments))
                .changes
                .is_empty()
        );
    }

    #[test]
    fn cuts_a_long_remark_in_the_wake_message_without_splitting_an_emoji() {
        let body = format!("{}🚀 and more", "x".repeat(196));
        let report = evaluate_at(
            &watch(),
            &detail(),
            Some(&[remark_with("reviewer", "2026-10-02T12:06:00Z", &body)]),
        );
        let text = watch_message(1, "https://github.com/o/r/pull/1", "main", None, &report);
        assert!(text.contains(&format!("\"{}...\"", "x".repeat(196))));
    }

    #[test]
    fn reports_a_conflict_once_until_the_branch_is_clean_again() {
        let conflicting = Detail {
            mergeability: Mergeability::Conflicting,
            ..detail()
        };
        let first = evaluate_at(&watch(), &conflicting, NO_REMARKS);
        assert_eq!(first.changes, [WatchChange::Conflicting]);
        let head_unread = Detail {
            head_sha: None,
            ..conflicting.clone()
        };
        let unread = evaluate_at(&first.next, &head_unread, NO_REMARKS);
        assert!(unread.changes.is_empty());
        assert!(
            evaluate_at(&unread.next, &conflicting, NO_REMARKS)
                .changes
                .is_empty()
        );
        let recomputing = evaluate_at(
            &first.next,
            &Detail {
                mergeability: Mergeability::Unknown,
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(
            evaluate_at(&recomputing.next, &conflicting, NO_REMARKS)
                .changes
                .is_empty()
        );
        let clean = evaluate_at(&first.next, &detail(), NO_REMARKS);
        assert_eq!(
            evaluate_at(&clean.next, &conflicting, NO_REMARKS).changes,
            [WatchChange::Conflicting]
        );
        let pushed = Detail {
            head_sha: Some("bbbbbbbbbb".into()),
            ..detail()
        };
        let recomputed = evaluate_at(
            &first.next,
            &Detail {
                mergeability: Mergeability::Unknown,
                ..pushed.clone()
            },
            NO_REMARKS,
        );
        assert_eq!(
            evaluate_at(
                &recomputed.next,
                &Detail {
                    mergeability: Mergeability::Conflicting,
                    ..pushed
                },
                NO_REMARKS
            )
            .changes,
            [WatchChange::Conflicting]
        );
    }

    #[test]
    fn reports_each_merge_queue_removal_once_while_the_head_checks_stay_green() {
        let green = Detail {
            checks: vec![check("test", CheckStatus::Success)],
            ..detail()
        };
        let watching = evaluate_at(&watch(), &green, NO_REMARKS).next;
        let removed = |id: &str, removed_at: &str| MergeQueueRemoval {
            id: id.into(),
            removed_at: at(removed_at),
            reason: Some("Merge conflict".into()),
        };

        let dropped = Detail {
            merge_queue_removal: Some(removed("first", "2026-10-02T12:10:00Z")),
            ..green.clone()
        };
        let first = evaluate_at(&watching, &dropped, NO_REMARKS);
        assert_eq!(
            first.changes,
            [WatchChange::MergeQueueRemoved {
                reason: Some("Merge conflict".into())
            }]
        );
        assert!(
            evaluate_at(&first.next, &dropped, NO_REMARKS)
                .changes
                .is_empty()
        );
        // A read without queue news keeps the removal already told.
        let quiet = evaluate_at(&first.next, &green, NO_REMARKS);
        assert!(
            evaluate_at(&quiet.next, &dropped, NO_REMARKS)
                .changes
                .is_empty()
        );

        let again = Detail {
            merge_queue_removal: Some(removed("second", "2026-10-02T12:20:00Z")),
            ..green.clone()
        };
        assert_eq!(
            evaluate_at(&first.next, &again, NO_REMARKS).changes.len(),
            1
        );
        let before = Detail {
            merge_queue_removal: Some(removed("before", "2026-10-02T11:00:00Z")),
            ..green.clone()
        };
        let baseline = evaluate_at(
            &Watch {
                head_sha: watching.head_sha.clone(),
                passed: true,
                passed_checks: watching.passed_checks.clone(),
                ..watch()
            },
            &before,
            NO_REMARKS,
        );
        assert!(baseline.changes.is_empty());
        for prior in [&watching, &baseline.next] {
            let skewed = evaluate_at(
                prior,
                &Detail {
                    merge_queue_removal: Some(removed("new", "2026-10-02T11:59:58Z")),
                    ..green.clone()
                },
                NO_REMARKS,
            );
            assert_eq!(
                skewed.changes,
                [WatchChange::MergeQueueRemoved {
                    reason: Some("Merge conflict".into())
                }]
            );
            assert!(
                evaluate_at(
                    &skewed.next,
                    &Detail {
                        merge_queue_removal: Some(removed("new", "2026-10-02T11:59:58Z")),
                        ..green.clone()
                    },
                    NO_REMARKS
                )
                .changes
                .is_empty()
            );
        }
    }

    #[test]
    fn reports_a_merge_queue_removal_stamped_in_the_second_the_watch_started() {
        let watching = Watch {
            started_at: at("2026-10-02T12:00:00.500Z"),
            ..watch()
        };
        let removed = |removed_at: &str| Detail {
            merge_queue_removal: Some(MergeQueueRemoval {
                id: "first".into(),
                removed_at: at(removed_at),
                reason: Some("CI failed".into()),
            }),
            ..detail()
        };
        assert_eq!(
            evaluate_at(&watching, &removed("2026-10-02T12:00:00Z"), NO_REMARKS).changes,
            [WatchChange::MergeQueueRemoved {
                reason: Some("CI failed".into())
            }]
        );
        assert!(
            evaluate_at(&watching, &removed("2026-10-02T11:59:59Z"), NO_REMARKS)
                .changes
                .is_empty()
        );
    }

    #[test]
    fn stays_quiet_when_the_queue_lets_go_of_a_pull_request_by_merging_it() {
        let watching = evaluate_at(&watch(), &detail(), NO_REMARKS).next;
        let merging = Detail {
            merge_queue_removal: Some(MergeQueueRemoval {
                id: "landed".into(),
                removed_at: at("2026-10-02T12:10:00Z"),
                reason: Some("merged".into()),
            }),
            ..detail()
        };
        let read = evaluate_at(&watching, &merging, NO_REMARKS);
        assert!(read.changes.is_empty());
        assert_eq!(read.next.merge_queue_removal_id.as_deref(), Some("landed"));
    }

    #[test]
    fn does_not_spend_the_comment_wake_limit_on_check_results() {
        let tired = Watch {
            head_sha: Some("aaaaaaaaaa".into()),
            wakes: WATCH_WAKE_LIMIT - 1,
            ..watch()
        };
        let result = evaluate_at(
            &tired,
            &Detail {
                checks: vec![check("lint", CheckStatus::Failure)],
                ..detail()
            },
            NO_REMARKS,
        );
        assert!(!result.exhausted);
        assert_eq!(result.next.wakes, 0);
    }

    #[test]
    fn stops_after_the_wake_limit_unless_the_head_moves() {
        let comments = [remark("reviewer", "2026-10-02T12:10:00Z")];
        let tired = Watch {
            head_sha: Some("aaaaaaaaaa".into()),
            wakes: WATCH_WAKE_LIMIT - 1,
            ..watch()
        };
        assert!(evaluate_at(&tired, &detail(), Some(&comments)).exhausted);
        let pushed = evaluate_at(
            &tired,
            &Detail {
                head_sha: Some("cccccccccc".into()),
                ..detail()
            },
            Some(&comments),
        );
        assert!(!pushed.exhausted);
        assert_eq!(pushed.next.wakes, 1);
    }

    fn an_hour_later(first: &Watch, read: &Detail) -> Vec<Vec<&'static str>> {
        let mut seen = Vec::new();
        let mut current = first.clone();
        for minute in 0..=60 {
            let report = evaluate(&current, read, NO_REMARKS, minute * 60_000);
            seen.push(kinds(&report));
            current = report.next;
        }
        seen
    }

    #[test]
    fn does_not_tell_a_watch_saved_before_the_no_checks_clock_that_its_seen_head_has_none() {
        // Such a watch may have seen this head's checks pending, which
        // records nothing.
        let legacy = Watch {
            head_sha: Some("aaaaaaaaaa".into()),
            ..watch()
        };
        assert!(
            an_hour_later(
                &legacy,
                &Detail {
                    checks: Vec::new(),
                    ..detail()
                }
            )
            .iter()
            .all(|kinds| kinds.is_empty())
        );
        let pushed = an_hour_later(
            &legacy,
            &Detail {
                head_sha: Some("bbbbbbbbbb".into()),
                checks: Vec::new(),
                ..detail()
            },
        );
        let flattened: Vec<&str> = pushed.into_iter().flatten().collect();
        assert_eq!(flattened, ["no-checks"]);
        let unknown = Detail {
            checks: vec![
                Check::new("Workflow approval status", CheckStatus::ActionRequired).placeholder(),
            ],
            ..detail()
        };
        let fresh = Watch::new(0, false);
        let flattened: Vec<&str> = an_hour_later(&fresh, &unknown)
            .into_iter()
            .flatten()
            .collect();
        assert!(flattened.is_empty());
        let unread = evaluate(&fresh, &unknown, NO_REMARKS, 0);
        assert_eq!(
            evaluate(
                &unread.next,
                &Detail {
                    checks: Vec::new(),
                    ..detail()
                },
                NO_REMARKS,
                60 * 60_000
            )
            .changes,
            [WatchChange::NoChecks { minutes: 60 }]
        );
    }

    #[test]
    fn starts_the_no_checks_clock_while_a_fresh_watch_awaits_its_baseline() {
        let fresh = Watch::new(0, false);
        let flattened: Vec<&str> = an_hour_later(
            &fresh,
            &Detail {
                checks: Vec::new(),
                ..detail()
            },
        )
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(flattened, ["no-checks"]);
    }

    #[test]
    fn never_reports_missing_checks_on_a_host_where_an_empty_list_says_nothing() {
        let silent = Detail {
            checks_always_report: capabilities(false),
            checks: Vec::new(),
            ..detail()
        };
        assert!(
            an_hour_later(&watch(), &silent)
                .iter()
                .all(|kinds| kinds.is_empty())
        );
    }

    #[test]
    fn counts_checks_as_pending_until_every_one_a_review_bots_included_has_finished() {
        let pending = |checks: Vec<Check>, from: &Watch| {
            evaluate(from, &Detail { checks, ..detail() }, NO_REMARKS, 0)
                .next
                .checks_pending
        };
        let ci = check("test", CheckStatus::Success).required();
        assert_eq!(
            pending(
                vec![ci.clone(), check("CodeRabbit", CheckStatus::Pending)],
                &watch()
            ),
            Some(true)
        );
        assert_eq!(
            pending(
                vec![ci.clone(), check("CodeRabbit", CheckStatus::Success)],
                &watch()
            ),
            Some(false)
        );
        assert_eq!(
            pending(
                vec![ci.clone(), check("CodeRabbit", CheckStatus::Failure)],
                &watch()
            ),
            Some(false)
        );
        assert_eq!(
            pending(
                vec![ci, check("deploy", CheckStatus::ActionRequired)],
                &watch()
            ),
            Some(false)
        );
        // A failed read keeps the last answer, as does one with only an
        // unreadable approval check.
        assert_eq!(
            pending(
                Vec::new(),
                &Watch {
                    head_sha: Some("aaaaaaaaaa".into()),
                    checks_pending: Some(false),
                    ..watch()
                }
            ),
            Some(false)
        );
        let unreadable = Check::new("approvals", CheckStatus::ActionRequired).placeholder();
        assert_eq!(
            pending(
                vec![unreadable],
                &Watch {
                    head_sha: Some("aaaaaaaaaa".into()),
                    checks_pending: Some(true),
                    ..watch()
                }
            ),
            Some(true)
        );
    }

    #[test]
    fn waits_a_while_for_a_new_heads_checks_on_every_host() {
        let pushed = |checks_always_report: bool| {
            evaluate(
                &Watch {
                    head_sha: Some("aaaaaaaaaa".into()),
                    checks_pending: Some(false),
                    ..watch()
                },
                &Detail {
                    head_sha: Some("bbbbbbbbbb".into()),
                    checks_always_report: capabilities(checks_always_report),
                    checks: Vec::new(),
                    ..detail()
                },
                NO_REMARKS,
                0,
            )
            .next
            .checks_pending
        };
        assert_eq!(pushed(true), Some(true));
        assert_eq!(pushed(false), Some(true));

        // Once the no-checks window passes, the watch stops waiting for them.
        for checks_always_report in [true, false] {
            let mut current = Watch {
                head_sha: Some("aaaaaaaaaa".into()),
                ..watch()
            };
            for minute in [0, 20] {
                current = evaluate(
                    &current,
                    &Detail {
                        head_sha: Some("bbbbbbbbbb".into()),
                        checks_always_report: capabilities(checks_always_report),
                        checks: Vec::new(),
                        ..detail()
                    },
                    NO_REMARKS,
                    minute * 60_000,
                )
                .next;
            }
            assert_eq!(current.checks_pending, Some(false));
        }

        // A watch with no answer waits on an empty list only while the
        // no-checks clock runs.
        let unanswered = |checks_awaited_since: Option<i64>| {
            evaluate(
                &Watch {
                    head_sha: Some("bbbbbbbbbb".into()),
                    checks_awaited_since,
                    ..watch()
                },
                &Detail {
                    head_sha: Some("bbbbbbbbbb".into()),
                    checks: Vec::new(),
                    ..detail()
                },
                NO_REMARKS,
                0,
            )
            .next
            .checks_pending
        };
        assert_eq!(unanswered(None), Some(false));
        assert_eq!(unanswered(Some(at(STARTED))), Some(true));
    }

    #[test]
    fn watch_end_message_names_the_merge_and_the_close() {
        assert_eq!(
            watch_end_message(
                7,
                "https://github.com/o/r/pull/7",
                SnapshotState::Merged,
                "main"
            ),
            "Pull request #7 (https://github.com/o/r/pull/7) was merged into main, so Atrium stopped watching it."
        );
        assert_eq!(
            watch_end_message(
                7,
                "https://github.com/o/r/pull/7",
                SnapshotState::Closed,
                "main"
            ),
            "Pull request #7 (https://github.com/o/r/pull/7) was closed without merging, so Atrium stopped watching it. If it is reopened, Atrium watches it again."
        );
    }

    fn snapshot(state: SnapshotState, updated: i64) -> Snapshot {
        Snapshot {
            state,
            updated_at: Some(updated),
            merged_at: None,
            closed_at: None,
        }
    }

    #[test]
    fn cadence_reads_new_and_active_pull_requests_every_sweep() {
        let now = 1_000_000_000;
        let state = SyncState::default();
        let active = snapshot(SnapshotState::Open, now - QUIET_AFTER_MS + 1);
        assert!(sync_due(&state, Some(&active), true, now));
        // No snapshot yet: read at once.
        assert!(sync_due(&state, None, true, now));
    }

    #[test]
    fn cadence_reads_quiet_open_pull_requests_on_the_quiet_interval() {
        let now = 1_000_000_000;
        let quiet = snapshot(SnapshotState::Open, now - QUIET_AFTER_MS);
        let synced = SyncState {
            last_synced: Some(now - QUIET_SYNC_INTERVAL_MS + 1),
            ..SyncState::default()
        };
        assert!(!sync_due(&synced, Some(&quiet), true, now));
        let synced = SyncState {
            last_synced: Some(now - QUIET_SYNC_INTERVAL_MS),
            ..SyncState::default()
        };
        assert!(sync_due(&synced, Some(&quiet), true, now));
    }

    #[test]
    fn cadence_stops_reading_merged_and_old_closed_pull_requests() {
        let now = 1_000_000_000;
        let merged = Snapshot {
            state: SnapshotState::Merged,
            updated_at: Some(now),
            merged_at: Some(now),
            closed_at: None,
        };
        let closed = Snapshot {
            state: SnapshotState::Closed,
            updated_at: Some(now),
            merged_at: None,
            closed_at: Some(now - CLOSED_FINAL_AFTER_MS),
        };
        assert!(!sync_due(&SyncState::default(), Some(&merged), true, now));
        assert!(!sync_due(&SyncState::default(), Some(&closed), true, now));
        // A closed pull request inside the window is still read slowly.
        let recent = Snapshot {
            closed_at: Some(now - CLOSED_FINAL_AFTER_MS + 1),
            ..closed
        };
        assert!(sync_due(
            &SyncState {
                last_synced: Some(now - SLOW_SYNC_INTERVAL_MS),
                ..SyncState::default()
            },
            Some(&recent),
            true,
            now
        ));
    }

    #[test]
    fn cadence_backs_off_after_failed_reads_and_recovers() {
        let now = 1_000_000_000;
        let mut state = SyncState::default();
        state.failed(now);
        assert_eq!(state.retry_at, Some(now + FAILED_READ_BACKOFF_MS));
        assert!(!sync_due(
            &state,
            None,
            true,
            now + FAILED_READ_BACKOFF_MS - 1
        ));
        assert!(sync_due(&state, None, true, now + FAILED_READ_BACKOFF_MS));
        state.failed(now);
        assert_eq!(state.retry_at, Some(now + 2 * FAILED_READ_BACKOFF_MS));
        for _ in 0..10 {
            state.failed(now);
        }
        assert_eq!(state.backoff, SLOW_SYNC_INTERVAL_MS);
        state.succeeded(now);
        assert_eq!(state.backoff, 0);
        assert_eq!(state.retry_at, None);
        assert_eq!(state.last_synced, Some(now));
    }

    #[test]
    fn cadence_discovers_a_new_pull_request_every_two_minutes() {
        let now = 1_000_000_000;
        let mut state = SyncState::default();
        state.saw_no_pr(now);
        assert!(state.no_pr);
        assert!(!sync_due(
            &state,
            None,
            true,
            now + NO_PR_SYNC_INTERVAL_MS - 1
        ));
        assert!(sync_due(&state, None, true, now + NO_PR_SYNC_INTERVAL_MS));
        state.succeeded(now);
        assert!(!state.no_pr);
    }

    #[test]
    fn cadence_retries_a_missing_host_on_the_slow_interval_and_honours_requests() {
        let now = 1_000_000_000;
        let mut state = SyncState::default();
        state.missing(now);
        assert!(!sync_due(&state, None, true, now));
        assert!(sync_due(&state, None, true, now + SLOW_SYNC_INTERVAL_MS));
        // A requested refresh reads even a final snapshot.
        let merged = Snapshot {
            state: SnapshotState::Merged,
            updated_at: Some(now),
            merged_at: Some(now),
            closed_at: None,
        };
        state.request();
        assert!(sync_due(&state, Some(&merged), true, now));
        // A retired stream is not read.
        let fresh = SyncState::default();
        assert!(!sync_due(&fresh, None, false, now));
    }

    #[test]
    fn snippet_collapses_whitespace_and_strips_hidden_comments() {
        assert_eq!(snippet("a\n\n  b\t c"), "a b c");
        assert_eq!(
            snippet("<!-- state -->Keep <b>this</b>"),
            "Keep <b>this</b>"
        );
        assert_eq!(snippet("  trimmed  "), "trimmed");
    }
}
