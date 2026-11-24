//! GPUI-free decisions for the app reading engine projections and contracts.
//!
//! Item 30: the everyday UI is driven by what the engine declares, not by
//! names baked into the app. Everything here takes `now` as an argument and is
//! tested without a window, so the UI layer only draws the result.

use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};

use atrium_core::{Capabilities, RateLimits};
use atrium_engine::contracts::ProviderCapabilities;
use jiff::{Timestamp, civil::Date, tz::TimeZone};

/// The agent icon a chat or pill shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMark {
    Codex,
    Claude,
    Antigravity,
    DeepSeek,
    Other,
}

/// The agent part of a backend id (`claude@studio` → `claude`).
pub fn backend_agent(backend: &str) -> &str {
    backend.split('@').next().unwrap_or(backend)
}

pub fn agent_mark(backend: &str) -> AgentMark {
    match backend_agent(backend) {
        "codex" => AgentMark::Codex,
        "claude" => AgentMark::Claude,
        "antigravity" => AgentMark::Antigravity,
        "deepseek" => AgentMark::DeepSeek,
        _ => AgentMark::Other,
    }
}

/// Which controls a session can honestly show.
///
/// The app's [`Capabilities`] and the engine's ported
/// [`ProviderCapabilities`] both map into this one record, so the composer
/// shows the same set whichever path started the session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Controls {
    pub images: bool,
    pub model: bool,
    pub effort: bool,
    pub speed: bool,
    pub plan: bool,
    pub approval: bool,
    pub queue: bool,
    pub steer: bool,
    pub interrupt: bool,
    pub usage_limits: bool,
    pub plan_cards: bool,
    pub todo: bool,
}

pub fn controls_from(caps: &Capabilities) -> Controls {
    Controls {
        images: caps.images,
        model: caps.model_choice,
        effort: caps.effort_choice,
        speed: caps.service_tiers,
        plan: caps.plan_mode,
        approval: caps.has_approval_choice(),
        queue: caps.queue,
        steer: caps.steer,
        interrupt: caps.interrupt,
        usage_limits: caps.usage_limits,
        plan_cards: caps.plan_mode,
        todo: false,
    }
}

/// T3's capability record has no per-mode approval list: the booleans say
/// whether approvals exist at all. Everything else maps one to one.
pub fn controls_from_engine(caps: &ProviderCapabilities) -> Controls {
    let approvals = caps.approvals.supports_command_approval
        || caps.approvals.supports_file_read_approval
        || caps.approvals.supports_file_change_approval
        || caps.approvals.supports_apply_patch_approval;
    Controls {
        // T3's capability record has no attachment field; the ported Codex
        // adapter accepts images, so keep that control.
        images: true,
        model: caps.sessions.supports_model_switch_in_session,
        effort: caps.sessions.supports_model_switch_in_session,
        speed: caps.sessions.supports_model_switch_in_session,
        plan: caps.planning.emits_plan_updated || caps.planning.emits_proposed_plan,
        approval: approvals,
        queue: caps.turns.supports_queued_messages,
        steer: caps.turns.supports_active_steering,
        interrupt: caps.turns.supports_interrupt,
        usage_limits: false,
        plan_cards: caps.planning.emits_plan_updated,
        todo: caps.planning.emits_todo_list,
    }
}

/// Whether an approval mode is the default and can stay out of the pill.
pub fn approval_is_default(label: &str) -> bool {
    let normalized: String = label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(normalized.as_str(), "auto" | "default" | "ask")
}

/// What the model · effort · speed pill shows, and what its overflow holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PillPlan {
    pub show_model: bool,
    pub show_effort: bool,
    pub show_speed: bool,
    pub show_approval: bool,
    pub overflow_plan: bool,
    pub overflow_approval: bool,
}

pub fn pill_plan(
    controls: &Controls,
    model: Option<&str>,
    effort: Option<&str>,
    speed: Option<&str>,
    approval: Option<&str>,
) -> PillPlan {
    PillPlan {
        show_model: controls.model && model.is_some_and(|m| !m.trim().is_empty()),
        show_effort: controls.effort && effort.is_some_and(|e| !e.trim().is_empty()),
        show_speed: controls.speed && speed.is_some(),
        show_approval: controls.approval && approval.is_some_and(|a| !approval_is_default(a)),
        overflow_plan: controls.plan,
        overflow_approval: controls.approval,
    }
}

