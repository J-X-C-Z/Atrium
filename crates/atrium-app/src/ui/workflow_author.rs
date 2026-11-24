//! Writing a workflow template with an agent: the form that says what the
//! workflow should do, and, beside the authoring chat, what Atrium makes
//! of the draft so far — whether it reads, what will not work as meant,
//! and the inputs and steps a run would have.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    time::SystemTime,
};

use atrium_workflow::{InputKind, Template, author, author::Report};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    data_display::{Badge, Tone},
    forms::{DropZone, Input as TextField, TextInput},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::RootView;
use crate::i18n::{t, tf};
use crate::workspace::{AgentStatus, Authoring, Workspace, draft_of, draft_report};

/// A draft folder, its file's time, and what was made of it.
type Cached = (PathBuf, Option<SystemTime>, Option<Report>);

/// The form's own state, and the last draft read.
pub struct AuthorForm {
    pub request: Entity<TextInput>,
    pub references: Vec<PathBuf>,
    /// The agent picked; the first one up when `None`.
    pub agent: Option<String>,
    /// The draft chat whose panel was opened, so it opens once.
    pub followed: Option<String>,
    /// Whether the draft file is being watched for changes made outside a
    /// turn (the user's own edits).
    watching: bool,
    cache: RefCell<Option<Cached>>,
}

impl AuthorForm {
    pub fn new(window: &mut Window, cx: &mut Context<RootView>) -> Self {
        Self {
            request: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multi_line(4, 10)
                    .placeholder(t("What goes in, what should come out, and where you want to check before it goes on."))
            }),
            references: Vec::new(),
            agent: None,
            followed: None,
            watching: false,
            cache: RefCell::new(None),
        }
    }

    /// The draft's report, read again only when the file changed.
    fn report(&self, dir: &Path) -> Option<Report> {
        let file = dir.join(author::DRAFT_FILE);
        let mtime = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        let mut cache = self.cache.borrow_mut();
        if let Some((d, m, report)) = cache.as_ref()
            && d == dir
            && *m == mtime
        {
            return report.clone();
        }
        let report = draft_report(dir);
        *cache = Some((dir.to_path_buf(), mtime, report.clone()));
        report
    }

    /// Whether the draft file changed since it was last read.
    fn stale(&self, dir: &std::path::Path) -> bool {
        let mtime = std::fs::metadata(dir.join(author::DRAFT_FILE))
            .and_then(|m| m.modified())
            .ok();
        match self.cache.borrow().as_ref() {
            Some((d, m, _)) => d != dir || *m != mtime,
            None => true,
        }
    }
}

/// The agents that can write a template here: Codex and the ACP agents
/// that are up on this machine.
fn writers(ws: &Workspace) -> Vec<String> {
    let mut out = Vec::new();
    if ws.backend_ready(crate::workspace::CODEX) {
        out.push(crate::workspace::CODEX.to_string());
    }
    for agent in &ws.agents {
        if agent.remote.is_none() && agent.status == AgentStatus::Ready {
            out.push(agent.profile.id.clone());
        }
    }
    out
}

const IDEAS: &[&str] = &[
    "Each week, read new papers on my topic and write a briefing with the ones that matter",
    "Course paper: from picking the topic to a typeset PDF",
    "Data analysis report: clean the data, model it, make the figures, write the findings",
];

/// The draft folder of the chat shown, when it is one.
pub fn shown_draft(ws: &Workspace) -> Option<PathBuf> {
    ws.active_thread().and_then(|t| draft_of(&t.cwd))
}

impl RootView {
    /// Opens the draft panel the first time a draft chat shows, and leaves
    /// it when another chat does.
    pub(super) fn follow_draft(&mut self, cx: &mut Context<Self>) {
        let ws = self.workspace.read(cx);
        let draft = shown_draft(ws).and(ws.active_thread_id().map(str::to_string));
        match draft {
            Some(id) if self.author.followed.as_ref() != Some(&id) => {
                self.author.followed = Some(id);
                self.review.open = true;
                self.review.tab = super::changes::SideTab::Draft;
            }
            None if self.review.tab == super::changes::SideTab::Draft => {
                self.review.tab = super::changes::SideTab::Overview;
            }
            _ => {}
        }
        if self.review.open && self.review.tab == super::changes::SideTab::Draft {
            self.watch_draft(cx);
        }
    }

