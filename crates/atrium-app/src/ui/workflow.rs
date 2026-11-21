//! The Workflows room: templates to start a run from, the runs in the
//! sidebar, and a run's steps with where each stands, what its checks
//! found, its files, and the user's decisions at its stops.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use atrium_workflow::{
    Input, InputKind, condition_holds,
    run::{RunState, StepRun, StepStatus},
};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    data_display::{Badge, Tone},
    forms::{DropZone, Input as TextField, Switch, TextInput},
    motion::{ProgressBar, Spinner, SpinnerStyle},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::RootView;
use crate::i18n::{t, tf};
use crate::workspace::{Room, RunEntry, TemplateSource, Workspace};

/// The start form and the run view's own state.
pub struct WorkflowForm {
    /// The template picked to start from.
    pub template: Option<String>,
    pub texts: HashMap<String, Entity<TextInput>>,
    pub choices: HashMap<String, String>,
    pub files: HashMap<String, Vec<PathBuf>>,
    /// Optional steps left out.
    pub skip: HashSet<String>,
    /// What to change, for "redo with a note".
    pub note: Entity<TextInput>,
    /// The step whose details are open, by run folder.
    pub open: HashMap<PathBuf, String>,
    /// The option picked at a deciding stop, by `<run dir>|<step>`.
    pub picked: HashMap<String, String>,
}

impl WorkflowForm {
    pub fn new(window: &mut Window, cx: &mut Context<RootView>) -> Self {
        Self {
            template: None,
            texts: HashMap::new(),
            choices: HashMap::new(),
            files: HashMap::new(),
            skip: HashSet::new(),
            note: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(2, 5)
                    .placeholder(t("What should change? The agent gets this with the step."))
            }),
            open: HashMap::new(),
            picked: HashMap::new(),
        }
    }

    fn reset(&mut self, cx: &mut App) {
        self.choices.clear();
        self.files.clear();
        self.skip.clear();
        for input in self.texts.values() {
            input.update(cx, |i, cx| i.set_text("", cx));
        }
    }
}

fn agent_name(id: Option<&str>) -> String {
    match id {
        Some(id) => crate::agents::backend_label(id),
        None => t("Default agent").to_string(),
    }
}

fn status_label(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => t("Waiting"),
        StepStatus::Running => t("Working"),
        StepStatus::Checking => t("Checking"),
        StepStatus::Review => t("Waiting for you"),
        StepStatus::Done => t("Done"),
        StepStatus::Failed => t("Needs you"),
        StepStatus::Skipped => t("Skipped"),
    }
}

fn run_label(state: RunState) -> (&'static str, Tone) {
    match state {
        RunState::Running => (t("Running"), Tone::Accent),
        RunState::Paused => (t("Paused"), Tone::Neutral),
        RunState::Waiting => (t("Waiting for you"), Tone::Warning),
        RunState::Done => (t("Finished"), Tone::Success),
        RunState::Stopped => (t("Stopped"), Tone::Neutral),
    }
}

fn status_mark(id: SharedString, status: StepStatus, cx: &App) -> AnyElement {
    let colors = cx.theme().colors.clone();
    let icon = |name: IconName, color| {
        Icon::new(name)
            .size(IconSize::Sm)
            .color(color)
            .into_any_element()
    };
    match status {
        StepStatus::Running | StepStatus::Checking => Spinner::new(id)
            .style(SpinnerStyle::Ring)
            .size(IconSize::Sm)
            .color(colors.accent)
            .into_any_element(),
        StepStatus::Pending => div()
            .size(px(14.))
            .rounded_full()
            .border_1()
            .border_color(colors.border_strong)
            .into_any_element(),
        StepStatus::Review => icon(IconName::Eye, colors.warning),
        StepStatus::Done => icon(IconName::CircleCheck, colors.success),
        StepStatus::Failed => icon(IconName::CircleX, colors.danger),
        StepStatus::Skipped => icon(IconName::Minus, colors.fg_subtle),
    }
}

fn minutes(ms: i64) -> String {
    let m = (ms / 60_000).max(0);
    if m < 1 {
        t("under a minute").to_string()
    } else if m < 60 {
        tf("{m} min", &[("m", &m)])
    } else {
        tf("{h} h {m} min", &[("h", &(m / 60)), ("m", &(m % 60))])
    }
}