/// How a message reaches the agent when the chat is (or is not) running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    Start,
    Steer,
    Queue,
}

pub fn send_mode(controls: &Controls, running: bool) -> SendMode {
    match (running, controls.steer, controls.queue) {
        (false, ..) => SendMode::Start,
        (true, true, _) => SendMode::Steer,
        (true, false, true) => SendMode::Queue,
        // Nothing running to steer and no queue: let the provider refuse it.
        (true, false, false) => SendMode::Start,
    }
}

/// The next value in a cycle of choices, with `None` meaning "agent default".
pub fn cycle_choice(len: usize, current: Option<usize>) -> Option<usize> {
    match (len, current) {
        (0, _) => None,
        (_, None) => Some(0),
        (len, Some(ix)) if ix + 1 < len => Some(ix + 1),
        _ => None,
    }
}

/// One usage window from the engine's report (item 26 fills it).
#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    pub used_percent: f64,
    /// Unix seconds, when the provider says.
    pub resets_at: Option<i64>,
}

/// Item 26's typed seam: real numbers only. `None` means nothing to show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageReport {
    pub provider: String,
    pub windows: Vec<UsageWindow>,
    pub reached: bool,
    /// Remaining context, when the provider reports it.
    pub context_left_percent: Option<f64>,
}

/// What the banner says; the UI translates it.
#[derive(Clone, Debug, PartialEq)]
pub enum UsageBanner {
    Near {
        window: String,
        used_percent: f64,
        resets_in: Option<Duration>,
    },
    Reached {
        window: String,
        resets_in: Option<Duration>,
    },
}

pub const USAGE_BANNER_PERCENT: f64 = 80.;

/// Fills the report's context share from a real token report.
///
/// Item 26's typed read API (`Workspace::usage_overview` / `context_window`)
/// is not on this branch. Until it lands, this keeps the [`UsageReport`]
/// seam honest: the percentage comes only from numbers the agent sent, and
/// stays absent when the report has no window.
pub fn usage_report_with_context(
    mut report: UsageReport,
    usage: Option<&atrium_core::TokenUsage>,
) -> UsageReport {
    if let Some(usage) = usage
        && let Some(share) = usage.context_share()
    {
        report.context_left_percent = Some(((1.0 - share) * 100.0).clamp(0., 100.));
    }
    report
}

/// Builds the seam's report from the live limit reading; no numbers invented.
pub fn usage_report_from_limits(provider: impl Into<String>, limits: &RateLimits) -> UsageReport {
    let windows = [limits.primary, limits.secondary]
        .into_iter()
        .flatten()
        .map(|window| UsageWindow {
            label: window.label(),
            used_percent: window.used_percent,
            resets_at: window.resets_at,
        })
        .collect();
    UsageReport {
        provider: provider.into(),
        windows,
        reached: limits.reached,
        context_left_percent: None,
    }
}

fn resets_in(resets_at: Option<i64>, now: Timestamp) -> Option<Duration> {
    let at = resets_at?;
    Some(Duration::from_secs(
        at.saturating_sub(now.as_second()).max(0) as u64,
    ))
}

/// The fullest window drives the banner; it shows near a limit or once hit.
pub fn usage_banner(report: Option<&UsageReport>, now: Timestamp) -> Option<UsageBanner> {
    let report = report?;
    let window = report
        .windows
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))?;
    let resets_in = resets_in(window.resets_at, now);
    if report.reached || window.used_percent >= 100. {
        return Some(UsageBanner::Reached {
            window: window.label.clone(),
            resets_in,
        });
    }
    (window.used_percent >= USAGE_BANNER_PERCENT).then(|| UsageBanner::Near {
        window: window.label.clone(),
        used_percent: window.used_percent,
        resets_in,
    })
}

/// What one engine projection says about a chat in the list.
///
/// The store primitives are `projection_*` rows (item 21). The app maps them
/// here rather than guessing from the app's own transcripts, so a thread that
/// runs on another device reads the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedChat {
    pub activity: ChatActivity,
    pub archived: bool,
    pub at_sec: i64,
}