    /// Repaints the draft panel when its file changes, while it shows.
    fn watch_draft(&mut self, cx: &mut Context<Self>) {
        if self.author.watching {
            return;
        }
        self.author.watching = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let go_on = this.update(cx, |root, cx| {
                    let shown =
                        root.review.open && root.review.tab == super::changes::SideTab::Draft;
                    let dir = shown_draft(root.workspace.read(cx));
                    match dir.filter(|_| shown) {
                        Some(dir) => {
                            if root.author.stale(&dir) {
                                cx.notify();
                            }
                            true
                        }
                        None => {
                            root.author.watching = false;
                            false
                        }
                    }
                });
                if !go_on.unwrap_or(false) {
                    return;
                }
            }
        })
        .detach();
    }

    pub(super) fn author_view(
        &mut self,
        authoring: Authoring,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let root = cx.entity();
        let (base, agents) = {
            let ws = self.workspace.read(cx);
            let base = authoring.base.as_ref().and_then(|id| {
                ws.workflow_templates()
                    .into_iter()
                    .find(|e| &e.template.id == id)
                    .map(|e| e.template)
            });
            (base, writers(ws))
        };
        let agent = self
            .author
            .agent
            .clone()
            .filter(|a| agents.contains(a))
            .or_else(|| agents.iter().find(|a| *a == "claude").cloned())
            .or_else(|| agents.first().cloned());

        // Filled in before the theme is borrowed.
        let field = TextField::new(&self.author.request).into_any_element();

        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);

        let title = match &base {
            Some(t) => tf("Change {name} with AI", &[("name", &t.name)]),
            None => t("New workflow with AI").to_string(),
        };

        let mut ideas = div().flex().flex_wrap().gap_2();
        if base.is_none() {
            for (ix, idea) in IDEAS.iter().enumerate() {
                let fill = root.clone();
                let text = t(idea);
                ideas = ideas.child(
                    div()
                        .id(SharedString::from(format!("wf-idea-{ix}")))
                        .px_2p5()
                        .py_1()
                        .rounded_full()
                        .border_1()
                        .border_color(colors.border)
                        .text_size(tiny)
                        .text_color(colors.fg_muted)
                        .cursor_pointer()
                        .hover(|d| d.bg(colors.hover).text_color(colors.fg))
                        .child(text)
                        .on_click(move |_, _, cx| {
                            fill.update(cx, |root, cx| {
                                root.author.request.update(cx, |i, cx| i.set_text(text, cx));
                                cx.notify();
                            })
                        }),
                );
            }
        }

        let mut files = div().flex().flex_wrap().gap_1();
        for (ix, path) in self.author.references.iter().enumerate() {
            let drop = root.clone();
            files = files.child(
                div()
                    .id(SharedString::from(format!("wf-ref-{ix}")))
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
                            .id(SharedString::from(format!("wf-ref-x-{ix}")))
                            .cursor_pointer()
                            .child(
                                Icon::new(IconName::X)
                                    .size(IconSize::Xs)
                                    .color(colors.fg_subtle),
                            )
                            .on_click(move |_, _, cx| {
                                drop.update(cx, |root, cx| {
                                    if ix < root.author.references.len() {
                                        root.author.references.remove(ix);
                                    }
                                    cx.notify();
                                })
                            }),
                    ),
            );
        }
        let add = root.clone();
        let references = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                DropZone::new("wf-ref-drop")
                    .multiple()
                    .hint(t(
                        "Notes, a rule book, an example of the result, or how you do it by hand.",
                    ))
                    .on_drop(move |paths, _, cx| {
                        add.update(cx, |root, cx| {
                            for p in paths {
                                if !root.author.references.contains(&p) {
                                    root.author.references.push(p);
                                }
                            }
                            cx.notify();
                        })
                    }),
            )
            .child(files);

        let picker: AnyElement = if agents.is_empty() {
            div()
                .text_size(small)
                .text_color(colors.warning)
                .child(t(
                    "No agent is running here. Start Codex or Claude Code first.",
                ))
                .into_any_element()
        } else {
            let pick = root.clone();
            let mut control =
                SegmentedControl::new("wf-author-agent", agent.clone().unwrap_or_default())
                    .size(ControlSize::Sm);
            for a in &agents {
                control =
                    control.segment(a.clone(), crate::agents::backend_name(a).to_string(), None);
            }
            control
                .on_change(move |value, _, cx| {
                    let value = value.to_string();
                    pick.update(cx, |root, cx| {
                        root.author.agent = Some(value);
                        cx.notify();
                    })
                })
                .into_any_element()
        };

        let cancel = self.workspace.clone();
        let go = root.clone();
        let base_id = authoring.base.clone();
        let ready = agent.is_some();

        div()
            .id("wf-author")
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
                    .max_w(px(720.))
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
                                    .child(Icon::new(IconName::Sparkles).size(IconSize::Md).color(colors.accent))
                                    .child(
                                        div()
                                            .text_size(theme.text_size(TextSize::Xl))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(title),
                                    ),
                            )
                            .child(div().text_size(small).text_color(colors.fg_muted).child(t(
                                "Say what the workflow starts from, what it should end with, and where you want to look before it goes on. The agent asks what it needs to know, then writes the template; beside the chat you see whether it reads and what its steps are, and you save it when you like it.",
                            ))),
                    )
                    .child(section(
                        if base.is_some() { t("What should change") } else { t("What the workflow should do") },
                        div().flex().flex_col().gap_3().child(field).child(ideas),
                        cx,
                    ))
                    .child(section(t("References (optional)"), references, cx))
                    .child(section(t("Written by"), picker, cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("wf-author-cancel", t("Cancel"))
                                    .variant(ButtonVariant::Ghost)
                                    .on_click(move |_, _, cx| {
                                        cancel.update(cx, |ws, cx| {
                                            ws.authoring = None;
                                            cx.notify();
                                        })
                                    }),
                            )
                            .child(
                                Button::new("wf-author-start", t("Start writing"))
                                    .icon(IconName::Sparkles)
                                    .disabled(!ready)
                                    .on_click(move |_, _, cx| {
                                        let base = base_id.clone();
                                        go.update(cx, |root, cx| {
                                            let Some(agent) = agent.clone() else { return };
                                            let request = root.author.request.read(cx).text().to_string();
                                            let references = root.author.references.clone();
                                            let started = !request.trim().is_empty();
                                            root.workspace.update(cx, |ws, cx| {
                                                ws.start_authoring(request, base, agent, references, cx)
                                            });
                                            if started {
                                                root.author.references.clear();
                                                root.author.request.update(cx, |i, cx| i.set_text("", cx));
                                            }
                                        })
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The draft panel beside an authoring chat.
    pub(super) fn draft_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let Some(dir) = shown_draft(ws) else {
            return div().into_any_element();
        };
        let running = ws.is_running();
        let report = self.author.report(&dir);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let tiny = theme.text_size(TextSize::Xs);

        let (label, tone) = match &report {
            None if running => (t("The agent is writing…").to_string(), Tone::Neutral),
            None => (t("No template written yet").to_string(), Tone::Neutral),
            Some(r) if r.template.is_none() => (t("Does not read").to_string(), Tone::Danger),
            Some(r) if !r.warnings.is_empty() => (
                tf("{n} things to look at", &[("n", &r.warnings.len())]),
                Tone::Warning,
            ),
            Some(_) => (t("Ready to save").to_string(), Tone::Success),
        };

        let mut body = div().flex().flex_col().gap_4().p_3();
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(Badge::new(label).tone(tone))
                .child(div().flex_1())
                .child(
                    Button::new("draft-folder", t("Folder"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .icon(IconName::Folder)
                        .on_click({
                            let dir = dir.clone();
                            move |_, _, _| {
                                let _ = crate::opener::open_default(&dir);
                            }
                        }),
                ),
        );

        if let Some(report) = &report {
            let mut findings = div().flex().flex_col().gap_1();
            let lines = report
                .error
                .iter()
                .map(|e| (e, colors.danger))
                .chain(report.warnings.iter().map(|w| (w, colors.warning)));
            let mut any = false;
            for (text, color) in lines {
                any = true;
                findings = findings.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_size(tiny)
                        .child(
                            div()
                                .flex_none()
                                .pt(px(5.))
                                .child(div().size(px(5.)).rounded_full().bg(color)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(colors.fg)
                                .child(text.clone()),
                        ),
                );
            }
            if any {
                let feedback = report.as_feedback(lang());
                let send = self.workspace.clone();
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_2()
                        .rounded(theme.radius(Radius::Md))
                        .bg(colors.sunken)
                        .child(findings)
                        .child(
                            div().flex().justify_end().child(
                                Button::new("draft-fix", t("Ask the agent to fix these"))
                                    .size(ControlSize::Sm)
                                    .variant(ButtonVariant::Secondary)
                                    .icon(IconName::Wrench)
                                    .disabled(running)
                                    .on_click(move |_, _, cx| {
                                        if let Some(text) = feedback.clone() {
                                            send.update(cx, |ws, cx| {
                                                ws.send(crate::workspace::Composed::text(text), cx)
                                            });
                                        }
                                    }),
                            ),
                        ),
                );
            }
            if let Some(template) = &report.template {
                let save = self.workspace.clone();
                let save_dir = dir.clone();
                body = body
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(tiny)
                                    .text_color(colors.fg_muted)
                                    .child(t("You can save it now and keep changing it here.")),
                            )
                            .child(
                                Button::new("draft-save", t("Save as template"))
                                    .size(ControlSize::Sm)
                                    .icon(IconName::Check)
                                    .on_click(move |_, _, cx| {
                                        let dir = save_dir.clone();
                                        save.update(cx, |ws, cx| ws.save_draft(&dir, cx))
                                    }),
                            ),
                    )
                    .child(preview(template, cx));
            }
        } else {
            body = body.child(div().text_size(small).text_color(colors.fg_muted).child(t(
                "When the agent writes template.toml, Atrium checks it here and shows the steps a run would have.",
            )));
        }
        body.into_any_element()
    }
}

