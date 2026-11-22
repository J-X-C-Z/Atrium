//! The visual template editor: a template's basics, its inputs and its
//! steps with their checks, edited in place and checked as you go. It
//! saves the same TOML a hand-written template has.

use std::collections::HashMap;

use atrium_workflow::{
    Check, Decide, Each, Input, InputKind, Step, StepCheck, Template, author, render::Lang,
};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton, SegmentedControl},
    data_display::{Badge, Tone},
    forms::{Input as TextField, Switch, TextInput},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*,
};

use super::RootView;
use crate::i18n::{t, tf};
use crate::workspace::{AGENT_IDS, Editing, TemplateSource};

/// What the editor holds while it shows.
pub struct TemplateEditor {
    pub opened: Editing,
    /// The template as last laid out; text fields hold newer text.
    draft: Template,
    source: Option<TemplateSource>,
    /// Whether saving drops comments the file has.
    has_comments: bool,
    fields: HashMap<String, Entity<TextInput>>,
    /// Stable keys for inputs, steps and checks, so fields follow them
    /// when they move.
    inputs: Vec<u64>,
    steps: Vec<u64>,
    checks: Vec<Vec<u64>>,
    next: u64,
    open_input: Option<u64>,
    open_step: Option<u64>,
}

const KINDS: &[(&str, &str)] = &[
    ("exists", "File exists"),
    ("contains", "File contains"),
    ("absent", "File leaves out"),
    ("command", "Command succeeds"),
    ("pdf_pages", "PDF pages"),
    ("verdict", "Verdict file"),
    ("review", "Agent review"),
];

fn kind_of(check: &Check) -> &'static str {
    match check {
        Check::Exists { .. } => "exists",
        Check::Contains { .. } => "contains",
        Check::Absent { .. } => "absent",
        Check::Command { .. } => "command",
        Check::PdfPages { .. } => "pdf_pages",
        Check::Verdict { .. } => "verdict",
        Check::Review { .. } => "review",
    }
}

/// A check's fields as text, in the order `fields_of` names them.
fn slots(check: &Check) -> [String; 4] {
    let n = |v: Option<u32>| v.map(|v| v.to_string()).unwrap_or_default();
    let o = |v: &Option<String>| v.clone().unwrap_or_default();
    match check {
        Check::Exists { path, min } => [
            path.clone(),
            min.map(|v| v.to_string()).unwrap_or_default(),
            String::new(),
            String::new(),
        ],
        Check::Contains {
            path,
            pattern,
            message,
        } => [path.clone(), pattern.clone(), o(message), String::new()],
        Check::Absent {
            path,
            pattern,
            input,
            message,
        } => [path.clone(), o(pattern), o(input), o(message)],
        Check::Command {
            run,
            cwd,
            timeout_min,
            message,
        } => [run.clone(), o(cwd), timeout_min.to_string(), o(message)],
        Check::PdfPages { path, max, min } => [path.clone(), n(*min), n(*max), String::new()],
        Check::Verdict { path } => [path.clone(), String::new(), String::new(), String::new()],
        Check::Review { rubric, files, .. } => [
            rubric.clone(),
            files.join("\n"),
            String::new(),
            String::new(),
        ],
    }
}

/// The labels of a kind's fields, and whether each takes several lines.
fn fields_of(kind: &str) -> &'static [(&'static str, bool)] {
    match kind {
        "exists" => &[
            ("File (glob allowed)", false),
            ("At least this many", false),
        ],
        "contains" => &[
            ("File", false),
            ("Pattern (regular expression)", false),
            ("Said when it fails", false),
        ],
        "absent" => &[
            ("Files (glob allowed)", false),
            ("Pattern (regular expression)", false),
            ("Private input whose values must not appear", false),
            ("Said when it fails", false),
        ],
        "command" => &[
            ("Command", false),
            ("Run in folder", false),
            ("Time limit (minutes)", false),
            ("Said when it fails", false),
        ],
        "pdf_pages" => &[
            ("PDF file", false),
            ("At least (pages)", false),
            ("At most (pages)", false),
        ],
        "verdict" => &[("Verdict file", false)],
        "review" => &[
            ("What the reviewer checks", true),
            ("Files it reads, one per line", true),
        ],
        _ => &[],
    }
}