/// `running` and the states T3 keeps while a turn is live. A pending approval
/// outranks a running turn: the chat waits on the user.
#[allow(dead_code)]
pub fn chat_from_projection(
    thread: &atrium_engine::store::ThreadProjection,
    turns: &[atrium_engine::store::TurnProjection],
    pending_approvals: usize,
) -> ProjectedChat {
    let live = turns.iter().any(|turn| {
        matches!(
            turn.state.as_str(),
            "running" | "started" | "streaming" | "waiting"
        )
    });
    ProjectedChat {
        activity: if pending_approvals > 0 {
            ChatActivity::Waiting
        } else if live {
            ChatActivity::Working
        } else {
            ChatActivity::Idle
        },
        archived: thread.archived_at.is_some(),
        at_sec: thread
            .updated_at
            .parse::<Timestamp>()
            .map(|at| at.as_second())
            .unwrap_or(0),
    }
}

/// What a chat is doing, for the list's sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatActivity {
    Idle,
    Working,
    Waiting,
    Unread(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatItem {
    pub at_sec: i64,
    pub pinned: bool,
    pub archived: bool,
    pub activity: ChatActivity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatSection {
    Pinned,
    Working,
    Day(Date),
    Archived,
}

fn date_of(item: &ChatItem, zone: &TimeZone) -> Date {
    Timestamp::from_second(item.at_sec)
        .map(|at| at.to_zoned(zone.clone()).date())
        .unwrap_or(Date::MIN)
}

/// Pinned, then working (waiting on the user first), then recent by day,
/// then archived. `now` only reaches the list as `today`, as the day names do.
pub fn chat_sections(
    items: &[ChatItem],
    today: Date,
    zone: &TimeZone,
) -> Vec<(ChatSection, Vec<usize>)> {
    let mut pinned: Vec<usize> = (0..items.len())
        .filter(|ix| items[*ix].pinned && !items[*ix].archived)
        .collect();
    pinned.sort_by_key(|ix| std::cmp::Reverse(items[*ix].at_sec));

    let mut working: Vec<usize> = (0..items.len())
        .filter(|ix| {
            let item = &items[*ix];
            !item.pinned
                && !item.archived
                && matches!(item.activity, ChatActivity::Working | ChatActivity::Waiting)
        })
        .collect();
    working.sort_by(|a, b| {
        let waiting = |ix: usize| items[ix].activity == ChatActivity::Waiting;
        waiting(*b)
            .cmp(&waiting(*a))
            .then_with(|| items[*b].at_sec.cmp(&items[*a].at_sec))
    });

    let mut recent: Vec<usize> = (0..items.len())
        .filter(|ix| {
            let item = &items[*ix];
            !item.pinned
                && !item.archived
                && !matches!(item.activity, ChatActivity::Working | ChatActivity::Waiting)
        })
        .collect();
    recent.sort_by_key(|ix| std::cmp::Reverse(items[*ix].at_sec));

    let mut archived: Vec<usize> = (0..items.len()).filter(|ix| items[*ix].archived).collect();
    archived.sort_by_key(|ix| std::cmp::Reverse(items[*ix].at_sec));

    let mut sections = Vec::new();
    if !pinned.is_empty() {
        sections.push((ChatSection::Pinned, pinned));
    }
    if !working.is_empty() {
        sections.push((ChatSection::Working, working));
    }
    for ix in recent {
        let day = date_of(&items[ix], zone);
        match sections.last_mut() {
            Some((ChatSection::Day(last), rows)) if *last == day => rows.push(ix),
            _ => sections.push((ChatSection::Day(day), vec![ix])),
        }
    }
    let _ = today;
    if !archived.is_empty() {
        sections.push((ChatSection::Archived, archived));
    }
    sections
}

/// Keeps one draft per chat while the user moves between them.
pub fn swap_draft(
    drafts: &mut HashMap<String, String>,
    from: &str,
    to: &str,
    text: &str,
) -> String {
    if from == to {
        return text.to_string();
    }
    if text.trim().is_empty() {
        drafts.remove(from);
    } else {
        drafts.insert(from.to_string(), text.to_string());
    }
    drafts.get(to).cloned().unwrap_or_default()
}

/// Recalls a sent prompt, newest first. `None` is "not recalling".
pub fn recall_prompt(
    history: &[String],
    index: Option<usize>,
    older: bool,
) -> (Option<usize>, Option<String>) {
    if history.is_empty() {
        return (None, None);
    }
    if older {
        let next = index.map_or(0, |ix| (ix + 1).min(history.len() - 1));
        (Some(next), Some(history[next].clone()))
    } else {
        match index {
            Some(0) | None => (None, None),
            Some(ix) => (Some(ix - 1), Some(history[ix - 1].clone())),
        }
    }
}

/// One side of a side-by-side diff row. `source` indexes the unified lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideCell {
    pub number: Option<u32>,
    pub text: String,
    pub source: usize,
    pub kind: DiffKind,
}

/// A two-column diff row, a hunk header, or one context line on both sides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideRow {
    pub hunk: Option<String>,
    pub left: Option<SideCell>,
    pub right: Option<SideCell>,
}

