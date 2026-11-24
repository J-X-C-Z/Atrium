//! Runs of work steps (commands, file edits, tool calls) folded into one line
//! that opens to a line per step, and from there to the full card.

use atrium_core::{ChangeKind, Entry, EntryKind, FileChange, Item, ItemStatus};
use ely_gpui_component::{
    motion::{Spinner, SpinnerStyle},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, IconSize, TextSize},
};
use gpui::{
    AnyElement, App, Entity, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};

use crate::i18n::{chinese, t, tf};
use crate::workspace::Workspace;

/// Entries `start..end` of one turn, folded together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
}

fn is_step(item: &Item) -> bool {
    matches!(
        item,
        Item::Command { .. }
            | Item::FileChange { .. }
            | Item::Tool { .. }
            | Item::WebSearch { .. }
            | Item::SubAgent { .. }
    )
}

/// Steps, and the thinking and answered approvals between them, fold together.
fn foldable(entry: &Entry) -> Option<bool> {
    match &entry.kind {
        EntryKind::Item(item) if is_step(item) => Some(true),
        EntryKind::Item(Item::Reasoning { .. }) => Some(false),
        // An approval already answered is part of the work, not a stop in it.
        EntryKind::Approval {
            answer: Some(_), ..
        } => Some(false),
        _ => None,
    }
}

/// The run `ix` belongs to: two steps or more of one turn, with nothing else
/// between them but thinking. Thinking at either end stays outside.
pub fn run_at(entries: &[Entry], ix: usize) -> Option<Run> {
    foldable(entries.get(ix)?)?;
    let turn = &entries[ix].turn_id;
    let same = |e: &Entry| &e.turn_id == turn && foldable(e).is_some();
    let mut start = ix;
    while start > 0 && same(&entries[start - 1]) {
        start -= 1;
    }
    let mut end = ix + 1;
    while end < entries.len() && same(&entries[end]) {
        end += 1;
    }
    while start < end && foldable(&entries[start]) == Some(false) {
        start += 1;
    }
    while end > start && foldable(&entries[end - 1]) == Some(false) {
        end -= 1;
    }
    let steps = entries[start..end]
        .iter()
        .filter(|e| foldable(e) == Some(true))
        .count();
    (steps >= 2 && (start..end).contains(&ix)).then_some(Run { start, end })
}

/// What a shell command mostly does, from its first word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Does {
    Read,
    Search,
    Run,
}

