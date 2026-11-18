//! Where messages are written: text, attached images, `@` file mentions,
//! `/` commands, and the pickers for access, effort and folder.

use std::path::{Path, PathBuf};

use atrium_core::ConfigOption;
use codex_client::AccessMode;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton, ToggleButton, ToggleItem},
    chat::{ContextChips, PromptInput},
    forms::{Choice, TextInput},
    menus::{DropdownMenu, Menu, MenuItem},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, TextSize},
};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Render, SharedString, Styled, Subscription, Task, Window,
    actions, div, prelude::FluentBuilder,
};

use super::{NewChat, OpenSettings, ToggleSidebar};
use crate::workspace::{Composed, Status, Workspace, is_image};

pub struct Composer {
    workspace: Entity<Workspace>,
    pub draft: Entity<TextInput>,
    images: Vec<PathBuf>,
    mentions: Vec<PathBuf>,
    /// Files offered after `@`, from the last search.
    found: Vec<PathBuf>,
    /// The `@` query the last search ran for.
    searched: Option<String>,
    /// The placeholder last shown: the default, a prediction, or a steering hint.
    placeholder: SharedString,
    _search: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

actions!(atrium, [AcceptPrediction]);

const DEFAULT_PLACEHOLDER: &str = "Ask Codex anything. @ to add files, / for commands";

/// Slash commands: value, label, icon, what it says.
const COMMANDS: [(&str, &str, IconName, &str); 6] = [
    ("new", "/new", IconName::SquarePen, "Start a new chat"),
    (
        "review",
        "/review",
        IconName::Eye,
        "Review uncommitted changes",
    ),
    (
        "compact",
        "/compact",
        IconName::Minimize2,
        "Summarize to free context",
    ),
    (
        "attach",
        "/attach",
        IconName::Paperclip,
        "Attach images or files",
    ),
    (
        "settings",
        "/settings",
        IconName::SlidersHorizontal,
        "Open settings",
    ),
    (
        "sidebar",
        "/sidebar",
        IconName::PanelLeft,
        "Show or hide the sidebar",
    ),
];

impl Composer {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let draft = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line(1, 12)
                .placeholder(DEFAULT_PLACEHOLDER)
        });
        let subscriptions = vec![
            cx.observe(&draft, |this, _, cx| this.watch_mention(cx)),
            cx.observe(&workspace, |_, _, cx| cx.notify()),
        ];
        Self {
            workspace,
            draft,
            images: Vec::new(),
            mentions: Vec::new(),
            found: Vec::new(),
            searched: None,
            placeholder: DEFAULT_PLACEHOLDER.into(),
            _search: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let focus = self.draft.focus_handle(cx);
        window.focus(&focus, cx);
    }

    /// Focuses `composer`'s field.
    pub fn focus_entity(composer: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let focus = composer.read(cx).draft.focus_handle(cx);
        window.focus(&focus, cx);
    }

    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.update(cx, |input, cx| input.set_text(text, cx));
        self.focus(window, cx);
    }

    /// The `@word` being typed at the caret, if any.
    fn mention_query(&self, cx: &App) -> Option<String> {
        let input = self.draft.read(cx);
        let (text, caret) = (input.text(), input.cursor().min(input.text().len()));
        let before = &text[..caret];
        let start = before
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(0, |(ix, ch)| ix + ch.len_utf8());
        before[start..].strip_prefix('@').map(str::to_owned)
    }

    /// Searches the chat's folder as an `@` query changes.
    fn watch_mention(&mut self, cx: &mut Context<Self>) {
        let query = self.mention_query(cx);
        if query == self.searched {
            return;
        }
        self.searched = query.clone();
        let Some(query) = query else {
            self.found.clear();
            return;
        };
        let search = self
            .workspace
            .update(cx, |ws, cx| ws.search_files(query, cx));
        self._search = Some(cx.spawn(async move |this, cx| {
            let found = search.await;
            let _ = this.update(cx, |this, cx| {
                this.found = found;
                cx.notify();
            });
        }));
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft.read(cx).text().to_string();
        let ws = self.workspace.read(cx);
        // While Codex works, a message steers the running turn.
        if (text.trim().is_empty() && self.images.is_empty()) || !ws.is_ready() {
            return;
        }
        let composed = Composed {
            text,
            images: std::mem::take(&mut self.images),
            mentions: std::mem::take(&mut self.mentions),
        };
        self.draft.update(cx, |input, cx| input.set_text("", cx));
        self.workspace.update(cx, |ws, cx| ws.send(composed, cx));
        self.focus(window, cx);
        cx.notify();
    }

    /// Fills the composer with `composed`, such as a message being edited.
    pub fn fill(
        &mut self,
        composed: crate::workspace::Composed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.images = composed.images;
        self.mentions = composed.mentions;
        self.set_text(&composed.text, window, cx);
        cx.notify();
    }

    /// Takes the predicted next message into the composer.
    fn accept_prediction(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(prediction) = self.workspace.read(cx).prediction().map(str::to_owned) else {
            return false;
        };
        if !self.draft.read(cx).text().is_empty() {
            return false;
        }
        self.set_text(&prediction, window, cx);
        self.workspace
            .update(cx, |ws, cx| ws.dismiss_prediction(cx));
        true
    }

    /// Keeps the placeholder saying what Enter will do.
    fn update_placeholder(&mut self, cx: &mut Context<Self>) {
        let ws = self.workspace.read(cx);
        let wanted: SharedString = if let Some(prediction) = ws.prediction() {
            format!("{prediction}   ⇥ Tab").into()
        } else if ws.is_running() && ws.is_codex_view() {
            "Add to what Codex is doing: it reads this at its next step".into()
        } else if ws.is_running() {
            format!(
                "{} is working. Wait for it, or stop it to change course",
                crate::agents::backend_name(ws.view_backend())
            )
            .into()
        } else if ws.editing.is_some() {
            "Edit your message".into()
        } else if ws.settings.plan_mode {
            "Describe the change: Codex plans first and waits for you".into()
        } else if !ws.is_codex_view() {
            format!(
                "Ask {} anything. @ to add files, / for commands",
                crate::agents::backend_name(ws.view_backend())
            )
            .into()
        } else {
            DEFAULT_PLACEHOLDER.into()
        };
        if wanted != self.placeholder {
            self.placeholder = wanted.clone();
            self.draft
                .update(cx, |input, cx| input.set_placeholder(wanted, cx));
        }
    }

    /// Adds dropped or picked paths: images to show, other files to mention.
    pub fn attach(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let list = if is_image(&path) {
                &mut self.images
            } else {
                &mut self.mentions
            };
            if !list.contains(&path) {
                list.push(path);
            }
        }
        cx.notify();
    }

    pub fn pick_files(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update(cx, |this, cx| this.attach(paths, cx));
            }
        })
        .detach();
    }

    fn run_command(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            "new" => window.dispatch_action(Box::new(NewChat), cx),
            "review" => {
                let extra = self.draft.read(cx).text().trim().to_string();
                self.draft.update(cx, |input, cx| input.set_text("", cx));
                self.workspace.update(cx, |ws, cx| {
                    ws.review(Some(extra).filter(|t| !t.is_empty()), cx)
                });
            }
            "compact" => self.workspace.update(cx, |ws, cx| ws.compact(cx)),
            "attach" => self.pick_files(cx),
            "settings" => window.dispatch_action(Box::new(OpenSettings), cx),
            agent if agent.starts_with("agent:") => {
                // An agent's command is a message that starts with it.
                let name = &agent["agent:".len()..];
                self.set_text(&format!("/{name} "), window, cx);
            }
            "sidebar" => window.dispatch_action(Box::new(ToggleSidebar), cx),
            other => log::warn!("unknown command {other}"),
        }
    }

    /// Codex's pickers: plan mode, Fast, access and reasoning effort.
    fn codex_tools(&self, tools: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let ws = self.workspace.read(cx);
        // Plan mode and Fast.
        let workspace = self.workspace.clone();
        tools.push(
            ToggleButton::new(
                "plan-mode",
                ToggleItem::new("plan")
                    .label("Plan")
                    .icon(IconName::ListChecks)
                    .tooltip("Plan mode: Codex proposes a plan and waits before changing anything"),
                ws.settings.plan_mode,
            )
            .size(ControlSize::Sm)
            .on_toggle(move |on, _, cx| {
                workspace.update(cx, |ws, cx| {
                    ws.settings.plan_mode = on;
                    ws.settings.save();
                    cx.notify();
                })
            })
            .into_any_element(),
        );
        if ws.fast_tier().is_some() {
            let workspace = self.workspace.clone();
            tools.push(
                ToggleButton::new(
                    "fast",
                    ToggleItem::new("fast")
                        .label("Fast")
                        .icon(IconName::Zap)
                        .tooltip("About twice the speed; uses more of your plan's usage"),
                    ws.settings.fast,
                )
                .size(ControlSize::Sm)
                .on_toggle(move |on, _, cx| {
                    workspace.update(cx, |ws, cx| {
                        ws.settings.fast = on;
                        ws.settings.save();
                        cx.notify();
                    })
                })
                .into_any_element(),
            );
        }

        // Access mode.
        let current = ws.settings.mode;
        let mut menu = Menu::new();
        for mode in AccessMode::ALL {
            let workspace = self.workspace.clone();
            menu = menu.item(
                MenuItem::radio(
                    format!("{} — {}", mode.label(), mode.description()),
                    mode == current,
                )
                .on_click(move |_, cx| workspace.update(cx, |ws, cx| ws.set_mode(mode, cx))),
            );
        }
        let icon = match current {
            AccessMode::ReadOnly => IconName::Lock,
            AccessMode::Auto => IconName::Shield,
            AccessMode::FullAccess => IconName::LockOpen,
        };
        tools.push(
            DropdownMenu::new("mode", current.label(), menu)
                .icon(icon)
                .variant(ButtonVariant::Ghost)
                .into_any_element(),
        );

        // Reasoning effort, when the model offers a choice.
        let (efforts, effort) = ws.efforts();
        if !efforts.is_empty() {
            let mut menu = Menu::new();
            for level in &efforts {
                let workspace = self.workspace.clone();
                let value = level.clone();
                menu = menu.item(
                    MenuItem::radio(title_case(level), effort.as_deref() == Some(level)).on_click(
                        move |_, cx| {
                            workspace.update(cx, |ws, cx| ws.set_effort(Some(value.clone()), cx))
                        },
                    ),
                );
            }
            let label = effort
                .as_deref()
                .map(title_case)
                .unwrap_or_else(|| "Effort".into());
            tools.push(
                DropdownMenu::new("effort", label, menu)
                    .icon(IconName::Gauge)
                    .variant(ButtonVariant::Ghost)
                    .into_any_element(),
            );
        }
    }

    /// An ACP agent's pickers, as the agent offers them: mode, model,
    /// reasoning level and whatever else it reports.
    fn agent_tools(&self, tools: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let ws = self.workspace.read(cx);
        let options: Vec<_> = ws
            .agent_options()
            .into_iter()
            .filter(|o| o.choices.len() > 1)
            .collect();
        let primary = |o: &ConfigOption| {
            matches!(
                o.category.as_deref(),
                Some("mode" | "model" | "thought_level")
            )
        };
        // Mode, model and reasoning level get their own pickers.
        for option in options.iter().filter(|o| primary(o)) {
            let icon = match option.category.as_deref() {
                Some("mode") => IconName::Shield,
                Some("model") => IconName::Sparkles,
                _ => IconName::Gauge,
            };
            tools.push(
                DropdownMenu::new(
                    SharedString::from(format!("agent-option-{}", option.id)),
                    option.current_name(),
                    self.choices_menu(option),
                )
                .icon(icon)
                .variant(ButtonVariant::Ghost)
                .into_any_element(),
            );
        }
        // Everything else the agent offers shares one menu, each by name.
        let rest: Vec<&ConfigOption> = options.iter().filter(|o| !primary(o)).collect();
        if !rest.is_empty() {
            let mut menu = Menu::new();
            for option in rest {
                menu = menu.item(MenuItem::submenu(
                    format!("{}: {}", option.name, option.current_name()),
                    self.choices_menu(option),
                ));
            }
            tools.push(
                DropdownMenu::new("agent-options", "Options", menu)
                    .icon(IconName::SlidersHorizontal)
                    .variant(ButtonVariant::Ghost)
                    .into_any_element(),
            );
        }
    }

    /// One agent setting's choices, grouped as the agent groups them.
    fn choices_menu(&self, option: &ConfigOption) -> Menu {
        let grouped = option
            .choices
            .iter()
            .filter_map(|c| c.group.as_deref())
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1;
        let mut menu = Menu::new();
        let mut last_group: Option<String> = None;
        for choice in &option.choices {
            if grouped && choice.group != last_group && last_group.is_some() {
                menu = menu.separator();
            }
            last_group = choice.group.clone();
            let label = match (&choice.group, grouped) {
                (Some(group), true) => format!("{group} · {}", choice.name),
                _ => choice.name.clone(),
            };
            let label = match &choice.description {
                Some(description) if option.category.as_deref() == Some("mode") => {
                    format!("{label} — {description}")
                }
                _ => label,
            };
            let (workspace, id, value) = (
                self.workspace.clone(),
                option.id.clone(),
                choice.value.clone(),
            );
            menu = menu.item(
                MenuItem::radio(label, choice.value == option.current).on_click(move |_, cx| {
                    workspace.update(cx, |ws, cx| {
                        ws.set_agent_option(id.clone(), value.clone(), cx)
                    })
                }),
            );
        }
        menu
    }

    fn tools(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ws = self.workspace.read(cx);
        let mut tools = Vec::new();
        let this = cx.entity();

        tools.push(
            IconButton::new("attach", IconName::Paperclip)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .tooltip("Attach images or files")
                .on_click(move |_, _, cx| this.update(cx, |this, cx| this.pick_files(cx)))
                .into_any_element(),
        );

        // A device's Codex keeps its own model settings.
        if ws.is_codex_view() && !ws.is_remote_view() {
            self.codex_tools(&mut tools, cx);
        } else if !ws.is_codex_view() {
            self.agent_tools(&mut tools, cx);
        }
        let ws = self.workspace.read(cx);

        // On another device: its folders, browsed through its node.
        if ws.active_thread().is_none() && ws.is_remote_view() {
            let backend = ws.view_backend().to_string();
            let device = crate::agents::split_backend(&backend)
                .1
                .unwrap_or_default()
                .to_string();
            let folder = ws.folder_for(&backend);
            let pick = |path: String, label: String, icon: IconName, selected: bool| {
                let (workspace, device) = (self.workspace.clone(), device.clone());
                MenuItem::radio(label, selected)
                    .icon(icon)
                    .on_click(move |_, cx| {
                        let (device, path) = (device.clone(), path.clone());
                        workspace.update(cx, |ws, cx| ws.set_device_folder(&device, path, cx))
                    })
            };
            let mut menu = Menu::new();
            if let Some(device) = ws.device(&device) {
                let roots: Vec<MenuItem> = device
                    .info
                    .roots
                    .iter()
                    .map(|root| {
                        let label = folder_name(Path::new(root));
                        pick(root.clone(), label, IconName::House, *root == folder)
                    })
                    .collect();
                menu = menu.group("Roots", roots);
            }
            let parent = std::path::Path::new(&folder)
                .parent()
                .map(|p| p.display().to_string());
            let inside_roots = |path: &str| {
                ws.device(&device)
                    .is_some_and(|d| d.info.roots.iter().any(|r| path.starts_with(r.as_str())))
            };
            let mut here = Vec::new();
            if let Some(parent) = parent.filter(|p| inside_roots(p)) {
                here.push(pick(
                    parent,
                    "Up one level".into(),
                    IconName::ChevronLeft,
                    false,
                ));
            }
            match ws.remote_children(&device, &folder) {
                Some(children) => here.extend(children.iter().map(|name| {
                    let path = format!("{}/{name}", folder.trim_end_matches('/'));
                    pick(path, name.clone(), IconName::Folder, false)
                })),
                None => here.push(MenuItem::new("Loading…").disabled(true)),
            }
            menu = menu.group(format!("In {}", folder_name(Path::new(&folder))), here);
            tools.push(
                DropdownMenu::new(
                    "folder",
                    format!("{device}:{}", folder_name(Path::new(&folder))),
                    menu,
                )
                .icon(IconName::Monitor)
                .variant(ButtonVariant::Ghost)
                .into_any_element(),
            );
        }

        // Folder: chosen for a new chat; fixed once the chat exists.
        if ws.active_thread().is_none() && !ws.is_remote_view() {
            let folder = ws.settings.working_dir();
            let mut menu = Menu::new();
            for recent in ws.settings.recent_folders.iter().filter(|dir| dir.is_dir()) {
                let workspace = self.workspace.clone();
                let dir = recent.clone();
                menu = menu.item(
                    MenuItem::radio(short_path(recent), *recent == folder)
                        .icon(IconName::Folder)
                        .on_click(move |_, cx| {
                            workspace.update(cx, |ws, cx| ws.set_working_dir(dir.clone(), cx))
                        }),
                );
            }
            let workspace = self.workspace.clone();
            menu = menu.separator().item(
                MenuItem::new("Choose a folder…")
                    .icon(IconName::FolderOpen)
                    .on_click(move |_, cx| choose_folder(workspace.clone(), cx)),
            );
            tools.push(
                DropdownMenu::new("folder", folder_name(&folder), menu)
                    .icon(IconName::Folder)
                    .variant(ButtonVariant::Ghost)
                    .into_any_element(),
            );
        }
        tools
    }
}