fn opt(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn num<T: std::str::FromStr>(text: &str) -> Option<T> {
    text.trim().parse().ok()
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn check_from(kind: &str, s: &[String; 4], old: &Check) -> Check {
    match kind {
        "exists" => Check::Exists {
            path: s[0].trim().into(),
            min: num(&s[1]),
        },
        "contains" => Check::Contains {
            path: s[0].trim().into(),
            pattern: s[1].trim().into(),
            message: opt(&s[2]),
        },
        "absent" => Check::Absent {
            path: s[0].trim().into(),
            pattern: opt(&s[1]),
            input: opt(&s[2]),
            message: opt(&s[3]),
        },
        "command" => Check::Command {
            run: s[0].trim().into(),
            cwd: opt(&s[1]),
            timeout_min: num(&s[2]).unwrap_or(5),
            message: opt(&s[3]),
        },
        "pdf_pages" => Check::PdfPages {
            path: s[0].trim().into(),
            min: num(&s[1]),
            max: num(&s[2]),
        },
        "verdict" => Check::Verdict {
            path: s[0].trim().into(),
        },
        _ => Check::Review {
            rubric: s[0].trim().into(),
            files: lines(&s[1]),
            agent: match old {
                Check::Review { agent, .. } => agent.clone(),
                _ => None,
            },
        },
    }
}

fn blank_check(kind: &str) -> Check {
    check_from(
        kind,
        &Default::default(),
        &Check::Verdict {
            path: String::new(),
        },
    )
}

fn blank_step(id: String, n: usize) -> Step {
    Step {
        id,
        title: tf("Step {n}", &[("n", &n)]),
        summary: String::new(),
        agent: None,
        effort: None,
        skill: None,
        prompt: String::new(),
        needs: None,
        outputs: Vec::new(),
        checks: Vec::new(),
        gate: false,
        optional: false,
        fixes: Vec::new(),
        decide: None,
        each: None,
        when: None,
        attempts: None,
        timeout_min: None,
    }
}

fn blank_template() -> Template {
    let mut template = Template {
        id: "my-workflow".into(),
        name: t("New workflow").into(),
        description: String::new(),
        version: 1,
        inputs: Vec::new(),
        defaults: Default::default(),
        preamble: String::new(),
        steps: Vec::new(),
    };
    template.steps.push(blank_step("step1".into(), 1));
    template
}

fn lang() -> Lang {
    if crate::i18n::chinese() {
        Lang::Zh
    } else {
        Lang::En
    }
}

fn free_id(taken: &[String], stem: &str) -> String {
    (1..)
        .map(|n| format!("{stem}{n}"))
        .find(|id| !taken.contains(id))
        .unwrap_or_default()
}

impl TemplateEditor {
    pub fn new(opened: Editing, source: Option<(Template, TemplateSource, String)>) -> Self {
        let (draft, source, has_comments) = match source {
            Some((template, source, text)) => {
                let comments = text.lines().any(|l| l.trim_start().starts_with('#'));
                (template, Some(source), comments)
            }
            None => (blank_template(), None, false),
        };
        let mut editor = Self {
            opened,
            draft,
            source,
            has_comments,
            fields: HashMap::new(),
            inputs: Vec::new(),
            steps: Vec::new(),
            checks: Vec::new(),
            next: 0,
            open_input: None,
            open_step: None,
        };
        editor.inputs = (0..editor.draft.inputs.len())
            .map(|_| editor.key())
            .collect();
        editor.steps = (0..editor.draft.steps.len())
            .map(|_| editor.key())
            .collect();
        editor.checks = editor
            .draft
            .steps
            .clone()
            .iter()
            .map(|s| s.checks.iter().map(|_| editor.key()).collect())
            .collect();
        if editor.source.is_none() {
            editor.open_step = editor.steps.first().copied();
        }
        editor
    }

    fn key(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn text(&self, key: &str, fallback: &str, cx: &App) -> String {
        match self.fields.get(key) {
            Some(field) => field.read(cx).text().to_string(),
            None => fallback.to_string(),
        }
    }

    /// The template as it stands, text fields included.
    pub fn current(&self, cx: &App) -> Template {
        let mut out = self.draft.clone();
        let d = &self.draft;
        out.name = self.text("name", &d.name, cx).trim().into();
        out.id = self.text("id", &d.id, cx).trim().into();
        out.description = self.text("desc", &d.description, cx).trim().into();
        out.preamble = self.text("preamble", &d.preamble, cx);
        out.defaults.attempts = num(&self.text("attempts", &d.defaults.attempts.to_string(), cx))
            .unwrap_or(d.defaults.attempts);
        for (ix, u) in self.inputs.iter().enumerate() {
            let old = &d.inputs[ix];
            let f = |name: &str, v: &str| self.text(&format!("i{u}.{name}"), v, cx);
            let input = &mut out.inputs[ix];
            input.key = f("key", &old.key).trim().into();
            input.label = f("label", &old.label).trim().into();
            input.help = f("help", &old.help).trim().into();
            input.default = opt(&f("default", old.default.as_deref().unwrap_or("")));
            input.options = lines(&f("options", &old.options.join("\n")));
            input.when = opt(&f("when", old.when.as_deref().unwrap_or("")));
        }
        let mut renamed = HashMap::new();
        for (ix, u) in self.steps.iter().enumerate() {
            let old = &d.steps[ix];
            let f = |name: &str, v: &str| self.text(&format!("s{u}.{name}"), v, cx);
            let step = &mut out.steps[ix];
            step.id = f("id", &old.id).trim().into();
            if step.id != old.id {
                renamed.insert(old.id.clone(), step.id.clone());
            }
            step.title = f("title", &old.title).trim().into();
            step.summary = f("summary", &old.summary).trim().into();
            step.prompt = f("prompt", &old.prompt);
            step.outputs = lines(&f("outputs", &old.outputs.join("\n")));
            step.when = opt(&f("when", old.when.as_deref().unwrap_or("")));
            step.attempts = num(&f(
                "attempts",
                &old.attempts.map(|a| a.to_string()).unwrap_or_default(),
            ));
            step.each = opt(&f(
                "each",
                old.each.as_ref().map(|e| e.from.as_str()).unwrap_or(""),
            ))
            .map(|from| Each {
                from,
                max: old.each.as_ref().map_or(8, |e| e.max),
            });
            step.decide = opt(&f(
                "decide",
                old.decide
                    .as_ref()
                    .map(|e| e.options.as_str())
                    .unwrap_or(""),
            ))
            .map(|options| Decide {
                options,
                question: f(
                    "question",
                    old.decide
                        .as_ref()
                        .map(|e| e.question.as_str())
                        .unwrap_or(""),
                )
                .trim()
                .into(),
            });
            for (cx_ix, c) in self.checks[ix].iter().enumerate() {
                let old_check = &old.checks[cx_ix].check;
                let kind = kind_of(old_check);
                let old_slots = slots(old_check);
                let s: [String; 4] =
                    std::array::from_fn(|n| self.text(&format!("c{c}.{n}"), &old_slots[n], cx));
                step.checks[cx_ix].check = check_from(kind, &s, old_check);
            }
        }
        // Steps that point at a renamed step follow it.
        if !renamed.is_empty() {
            for step in &mut out.steps {
                if let Some(needs) = &mut step.needs {
                    for n in needs {
                        if let Some(new) = renamed.get(n) {
                            *n = new.clone();
                        }
                    }
                }
                for n in &mut step.fixes {
                    if let Some(new) = renamed.get(n) {
                        *n = new.clone();
                    }
                }
            }
        }
        out
    }

    /// Takes the text fields into the draft, before steps move or go.
    fn settle(&mut self, cx: &App) {
        self.draft = self.current(cx);
    }

    fn field(
        &mut self,
        key: String,
        initial: &str,
        rows: Option<(usize, usize)>,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<RootView>,
    ) -> AnyElement {
        let entity = self
            .fields
            .entry(key)
            .or_insert_with(|| {
                let initial = initial.to_string();
                let placeholder = placeholder.to_string();
                cx.new(|cx| {
                    let mut input = TextInput::new(window, cx).placeholder(placeholder);
                    if let Some((min, max)) = rows {
                        input = input.multi_line(min, max);
                    }
                    input.set_text(initial, cx);
                    input
                })
            })
            .clone();
        TextField::new(&entity).into_any_element()
    }
}

/// Runs `f` on the editor and repaints.
fn edit(
    root: &Entity<RootView>,
    f: impl Fn(&mut TemplateEditor, &App) + 'static,
) -> impl Fn(&mut App) + 'static {
    let root = root.clone();
    move |cx: &mut App| {
        root.update(cx, |root, cx| {
            if let Some(editor) = root.editor.as_mut() {
                f(editor, cx);
            }
            cx.notify();
        })
    }
}

fn labelled(label: &str, body: AnyElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(theme.colors.fg_muted)
                .child(label.to_string()),
        )
        .child(body)
        .into_any_element()
}

fn chip(
    id: SharedString,
    label: String,
    on: bool,
    click: impl Fn(&mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .text_size(theme.text_size(TextSize::Xs))
        .cursor_pointer()
        .when(on, |d| {
            d.border_color(colors.accent)
                .bg(colors.accent.opacity(0.12))
                .text_color(colors.fg)
        })
        .when(!on, |d| {
            d.border_color(colors.border)
                .text_color(colors.fg_muted)
                .hover(|d| d.bg(colors.hover))
        })
        .child(label)
        .on_click(move |_, _, cx| click(cx))
        .into_any_element()
}

fn agent_label(id: &str) -> String {
    if id.is_empty() {
        t("Default").to_string()
    } else {
        crate::agents::backend_name(id).to_string()
    }
}

/// Up, down and remove for an entry of a list.
fn row_tools(
    prefix: &str,
    ix: usize,
    len: usize,
    root: &Entity<RootView>,
    up: impl Fn(&mut TemplateEditor, &App, usize) + Clone + 'static,
    remove: impl Fn(&mut TemplateEditor, &App, usize) + 'static,
) -> AnyElement {
    let (a, b) = (up.clone(), up);
    let (go_up, go_down, go_away) = (
        edit(root, move |e, cx| a(e, cx, ix)),
        edit(root, move |e, cx| b(e, cx, ix + 1)),
        edit(root, move |e, cx| remove(e, cx, ix)),
    );
    div()
        .flex()
        .items_center()
        .gap_0p5()
        .child(
            IconButton::new(
                SharedString::from(format!("{prefix}-up-{ix}")),
                IconName::ArrowUp,
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .disabled(ix == 0)
            .tooltip(t("Move up"))
            .on_click(move |_, _, cx| go_up(cx)),
        )
        .child(
            IconButton::new(
                SharedString::from(format!("{prefix}-down-{ix}")),
                IconName::ArrowDown,
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .disabled(ix + 1 >= len)
            .tooltip(t("Move down"))
            .on_click(move |_, _, cx| go_down(cx)),
        )
        .child(
            IconButton::new(
                SharedString::from(format!("{prefix}-rm-{ix}")),
                IconName::Trash2,
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .tooltip(t("Remove"))
            .on_click(move |_, _, cx| go_away(cx)),
        )
        .into_any_element()
}

impl RootView {
    pub(super) fn editor_view(
        &mut self,
        editing: Editing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.editor.as_ref().map(|e| &e.opened) != Some(&editing) {
            let source = editing
                .id
                .as_deref()
                .and_then(|id| self.workspace.read(cx).template_source(id));
            self.editor = Some(TemplateEditor::new(editing.clone(), source));
        }
        let root = cx.entity();
        let mut editor = self.editor.take().expect("set above");
        let body = editor_body(&mut editor, &root, &self.workspace, window, cx);
        self.editor = Some(editor);
        body
    }
}

fn editor_body(
    e: &mut TemplateEditor,
    root: &Entity<RootView>,
    workspace: &Entity<crate::workspace::Workspace>,
    window: &mut Window,
    cx: &mut Context<RootView>,
) -> AnyElement {
    let current = e.current(cx);
    let report = author::inspect(&current.to_toml(), AGENT_IDS, lang());

    // Fields first: they need the window, the rest only reads.
    let d = e.draft.clone();
    let name = e.field("name".into(), &d.name, None, t("Name"), window, cx);
    let id = e.field("id".into(), &d.id, None, "my-workflow", window, cx);
    let desc = e.field(
        "desc".into(),
        &d.description,
        Some((2, 4)),
        t("What it does, in a sentence or two."),
        window,
        cx,
    );
    let preamble = e.field(
        "preamble".into(),
        &d.preamble,
        Some((3, 10)),
        t("Rules every step's agent is told first, such as the language to write in."),
        window,
        cx,
    );
    let attempts = e.field(
        "attempts".into(),
        &d.defaults.attempts.to_string(),
        None,
        "2",
        window,
        cx,
    );

    let (colors, small, tiny, xl, r_md, r_lg) = {
        let theme = cx.theme();
        (
            theme.colors.clone(),
            theme.text_size(TextSize::Sm),
            theme.text_size(TextSize::Xs),
            theme.text_size(TextSize::Xl),
            theme.radius(Radius::Md),
            theme.radius(Radius::Lg),
        )
    };
    let card = |id: SharedString| {
        div()
            .id(id)
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(r_lg)
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
    };

    // --- Basics.
    let agent_pick = {
        let mut control =
            SegmentedControl::new("wfe-agent", d.defaults.agent.clone().unwrap_or_default())
                .size(ControlSize::Sm);
        for a in std::iter::once("").chain(AGENT_IDS.iter().copied()) {
            control = control.segment(a.to_string(), agent_label(a), None);
        }
        let set = root.clone();
        control
            .on_change(move |value, _, cx| {
                let value = value.to_string();
                edit(&set, move |e, _| e.draft.defaults.agent = opt(&value))(cx)
            })
            .into_any_element()
    };
    let basics = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .gap_3()
                .child(div().flex_1().child(labelled(t("Name"), name, cx)))
                .child(div().w(gpui::px(200.)).child(labelled(
                    t("Id (letters, digits, - and _)"),
                    id,
                    cx,
                ))),
        )
        .child(labelled(t("Description"), desc, cx))
        .child(labelled(t("Shared rules for every step"), preamble, cx))
        .child(
            div()
                .flex()
                .gap_3()
                .items_end()
                .child(div().flex_1().child(labelled(
                    t("Agent unless a step says"),
                    agent_pick,
                    cx,
                )))
                .child(
                    div()
                        .w(gpui::px(120.))
                        .child(labelled(t("Tries per step"), attempts, cx)),
                ),
        );

    // --- Inputs.
    let mut inputs = div().flex().flex_col().gap_2();
    let n_inputs = e.inputs.len();
    for ix in 0..n_inputs {
        let u = e.inputs[ix];
        let input = current.inputs[ix].clone();
        let open = e.open_input == Some(u);
        let toggle = edit(root, move |e, _| {
            e.open_input = if e.open_input == Some(u) {
                None
            } else {
                Some(u)
            };
        });
        let tools = row_tools(
            "wfe-in",
            ix,
            n_inputs,
            root,
            |e, cx, at| {
                if at == 0 || at >= e.inputs.len() {
                    return;
                }
                e.settle(cx);
                e.inputs.swap(at - 1, at);
                e.draft.inputs.swap(at - 1, at);
            },
            |e, cx, at| {
                e.settle(cx);
                e.inputs.remove(at);
                e.draft.inputs.remove(at);
            },
        );
        let head = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id(SharedString::from(format!("wfe-in-head-{u}")))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(IconSize::Xs)
                        .color(colors.fg_subtle),
                    )
                    .child(
                        div()
                            .text_size(small)
                            .truncate()
                            .child(if input.label.is_empty() {
                                input.key.clone()
                            } else {
                                input.label.clone()
                            }),
                    )
                    .child(
                        Badge::new(super::workflow_author::kind_label(input.kind))
                            .tone(Tone::Neutral),
                    )
                    .when(input.required, |d| {
                        d.child(Badge::new(t("Required")).tone(Tone::Warning))
                    })
                    .child(
                        div()
                            .text_size(tiny)
                            .text_color(colors.fg_subtle)
                            .child(format!("{{{{input.{}}}}}", input.key)),
                    )
                    .on_click(move |_, _, cx| toggle(cx)),
            )
            .child(tools);
        let mut body = card(SharedString::from(format!("wfe-in-{u}"))).child(head);
        if open {
            let old = &d.inputs[ix];
            let key = e.field(format!("i{u}.key"), &old.key, None, "topic", window, cx);
            let label = e.field(format!("i{u}.label"), &old.label, None, "", window, cx);
            let help = e.field(format!("i{u}.help"), &old.help, None, "", window, cx);
            let default = e.field(
                format!("i{u}.default"),
                old.default.as_deref().unwrap_or(""),
                None,
                "",
                window,
                cx,
            );
            let when = e.field(
                format!("i{u}.when"),
                old.when.as_deref().unwrap_or(""),
                None,
                t("For example contest=CUMCM"),
                window,
                cx,
            );
            let mut kind = SegmentedControl::new(
                SharedString::from(format!("wfe-in-kind-{u}")),
                format!("{:?}", input.kind),
            )
            .size(ControlSize::Sm);
            for k in [
                InputKind::Text,
                InputKind::Long,
                InputKind::Files,
                InputKind::Choice,
                InputKind::Number,
            ] {
                kind = kind.segment(
                    format!("{k:?}"),
                    super::workflow_author::kind_label(k).to_string(),
                    None,
                );
            }
            let set_kind = root.clone();
            let kind = kind.on_change(move |value, _, cx| {
                let k = match value.as_ref() {
                    "Long" => InputKind::Long,
                    "Files" => InputKind::Files,
                    "Choice" => InputKind::Choice,
                    "Number" => InputKind::Number,
                    _ => InputKind::Text,
                };
                edit(&set_kind, move |e, _| {
                    if let Some(i) = e.inputs.iter().position(|x| *x == u) {
                        e.draft.inputs[i].kind = k;
                    }
                })(cx)
            });
            let flag = |name: &'static str, on: bool, label: &'static str| {
                let set = root.clone();
                Switch::new(SharedString::from(format!("wfe-in-{name}-{u}")), on)
                    .label(t(label))
                    .on_change(move |on, _, cx| {
                        edit(&set, move |e, _| {
                            if let Some(i) = e.inputs.iter().position(|x| *x == u) {
                                match name {
                                    "required" => e.draft.inputs[i].required = on,
                                    _ => e.draft.inputs[i].private = on,
                                }
                            }
                        })(cx)
                    })
            };
            body = body
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().child(labelled(t("Label"), label, cx)))
                        .child(div().w(gpui::px(180.)).child(labelled(
                            t("Key, for {{input.key}}"),
                            key,
                            cx,
                        ))),
                )
                .child(labelled(t("Kind"), kind.into_any_element(), cx))
                .child(labelled(t("Help shown under it"), help, cx));
            if input.kind == InputKind::Choice {
                let options = e.field(
                    format!("i{u}.options"),
                    &old.options.join("\n"),
                    Some((2, 6)),
                    t("One option per line; the first is the default."),
                    window,
                    cx,
                );
                body = body.child(labelled(t("Options"), options, cx));
            }
            body = body
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().child(labelled(t("Default"), default, cx)))
                        .child(div().flex_1().child(labelled(
                            t("Only when (another input=value)"),
                            when,
                            cx,
                        ))),
                )
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(flag("required", input.required, "Required"))
                        .child(flag(
                            "private",
                            input.private,
                            "Private: only checks read it",
                        )),
                );
        }
        inputs = inputs.child(body);
    }
    let add_input = edit(root, |e, cx| {
        e.settle(cx);
        let taken: Vec<String> = e.draft.inputs.iter().map(|i| i.key.clone()).collect();
        let key = free_id(&taken, "input");
        e.draft.inputs.push(Input {
            key: key.clone(),
            label: key,
            help: String::new(),
            kind: InputKind::Text,
            required: false,
            options: Vec::new(),
            default: None,
            private: false,
            when: None,
        });
        let u = e.key();
        e.inputs.push(u);
        e.open_input = Some(u);
    });
    inputs = inputs.child(
        div().child(
            Button::new("wfe-add-input", t("Add an input"))
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .icon(IconName::Plus)
                .on_click(move |_, _, cx| add_input(cx)),
        ),
    );

    // --- Steps.
    let mut steps = div().flex().flex_col().gap_2();
    let n_steps = e.steps.len();
    for ix in 0..n_steps {
        let u = e.steps[ix];
        let step = current.steps[ix].clone();
        let open = e.open_step == Some(u);
        let toggle = edit(root, move |e, _| {
            e.open_step = if e.open_step == Some(u) {
                None
            } else {
                Some(u)
            };
        });
        let tools = row_tools(
            "wfe-st",
            ix,
            n_steps,
            root,
            |e, cx, at| {
                if at == 0 || at >= e.steps.len() {
                    return;
                }
                e.settle(cx);
                e.steps.swap(at - 1, at);
                e.checks.swap(at - 1, at);
                e.draft.steps.swap(at - 1, at);
            },
            |e, cx, at| {
                e.settle(cx);
                let gone = e.draft.steps[at].id.clone();
                e.steps.remove(at);
                e.checks.remove(at);
                e.draft.steps.remove(at);
                for s in &mut e.draft.steps {
                    if let Some(needs) = &mut s.needs {
                        needs.retain(|n| *n != gone);
                    }
                    s.fixes.retain(|n| *n != gone);
                }
            },
        );
        let mut badges = div().flex().items_center().gap_1();
        badges = badges.child(
            Badge::new(agent_label(step.agent.as_deref().unwrap_or(""))).tone(Tone::Neutral),
        );
        if step.gate {
            badges = badges.child(Badge::new(t("Stops for you")).tone(Tone::Warning));
        }
        if step.decide.is_some() {
            badges = badges.child(Badge::new(t("You choose")).tone(Tone::Warning));
        }
        if step.each.is_some() {
            badges = badges.child(Badge::new(t("Once per item")).tone(Tone::Info));
        }
        if step.optional {
            badges = badges.child(Badge::new(t("Optional")).tone(Tone::Neutral));
        }
        let head = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id(SharedString::from(format!("wfe-st-head-{u}")))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(IconSize::Xs)
                        .color(colors.fg_subtle),
                    )
                    .child(
                        div()
                            .text_size(tiny)
                            .text_color(colors.fg_subtle)
                            .child(format!("{}", ix + 1)),
                    )
                    .child(
                        div()
                            .text_size(small)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .truncate()
                            .child(step.title.clone()),
                    )
                    .child(badges)
                    .child(div().text_size(tiny).text_color(colors.fg_subtle).child(tf(
                        "{n} checks",
                        &[("n", &(step.checks.len() + step.outputs.len()))],
                    )))
                    .on_click(move |_, _, cx| toggle(cx)),
            )
            .child(tools);
        let mut body = card(SharedString::from(format!("wfe-st-{u}"))).child(head);
        if open {
            let old = d.steps[ix].clone();
            let f_title = e.field(format!("s{u}.title"), &old.title, None, "", window, cx);
            let f_id = e.field(format!("s{u}.id"), &old.id, None, "", window, cx);
            let f_summary = e.field(format!("s{u}.summary"), &old.summary, None, "", window, cx);
            let f_prompt = e.field(
                format!("s{u}.prompt"),
                &old.prompt,
                Some((5, 16)),
                t("What the agent does in this step. {{input.key}} puts in what the user gave."),
                window,
                cx,
            );
            let f_outputs = e.field(
                format!("s{u}.outputs"),
                &old.outputs.join("\n"),
                Some((2, 6)),
                t("One per line, such as paper/main.pdf"),
                window,
                cx,
            );
            let f_attempts = e.field(
                format!("s{u}.attempts"),
                &old.attempts.map(|a| a.to_string()).unwrap_or_default(),
                None,
                t("As the template says"),
                window,
                cx,
            );
            let f_when = e.field(
                format!("s{u}.when"),
                old.when.as_deref().unwrap_or(""),
                None,
                t("For example contest=CUMCM"),
                window,
                cx,
            );
            let f_each = e.field(
                format!("s{u}.each"),
                old.each.as_ref().map(|x| x.from.as_str()).unwrap_or(""),
                None,
                t("For example reports/questions.json"),
                window,
                cx,
            );
            let f_decide = e.field(
                format!("s{u}.decide"),
                old.decide
                    .as_ref()
                    .map(|x| x.options.as_str())
                    .unwrap_or(""),
                None,
                t("For example reports/options.json"),
                window,
                cx,
            );
            let f_question = e.field(
                format!("s{u}.question"),
                old.decide
                    .as_ref()
                    .map(|x| x.question.as_str())
                    .unwrap_or(""),
                None,
                t("What you are asked at the stop"),
                window,
                cx,
            );

            let mut agent = SegmentedControl::new(
                SharedString::from(format!("wfe-st-agent-{u}")),
                step.agent.clone().unwrap_or_default(),
            )
            .size(ControlSize::Sm);
            for a in std::iter::once("").chain(AGENT_IDS.iter().copied()) {
                agent = agent.segment(a.to_string(), agent_label(a), None);
            }
            let set_agent = root.clone();
            let agent = agent.on_change(move |value, _, cx| {
                let value = value.to_string();
                edit(&set_agent, move |e, _| {
                    if let Some(i) = e.steps.iter().position(|x| *x == u) {
                        e.draft.steps[i].agent = opt(&value);
                    }
                })(cx)
            });

            // Needs and send-back targets: earlier steps, as chips.
            let earlier: Vec<(String, String)> = current.steps[..ix]
                .iter()
                .map(|s| (s.id.clone(), s.title.clone()))
                .collect();
            let mut needs = div().flex().flex_wrap().gap_1();
            if ix > 0 {
                needs = needs.child(chip(
                    SharedString::from(format!("wfe-need-prev-{u}")),
                    t("The step before").to_string(),
                    step.needs.is_none(),
                    edit(root, move |e, _| {
                        if let Some(i) = e.steps.iter().position(|x| *x == u) {
                            e.draft.steps[i].needs = None;
                        }
                    }),
                    cx,
                ));
            }
            needs = needs.child(chip(
                SharedString::from(format!("wfe-need-none-{u}")),
                t("Nothing: starts at once").to_string(),
                step.needs.as_ref().is_some_and(|n| n.is_empty()),
                edit(root, move |e, _| {
                    if let Some(i) = e.steps.iter().position(|x| *x == u) {
                        e.draft.steps[i].needs = Some(Vec::new());
                    }
                }),
                cx,
            ));
            let mut fixes = div().flex().flex_wrap().gap_1();
            for (n, (sid, title)) in earlier.iter().enumerate() {
                let on = step.needs.as_ref().is_some_and(|v| v.contains(sid));
                let (a, b) = (sid.clone(), sid.clone());
                needs = needs.child(chip(
                    SharedString::from(format!("wfe-need-{u}-{n}")),
                    title.clone(),
                    on,
                    edit(root, move |e, cx| {
                        e.settle(cx);
                        if let Some(i) = e.steps.iter().position(|x| *x == u) {
                            let list = e.draft.steps[i].needs.get_or_insert_with(Vec::new);
                            if let Some(p) = list.iter().position(|x| *x == a) {
                                list.remove(p);
                            } else {
                                list.push(a.clone());
                            }
                        }
                    }),
                    cx,
                ));
                fixes = fixes.child(chip(
                    SharedString::from(format!("wfe-fix-{u}-{n}")),
                    title.clone(),
                    step.fixes.contains(sid),
                    edit(root, move |e, cx| {
                        e.settle(cx);
                        if let Some(i) = e.steps.iter().position(|x| *x == u) {
                            let list = &mut e.draft.steps[i].fixes;
                            if let Some(p) = list.iter().position(|x| *x == b) {
                                list.remove(p);
                            } else {
                                list.push(b.clone());
                            }
                        }
                    }),
                    cx,
                ));
            }

            let flag = |name: &'static str, on: bool, label: &'static str| {
                let set = root.clone();
                Switch::new(SharedString::from(format!("wfe-st-{name}-{u}")), on)
                    .label(t(label))
                    .on_change(move |on, _, cx| {
                        edit(&set, move |e, _| {
                            if let Some(i) = e.steps.iter().position(|x| *x == u) {
                                match name {
                                    "gate" => e.draft.steps[i].gate = on,
                                    _ => e.draft.steps[i].optional = on,
                                }
                            }
                        })(cx)
                    })
            };

            // Checks.
            let mut checks = div().flex().flex_col().gap_2();
            let n_checks = e.checks[ix].len();
            for (c_ix, c) in e.checks[ix].clone().into_iter().enumerate() {
                let check = step.checks[c_ix].check.clone();
                let kind = kind_of(&check);
                let old_slots = slots(&d.steps[ix].checks[c_ix].check);
                let mut pick = SegmentedControl::new(
                    SharedString::from(format!("wfe-ck-kind-{c}")),
                    kind.to_string(),
                )
                .size(ControlSize::Sm);
                for (k, label) in KINDS {
                    pick = pick.segment(k.to_string(), t(label).to_string(), None);
                }
                let set_kind = root.clone();
                let pick = pick.on_change(move |value, _, cx| {
                    let value = value.to_string();
                    edit(&set_kind, move |e, cx| {
                        e.settle(cx);
                        let Some(i) = e.steps.iter().position(|x| *x == u) else {
                            return;
                        };
                        let Some(j) = e.checks[i].iter().position(|x| *x == c) else {
                            return;
                        };
                        if kind_of(&e.draft.steps[i].checks[j].check) == value {
                            return;
                        }
                        e.draft.steps[i].checks[j].check = blank_check(&value);
                        e.fields.retain(|k, _| !k.starts_with(&format!("c{c}.")));
                    })(cx)
                });
                let remove = edit(root, move |e, cx| {
                    e.settle(cx);
                    let Some(i) = e.steps.iter().position(|x| *x == u) else {
                        return;
                    };
                    if let Some(j) = e.checks[i].iter().position(|x| *x == c) {
                        e.checks[i].remove(j);
                        e.draft.steps[i].checks.remove(j);
                    }
                });
                let mut fields = div().flex().flex_col().gap_2();
                for (n, (label, multi)) in fields_of(kind).iter().enumerate() {
                    let field = e.field(
                        format!("c{c}.{n}"),
                        &old_slots[n],
                        multi.then_some((2, 6)),
                        "",
                        window,
                        cx,
                    );
                    fields = fields.child(labelled(t(label), field, cx));
                }
                checks = checks.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_2()
                        .rounded(r_md)
                        .bg(colors.sunken)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().flex_1().min_w_0().child(pick))
                                .child(
                                    IconButton::new(
                                        SharedString::from(format!("wfe-ck-rm-{c}")),
                                        IconName::Trash2,
                                    )
                                    .variant(ButtonVariant::Ghost)
                                    .size(ControlSize::Sm)
                                    .tooltip(t("Remove"))
                                    .on_click(move |_, _, cx| remove(cx)),
                                ),
                        )
                        .child(fields),
                );
            }
            let _ = n_checks;
            let add_check = edit(root, move |e, cx| {
                e.settle(cx);
                let Some(i) = e.steps.iter().position(|x| *x == u) else {
                    return;
                };
                e.draft.steps[i].checks.push(StepCheck {
                    check: blank_check("exists"),
                    when: None,
                });
                let k = e.key();
                e.checks[i].push(k);
            });
            checks = checks.child(
                div().child(
                    Button::new(
                        SharedString::from(format!("wfe-add-ck-{u}")),
                        t("Add a check"),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Sm)
                    .icon(IconName::Plus)
                    .on_click(move |_, _, cx| add_check(cx)),
                ),
            );

            body = body
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().child(labelled(t("Title"), f_title, cx)))
                        .child(div().w(gpui::px(160.)).child(labelled(t("Id"), f_id, cx))),
                )
                .child(labelled(t("One line on what it is for"), f_summary, cx))
                .child(labelled(t("What the agent is told"), f_prompt, cx))
                .child(labelled(t("Who does it"), agent.into_any_element(), cx))
                .child(labelled(t("Starts after"), needs.into_any_element(), cx))
                .child(labelled(t("Files it must leave"), f_outputs, cx))
                .child(labelled(t("Checks"), checks.into_any_element(), cx))
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(flag("gate", step.gate, "Stop for me when it passes"))
                        .child(flag(
                            "optional",
                            step.optional,
                            "Can be skipped at the start",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().child(labelled(
                            t("Once per item of (a list file an earlier step writes)"),
                            f_each,
                            cx,
                        )))
                        .child(
                            div()
                                .w(gpui::px(140.))
                                .child(labelled(t("Tries"), f_attempts, cx)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(div().flex_1().child(labelled(
                            t("You choose from (an options file it writes)"),
                            f_decide,
                            cx,
                        )))
                        .child(
                            div()
                                .flex_1()
                                .child(labelled(t("Question"), f_question, cx)),
                        ),
                )
                .child(labelled(t("Only when (input=value)"), f_when, cx));
            if !earlier.is_empty() {
                body = body.child(labelled(
                    t("When its checks fail, send the faults back to"),
                    fixes.into_any_element(),
                    cx,
                ));
            }
        }
        steps = steps.child(body);
    }
    let add_step = edit(root, |e, cx| {
        e.settle(cx);
        let taken: Vec<String> = e.draft.steps.iter().map(|s| s.id.clone()).collect();
        let id = free_id(&taken, "step");
        let n = e.draft.steps.len() + 1;
        e.draft.steps.push(blank_step(id, n));
        let u = e.key();
        e.steps.push(u);
        e.checks.push(Vec::new());
        e.open_step = Some(u);
    });
    steps = steps.child(
        div().child(
            Button::new("wfe-add-step", t("Add a step"))
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .icon(IconName::Plus)
                .on_click(move |_, _, cx| add_step(cx)),
        ),
    );

    // --- What Atrium makes of it, and saving.
    let (status, tone) = match (&report.error, report.warnings.len()) {
        (Some(_), _) => (t("Cannot be saved yet").to_string(), Tone::Danger),
        (None, 0) => (t("Ready to save").to_string(), Tone::Success),
        (None, n) => (tf("{n} things to look at", &[("n", &n)]), Tone::Warning),
    };
    let mut findings = div().flex().flex_col().gap_1();
    for (n, line) in report
        .error
        .iter()
        .chain(report.warnings.iter())
        .enumerate()
    {
        let error = n == 0 && report.error.is_some();
        findings = findings.child(
            div()
                .flex()
                .gap_2()
                .text_size(tiny)
                .text_color(if error {
                    colors.danger
                } else {
                    colors.fg_muted
                })
                .child(
                    Icon::new(if error {
                        IconName::CircleAlert
                    } else {
                        IconName::TriangleAlert
                    })
                    .size(IconSize::Xs)
                    .color(if error { colors.danger } else { colors.warning }),
                )
                .child(div().flex_1().child(line.clone())),
        );
    }
    let was = e.opened.id.clone();
    let save_root = root.clone();
    let save_ws = workspace.clone();
    let can_save = report.error.is_none();
    let cancel = workspace.clone();
    let file = match &e.source {
        Some(TemplateSource::User(p) | TemplateSource::Folder(p)) => Some(p.clone()),
        _ => None,
    };
    let note = match (&e.source, e.has_comments) {
        (Some(TemplateSource::Builtin), _) => Some(t(
            "This is built in: saving keeps your version beside it, used in its place.",
        )),
        (Some(_), true) => Some(t(
            "Saving writes the file afresh; the comments in it are not kept.",
        )),
        _ => None,
    };
    let footer = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(r_lg)
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(Badge::new(status).tone(tone))
                .child(div().flex_1())
                .when_some(file, |d, path| {
                    d.child(
                        Button::new("wfe-open-file", t("Open the file"))
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .icon(IconName::FileCode)
                            .on_click(move |_, _, _| {
                                let _ = crate::opener::open_default(&path);
                            }),
                    )
                })
                .child(
                    Button::new("wfe-cancel", t("Cancel"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, |ws, cx| ws.enter_workflows(None, cx))
                        }),
                )
                .child(
                    Button::new("wfe-save", t("Save"))
                        .primary()
                        .size(ControlSize::Sm)
                        .icon(IconName::Save)
                        .disabled(!can_save)
                        .on_click(move |_, _, cx| {
                            let template =
                                save_root.read(cx).editor.as_ref().map(|e| e.current(cx));
                            if let Some(template) = template {
                                let was = was.clone();
                                save_ws.update(cx, |ws, cx| {
                                    ws.save_template(&template, was.as_deref(), cx)
                                });
                            }
                        }),
                ),
        )
        .child(findings)
        .when_some(note, |d, note| {
            d.child(
                div()
                    .text_size(tiny)
                    .text_color(colors.fg_subtle)
                    .child(note),
            )
        });

    let title = match &e.opened.id {
        Some(_) => tf("Edit {name}", &[("name", &current.name)]),
        None => t("New workflow").to_string(),
    };
    let section = |title: &'static str, hint: &'static str, body: AnyElement| {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(small)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(t(title)),
            )
            .when(!hint.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(tiny)
                        .text_color(colors.fg_muted)
                        .child(t(hint)),
                )
            })
            .child(body)
    };

    div()
        .id("wf-editor")
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
                .max_w(gpui::px(760.))
                .px_6()
                .py_8()
                .flex()
                .flex_col()
                .gap_6()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Icon::new(IconName::Workflow).size(IconSize::Md).color(colors.accent))
                        .child(
                            div()
                                .text_size(xl)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(title),
                        ),
                )
                .child(section("About", "", basics.into_any_element()))
                .child(section(
                    "What to start from",
                    "What the user gives when a run starts. Prompts use them as {{input.key}}.",
                    inputs.into_any_element(),
                ))
                .child(section(
                    "Steps",
                    "Each step is one agent conversation. It is done when its files exist and its checks pass.",
                    steps.into_any_element(),
                ))
                .child(footer),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_check_kind_survives_its_fields() {
        for (kind, _) in KINDS {
            let check = blank_check(kind);
            assert_eq!(kind_of(&check), *kind);
            let back = check_from(kind, &slots(&check), &check);
            assert_eq!(back, check, "{kind}");
        }
        let command = Check::Command {
            run: "xelatex main.tex".into(),
            cwd: Some("paper".into()),
            timeout_min: 10,
            message: Some("compiles".into()),
        };
        assert_eq!(check_from("command", &slots(&command), &command), command);
        let review = Check::Review {
            rubric: "Is it right?".into(),
            files: vec!["a.md".into(), "b.md".into()],
            agent: Some("claude".into()),
        };
        assert_eq!(check_from("review", &slots(&review), &review), review);
    }

    #[test]
    fn a_new_template_is_almost_ready() {
        let template = blank_template();
        // Only the empty prompt keeps it from saving.
        let report = author::inspect(&template.to_toml(), AGENT_IDS, Lang::En);
        assert!(report.error.is_some());
        let mut filled = template.clone();
        filled.steps[0].prompt = "Write notes.md".into();
        filled.steps[0].outputs = vec!["notes.md".into()];
        filled.steps[0].gate = true;
        let report = author::inspect(&filled.to_toml(), AGENT_IDS, Lang::En);
        assert!(report.error.is_none(), "{:?}", report.error);
    }

    #[test]
    fn the_builtin_template_reads_back_the_same() {
        for (id, text) in atrium_workflow::builtin::SOURCES {
            let template = Template::parse(text).unwrap();
            let editor = TemplateEditor::new(
                Editing {
                    id: Some(id.to_string()),
                    nonce: 0,
                },
                Some((template.clone(), TemplateSource::Builtin, text.to_string())),
            );
            let again = Template::parse(&editor.draft.to_toml()).unwrap();
            assert_eq!(again, template);
        }
    }
}