/// Changed files and tool rows share these kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Hunk,
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// Pairs removed and added lines in each run into two columns.
pub fn side_by_side(lines: &[DiffLine]) -> Vec<SideRow> {
    let mut out = Vec::new();
    let mut ix = 0;
    while ix < lines.len() {
        match lines[ix].kind {
            DiffKind::Hunk => {
                out.push(SideRow {
                    hunk: Some(lines[ix].text.clone()),
                    left: None,
                    right: None,
                });
                ix += 1;
            }
            DiffKind::Context => {
                let line = &lines[ix];
                out.push(SideRow {
                    hunk: None,
                    left: Some(SideCell {
                        number: line.old,
                        text: line.text.clone(),
                        source: ix,
                        kind: DiffKind::Context,
                    }),
                    right: Some(SideCell {
                        number: line.new,
                        text: line.text.clone(),
                        source: ix,
                        kind: DiffKind::Context,
                    }),
                });
                ix += 1;
            }
            DiffKind::Removed | DiffKind::Added => {
                let start = ix;
                while ix < lines.len()
                    && matches!(lines[ix].kind, DiffKind::Removed | DiffKind::Added)
                {
                    ix += 1;
                }
                let block = &lines[start..ix];
                let removed: Vec<&DiffLine> = block
                    .iter()
                    .filter(|l| l.kind == DiffKind::Removed)
                    .collect();
                let added: Vec<&DiffLine> =
                    block.iter().filter(|l| l.kind == DiffKind::Added).collect();
                let source = |line: &DiffLine| {
                    start
                        + block
                            .iter()
                            .position(|l| std::ptr::eq(l, line))
                            .unwrap_or(0)
                };
                for row in 0..removed.len().max(added.len()) {
                    out.push(SideRow {
                        hunk: None,
                        left: removed.get(row).map(|line| SideCell {
                            number: line.old,
                            text: line.text.clone(),
                            source: source(line),
                            kind: DiffKind::Removed,
                        }),
                        right: added.get(row).map(|line| SideCell {
                            number: line.new,
                            text: line.text.clone(),
                            source: source(line),
                            kind: DiffKind::Added,
                        }),
                    });
                }
            }
        }
    }
    out
}

/// A tool call's one-line summary: a known argument, else its first line.
pub fn tool_summary(arguments: &str, max: usize) -> String {
    let from_json = serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| {
            [
                "summary",
                "description",
                "command",
                "file_path",
                "path",
                "query",
                "pattern",
                "url",
            ]
            .into_iter()
            .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
            .map(str::to_string)
        });
    let line = from_json.unwrap_or_else(|| {
        arguments
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_string()
    });
    let line = line.trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A changed file's full path: relative paths join the chat's folder.
pub fn resolve_file_path(path: &str, cwd: &str) -> String {
    let path_ref = Path::new(path);
    if path_ref.is_absolute() || path.starts_with('~') {
        return path.to_string();
    }
    Path::new(cwd).join(path_ref).display().to_string()
}

