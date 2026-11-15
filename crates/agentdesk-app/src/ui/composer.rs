//! Where messages are written: text, attached images, `@` file mentions,
//! `/` commands, and the pickers for access, effort and folder.

use std::path::{Path, PathBuf};

use codex_client::AccessMode;
use ely_gpui_component::{
    buttons::{ButtonVariant, IconButton},
    chat::{ContextChips, PromptInput},
    forms::{Choice, TextInput},
    menus::{DropdownMenu, Menu, MenuItem},
    primitives::IconName,
    theme::{ActiveTheme, ControlSize, TextSize},
};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, Focusable, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Subscription, Task, Window, div,
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
    _search: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

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
                .placeholder("Ask Codex anything. @ to add files, / for commands")
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
        if (text.trim().is_empty() && self.images.is_empty()) || ws.is_running() || !ws.is_ready() {
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
            "sidebar" => window.dispatch_action(Box::new(ToggleSidebar), cx),
            other => log::warn!("unknown command {other}"),
        }
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

        // Folder: chosen for a new chat; fixed once the chat exists.
        if ws.active_thread().is_none() {
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
                    (ws.signed_in == Some(false))
                        .then(|| "Codex is not signed in: run `codex login` in a terminal.".into())
                }),
        };
        let root = self.workspace.read(cx).folder();

        let this = cx.entity();
        let mut input = PromptInput::new("composer", &self.draft, move |window, cx| {
            this.update(cx, |this, cx| this.send(window, cx))
        });
        if running {
            let workspace = self.workspace.clone();
            input = input.busy(move |_, cx| workspace.update(cx, |ws, cx| ws.stop(cx)));
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

        let commands = COMMANDS
            .iter()
            .map(|(value, label, icon, note)| Choice::new(*value, *label).icon(*icon).note(*note));
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
                        "Codex can make mistakes. Review its changes before you ship them.".into()
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