/// The runs and templates in the sidebar.
pub fn sidebar_list(workspace: &Entity<Workspace>, cx: &App) -> AnyElement {
    let ws = workspace.read(cx);
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let small = theme.text_size(TextSize::Sm);
    let shown = match &ws.room {
        Room::Workflows { run } => run.clone(),
        _ => None,
    };
    let heading = |text: &'static str| {
        div()
            .px_2()
            .pt_3()
            .pb_1()
            .text_size(theme.text_size(TextSize::Xs))
            .text_color(colors.fg_subtle)
            .child(t(text))
    };
    let mut column = div().flex().flex_col().gap_0p5().child(heading("Runs"));
    if ws.workflow_runs.is_empty() {
        column = column.child(
            div()
                .px_2()
                .py_1()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(t("No runs yet. Pick a template to start one.")),
        );
    }
    for (ix, entry) in ws.workflow_runs.iter().enumerate() {
        let (done, total) = entry.run.progress();
        let active = shown.as_deref() == Some(entry.dir.as_path());
        let open = workspace.clone();
        let dir = entry.dir.clone();
        let working = entry
            .run
            .steps
            .iter()
            .any(|s| matches!(s.status, StepStatus::Running | StepStatus::Checking));
        let mark = if working {
            Spinner::new(SharedString::from(format!("wf-run-{ix}")))
                .style(SpinnerStyle::Ring)
                .size(IconSize::Xs)
                .color(colors.accent)
                .into_any_element()
        } else {
            let color = match entry.run.state {
                RunState::Waiting => colors.warning,
                RunState::Done => colors.success,
                RunState::Running => colors.accent,
                _ => colors.fg_subtle,
            };
            div()
                .size(px(7.))
                .rounded_full()
                .bg(color)
                .into_any_element()
        };
        column = column.child(
            div()
                .id(SharedString::from(format!("wf-run-row-{ix}")))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded(theme.radius(Radius::Md))
                .cursor_pointer()
                .text_size(small)
                .when(active, |d| d.bg(colors.active).text_color(colors.fg))
                .when(!active, |d| {
                    d.text_color(colors.fg_muted).hover(|d| d.bg(colors.hover))
                })
                .child(
                    div()
                        .flex_none()
                        .w(px(14.))
                        .flex()
                        .justify_center()
                        .child(mark),
                )
                .child(div().flex_1().min_w_0().truncate().child(entry.title()))
                .child(
                    div()
                        .flex_none()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_subtle)
                        .child(format!("{done}/{total}")),
                )
                .on_click(move |_, _, cx| {
                    let dir = dir.clone();
                    open.update(cx, |ws, cx| ws.enter_workflows(Some(dir), cx))
                }),
        );
    }
    column = column.child(heading("Templates"));
    let author = workspace.clone();
    let authoring = ws.authoring.as_ref().is_some_and(|a| a.base.is_none());
    column = column.child(
        div()
            .id("wf-tpl-ai")
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(theme.radius(Radius::Md))
            .cursor_pointer()
            .text_size(small)
            .when(authoring, |d| d.bg(colors.active).text_color(colors.fg))
            .when(!authoring, |d| {
                d.text_color(colors.accent).hover(|d| d.bg(colors.hover))
            })
            .child(
                Icon::new(IconName::Sparkles)
                    .size(IconSize::Sm)
                    .color(colors.accent),
            )
            .child(div().flex_1().min_w_0().truncate().child(t("New with AI…")))
            .on_click(move |_, _, cx| author.update(cx, |ws, cx| ws.open_authoring(None, cx))),
    );
    for (ix, entry) in ws.workflow_templates().into_iter().enumerate() {
        let open = workspace.clone();
        let id = entry.template.id.clone();
        column = column.child(
            div()
                .id(SharedString::from(format!("wf-tpl-{ix}")))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded(theme.radius(Radius::Md))
                .cursor_pointer()
                .text_size(small)
                .text_color(colors.fg_muted)
                .hover(|d| d.bg(colors.hover))
                .child(
                    Icon::new(IconName::Workflow)
                        .size(IconSize::Sm)
                        .color(colors.fg_subtle),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(entry.template.name.clone()),
                )
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    open.update(cx, |ws, cx| {
                        ws.pick_template = Some(id);
                        ws.enter_workflows(None, cx)
                    })
                }),
        );
    }
    column.into_any_element()
}