pub fn choose_folder(workspace: Entity<Workspace>, cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Use this folder".into()),
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = picked.await
            && let Some(dir) = paths.into_iter().next()
        {
            workspace.update(cx, |ws, cx| ws.set_working_dir(dir, cx));
        }
    })
    .detach();
}

pub fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// `~/code/app` for `/Users/me/code/app`.
pub fn short_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

fn title_case(word: &str) -> String {
    match word {
        "xhigh" => "Extra high".into(),
        _ => {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_placeholder(cx);
        let predicting =
            self.workspace.read(cx).prediction().is_some() && self.draft.read(cx).text().is_empty();
        let tools = self.tools(cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let ws = self.workspace.read(cx);
        let running = ws.is_running();
        let notice: Option<SharedString> = match &ws.status {
            Status::Connecting => Some("Starting Codex…".into()),
            Status::Failed(_) => Some("Codex is not running. Restart it from the sidebar.".into()),
            Status::Ready => ws
                .transcript()
                .and_then(|t| t.notice())
                .map(|notice| SharedString::from(notice.to_string()))
                .or_else(|| {
                    (ws.signed_in == Some(false)).then(|| {
                        "Not signed in: sign in with ChatGPT from the sidebar to use your plan."
                            .into()
                    })
                }),
        };
        let root = self.workspace.read(cx).folder();

        let this = cx.entity();
        let mut input = PromptInput::new("composer", &self.draft, move |window, cx| {
            this.update(cx, |this, cx| this.send(window, cx))
        });
        if running {
            // Not `busy`: Enter must still send, to steer the running turn.
            let workspace = self.workspace.clone();
            input = input.tool(
                IconButton::new("stop", IconName::Square)
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Sm)
                    .tooltip("Stop (⌘.)")
                    .on_click(move |_, _, cx| workspace.update(cx, |ws, cx| ws.stop(cx))),
            );
        }
        let editing = ws.editing.is_some();
        if editing {
            let workspace = self.workspace.clone();
            let draft = self.draft.clone();
            input = input.above(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(colors.accent)
                    .child(
                        Icon::new(IconName::Pencil)
                            .size(IconSize::Xs)
                            .color(colors.accent),
                    )
                    .child("Editing a message: sending replaces it and everything after")
                    .child(div().flex_1())
                    .child(
                        Button::new("cancel-edit", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                draft.update(cx, |input, cx| input.set_text("", cx));
                                workspace.update(cx, |ws, cx| ws.cancel_edit(cx))
                            }),
                    ),
            );
        }

        let attached: Vec<(String, IconName)> = self
            .images
            .iter()
            .map(|path| (folder_name(path), IconName::Image))
            .chain(
                self.mentions
                    .iter()
                    .map(|path| (folder_name(path), IconName::FileText)),
            )
            .collect();
        if !attached.is_empty() {
            let this = cx.entity();
            let images = self.images.len();
            input = input.above(ContextChips::new("attached", attached).on_remove(
                move |ix, _, cx| {
                    this.update(cx, |this, cx| {
                        if ix < images {
                            this.images.remove(ix);
                        } else if ix - images < this.mentions.len() {
                            this.mentions.remove(ix - images);
                        }
                        cx.notify();
                    })
                },
            ));
        }

        // Review and compact are Codex's; an ACP agent brings its own commands.
        let codex = self.workspace.read(cx).is_codex_view();
        let mut commands: Vec<Choice> = COMMANDS
            .iter()
            .filter(|(value, ..)| codex || !matches!(*value, "review" | "compact"))
            .map(|(value, label, icon, note)| Choice::new(*value, *label).icon(*icon).note(*note))
            .collect();
        for command in self.workspace.read(cx).agent_slash_commands() {
            commands.push(
                Choice::new(
                    format!("agent:{}", command.name),
                    format!("/{}", command.name),
                )
                .icon(IconName::Bot)
                .note(command.description),
            );
        }
        let this = cx.entity();
        input = input.commands(commands, move |value, window, cx| {
            let value = value.to_string();
            this.update(cx, |this, cx| this.run_command(&value, window, cx))
        });

        let files: Vec<Choice> = self
            .found
            .iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                Choice::new(path.display().to_string(), relative).icon(IconName::FileText)
            })
            .collect();
        let this = cx.entity();
        input = input.context(files, move |value, _, cx| {
            let path = PathBuf::from(value.to_string());
            this.update(cx, |this, cx| this.attach(vec![path], cx))
        });

        let this = cx.entity();
        input =
            input.on_drop(move |paths, _, cx| this.update(cx, |this, cx| this.attach(paths, cx)));

        for tool in tools {
            input = input.tool(tool);
        }

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1p5()
            // Tab takes the prediction: bound only while one shows, so Tab
            // keeps moving focus otherwise.
            .when(predicting, |column| column.key_context("Predicting"))
            .on_action(cx.listener(|this, _: &AcceptPrediction, window, cx| {
                this.accept_prediction(window, cx);
            }))
            .child(input)
            .child(
                div()
                    .h_4()
                    .px_2()
                    .flex()
                    .justify_center()
                    .text_size(theme.text_size(TextSize::Xs))
                    .text_color(colors.fg_subtle)
                    .child(notice.unwrap_or_else(|| {
                        format!(
                            "{} can make mistakes. Review its changes before you ship them.",
                            crate::agents::backend_name(self.workspace.read(cx).view_backend())
                        )
                        .into()
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efforts_read_as_words() {
        assert_eq!(title_case("high"), "High");
        assert_eq!(title_case("xhigh"), "Extra high");
    }
}
