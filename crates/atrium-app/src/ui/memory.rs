//! The memory page: what agents know about the user and their projects,
//! shared by every agent. Switches for how memory is used and learned,
//! then the entries themselves (in use, waiting for the user, archived),
//! the recent changes with undo, and the skills.

use std::{path::PathBuf, time::SystemTime};

use atrium_memory::{
    Change, Config, Entry, HindsightConfig, Op, Scope, State, Store, Target, estimate_tokens,
};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton, SegmentedControl},
    data_display::{Badge, Tone},
    forms::{Input as TextField, Switch, TextInput},
    menus::{DropdownMenu, Menu, MenuItem},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::RootView;
use crate::i18n::{t, tf};
use crate::workspace::memory::memory_root;

#[derive(Clone, Debug, Default, PartialEq)]
enum Filter {
    #[default]
    All,
    User,
    General,
    Project(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Tab {
    #[default]
    Active,
    Pending,
    Archived,
}

pub struct MemoryUi {
    search: Entity<TextInput>,
    add: Entity<TextInput>,
    url: Entity<TextInput>,
    key: Entity<TextInput>,
    filter: Filter,
    tab: Tab,
    adding: bool,
    /// The entry being edited: its scope, id and field.
    editing: Option<(Scope, String, Entity<TextInput>)>,
    /// Repaints while the page shows, when another writer changed the
    /// memory.
    watching: bool,
    stamp: Option<SystemTime>,
}

impl MemoryUi {
    pub fn new(window: &mut Window, cx: &mut Context<RootView>) -> Self {
        let config = memory_root().map(|r| Config::load(&r)).unwrap_or_default();
        let hs = config.hindsight.clone().unwrap_or_default();
        Self {
            search: cx.new(|cx| TextInput::new(window, cx).placeholder(t("Search memory"))),
            add: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(2, 5)
                    .placeholder(t("One fact in a sentence or two, such as: run the tests with cargo test --workspace"))
            }),
            url: cx.new(|cx| {
                let mut input = TextInput::new(window, cx).placeholder("http://localhost:8888");
                input.set_text(hs.url.clone(), cx);
                input
            }),
            key: cx.new(|cx| {
                let mut input = TextInput::new(window, cx)
                    .masked()
                    .placeholder(t("API key, if the server asks for one"));
                input.set_text(hs.api_key.clone().unwrap_or_default(), cx);
                input
            }),
            filter: Filter::All,
            tab: Tab::Active,
            adding: false,
            editing: None,
            watching: false,
            stamp: None,
        }
    }
}

/// An entry with where it lives.
struct Row {
    scope: Scope,
    place: String,
    entry: Entry,
}

fn who(author: &str) -> String {
    match author {
        "user" => t("You").to_string(),
        a => match a.strip_prefix("agent:") {
            Some(agent) => crate::agents::backend_name(agent).to_string(),
            None => a.to_string(),
        },
    }
}

fn when(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC))
                .strftime("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn project_name(path: &Option<PathBuf>, key: &str) -> String {
    path.as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| key.to_string())
}