impl RootView {
    /// The main area in the Workflows room.
    pub(super) fn workflow_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (run, picked, authoring) = {
            let ws = self.workspace.read(cx);
            let run = match &ws.room {
                Room::Workflows { run } => run.clone(),
                _ => None,
            };
            (run, ws.pick_template.clone(), ws.authoring.clone())
        };
        if let Some(authoring) = authoring {
            return self.author_view(authoring, cx);
        }
        if let Some(id) = picked {
            self.workspace.update(cx, |ws, _| ws.pick_template = None);
            if self.workflow.template.as_deref() != Some(id.as_str()) {
                self.workflow.template = Some(id);
                self.workflow.reset(cx);
            }
        }
        match run.and_then(|dir| self.workspace.read(cx).workflow_run(&dir).cloned()) {
            Some(entry) => self.run_view(entry, cx),
            None => self.start_view(window, cx),
        }
    }

    fn start_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let templates = self.workspace.read(cx).workflow_templates();
        if self.workflow.template.is_none() {
            self.workflow.template = templates.first().map(|e| e.template.id.clone());
        }
        let Some(entry) = templates
            .iter()
            .find(|e| Some(&e.template.id) == self.workflow.template.as_ref())
            .cloned()
        else {
            return div().into_any_element();
        };
        let template = entry.template.clone();
        // Text inputs are made once per input key.
        for input in &template.inputs {
            if matches!(
                input.kind,
                InputKind::Text | InputKind::Long | InputKind::Number
            ) && !self.workflow.texts.contains_key(&input.key)
            {
                let long = input.kind == InputKind::Long;
                let placeholder = input.help.clone();
                let field = cx.new(|cx| {
                    let field = TextInput::new(window, cx).placeholder(placeholder);
                    if long { field.multi_line(2, 6) } else { field }
                });
                self.workflow.texts.insert(input.key.clone(), field);
            }
        }
        let root = cx.entity();

        // Values so far, to decide what shows.
        let mut values: HashMap<String, String> = HashMap::new();
        for input in &template.inputs {
            let v = match input.kind {
                InputKind::Choice => self
                    .workflow
                    .choices
                    .get(&input.key)
                    .cloned()
                    .or_else(|| input.default.clone())
                    .or_else(|| input.options.first().cloned()),
                _ => None,
            };
            if let Some(v) = v {
                values.insert(input.key.clone(), v);
            }
        }

        let mut form = div().flex().flex_col().gap_4();
        for input in &template.inputs {
            if !condition_holds(input.when.as_deref(), &values) {
                continue;
            }
            form = form.child(self.input_field(input, &values, cx));
        }

        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);

        // The steps, with the optional ones to leave out.
        let mut steps = div().flex().flex_col().gap_1();
        for (ix, step) in template.steps.iter().enumerate() {
            if !condition_holds(step.when.as_deref(), &values) {
                continue;
            }
            let skipped = self.workflow.skip.contains(&step.id);
            let toggle = root.clone();
            let id = step.id.clone();
            steps = steps.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_1p5()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .flex_none()
                            .w(px(22.))
                            .text_size(tiny)
                            .text_color(colors.fg_subtle)
                            .child(format!("{}", ix + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(small)
                                    .text_color(if skipped { colors.fg_subtle } else { colors.fg })
                                    .when(skipped, |d| d.line_through())
                                    .child(step.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(tiny)
                                    .text_color(colors.fg_muted)
                                    .truncate()
                                    .child(step.summary.clone()),
                            ),
                    )
                    .child(
                        Badge::new(agent_name(template.agent_of(step).as_deref()))
                            .tone(Tone::Neutral),
                    )
                    .when(step.gate, |d| {
                        d.child(Badge::new(t("Stops for you")).tone(Tone::Warning))
                    })
                    .when(step.optional, |d| {
                        d.child(
                            Switch::new(SharedString::from(format!("wf-opt-{id}")), !skipped)
                                .on_change(move |on, _, cx| {
                                    let id = id.clone();
                                    toggle.update(cx, |root, cx| {
                                        if on {
                                            root.workflow.skip.remove(&id);
                                        } else {
                                            root.workflow.skip.insert(id);
                                        }
                                        cx.notify();
                                    })
                                }),
                        )
                    }),
            );
        }

        let start = root.clone();
        let template_id = template.id.clone();
        let home = self.workspace.read(cx).workflow_home();
        let copy = self.workspace.clone();
        let copy_id = template.id.clone();
        let change = self.workspace.clone();
        let change_id = template.id.clone();
        let source = match &entry.source {
            TemplateSource::Builtin => t("Built in").to_string(),
            TemplateSource::User(path) => path.display().to_string(),
        };

        div()
            .id("wf-start")
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
                    .max_w(px(760.))
                    .px_6()
                    .py_8()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Icon::new(IconName::Workflow)
                                            .size(IconSize::Md)
                                            .color(colors.accent),
                                    )
                                    .child(
                                        div()
                                            .text_size(theme.text_size(TextSize::Xl))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(template.name.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(small)
                                    .text_color(colors.fg_muted)
                                    .child(template.description.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .text_size(tiny)
                                    .text_color(colors.fg_subtle)
                                    .child(tf("Template: {source}", &[("source", &source)]))
                                    .child(
                                        Button::new("wf-copy-template", t("Copy and edit"))
                                            .variant(ButtonVariant::Ghost)
                                            .size(ControlSize::Sm)
                                            .icon(IconName::Pencil)
                                            .on_click(move |_, _, cx| {
                                                let path = copy.update(cx, |ws, cx| ws.copy_template(&copy_id, cx));
                                                if let Some(path) = path {
                                                    let _ = crate::opener::open_default(&path);
                                                }
                                            }),
                                    )
                                    .child(
                                        Button::new("wf-change-template", t("Change with AI"))
                                            .variant(ButtonVariant::Ghost)
                                            .size(ControlSize::Sm)
                                            .icon(IconName::Sparkles)
                                            .on_click(move |_, _, cx| {
                                                let id = change_id.clone();
                                                change.update(cx, |ws, cx| ws.open_authoring(Some(id), cx))
                                            }),
                                    ),
                            ),
                    )
                    .children(atrium_workflow_experience(&template.id).map(|count| {
                        let id = template.id.clone();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded(theme.radius(Radius::Md))
                            .bg(colors.accent.opacity(0.08))
                            .text_size(tiny)
                            .text_color(colors.fg_muted)
                            .child(Icon::new(IconName::BookOpen).size(IconSize::Xs).color(colors.accent))
                            .child(div().flex_1().child(tf(
                                "New runs start with the lessons you kept from {n} earlier runs.",
                                &[("n", &count)],
                            )))
                            .child(
                                Button::new("wf-lessons", t("Edit"))
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .on_click(move |_, _, _| {
                                        if let Some(path) = crate::workspace::experience_file(&id) {
                                            let _ = crate::opener::open_default(&path);
                                        }
                                    }),
                            )
                    }))
                    .child(section(t("What to start from"), form, cx))
                    .child(section(t("Steps"), steps, cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(tiny)
                                    .text_color(colors.fg_muted)
                                    .child(tf(
                                        "The run gets a folder in {home}. Steps that stop for you wait there; the rest go on by themselves.",
                                        &[("home", &home.display())],
                                    )),
                            )
                            .child(
                                Button::new("wf-start-run", t("Start"))
                                    .icon(IconName::Play)
                                    .on_click(move |_, _, cx| {
                                        let id = template_id.clone();
                                        start.update(cx, |root, cx| root.start_workflow(&id, cx))
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn input_field(
        &self,
        input: &Input,
        values: &HashMap<String, String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);
        let root = cx.entity();
        let key = input.key.clone();
        let label = div()
            .flex()
            .items_center()
            .gap_1()
            .text_size(small)
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(input.label.clone())
            .when(input.required, |d| {
                d.child(div().text_color(colors.danger).child("*"))
            })
            .when(input.private, |d| {
                d.child(
                    Icon::new(IconName::Lock)
                        .size(IconSize::Xs)
                        .color(colors.fg_subtle),
                )
            });
        let help = (!input.help.is_empty()
            && input.kind != InputKind::Long
            && input.kind != InputKind::Text)
            .then(|| {
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_muted)
                    .child(input.help.clone())
            });
        let body: AnyElement = match input.kind {
            InputKind::Choice => {
                let selected = values.get(&key).cloned().unwrap_or_default();
                let mut control =
                    SegmentedControl::new(SharedString::from(format!("wf-in-{key}")), selected)
                        .size(ControlSize::Sm);
                for option in &input.options {
                    control = control.segment(option.clone(), option.clone(), None);
                }
                control
                    .on_change(move |value, _, cx| {
                        let key = key.clone();
                        let value = value.to_string();
                        root.update(cx, |root, cx| {
                            root.workflow.choices.insert(key, value);
                            cx.notify();
                        })
                    })
                    .into_any_element()
            }
            InputKind::Files => {
                let files = self.workflow.files.get(&key).cloned().unwrap_or_default();
                let add = root.clone();
                let add_key = key.clone();
                let mut list = div().flex().flex_wrap().gap_1();
                for (ix, path) in files.iter().enumerate() {
                    let drop = root.clone();
                    let k = key.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("wf-file-{key}-{ix}")))
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded(theme.radius(Radius::Md))
                            .bg(colors.sunken)
                            .text_size(tiny)
                            .child(Icon::new(IconName::FileText).size(IconSize::Xs))
                            .child(
                                path.file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("wf-file-x-{key}-{ix}")))
                                    .cursor_pointer()
                                    .child(
                                        Icon::new(IconName::X)
                                            .size(IconSize::Xs)
                                            .color(colors.fg_subtle),
                                    )
                                    .on_click(move |_, _, cx| {
                                        let k = k.clone();
                                        drop.update(cx, |root, cx| {
                                            if let Some(list) = root.workflow.files.get_mut(&k) {
                                                list.remove(ix);
                                            }
                                            cx.notify();
                                        })
                                    }),
                            ),
                    );
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        DropZone::new(SharedString::from(format!("wf-drop-{key}")))
                            .multiple()
                            .hint(t("Drop the problem PDF and its data here, or browse."))
                            .on_drop(move |paths, _, cx| {
                                let key = add_key.clone();
                                add.update(cx, |root, cx| {
                                    let list = root.workflow.files.entry(key).or_default();
                                    for p in paths {
                                        if !list.contains(&p) {
                                            list.push(p);
                                        }
                                    }
                                    cx.notify();
                                })
                            }),
                    )
                    .child(list)
                    .into_any_element()
            }
            _ => match self.workflow.texts.get(&key) {
                Some(field) => TextField::new(field).into_any_element(),
                None => div().into_any_element(),
            },
        };
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(label)
            .children(help)
            .child(body)
            .when(input.private, |d| {
                d.child(
                    div()
                        .text_size(tiny)
                        .text_color(colors.fg_subtle)
                        .child(t("Only checks read this; it is never sent to an agent.")),
                )
            })
            .into_any_element()
    }

    fn start_workflow(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(template) = self
            .workspace
            .read(cx)
            .workflow_templates()
            .into_iter()
            .find(|e| e.template.id == id)
            .map(|e| e.template)
        else {
            return;
        };
        let mut values = HashMap::new();
        for input in &template.inputs {
            match input.kind {
                InputKind::Choice => {
                    if let Some(v) = self.workflow.choices.get(&input.key) {
                        values.insert(input.key.clone(), v.clone());
                    }
                }
                InputKind::Files => {}
                _ => {
                    if let Some(field) = self.workflow.texts.get(&input.key) {
                        let text = field.read(cx).text().to_string();
                        if !text.trim().is_empty() {
                            values.insert(input.key.clone(), text);
                        }
                    }
                }
            }
        }
        let files = self.workflow.files.clone();
        let skip: Vec<String> = self.workflow.skip.iter().cloned().collect();
        let started = self
            .workspace
            .update(cx, |ws, cx| ws.start_run(id, values, files, skip, cx));
        if started.is_some() {
            self.workflow.reset(cx);
        }
    }

    fn run_view(&mut self, entry: RunEntry, cx: &mut Context<Self>) -> AnyElement {
        // The step the user is most likely to want open.
        let focus = self.workflow.open.get(&entry.dir).cloned().or_else(|| {
            entry
                .run
                .steps
                .iter()
                .find(|s| {
                    !s.expanded
                        && matches!(
                            s.status,
                            StepStatus::Review
                                | StepStatus::Failed
                                | StepStatus::Running
                                | StepStatus::Checking
                        )
                })
                .map(|s| s.id.clone())
        });

        let mut timeline = div().flex().flex_col();
        let count = entry.run.steps.len();
        for (ix, state) in entry.run.steps.iter().enumerate() {
            let open = focus.as_deref() == Some(state.id.as_str());
            timeline = timeline.child(self.step_row(&entry, ix, state, open, ix + 1 == count, cx));
        }

        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);
        let run = &entry.run;
        let dir = entry.dir.clone();
        let (done, total) = run.progress();
        let (state_text, tone) = run_label(run.state);
        let ws = self.workspace.clone();

        let control = |id: &'static str, label: &'static str, icon: IconName| {
            Button::new(id, t(label))
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .icon(icon)
        };
        let mut controls = div().flex().items_center().gap_1();
        match run.state {
            RunState::Running | RunState::Waiting => {
                let (w, d) = (ws.clone(), dir.clone());
                controls = controls.child(
                    control("wf-pause", "Pause", IconName::Pause)
                        .on_click(move |_, _, cx| w.update(cx, |ws, cx| ws.pause_run(&d, cx))),
                );
            }
            RunState::Paused => {
                let (w, d) = (ws.clone(), dir.clone());
                controls =
                    controls
                        .child(control("wf-resume", "Resume", IconName::Play).on_click(
                            move |_, _, cx| w.update(cx, |ws, cx| ws.resume_run(&d, cx)),
                        ));
            }
            _ => {}
        }
        if !matches!(run.state, RunState::Done | RunState::Stopped) {
            let (w, d) = (ws.clone(), dir.clone());
            controls = controls.child(
                control("wf-stop", "Stop", IconName::Square)
                    .on_click(move |_, _, cx| w.update(cx, |ws, cx| ws.stop_run(&d, cx))),
            );
        }
        let reveal = dir.clone();
        controls = controls.child(
            control("wf-folder", "Open folder", IconName::FolderOpen).on_click(move |_, _, _| {
                let _ = crate::opener::reveal(&reveal);
            }),
        );
        let (w, d) = (ws.clone(), dir.clone());
        controls = controls.child(
            control("wf-forget", "Remove from list", IconName::Trash2)
                .on_click(move |_, _, cx| w.update(cx, |ws, cx| ws.forget_run(&d, cx))),
        );

        let header = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text_size(TextSize::Lg))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(entry.title()),
                    )
                    .child(Badge::new(state_text).tone(tone))
                    .child(controls),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().child(ProgressBar::new(
                        "wf-progress",
                        if total == 0 {
                            0.
                        } else {
                            done as f32 / total as f32
                        },
                    )))
                    .child(
                        div()
                            .flex_none()
                            .text_size(tiny)
                            .text_color(colors.fg_muted)
                            .child(tf(
                                "{done} of {total} steps · {elapsed}",
                                &[
                                    ("done", &done),
                                    ("total", &total),
                                    ("elapsed", &minutes(run.updated_ms - run.created_ms)),
                                ],
                            )),
                    ),
            );

        // The paper, once there is one.
        let paper = ["paper/main.pdf", "paper/main.tex"]
            .iter()
            .map(|p| dir.join(p))
            .find(|p| p.exists());
        let paper_card = paper.map(|path| {
            let root = cx.entity();
            div()
                .flex()
                .items_center()
                .gap_3()
                .p_3()
                .rounded(theme.radius(Radius::Lg))
                .border_1()
                .border_color(colors.border)
                .bg(colors.surface)
                .child(
                    Icon::new(IconName::FileText)
                        .size(IconSize::Md)
                        .color(colors.accent),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(small).child(t("The paper")))
                        .child(
                            div()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .truncate()
                                .child(path.display().to_string()),
                        ),
                )
                .child(
                    Button::new("wf-paper", t("Preview"))
                        .size(ControlSize::Sm)
                        .icon(IconName::Eye)
                        .on_click(move |_, window, cx| {
                            let path = path.clone();
                            root.update(cx, |root, cx| root.open_preview(path, window, cx))
                        }),
                )
        });

        let retro = dir.join("reports").join("RETRO.md");
        let retro_card = retro.exists().then(|| {
            let (w, d, root, path) = (ws.clone(), dir.clone(), cx.entity(), retro.clone());
            div()
                .flex()
                .items_center()
                .gap_3()
                .p_3()
                .rounded(theme.radius(Radius::Lg))
                .border_1()
                .border_color(colors.border)
                .bg(colors.surface)
                .child(Icon::new(IconName::BookOpen).size(IconSize::Md).color(colors.accent))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(small).child(t("The retrospective")))
                        .child(
                            div()
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(t("Keep its lessons worth keeping, and the next runs of this template start with them.")),
                        ),
                )
                .child(
                    Button::new("wf-retro-open", t("Preview"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| {
                            let path = path.clone();
                            root.update(cx, |root, cx| root.open_preview(path, window, cx))
                        }),
                )
                .child(
                    Button::new("wf-retro-keep", t("Keep the lessons"))
                        .size(ControlSize::Sm)
                        .icon(IconName::Check)
                        .on_click(move |_, _, cx| w.update(cx, |ws, cx| ws.keep_lessons(&d, cx))),
                )
        });

        div()
            .id("wf-run")
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
                    .py_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(header)
                    .when(!super::workflow::RUNNER_READY, |d| {
                        d.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(theme.radius(Radius::Md))
                                .bg(colors.accent.opacity(0.08))
                                .text_size(tiny)
                                .text_color(colors.fg_muted)
                                .child(t("Running the steps by themselves is being built. Until then this shows the plan, and you can open a step's prompt, approve stops and ask for changes.")),
                        )
                    })
                    .children(paper_card)
                    .children(retro_card)
                    .child(timeline),
            )
            .into_any_element()
    }

    fn step_row(
        &self,
        entry: &RunEntry,
        ix: usize,
        state: &StepRun,
        open: bool,
        last: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);
        let run = &entry.run;
        let dir = entry.dir.clone();
        let Some(step) = run.spec(&state.id).cloned() else {
            return div().into_any_element();
        };
        let step = &step;
        let _ = ix;
        let tries = state.attempts.len();
        let took: i64 = state
            .attempts
            .iter()
            .filter_map(|a| a.finished_ms.map(|f| f - a.started_ms))
            .sum();
        let root = cx.entity();
        let toggle_dir = dir.clone();
        let toggle_id = state.id.clone();

        let mut meta = div().flex().items_center().gap_2().flex_none();
        meta = meta.child(
            Badge::new(agent_name(run.template.agent_of(step).as_deref())).tone(Tone::Neutral),
        );
        if tries > 1 {
            meta = meta.child(
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_muted)
                    .child(tf("try {n}", &[("n", &tries)])),
            );
        }
        if took > 0 {
            meta = meta.child(
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_muted)
                    .child(minutes(took)),
            );
        }

        let head = div()
            .id(SharedString::from(format!("wf-step-{}", state.id)))
            .flex()
            .items_center()
            .gap_3()
            .py_2()
            .cursor_pointer()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(small)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(if state.status == StepStatus::Skipped {
                                        colors.fg_subtle
                                    } else {
                                        colors.fg
                                    })
                                    .child(match &state.item {
                                        Some(item) => item.title.clone(),
                                        None => step.title.clone(),
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(tiny)
                                    .text_color(match state.status {
                                        StepStatus::Review => colors.warning,
                                        StepStatus::Failed => colors.danger,
                                        StepStatus::Done => colors.success,
                                        _ => colors.fg_subtle,
                                    })
                                    .child(status_label(state.status)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(tiny)
                            .text_color(colors.fg_muted)
                            .truncate()
                            .child(if state.expanded {
                                tf(
                                    "{n} sub-questions, each solved on its own",
                                    &[("n", &run.items_of(&state.id).len())],
                                )
                            } else if state.is_item() {
                                String::new()
                            } else {
                                step.summary.clone()
                            }),
                    ),
            )
            .child(meta)
            .on_click(move |_, _, cx| {
                let (d, id) = (toggle_dir.clone(), toggle_id.clone());
                root.update(cx, |root, cx| {
                    if root.workflow.open.get(&d) == Some(&id) {
                        root.workflow.open.insert(d, String::new());
                    } else {
                        root.workflow.open.insert(d, id);
                    }
                    cx.notify();
                })
            });

        // A repeating step does no work itself: its items carry the details.
        let details = (open && !state.expanded).then(|| self.step_details(entry, ix, state, cx));

        // A rail down the left joins the steps; sub-questions sit in.
        div()
            .flex()
            .gap_3()
            .when(state.is_item(), |d| d.pl_6())
            .child(
                div()
                    .flex_none()
                    .w(px(20.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .h(px(12.))
                            .w(px(1.))
                            .when(ix > 0, |d| d.bg(colors.border)),
                    )
                    .child(status_mark(
                        SharedString::from(format!("wf-mark-{}", state.id)),
                        state.status,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .w(px(1.))
                            .when(!last, |d| d.bg(colors.border)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pb_2()
                    .child(head)
                    .children(details),
            )
            .into_any_element()
    }

    fn step_details(
        &self,
        entry: &RunEntry,
        ix: usize,
        state: &StepRun,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let tiny = theme.text_size(TextSize::Xs);
        let small = theme.text_size(TextSize::Sm);
        let run = &entry.run;
        let dir = entry.dir.clone();
        let Some(step) = run.spec(&state.id).cloned() else {
            return div().into_any_element();
        };
        let step = &step;
        let _ = ix;
        let mut column = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .mb_2()
            .rounded(theme.radius(Radius::Lg))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface);

        // What the last try's checks found.
        if let Some(last) = state.last().filter(|a| !a.checks.is_empty()) {
            let mut list = div().flex().flex_col().gap_1();
            for (n, c) in last.checks.iter().enumerate() {
                list = list.child(
                    div()
                        .id(SharedString::from(format!("wf-check-{}-{n}", state.id)))
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_size(tiny)
                        .child(
                            Icon::new(if c.passed {
                                IconName::Check
                            } else {
                                IconName::X
                            })
                            .size(IconSize::Xs)
                            .color(if c.passed {
                                colors.success
                            } else {
                                colors.danger
                            }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().text_color(colors.fg).child(c.what.clone()))
                                .when(!c.detail.is_empty(), |d| {
                                    d.child(
                                        div().text_color(colors.fg_muted).child(c.detail.clone()),
                                    )
                                }),
                        ),
                );
            }
            column = column.child(label_block(t("Checks"), list, cx));
        }

        // The files it should leave, with the ones there to preview.
        let mut files = div().flex().flex_wrap().gap_1();
        let mut any = false;
        let outputs = run.outputs_of(&state.id);
        for output in &outputs {
            for path in atrium_workflow::check::matches(&dir, output)
                .into_iter()
                .take(6)
            {
                any = true;
                let root = cx.entity();
                let name = path
                    .strip_prefix(&dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                let previewable = super::preview::previewable(&path);
                let p = path.clone();
                files = files.child(
                    div()
                        .id(SharedString::from(format!("wf-out-{}-{name}", state.id)))
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .rounded(theme.radius(Radius::Md))
                        .bg(colors.sunken)
                        .text_size(tiny)
                        .cursor_pointer()
                        .hover(|d| d.bg(colors.hover))
                        .child(
                            Icon::new(if previewable {
                                IconName::Eye
                            } else {
                                IconName::FileText
                            })
                            .size(IconSize::Xs),
                        )
                        .child(name)
                        .on_click(move |_, window, cx| {
                            let p = p.clone();
                            if previewable {
                                root.update(cx, |root, cx| root.open_preview(p, window, cx));
                            } else {
                                let _ = crate::opener::open_default(&p);
                            }
                        }),
                );
            }
        }
        if !any && !outputs.is_empty() {
            files = files.child(
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_muted)
                    .child(outputs.join("、")),
            );
        }
        if !outputs.is_empty() {
            column = column.child(label_block(t("Files"), files, cx));
        }

        // The prompt, as the agent gets it.
        if let Some(prompt) = atrium_workflow::render::prompt(
            run,
            &state.id,
            run.template.agent_of(step).as_deref() == Some("claude"),
            if crate::i18n::chinese() {
                atrium_workflow::render::Lang::Zh
            } else {
                atrium_workflow::render::Lang::En
            },
        ) {
            column = column.child(label_block(
                t("What the agent is told"),
                div()
                    .id(SharedString::from(format!("wf-prompt-{}", state.id)))
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(theme.radius(Radius::Md))
                    .bg(colors.sunken)
                    .text_size(tiny)
                    .text_color(colors.fg_muted)
                    .whitespace_normal()
                    .child(prompt),
                cx,
            ));
        }

        // The conversation of its last try.
        if let Some(thread) = state.thread().map(str::to_owned) {
            let ws = self.workspace.clone();
            column = column.child(
                div().child(
                    Button::new(
                        SharedString::from(format!("wf-chat-{}", state.id)),
                        t("Open its conversation"),
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Sm)
                    .icon(IconName::MessageSquare)
                    .on_click(move |_, _, cx| {
                        let thread = thread.clone();
                        ws.update(cx, |ws, cx| {
                            ws.nav += 1;
                            ws.room = Room::Chats;
                            ws.open_thread(thread, cx);
                        })
                    }),
                ),
            );
        }

        // The options of a deciding stop, to pick from; a decision made.
        if let Some(decide) = step.decide.as_ref()
            && state.status == StepStatus::Review
        {
            column = column.child(self.options_block(entry, &state.id, &decide.question, cx));
        }
        if let Some(made) = run.decisions.iter().find(|d| d.step == state.id) {
            let mut text = String::new();
            if let Some(c) = &made.choice {
                text.push_str(&tf("You chose {choice}", &[("choice", &c.title)]));
            }
            if !made.note.is_empty() {
                if !text.is_empty() {
                    text.push_str(if crate::i18n::chinese() { "；" } else { "; " });
                }
                text.push_str(&tf("your note: {note}", &[("note", &made.note)]));
            }
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(small)
                    .text_color(colors.fg)
                    .child(
                        Icon::new(IconName::CircleCheck)
                            .size(IconSize::Sm)
                            .color(colors.success),
                    )
                    .child(text),
            );
        }

        // Decisions.
        let id = state.id.clone();
        let mut actions = div().flex().items_center().gap_2().flex_wrap();
        let note = self.workflow.note.clone();
        let decide = matches!(
            state.status,
            StepStatus::Review | StepStatus::Failed | StepStatus::Done
        );
        if state.status == StepStatus::Review {
            let (w, d, i, r, n) = (
                self.workspace.clone(),
                dir.clone(),
                id.clone(),
                cx.entity(),
                note.clone(),
            );
            actions = actions.child(
                Button::new(
                    SharedString::from(format!("wf-approve-{id}")),
                    t("Looks good, go on"),
                )
                .size(ControlSize::Sm)
                .icon(IconName::Check)
                .on_click(move |_, _, cx| {
                    let text = n.read(cx).text().to_string();
                    let key = format!("{}|{i}", d.display());
                    // The next step that needs the user opens by itself.
                    let choice = r.update(cx, |root, _| {
                        root.workflow.open.remove(&d);
                        root.workflow.picked.get(&key).cloned()
                    });
                    w.update(cx, |ws, cx| ws.approve_step(&d, &i, choice, &text, cx));
                    n.update(cx, |input, cx| input.set_text("", cx));
                }),
            );
        }
        if decide {
            let (w, d, i, n) = (
                self.workspace.clone(),
                dir.clone(),
                id.clone(),
                note.clone(),
            );
            let label = match state.status {
                StepStatus::Failed => t("Try again with this note"),
                StepStatus::Done => t("Redo this step and the ones after it"),
                _ => t("Ask for changes"),
            };
            actions = actions.child(
                Button::new(SharedString::from(format!("wf-redo-{id}")), label)
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Sm)
                    .icon(IconName::RotateCcw)
                    .on_click(move |_, _, cx| {
                        let text = n.read(cx).text().to_string();
                        w.update(cx, |ws, cx| ws.redo_step(&d, &i, &text, cx));
                        n.update(cx, |input, cx| input.set_text("", cx));
                    }),
            );
        }
        if step.optional && matches!(state.status, StepStatus::Failed | StepStatus::Pending) {
            let (w, d, i) = (self.workspace.clone(), dir.clone(), id.clone());
            actions = actions.child(
                Button::new(SharedString::from(format!("wf-skip-{id}")), t("Skip"))
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| w.update(cx, |ws, cx| ws.skip_step(&d, &i, cx))),
            );
        }
        if decide {
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(state.status == StepStatus::Review, |d| {
                        d.child(
                            div()
                                .text_size(small)
                                .text_color(colors.warning)
                                .child(t("This step stops for you. Look at its files, then let the run go on or say what to change.")),
                        )
                    })
                    .child(TextField::new(&note))
                    .child(actions),
            );
        } else if step.optional && state.status == StepStatus::Pending {
            column = column.child(actions);
        }
        column.into_any_element()
    }
}