/// Whole seconds since a step started; the caller formats them.
pub fn live_seconds(start: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(start).as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_engine::contracts::capabilities::{
        ApprovalCapabilities, PlanningCapabilities, ProviderCapabilities, SessionCapabilities,
        TurnCapabilities,
    };
    use jiff::civil::date;

    fn item(at_sec: i64, pinned: bool, archived: bool, activity: ChatActivity) -> ChatItem {
        ChatItem {
            at_sec,
            pinned,
            archived,
            activity,
        }
    }

    #[test]
    fn capabilities_from_either_record_hide_what_is_not_there() {
        let acp = controls_from(&Capabilities::acp());
        assert!(!acp.model && !acp.effort && !acp.speed && !acp.plan);
        assert!(acp.queue && !acp.steer);
        assert!(!acp.approval);

        let codex = controls_from(&Capabilities::codex());
        assert!(codex.model && codex.effort && codex.speed && codex.plan);
        assert!(codex.steer && codex.approval);

        let engine = controls_from_engine(&ProviderCapabilities {
            sessions: SessionCapabilities {
                supports_model_switch_in_session: true,
                ..Default::default()
            },
            turns: TurnCapabilities {
                supports_queued_messages: true,
                supports_active_steering: false,
                supports_interrupt: true,
                ..Default::default()
            },
            approvals: ApprovalCapabilities {
                supports_command_approval: true,
                ..Default::default()
            },
            planning: PlanningCapabilities {
                emits_plan_updated: true,
                emits_todo_list: true,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(engine.model && engine.plan && engine.approval && engine.todo);
        assert!(engine.queue && !engine.steer && engine.interrupt);
    }

    #[test]
    fn a_running_turn_is_steered_or_queued_by_what_the_agent_declares() {
        let codex = controls_from(&Capabilities::codex());
        assert_eq!(send_mode(&codex, false), SendMode::Start);
        assert_eq!(send_mode(&codex, true), SendMode::Steer);
        let acp = controls_from(&Capabilities::acp());
        assert_eq!(send_mode(&acp, true), SendMode::Queue);
        let plain = controls_from(&Capabilities {
            queue: false,
            ..Capabilities::NONE
        });
        assert_eq!(send_mode(&plain, true), SendMode::Start);
    }

    #[test]
    fn the_pill_hides_a_default_approval_and_moves_it_to_the_overflow() {
        let controls = controls_from(&Capabilities::codex());
        let plan = pill_plan(
            &controls,
            Some("GPT-6.2 Sol"),
            Some("high"),
            Some("fast"),
            Some("Auto"),
        );
        assert!(plan.show_model && plan.show_effort && plan.show_speed);
        assert!(!plan.show_approval);
        assert!(plan.overflow_approval && plan.overflow_plan);

        let full = pill_plan(
            &controls,
            Some("GPT-6.2 Sol"),
            Some("high"),
            None,
            Some("Full access"),
        );
        assert!(full.show_approval && !full.show_speed);
    }

    #[test]
    fn default_approval_names_are_recognised() {
        for name in ["Auto", "auto", "Default", " ask "] {
            assert!(approval_is_default(name), "{name}");
        }
        assert!(!approval_is_default("Full access"));
        assert!(!approval_is_default("Read only"));
    }

    #[test]
    fn cycles_go_from_default_through_the_choices_and_back() {
        assert_eq!(cycle_choice(0, None), None);
        assert_eq!(cycle_choice(3, None), Some(0));
        assert_eq!(cycle_choice(3, Some(0)), Some(1));
        assert_eq!(cycle_choice(3, Some(2)), None);
    }

    #[test]
    fn the_usage_banner_shows_only_real_readings_near_the_limit() {
        let now = Timestamp::from_second(1_000).unwrap();
        let report = UsageReport {
            provider: "codex".into(),
            windows: vec![
                UsageWindow {
                    label: "5-hour limit".into(),
                    used_percent: 40.0,
                    resets_at: Some(1_600),
                },
                UsageWindow {
                    label: "Weekly limit".into(),
                    used_percent: 85.0,
                    resets_at: Some(2_000),
                },
            ],
            reached: false,
            context_left_percent: None,
        };
        let banner = usage_banner(Some(&report), now).expect("weekly is near");
        assert_eq!(
            banner,
            UsageBanner::Near {
                window: "Weekly limit".into(),
                used_percent: 85.0,
                resets_in: Some(Duration::from_secs(1_000)),
            }
        );
        let calm = UsageReport {
            windows: vec![UsageWindow {
                label: "Weekly limit".into(),
                used_percent: 20.0,
                resets_at: None,
            }],
            ..report.clone()
        };
        assert!(usage_banner(Some(&calm), now).is_none());
        let reached = UsageReport {
            reached: true,
            ..report
        };
        assert!(matches!(
            usage_banner(Some(&reached), now),
            Some(UsageBanner::Reached { .. })
        ));
        assert!(usage_banner(None, now).is_none());
    }

    #[test]
    fn context_is_filled_only_from_a_real_token_report() {
        use atrium_core::{TokenCounts, TokenUsage};
        let report = UsageReport {
            provider: "codex".into(),
            windows: Vec::new(),
            reached: false,
            context_left_percent: None,
        };
        let usage = TokenUsage {
            total: TokenCounts::default(),
            last: TokenCounts::default(),
            context_used: Some(25_000),
            context_window: Some(100_000),
            cost: None,
        };
        let filled = usage_report_with_context(report.clone(), Some(&usage));
        assert_eq!(filled.context_left_percent, Some(75.0));
        assert_eq!(filled.windows, report.windows);
        assert_eq!(
            usage_report_with_context(report, None).context_left_percent,
            None
        );
        let unknown = TokenUsage::default();
        assert_eq!(
            usage_report_with_context(
                UsageReport {
                    context_left_percent: None,
                    ..UsageReport::default()
                },
                Some(&unknown)
            )
            .context_left_percent,
            None
        );
    }

    #[test]
    fn limits_become_a_report_without_inventing_numbers() {
        let limits = RateLimits {
            primary: Some(atrium_core::RateWindow {
                used_percent: 12.0,
                window_mins: Some(300),
                resets_at: Some(42),
            }),
            reached: false,
            ..Default::default()
        };
        let report = usage_report_from_limits("codex", &limits);
        assert_eq!(report.provider, "codex");
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].used_percent, 12.0);
        assert_eq!(report.windows[0].label, "5-hour limit");
        assert!(!report.reached);
    }

    #[test]
    fn the_chat_list_reads_pinned_working_days_then_archived() {
        let day = date(2026, 11, 24);
        let at = |day: i8| {
            date(2026, 11, day)
                .at(12, 0, 0, 0)
                .to_zoned(TimeZone::UTC)
                .unwrap()
                .timestamp()
                .as_second()
        };
        let items = vec![
            item(at(24), true, false, ChatActivity::Idle),
            item(at(23), false, false, ChatActivity::Waiting),
            item(at(24), false, false, ChatActivity::Working),
            item(at(24), false, false, ChatActivity::Unread(2)),
            item(at(23), false, false, ChatActivity::Idle),
            item(at(20), false, true, ChatActivity::Idle),
        ];
        let sections = chat_sections(&items, day, &TimeZone::UTC);
        assert_eq!(
            sections,
            vec![
                (ChatSection::Pinned, vec![0]),
                (ChatSection::Working, vec![1, 2]),
                (ChatSection::Day(day), vec![3]),
                (ChatSection::Day(date(2026, 11, 23)), vec![4]),
                (ChatSection::Archived, vec![5]),
            ]
        );
    }

    #[test]
    fn engine_projections_say_working_waiting_and_archived() {
        use atrium_engine::store::{ThreadProjection, TurnProjection};
        let thread = |updated: &str, archived: Option<&str>| ThreadProjection {
            thread_id: "t".into(),
            project_id: "p".into(),
            title: "Chat".into(),
            default_provider: "codex".into(),
            provider_instance_id: "codex".into(),
            runtime_mode: "default".into(),
            interaction_mode: "default".into(),
            active_provider_thread_id: None,
            created_at: "2026-11-24T10:00:00Z".into(),
            updated_at: updated.into(),
            archived_at: archived.map(str::to_string),
            deleted_at: None,
            payload_json: "{}".into(),
        };
        let turn = |state: &str| TurnProjection {
            thread_id: "t".into(),
            turn_id: Some("turn".into()),
            pending_message_id: None,
            assistant_message_id: None,
            state: state.into(),
            requested_at: "2026-11-24T10:00:00Z".into(),
            started_at: None,
            completed_at: None,
            checkpoint_turn_count: None,
            checkpoint_ref: None,
            checkpoint_status: None,
            checkpoint_files_json: "[]".into(),
        };
        let working =
            chat_from_projection(&thread("2026-11-24T10:00:00Z", None), &[turn("running")], 0);
        assert_eq!(working.activity, ChatActivity::Working);
        assert!(!working.archived);
        assert!(working.at_sec > 0);
        let waiting =
            chat_from_projection(&thread("2026-11-24T10:00:00Z", None), &[turn("running")], 1);
        assert_eq!(waiting.activity, ChatActivity::Waiting);
        let done = chat_from_projection(
            &thread("2026-11-24T10:00:00Z", Some("2026-11-24T11:00:00Z")),
            &[turn("completed")],
            0,
        );
        assert_eq!(done.activity, ChatActivity::Idle);
        assert!(done.archived);
    }

    #[test]
    fn drafts_follow_their_chat_and_empty_ones_are_forgotten() {
        let mut drafts = HashMap::new();
        assert_eq!(swap_draft(&mut drafts, "", "a", "hello"), "");
        assert_eq!(swap_draft(&mut drafts, "a", "b", "hello"), "");
        assert_eq!(drafts.get("a").map(String::as_str), Some("hello"));
        assert_eq!(swap_draft(&mut drafts, "b", "a", "  "), "hello");
        assert!(!drafts.contains_key("b"));
    }

    #[test]
    fn prompt_recall_walks_backwards_and_then_clears() {
        let history = vec!["newest".to_string(), "older".to_string()];
        assert_eq!(
            recall_prompt(&history, None, true),
            (Some(0), Some("newest".into()))
        );
        assert_eq!(
            recall_prompt(&history, Some(0), true),
            (Some(1), Some("older".into()))
        );
        assert_eq!(
            recall_prompt(&history, Some(1), true),
            (Some(1), Some("older".into()))
        );
        assert_eq!(
            recall_prompt(&history, Some(1), false),
            (Some(0), Some("newest".into()))
        );
        assert_eq!(recall_prompt(&history, Some(0), false), (None, None));
        assert_eq!(recall_prompt(&[], None, true), (None, None));
    }

    fn line(kind: DiffKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old,
            new,
            text: text.into(),
        }
    }

    #[test]
    fn side_by_side_pairs_removed_and_added_lines() {
        let lines = vec![
            line(DiffKind::Hunk, None, None, "@@ -1 +1 @@"),
            line(DiffKind::Context, Some(1), Some(1), "keep"),
            line(DiffKind::Removed, Some(2), None, "old a"),
            line(DiffKind::Removed, Some(3), None, "old b"),
            line(DiffKind::Added, None, Some(2), "new a"),
            line(DiffKind::Context, Some(4), Some(3), "tail"),
        ];
        let rows = side_by_side(&lines);
        assert!(rows[0].hunk.is_some());
        assert_eq!(rows[1].left.as_ref().unwrap().text, "keep");
        assert_eq!(rows[1].right.as_ref().unwrap().text, "keep");
        assert_eq!(rows[2].left.as_ref().unwrap().text, "old a");
        assert_eq!(rows[2].right.as_ref().unwrap().text, "new a");
        assert_eq!(rows[3].left.as_ref().unwrap().text, "old b");
        assert!(rows[3].right.is_none());
        assert_eq!(rows[4].left.as_ref().unwrap().number, Some(4));
        assert_eq!(rows[2].left.as_ref().unwrap().source, 2);
        assert_eq!(rows[2].right.as_ref().unwrap().source, 4);
    }

    #[test]
    fn a_tool_summary_uses_a_known_argument_or_the_first_line() {
        assert_eq!(
            tool_summary(r#"{"command":"cargo test -p atrium-app"}"#, 40),
            "cargo test -p atrium-app"
        );
        assert_eq!(
            tool_summary("\n  reading src/main.rs\nmore", 40),
            "reading src/main.rs"
        );
        assert_eq!(tool_summary("abcdefghij", 5), "abcd…");
        assert_eq!(tool_summary("   ", 5), "");
    }

    #[test]
    fn relative_file_paths_join_the_chat_folder() {
        assert_eq!(
            resolve_file_path("src/a.rs", "/work/app"),
            "/work/app/src/a.rs"
        );
        assert_eq!(resolve_file_path("/etc/hosts", "/work/app"), "/etc/hosts");
        assert_eq!(resolve_file_path("~/notes", "/work/app"), "~/notes");
    }

    #[test]
    fn live_seconds_counts_forward() {
        let start = Instant::now();
        assert_eq!(live_seconds(start, start), 0);
        assert_eq!(live_seconds(start, start + Duration::from_secs(65)), 65);
    }
}