/// The command as typed, without the shell wrapper Codex runs it in.
pub fn bare_command(command: &str) -> &str {
    let mut c = command.trim();
    for wrap in [
        "/bin/bash -lc ",
        "bash -lc ",
        "/bin/sh -c ",
        "sh -c ",
        "zsh -lc ",
    ] {
        if let Some(rest) = c.strip_prefix(wrap) {
            c = rest.trim();
            if let Some(inner) = c
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .or_else(|| c.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
            {
                c = inner;
            }
            break;
        }
    }
    c
}

pub fn does(command: &str) -> Does {
    let c = bare_command(command);
    let c = c.strip_prefix("cd ").map_or(c, |rest| {
        rest.split_once("&&")
            .map_or(rest, |(_, after)| after.trim())
    });
    if c.starts_with("git grep") {
        return Does::Search;
    }
    match c.split_whitespace().next().unwrap_or("") {
        "cat" | "sed" | "head" | "tail" | "nl" | "less" | "bat" | "wc" => Does::Read,
        "rg" | "grep" | "find" | "fd" | "ls" | "tree" => Does::Search,
        _ => Does::Run,
    }
}

/// Lines added and removed by one file's diff.
pub fn diff_counts(change: &FileChange) -> (usize, usize) {
    let lines = super::changes::parse_diff(&change.diff, &change.kind);
    let added = lines
        .iter()
        .filter(|l| l.kind == super::changes::LineKind::Added)
        .count();
    let removed = lines
        .iter()
        .filter(|l| l.kind == super::changes::LineKind::Removed)
        .count();
    (added, removed)
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Tally {
    edited: usize,
    read: usize,
    searched: usize,
    ran: usize,
    tools: usize,
    web: usize,
    agents: usize,
}

fn tally<'a>(items: impl Iterator<Item = &'a Item>) -> Tally {
    let mut n = Tally::default();
    for item in items {
        match item {
            Item::FileChange { changes, .. } => n.edited += changes.len(),
            Item::Command { command, .. } => match does(command) {
                Does::Read => n.read += 1,
                Does::Search => n.searched += 1,
                Does::Run => n.ran += 1,
            },
            Item::Tool { .. } => n.tools += 1,
            Item::WebSearch { .. } => n.web += 1,
            Item::SubAgent { .. } => n.agents += 1,
            _ => {}
        }
    }
    n
}

fn count(n: usize, one: &'static str, many: &'static str) -> String {
    tf(if n == 1 { one } else { many }, &[("n", &n)])
}

/// "Edited 3 files, read 2 files, ran a command".
fn describe(n: &Tally) -> String {
    let mut parts = Vec::new();
    if n.edited > 0 {
        parts.push(count(n.edited, "edited a file", "edited {n} files"));
    }
    if n.read > 0 {
        parts.push(count(n.read, "read a file", "read {n} files"));
    }
    if n.searched > 0 {
        parts.push(count(n.searched, "searched once", "searched {n} times"));
    }
    if n.ran > 0 {
        parts.push(count(n.ran, "ran a command", "ran {n} commands"));
    }
    if n.tools > 0 {
        parts.push(count(n.tools, "used a tool", "used {n} tools"));
    }
    if n.web > 0 {
        parts.push(count(
            n.web,
            "searched the web",
            "searched the web {n} times",
        ));
    }
    if n.agents > 0 {
        parts.push(count(
            n.agents,
            "started a subagent",
            "started {n} subagents",
        ));
    }
    let joined = parts.join(if chinese() { "，" } else { ", " });
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => joined,
    }
}

fn items(entries: &[Entry]) -> impl Iterator<Item = &Item> {
    entries.iter().filter_map(|e| match &e.kind {
        EntryKind::Item(item) => Some(item),
        _ => None,
    })
}

fn working(item: &Item) -> bool {
    match item {
        Item::Command { status, .. }
        | Item::FileChange { status, .. }
        | Item::Tool { status, .. }
        | Item::SubAgent { status, .. } => *status == ItemStatus::InProgress,
        _ => false,
    }
}

/// Whether the run shows its steps: open while they still run, unless the
/// user closed it; shut once done, unless the user opened it.
pub fn is_open(ws: &Workspace, entries: &[Entry], run: Run) -> bool {
    let key = first_id(entries, run);
    let live = entries[run.start..run.end]
        .iter()
        .any(|e| e.live || matches!(&e.kind, EntryKind::Item(i) if working(i)));
    ws.step_folds.get(&key).copied().unwrap_or(live)
}

fn first_id(entries: &[Entry], run: Run) -> String {
    items(&entries[run.start..run.end])
        .next()
        .map(|i| i.id().to_string())
        .unwrap_or_default()
}

/// The line a run folds into.
pub fn header(workspace: &Entity<Workspace>, entries: &[Entry], run: Run, cx: &App) -> AnyElement {
    let ws = workspace.read(cx);
    let open = is_open(ws, entries, run);
    let shown = &entries[run.start..run.end];
    let n = tally(items(shown));
    let busy = items(shown).any(working);
    let colors = cx.theme().colors.clone();
    let key = first_id(entries, run);
    let toggle = workspace.clone();
    let icon = if busy {
        Spinner::new(SharedString::from(format!("run-busy-{key}")))
            .style(SpinnerStyle::Ring)
            .size(IconSize::Sm)
            .into_any_element()
    } else {
        Icon::new(if n.edited > 0 {
            IconName::Pencil
        } else {
            IconName::Terminal
        })
        .size(IconSize::Sm)
        .color(colors.fg_muted)
        .into_any_element()
    };
    div()
        .id(SharedString::from(format!("run-{key}")))
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .cursor_pointer()
        .text_size(cx.theme().text_size(TextSize::Sm))
        .text_color(colors.fg_muted)
        .hover(|s| s.text_color(colors.fg))
        .min_w_0()
        .child(div().flex_none().child(icon))
        .child(div().min_w_0().truncate().child(describe(&n)))
        .child(
            div().flex_none().child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::Xs)
                .color(colors.fg_muted),
            ),
        )
        .on_click(move |_, _, cx| {
            let key = key.clone();
            toggle.update(cx, |ws, cx| {
                let entries = ws
                    .transcript()
                    .map(|t| t.entries().to_vec())
                    .unwrap_or_default();
                let now = is_open(ws, &entries, run);
                ws.step_folds.insert(key, !now);
                cx.notify();
            })
        })
        .into_any_element()
}