impl RootView {
    /// The options a deciding step wrote, as cards to pick one from.
    fn options_block(
        &self,
        entry: &RunEntry,
        id: &str,
        question: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);
        let key = format!("{}|{id}", entry.dir.display());
        let picked = self.workflow.picked.get(&key).cloned();
        let options = entry
            .run
            .options_file(id)
            .and_then(|f| std::fs::read_to_string(entry.dir.join(f)).ok())
            .map(|t| atrium_workflow::read_choices(&t))
            .unwrap_or_default();
        let mut list = div().flex().flex_col().gap_2();
        if options.is_empty() {
            list = list.child(div().text_size(tiny).text_color(colors.danger).child(t(
                "The step wrote no options to pick from. Ask for changes to have it write them.",
            )));
        }
        for option in options {
            let on = picked.as_deref() == Some(option.id.as_str());
            let root = cx.entity();
            let (k, choice) = (key.clone(), option.id.clone());
            list = list.child(
                div()
                    .id(SharedString::from(format!("wf-opt-{id}-{}", option.id)))
                    .flex()
                    .items_start()
                    .gap_3()
                    .p_3()
                    .rounded(theme.radius(Radius::Md))
                    .border_1()
                    .border_color(if on { colors.accent } else { colors.border })
                    .when(on, |d| d.bg(colors.accent.opacity(0.06)))
                    .cursor_pointer()
                    .hover(|d| d.border_color(colors.border_strong))
                    .child(
                        div()
                            .mt(px(3.))
                            .flex_none()
                            .size(px(14.))
                            .rounded_full()
                            .border_1()
                            .border_color(if on {
                                colors.accent
                            } else {
                                colors.border_strong
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(on, |d| {
                                d.child(div().size(px(7.)).rounded_full().bg(colors.accent))
                            }),
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
                                    .child(option.title.clone()),
                            )
                            .when(!option.summary.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_size(tiny)
                                        .text_color(colors.fg_muted)
                                        .whitespace_normal()
                                        .child(option.summary.clone()),
                                )
                            }),
                    )
                    .on_click(move |_, _, cx| {
                        let (k, choice) = (k.clone(), choice.clone());
                        root.update(cx, |root, cx| {
                            root.workflow.picked.insert(k, choice);
                            cx.notify();
                        })
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(small)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(if question.is_empty() {
                        t("Pick one").to_string()
                    } else {
                        question.to_string()
                    }),
            )
            .child(list)
            .into_any_element()
    }
}

/// How many earlier runs left lessons for template `id`, when any did.
fn atrium_workflow_experience(id: &str) -> Option<usize> {
    let text = crate::workspace::experience(id)?;
    Some(text.lines().filter(|l| l.starts_with("## ")).count().max(1))
}

/// Whether a runner drives runs; until it does, the run view says so.
pub const RUNNER_READY: bool = false;

fn section(title: &'static str, body: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(theme.text_size(TextSize::Sm))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.colors.fg)
                .child(title),
        )
        .child(body)
        .into_any_element()
}

fn label_block(title: &'static str, body: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(theme.colors.fg_subtle)
                .child(title),
        )
        .child(body)
        .into_any_element()
}