fn lang() -> atrium_workflow::render::Lang {
    if crate::i18n::chinese() {
        atrium_workflow::render::Lang::Zh
    } else {
        atrium_workflow::render::Lang::En
    }
}

pub(super) fn kind_label(kind: InputKind) -> &'static str {
    match kind {
        InputKind::Files => t("File input"),
        InputKind::Text => t("Text"),
        InputKind::Long => t("Long text"),
        InputKind::Choice => t("Choice"),
        InputKind::Number => t("Number"),
    }
}

/// The template as a run would start: its inputs, then its steps.
fn preview(template: &Template, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let small = theme.text_size(TextSize::Sm);
    let tiny = theme.text_size(TextSize::Xs);

    let mut inputs = div().flex().flex_col().gap_1();
    for input in &template.inputs {
        let detail = if input.kind == InputKind::Choice {
            input.options.join(" / ")
        } else {
            input.help.clone()
        };
        inputs = inputs.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(tiny)
                .child(
                    div()
                        .flex_none()
                        .text_size(small)
                        .text_color(colors.fg)
                        .child(input.label.clone()),
                )
                .when(input.required, |d| {
                    d.child(div().text_color(colors.danger).child("*"))
                })
                .when(input.private, |d| {
                    d.child(
                        Icon::new(IconName::Lock)
                            .size(IconSize::Xs)
                            .color(colors.fg_subtle),
                    )
                })
                .child(Badge::new(kind_label(input.kind)).tone(Tone::Neutral))
                .when(input.when.is_some(), |d| {
                    d.child(div().flex_none().text_color(colors.fg_subtle).child(tf(
                        "only if {when}",
                        &[("when", &input.when.clone().unwrap_or_default())],
                    )))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(colors.fg_muted)
                        .child(detail),
                ),
        );
    }

    let mut steps = div().flex().flex_col();
    for (ix, step) in template.steps.iter().enumerate() {
        let agent = template
            .agent_of(step)
            .map(|a| crate::agents::backend_name(&a).to_string())
            .unwrap_or_else(|| t("Default agent").to_string());
        let mut badges = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(Badge::new(agent).tone(Tone::Neutral));
        if step.decide.is_some() {
            badges = badges.child(Badge::new(t("You choose")).tone(Tone::Warning));
        } else if step.gate {
            badges = badges.child(Badge::new(t("Stops for you")).tone(Tone::Warning));
        }
        if let Some(each) = &step.each {
            badges = badges.child(
                Badge::new(tf("Once per item of {file}", &[("file", &each.from)])).tone(Tone::Info),
            );
        }
        if step.optional {
            badges = badges.child(Badge::new(t("Optional")).tone(Tone::Neutral));
        }
        if !step.fixes.is_empty() {
            badges = badges.child(
                Badge::new(tf(
                    "Sends back to {steps}",
                    &[("steps", &step.fixes.join(", "))],
                ))
                .tone(Tone::Info),
            );
        }
        let checks = step.outputs.len() + step.checks.len();
        steps = steps.child(
            div()
                .flex()
                .gap_2()
                .py_2()
                .border_b_1()
                .border_color(colors.border)
                .child(
                    div()
                        .flex_none()
                        .w(px(18.))
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
                        .gap_1()
                        .child(
                            div()
                                .text_size(small)
                                .text_color(colors.fg)
                                .child(step.title.clone()),
                        )
                        .when(!step.summary.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(tiny)
                                    .text_color(colors.fg_muted)
                                    .child(step.summary.clone()),
                            )
                        })
                        .child(badges)
                        .child(div().text_size(tiny).text_color(colors.fg_subtle).child(
                            if checks == 0 {
                                t("Nothing to check").to_string()
                            } else {
                                tf("{n} files and checks", &[("n", &checks)])
                            },
                        )),
                ),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Lg))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(template.name.clone()),
                )
                .when(!template.description.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(tiny)
                            .text_color(colors.fg_muted)
                            .child(template.description.clone()),
                    )
                }),
        )
        .when(!template.inputs.is_empty(), |d| {
            d.child(section(t("What to start from"), inputs, cx))
        })
        .child(section(t("Steps"), steps, cx))
        .into_any_element()
}

fn section(title: &str, body: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(theme.text_size(TextSize::Sm))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.colors.fg)
                .child(title.to_string()),
        )
        .child(body)
        .into_any_element()
}