/// What one step's line says: a verb and what it acted on.
/// A step's line: its icon, verb, what it acted on, and lines added and removed.
type Line = (IconName, String, String, Option<(usize, usize)>);

fn line_parts(item: &Item) -> Vec<Line> {
    let done = |status: &ItemStatus| *status != ItemStatus::InProgress;
    match item {
        Item::Command {
            command, status, ..
        } => {
            let (icon, verb) = match (does(command), done(status)) {
                (Does::Read, true) => (IconName::BookOpen, t("Read")),
                (Does::Read, false) => (IconName::BookOpen, t("Reading")),
                (Does::Search, true) => (IconName::Search, t("Searched")),
                (Does::Search, false) => (IconName::Search, t("Searching")),
                (Does::Run, true) => (IconName::Terminal, t("Ran")),
                (Does::Run, false) => (IconName::Terminal, t("Running")),
            };
            let what = bare_command(command)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            vec![(icon, verb.into(), what, None)]
        }
        Item::FileChange { changes, .. } => changes
            .iter()
            .map(|c| {
                let verb = match c.kind {
                    ChangeKind::Add => t("Created"),
                    ChangeKind::Delete => t("Deleted"),
                    _ => t("Edited"),
                };
                let icon = match c.kind {
                    ChangeKind::Add => IconName::FilePlus,
                    _ => IconName::Pencil,
                };
                (icon, verb.into(), c.path.clone(), Some(diff_counts(c)))
            })
            .collect(),
        Item::Tool {
            name, arguments, ..
        } => {
            let summary = crate::engine_view::tool_summary(arguments, 80);
            let what = if summary.is_empty() {
                name.clone()
            } else {
                format!("{name}: {summary}")
            };
            vec![(IconName::Wrench, t("Used").into(), what, None)]
        }
        Item::WebSearch { query, .. } => {
            vec![(
                IconName::Globe,
                t("Searched the web").into(),
                query.clone(),
                None,
            )]
        }
        Item::SubAgent { label, .. } => {
            vec![(IconName::Bot, t("Subagent").into(), label.clone(), None)]
        }
        _ => Vec::new(),
    }
}