impl RootView {
    /// Repaints the page when the memory changes on disk.
    fn watch_memory(&mut self, cx: &mut Context<Self>) {
        if self.memory_ui.watching {
            return;
        }
        self.memory_ui.watching = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                let go_on = this.update(cx, |root, cx| {
                    if !root.workspace.read(cx).memory_open() {
                        root.memory_ui.watching = false;
                        return false;
                    }
                    let stamp = memory_root().and_then(|r| Store::new(r).journal_stamp());
                    if stamp != root.memory_ui.stamp {
                        root.memory_ui.stamp = stamp;
                        cx.notify();
                    }
                    true
                });
                if !go_on.unwrap_or(false) {
                    return;
                }
            }
        })
        .detach();
    }

    pub(super) fn memory_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.watch_memory(cx);
        let Some(root_dir) = memory_root() else {
            return div().into_any_element();
        };
        let store = Store::new(&root_dir);
        let config = Config::load(&root_dir);
        let root = cx.entity();
        let workspace = self.workspace.clone();

        // Everything, read once.
        let projects = store.projects();
        let mut rows: Vec<Row> = store
            .all(&Scope::Global)
            .into_iter()
            .map(|entry| Row {
                scope: Scope::Global,
                place: match entry.target {
                    Target::User => t("About you").into(),
                    Target::Notes => t("General").into(),
                },
                entry,
            })
            .collect();
        for (key, path) in &projects {
            let name = project_name(path, key);
            rows.extend(
                store
                    .all(&Scope::Project(key.clone()))
                    .into_iter()
                    .map(|entry| Row {
                        scope: Scope::Project(key.clone()),
                        place: name.clone(),
                        entry,
                    }),
            );
        }
        let in_filter = |r: &Row| match &self.memory_ui.filter {
            Filter::All => true,
            Filter::User => r.scope == Scope::Global && r.entry.target == Target::User,
            Filter::General => r.scope == Scope::Global && r.entry.target == Target::Notes,
            Filter::Project(key) => r.scope == Scope::Project(key.clone()),
        };
        let count = |state: State| {
            rows.iter()
                .filter(|r| in_filter(r) && r.entry.state == state)
                .count()
        };
        let (n_active, n_pending, n_archived) = (
            count(State::Active),
            count(State::Pending),
            count(State::Archived),
        );
        let query = self.memory_ui.search.read(cx).text().to_lowercase();
        let want = match self.memory_ui.tab {
            Tab::Active => State::Active,
            Tab::Pending => State::Pending,
            Tab::Archived => State::Archived,
        };
        let mut shown: Vec<&Row> = rows
            .iter()
            .filter(|r| in_filter(r) && r.entry.state == want)
            .filter(|r| query.is_empty() || r.entry.text.to_lowercase().contains(&query))
            .collect();
        shown.sort_by_key(|r| (!r.entry.pinned, std::cmp::Reverse(r.entry.updated_ms)));
        let journal: Vec<Change> = store.journal().into_iter().rev().take(8).collect();
        let skills = store.skills(None);
        let pending_reviews = store.pending().len();

        // Fields before the theme is borrowed.
        let search = TextField::new(&self.memory_ui.search).into_any_element();
        let add_field = TextField::new(&self.memory_ui.add).into_any_element();
        let url_field = TextField::new(&self.memory_ui.url).into_any_element();
        let key_field = TextField::new(&self.memory_ui.key).into_any_element();
        let editing = self.memory_ui.editing.clone();
        let edit_field = editing
            .as_ref()
            .map(|(_, _, f)| TextField::new(f).into_any_element());

        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);
        let r_lg = theme.radius(Radius::Lg);
        let r_md = theme.radius(Radius::Md);

        let set = |f: fn(&mut Config, bool)| {
            let workspace = workspace.clone();
            move |on: bool, _: &mut Window, cx: &mut App| {
                workspace.update(cx, |ws, cx| {
                    let mut config = ws.memory_config();
                    f(&mut config, on);
                    ws.set_memory_config(config, cx);
                })
            }
        };
        let switch_row = |id: &'static str,
                          icon: IconName,
                          title: String,
                          detail: String,
                          control: AnyElement,
                          extra: Option<AnyElement>| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .gap_2()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(colors.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_none()
                                .size(px(32.))
                                .rounded(r_md)
                                .bg(colors.sunken)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(icon).size(IconSize::Sm).color(colors.fg_muted)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_size(small)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(tiny)
                                        .text_color(colors.fg_muted)
                                        .child(detail),
                                ),
                        )
                        .child(control),
                )
                .children(extra)
                .into_any_element()
        };

        let agents_line = {
            let ws = self.workspace.read(cx);
            let mut names = vec!["Codex".to_string()];
            names.extend(
                ws.agents
                    .iter()
                    .filter(|a| a.remote.is_none())
                    .map(|a| a.profile.name.clone()),
            );
            names.retain(|n| {
                let id = if n == "Codex" {
                    "codex".to_string()
                } else {
                    ws.agents
                        .iter()
                        .find(|a| &a.profile.name == n)
                        .map(|a| a.profile.id.clone())
                        .unwrap_or_default()
                };
                config.on_for(&id)
            });
            tf(
                "{agents} · at most {tokens} tokens per new conversation",
                &[
                    ("agents", &names.join(", ")),
                    ("tokens", &config.budget_tokens),
                ],
            )
        };
        let hs = config.hindsight.clone().unwrap_or_default();
        let hs_on = config.hindsight().is_some();
        let hindsight_extra = hs_on.then(|| {
            let save = (
                workspace.clone(),
                self.memory_ui.url.clone(),
                self.memory_ui.key.clone(),
            );
            let check = workspace.clone();
            div()
                .pl(px(44.))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().child(url_field))
                        .child(div().w(px(220.)).child(key_field)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("mem-hs-save", t("Save"))
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    let (ws, url, key) = save.clone();
                                    let url = url.read(cx).text().trim().to_string();
                                    let key = key.read(cx).text().trim().to_string();
                                    ws.update(cx, |ws, cx| {
                                        let mut config = ws.memory_config();
                                        let mut h = config.hindsight.clone().unwrap_or_default();
                                        h.url = url;
                                        h.api_key = (!key.is_empty()).then_some(key);
                                        config.hindsight = Some(h);
                                        ws.set_memory_config(config, cx);
                                    })
                                }),
                        )
                        .child(
                            Button::new("mem-hs-check", t("Check connection"))
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    check.update(cx, |ws, cx| ws.check_hindsight(cx))
                                }),
                        )
                        .child(div().text_size(tiny).text_color(colors.fg_subtle).child(tf(
                            "Banks: {bank} and {bank}-<project>",
                            &[("bank", &hs.bank)],
                        ))),
                )
                .into_any_element()
        });
        let learn_extra = config.learn.then(|| {
            div()
                .pl(px(44.))
                .child(
                    Switch::new("mem-confirm", config.confirm_learned)
                        .label(t("Let me confirm what is learned before agents see it"))
                        .on_change(set(|c, on| c.confirm_learned = on)),
                )
                .into_any_element()
        });
        let switches = div()
            .flex()
            .flex_col()
            .rounded(r_lg)
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .overflow_hidden()
            .child(switch_row(
                "mem-inject",
                IconName::FileText,
                t("Put memory into agents' instructions").into(),
                agents_line,
                Switch::new("mem-inject-on", config.inject)
                    .on_change(set(|c, on| c.inject = on))
                    .into_any_element(),
                None,
            ))
            .child(switch_row(
                "mem-learn",
                IconName::Search,
                t("Learn from conversations").into(),
                tf(
                    "A conversation quiet for {min} minutes goes to the memory service (and Hindsight) to find what is worth keeping.",
                    &[("min", &config.review_after_idle_min)],
                ),
                Switch::new("mem-learn-on", config.learn)
                    .on_change(set(|c, on| c.learn = on))
                    .into_any_element(),
                learn_extra,
            ))
            .child(switch_row(
                "mem-tidy",
                IconName::History,
                t("Tidy up every night").into(),
                t("The memory service merges duplicates and archives what went stale. What you wrote or pinned stays as it is.").into(),
                Switch::new("mem-tidy-on", config.nightly_tidy)
                    .on_change(set(|c, on| c.nightly_tidy = on))
                    .into_any_element(),
                None,
            ))
            .child(switch_row(
                "mem-meaning",
                IconName::Database,
                t("Search by meaning (Hindsight)").into(),
                t("A Hindsight server keeps every conversation and finds what bears on a new one, in any language. What it recalls goes in with the memory.").into(),
                Switch::new("mem-hs-on", hs_on)
                    .on_change({
                        let workspace = workspace.clone();
                        move |on, _, cx| {
                            workspace.update(cx, |ws, cx| {
                                let mut config = ws.memory_config();
                                let mut h: HindsightConfig = config.hindsight.clone().unwrap_or_default();
                                h.enabled = on;
                                config.hindsight = Some(h);
                                ws.set_memory_config(config, cx);
                            })
                        }
                    })
                    .into_any_element(),
                hindsight_extra,
            ));

        // The filter and the tabs.
        let filter_label = match &self.memory_ui.filter {
            Filter::All => t("All").to_string(),
            Filter::User => t("About you").to_string(),
            Filter::General => t("General").to_string(),
            Filter::Project(key) => projects
                .iter()
                .find(|(k, _)| k == key)
                .map(|(k, p)| project_name(p, k))
                .unwrap_or_else(|| key.clone()),
        };
        let pick = |filter: Filter| {
            let root = root.clone();
            move |_: &mut Window, cx: &mut App| {
                let filter = filter.clone();
                root.update(cx, |root, cx| {
                    root.memory_ui.filter = filter;
                    cx.notify();
                })
            }
        };
        let mut menu = Menu::new()
            .item(
                MenuItem::radio(t("All"), self.memory_ui.filter == Filter::All)
                    .on_click(pick(Filter::All)),
            )
            .item(
                MenuItem::radio(t("About you"), self.memory_ui.filter == Filter::User)
                    .on_click(pick(Filter::User)),
            )
            .item(
                MenuItem::radio(t("General"), self.memory_ui.filter == Filter::General)
                    .on_click(pick(Filter::General)),
            );
        if !projects.is_empty() {
            let items: Vec<MenuItem> = projects
                .iter()
                .map(|(key, path)| {
                    MenuItem::radio(
                        project_name(path, key),
                        self.memory_ui.filter == Filter::Project(key.clone()),
                    )
                    .on_click(pick(Filter::Project(key.clone())))
                })
                .collect();
            menu = menu.group(t("Projects"), items);
        }
        let tabs = {
            let root = root.clone();
            SegmentedControl::new(
                "mem-tab",
                match self.memory_ui.tab {
                    Tab::Active => "active",
                    Tab::Pending => "pending",
                    Tab::Archived => "archived",
                },
            )
            .size(ControlSize::Sm)
            .segment("active", tf("In use · {n}", &[("n", &n_active)]), None)
            .segment(
                "pending",
                tf("To confirm · {n}", &[("n", &n_pending)]),
                None,
            )
            .segment(
                "archived",
                tf("Archived · {n}", &[("n", &n_archived)]),
                None,
            )
            .on_change(move |value, _, cx| {
                let tab = match value.as_ref() {
                    "pending" => Tab::Pending,
                    "archived" => Tab::Archived,
                    _ => Tab::Active,
                };
                root.update(cx, |root, cx| {
                    root.memory_ui.tab = tab;
                    cx.notify();
                })
            })
        };
        let toggle_add = root.clone();
        let bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w(px(200.)).child(search))
            .child(
                DropdownMenu::new("mem-filter", filter_label.clone(), menu)
                    .variant(ButtonVariant::Outline),
            )
            .child(tabs)
            .child(
                Button::new("mem-add", t("Add"))
                    .icon(IconName::Plus)
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        toggle_add.update(cx, |root, cx| {
                            root.memory_ui.adding = !root.memory_ui.adding;
                            cx.notify();
                        })
                    }),
            );

        // Where a new entry goes: the filter's place, else general.
        let (add_scope, add_target, add_place) = match &self.memory_ui.filter {
            Filter::User => (Scope::Global, Target::User, t("About you").to_string()),
            Filter::Project(key) => (
                Scope::Project(key.clone()),
                Target::Notes,
                filter_label.clone(),
            ),
            _ => (Scope::Global, Target::Notes, t("General").to_string()),
        };
        let add_form = self.memory_ui.adding.then(|| {
            let save = (root.clone(), workspace.clone(), self.memory_ui.add.clone());
            let (scope, target) = (add_scope.clone(), add_target);
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(r_lg)
                .border_1()
                .border_color(colors.border)
                .child(add_field)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(tf(
                                    "Goes to: {place}. Pick another place in the filter.",
                                    &[("place", &add_place)],
                                )),
                        )
                        .child(
                            Button::new("mem-add-save", t("Keep"))
                                .primary()
                                .size(ControlSize::Sm)
                                .on_click(move |_, window, cx| {
                                    let (root, ws, field) = save.clone();
                                    let text = field.read(cx).text().to_string();
                                    if text.trim().is_empty() {
                                        return;
                                    }
                                    let (scope, target) = (scope.clone(), target);
                                    ws.update(cx, |ws, cx| {
                                        ws.memory_apply(
                                            scope,
                                            vec![Op::Add {
                                                content: text,
                                                target,
                                            }],
                                            cx,
                                        )
                                    });
                                    field.update(cx, |f, cx| f.set_text("", cx));
                                    root.update(cx, |root, cx| {
                                        root.memory_ui.adding = false;
                                        cx.notify();
                                    });
                                    let _ = window;
                                }),
                        ),
                )
                .into_any_element()
        });

        // Usage of the place in the filter.
        let usage = {
            let parts: Vec<(String, Scope, Target)> = match &self.memory_ui.filter {
                Filter::All => vec![
                    (t("About you").into(), Scope::Global, Target::User),
                    (t("General").into(), Scope::Global, Target::Notes),
                ],
                Filter::User => vec![(t("About you").into(), Scope::Global, Target::User)],
                Filter::General => vec![(t("General").into(), Scope::Global, Target::Notes)],
                Filter::Project(key) => vec![(
                    filter_label.clone(),
                    Scope::Project(key.clone()),
                    Target::Notes,
                )],
            };
            let mut line = div().flex().flex_wrap().gap_4();
            for (label, scope, target) in parts {
                let (used, limit) = store.usage(&scope, target);
                let tokens: usize = store
                    .active(&scope, target)
                    .iter()
                    .map(|e| estimate_tokens(&e.text))
                    .sum();
                let full = used * 100 / limit.max(1);
                line = line.child(
                    div()
                        .text_size(tiny)
                        .text_color(if full >= 80 {
                            colors.warning
                        } else {
                            colors.fg_subtle
                        })
                        .child(tf(
                            "{place}: {used}/{limit} characters (about {tokens} tokens)",
                            &[
                                ("place", &label),
                                ("used", &used),
                                ("limit", &limit),
                                ("tokens", &tokens),
                            ],
                        )),
                );
            }
            line
        };

        // The entries.
        let mut list = div().flex().flex_col().gap_2();
        if shown.is_empty() {
            list = list.child(
                div()
                    .px_4()
                    .py_5()
                    .rounded(r_lg)
                    .border_1()
                    .border_color(colors.border)
                    .text_size(small)
                    .text_color(colors.fg_muted)
                    .child(match (self.memory_ui.tab, query.is_empty()) {
                        (_, false) => t("Nothing matches."),
                        (Tab::Active, true) => {
                            t("Nothing yet. Tell an agent 'remember …', or add one here.")
                        }
                        (Tab::Pending, true) => t("Nothing waits for you."),
                        (Tab::Archived, true) => t("Nothing archived."),
                    }),
            );
        }
        let mut edit_field = edit_field;
        for (n, row) in shown.iter().enumerate() {
            let e = &row.entry;
            let is_editing = editing
                .as_ref()
                .is_some_and(|(s, id, _)| *s == row.scope && *id == e.id);
            let op = |make: fn(String) -> Op| {
                let (ws, scope, id) = (workspace.clone(), row.scope.clone(), e.id.clone());
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                    let (scope, op) = (scope.clone(), make(id.clone()));
                    ws.update(cx, |ws, cx| ws.memory_apply(scope, vec![op], cx))
                }
            };
            let icon_button = |name: &str, icon: IconName, tip: &'static str| {
                IconButton::new(SharedString::from(format!("mem-{name}-{n}")), icon)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .tooltip(t(tip))
            };
            let mut actions = div().flex().items_center().gap_0p5();
            match e.state {
                State::Active => {
                    let edit = (
                        root.clone(),
                        row.scope.clone(),
                        e.id.clone(),
                        e.text.clone(),
                    );
                    actions = actions
                        .child(
                            icon_button(
                                "pin",
                                if e.pinned {
                                    IconName::PinOff
                                } else {
                                    IconName::Pin
                                },
                                if e.pinned {
                                    "Unpin"
                                } else {
                                    "Pin: tidying leaves it alone"
                                },
                            )
                            .on_click({
                                let (ws, scope, id, pinned) = (
                                    workspace.clone(),
                                    row.scope.clone(),
                                    e.id.clone(),
                                    !e.pinned,
                                );
                                move |_, _, cx| {
                                    let op = Op::Pin {
                                        old: id.clone(),
                                        pinned,
                                    };
                                    let scope = scope.clone();
                                    ws.update(cx, |ws, cx| ws.memory_apply(scope, vec![op], cx))
                                }
                            }),
                        )
                        .child(icon_button("edit", IconName::Pencil, "Edit").on_click(
                            move |_, window, cx| {
                                let (root, scope, id, text) = edit.clone();
                                root.update(cx, |root, cx| {
                                    let field = cx.new(|cx| {
                                        let mut input = TextInput::new(window, cx).multi_line(2, 8);
                                        input.set_text(text.clone(), cx);
                                        input
                                    });
                                    root.memory_ui.editing = Some((scope, id, field));
                                    cx.notify();
                                })
                            },
                        ))
                        .child(
                            icon_button("archive", IconName::Archive, "Archive")
                                .on_click(op(|old| Op::Archive { old })),
                        )
                        .child(
                            icon_button("rm", IconName::Trash2, "Delete")
                                .on_click(op(|old| Op::Remove { old })),
                        );
                }
                State::Pending => {
                    actions = actions
                        .child(
                            Button::new(SharedString::from(format!("mem-keep-{n}")), t("Keep"))
                                .icon(IconName::Check)
                                .size(ControlSize::Sm)
                                .on_click(op(|old| Op::Restore { old })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("mem-drop-{n}")), t("Drop"))
                                .icon(IconName::X)
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .on_click(op(|old| Op::Remove { old })),
                        );
                }
                State::Archived => {
                    actions = actions
                        .child(
                            icon_button("restore", IconName::Undo2, "Put back in use")
                                .on_click(op(|old| Op::Restore { old })),
                        )
                        .child(
                            icon_button("rm", IconName::Trash2, "Delete")
                                .on_click(op(|old| Op::Remove { old })),
                        );
                }
            }
            let body: AnyElement = if is_editing {
                let save = (
                    root.clone(),
                    workspace.clone(),
                    row.scope.clone(),
                    e.id.clone(),
                );
                let cancel = root.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(edit_field.take())
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("mem-edit-save", t("Save"))
                                    .primary()
                                    .size(ControlSize::Sm)
                                    .on_click(move |_, _, cx| {
                                        let (root, ws, scope, id) = save.clone();
                                        let text = root
                                            .read(cx)
                                            .memory_ui
                                            .editing
                                            .as_ref()
                                            .map(|(_, _, f)| f.read(cx).text().to_string())
                                            .unwrap_or_default();
                                        ws.update(cx, |ws, cx| {
                                            ws.memory_apply(
                                                scope,
                                                vec![Op::Patch { old: id, new: text }],
                                                cx,
                                            )
                                        });
                                        root.update(cx, |root, cx| {
                                            root.memory_ui.editing = None;
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                Button::new("mem-edit-cancel", t("Cancel"))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .on_click(move |_, _, cx| {
                                        cancel.update(cx, |root, cx| {
                                            root.memory_ui.editing = None;
                                            cx.notify();
                                        })
                                    }),
                            ),
                    )
                    .into_any_element()
            } else {
                div()
                    .text_size(small)
                    .whitespace_normal()
                    .child(e.text.clone())
                    .into_any_element()
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("mem-row-{n}")))
                    .flex()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .rounded(r_lg)
                    .border_1()
                    .border_color(if e.state == State::Pending {
                        colors.warning.opacity(0.5)
                    } else {
                        colors.border
                    })
                    .bg(colors.surface)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(body)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(tiny)
                                    .text_color(colors.fg_subtle)
                                    .child(Badge::new(row.place.clone()).tone(Tone::Neutral))
                                    .when(e.pinned, |d| {
                                        d.child(Badge::new(t("Pinned")).tone(Tone::Info))
                                    })
                                    .child(tf(
                                        "by {who} · {when}",
                                        &[("who", &who(&e.author)), ("when", &when(e.updated_ms))],
                                    )),
                            ),
                    )
                    .child(actions),
            );
        }

        // Recent changes, with undo.
        let mut changes = div().flex().flex_col().gap_1();
        for (n, change) in journal.iter().enumerate() {
            let (added, removed) = change.summary();
            let what = match (change.undoes.is_some(), added.first(), removed.first()) {
                (true, _, _) => t("undid a change").to_string(),
                (_, Some(e), _) => e
                    .text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(60)
                    .collect(),
                (_, None, Some(e)) => tf(
                    "removed: {text}",
                    &[("text", &e.text.chars().take(50).collect::<String>())],
                ),
                _ => String::new(),
            };
            let undo = (workspace.clone(), change.id.clone());
            changes = changes.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(tiny)
                    .text_color(colors.fg_muted)
                    .child(div().w(px(84.)).flex_none().child(when(change.at_ms)))
                    .child(
                        div()
                            .w(px(90.))
                            .flex_none()
                            .truncate()
                            .child(who(&change.author)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(colors.success)
                            .child(format!("+{}", added.len())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(colors.danger)
                            .child(format!("−{}", removed.len())),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(what))
                    .when(change.undoes.is_none(), |d| {
                        d.child(
                            Button::new(SharedString::from(format!("mem-undo-{n}")), t("Undo"))
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    let (ws, id) = undo.clone();
                                    ws.update(cx, |ws, cx| ws.memory_undo(&id, cx))
                                }),
                        )
                    }),
            );
        }

        let mut skill_list = div().flex().flex_wrap().gap_1();
        for skill in &skills {
            skill_list = skill_list.child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded(r_md)
                    .bg(colors.sunken)
                    .text_size(tiny)
                    .child(skill.name.clone()),
            );
        }

        let section = |title: String, body: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(small)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(body)
        };
        let open_dir = root_dir.clone();
        let skills_dir = store.skills_dir(&Scope::Global);
        let close = workspace.clone();

        div()
            .id("memory-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .items_start()
            .child(
                div()
                    .w_full()
                    .max_w(px(820.))
                    .px_6()
                    .py_8()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::Lightbulb).size(IconSize::Md).color(colors.accent))
                            .child(div().flex_1().text_size(theme.text_size(TextSize::Xl)).font_weight(gpui::FontWeight::SEMIBOLD).child(t("Memory")))
                            .child(
                                Button::new("mem-folder", t("Memory folder"))
                                    .icon(IconName::Folder)
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .on_click(move |_, _, _| {
                                        let _ = std::fs::create_dir_all(&open_dir);
                                        let _ = crate::opener::open_default(&open_dir);
                                    }),
                            )
                            .child(
                                IconButton::new("mem-close", IconName::X)
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .tooltip(t("Close"))
                                    .on_click(move |_, _, cx| close.update(cx, |ws, cx| ws.close_memory(cx))),
                            ),
                    )
                    .child(div().text_size(small).text_color(colors.fg_muted).child(t(
                        "What agents know about you and your projects, shared by every agent. They look things up and keep what they learn with the recall and remember tools; agents switched on below also start each conversation with it.",
                    )))
                    .child(switches)
                    .when(pending_reviews > 0, |d| {
                        d.child(div().text_size(tiny).text_color(colors.fg_subtle).child(tf(
                            "{n} conversations wait for the memory service.",
                            &[("n", &pending_reviews)],
                        )))
                    })
                    .child(bar)
                    .children(add_form)
                    .child(usage)
                    .child(list)
                    .when(!journal.is_empty(), |d| {
                        d.child(section(t("Recent changes").into(), changes.into_any_element()))
                    })
                    .child(section(
                        tf("Skills · {n}", &[("n", &skills.len())]),
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().text_size(tiny).text_color(colors.fg_muted).child(t(
                                "Ways of doing things that worked, kept as SKILL.md files. Agents see their names and read one when it applies.",
                            )))
                            .child(skill_list)
                            .child(
                                div().child(
                                    Button::new("mem-skills-dir", t("Skills folder"))
                                        .icon(IconName::Folder)
                                        .variant(ButtonVariant::Ghost)
                                        .size(ControlSize::Sm)
                                        .on_click(move |_, _, _| {
                                            let _ = std::fs::create_dir_all(&skills_dir);
                                            let _ = crate::opener::open_default(&skills_dir);
                                        }),
                                ),
                            )
                            .into_any_element(),
                    )),
            )
            .into_any_element()
    }
}