/// One step as a line; a click opens its full card below it.
pub fn line(
    workspace: &Entity<Workspace>,
    item: &Item,
    card: impl FnOnce() -> AnyElement,
    cx: &App,
) -> AnyElement {
    if !is_step(item) {
        return div().pl_6().child(card()).into_any_element();
    }
    let id = item.id().to_string();
    let open = workspace.read(cx).open_steps.contains(&id);
    let colors = cx.theme().colors.clone();
    let failed = matches!(
        item,
        Item::Command {
            status: ItemStatus::Failed | ItemStatus::Declined,
            ..
        } | Item::FileChange {
            status: ItemStatus::Failed | ItemStatus::Declined,
            ..
        } | Item::Tool {
            status: ItemStatus::Failed | ItemStatus::Declined,
            ..
        }
    );
    let toggle = workspace.clone();
    let rows = line_parts(item)
        .into_iter()
        .enumerate()
        .map(|(ix, (icon, verb, what, counts))| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(Icon::new(icon).size(IconSize::Sm).color(colors.fg_muted))
                .child(
                    div()
                        .flex_none()
                        .text_color(if failed {
                            colors.danger
                        } else {
                            colors.fg_muted
                        })
                        .child(verb),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(colors.fg)
                        .when(counts.is_some(), |d| d.underline())
                        .child(what),
                )
                .children(counts.map(|(added, removed)| {
                    div()
                        .flex()
                        .flex_none()
                        .gap_1()
                        .child(div().text_color(colors.success).child(format!("+{added}")))
                        .child(div().text_color(colors.danger).child(format!("-{removed}")))
                }))
                .id(SharedString::from(format!("step-{id}-{ix}")))
        });
    let live = working(item).then(|| {
        let took = workspace.read(cx).item_took(&id).unwrap_or_default();
        div()
            .flex()
            .items_center()
            .gap_1()
            .text_size(cx.theme().text_size(TextSize::Xs))
            .text_color(colors.fg_subtle)
            .child(
                Spinner::new(SharedString::from(format!("step-busy-{id}")))
                    .style(SpinnerStyle::Ring)
                    .size(IconSize::Xs),
            )
            .child(super::rows::format_duration(took))
    });
    let id_click = id.clone();
    div()
        .pl_6()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .id(SharedString::from(format!("step-{id}")))
                .flex()
                .flex_col()
                .gap_1()
                .py_0p5()
                .cursor_pointer()
                .text_size(cx.theme().text_size(TextSize::Sm))
                .tooltip(ely_gpui_component::primitives::Tooltip::text(t("Inspect")))
                .children(rows)
                .children(live)
                .on_click(move |_, _, cx| {
                    let id = id_click.clone();
                    toggle.update(cx, |ws, cx| {
                        if !ws.open_steps.remove(&id) {
                            ws.open_steps.insert(id);
                        }
                        cx.notify();
                    })
                }),
        )
        .when(open, |d| d.child(div().pt_1().child(card())))
        .max_w(px(super::rows::COLUMN))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(turn: &str, item: Item) -> Entry {
        Entry {
            turn_id: turn.into(),
            kind: EntryKind::Item(item),
            live: false,
        }
    }

    fn cmd(id: &str, command: &str) -> Item {
        Item::Command {
            id: id.into(),
            command: command.into(),
            cwd: "/".into(),
            status: ItemStatus::Completed,
            output: String::new(),
            exit_code: Some(0),
            duration_ms: None,
        }
    }

    fn think(id: &str) -> Item {
        Item::Reasoning {
            id: id.into(),
            text: String::new(),
        }
    }

    #[test]
    fn steps_of_one_turn_fold_with_the_thinking_between() {
        let entries = vec![
            entry("a", think("r0")),
            entry("a", cmd("c1", "cat src/main.rs")),
            entry("a", think("r1")),
            entry("a", cmd("c2", "cargo test")),
            entry("a", think("r2")),
            entry(
                "a",
                Item::Agent {
                    id: "m".into(),
                    text: "done".into(),
                },
            ),
            entry("b", cmd("c3", "ls")),
        ];
        assert_eq!(run_at(&entries, 0), None, "thinking before stays outside");
        assert_eq!(run_at(&entries, 2), Some(Run { start: 1, end: 4 }));
        assert_eq!(run_at(&entries, 4), None, "thinking after stays outside");
        assert_eq!(run_at(&entries, 6), None, "a single step is not folded");
    }

    #[test]
    fn commands_read_as_what_they_do() {
        assert_eq!(does("/bin/bash -lc 'sed -n 1,80p src/lib.rs'"), Does::Read);
        assert_eq!(does("bash -lc \"cd crates && rg foo\""), Does::Search);
        assert_eq!(does("git grep foo"), Does::Search);
        assert_eq!(does("git status"), Does::Run);
        assert_eq!(does("cargo test"), Does::Run);
        assert_eq!(bare_command("bash -lc 'echo hi'"), "echo hi");
    }

    #[test]
    fn the_folded_line_counts_what_was_done() {
        let items = [
            cmd("1", "cat a"),
            cmd("2", "cat b"),
            cmd("3", "cargo build"),
            Item::FileChange {
                id: "4".into(),
                changes: vec![FileChange {
                    path: "a.rs".into(),
                    kind: ChangeKind::Update { move_to: None },
                    diff: "@@ -1 +1 @@\n-a\n+b\n".into(),
                }],
                status: ItemStatus::Completed,
            },
        ];
        let n = tally(items.iter());
        assert_eq!((n.edited, n.read, n.ran), (1, 2, 1));
        assert_eq!(describe(&n), "Edited a file, read 2 files, ran a command");
    }
}
