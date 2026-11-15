//! The window: a ChatGPT-style sidebar of conversations beside the chat,
//! with a command palette, settings and toasts over it.

mod chat;
mod composer;
mod rows;
mod sidebar;

use codex_client::AccessMode;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    feedback::{Toast, ToastViewport, Toaster},
    forms::{Input, TextInput},
    navigation::{Command, CommandPalette},
    overlays::{Dialog, PromptDialog},
    primitives::{FocusScope, IconName, Severity},
    theme::{ActiveTheme, ControlSize, Mode, TextSize, Theme},
};
use gpui::{
    Action, AnyElement, App, Context, Entity, FocusHandle, Global, IntoElement, ParentElement,
    Render, Styled, Subscription, WeakEntity, Window, WindowAppearance, actions, div, prelude::*,
};
use gpui_kit::component::theme::{Theme as KitTheme, ThemeMode};

use crate::{
    settings::ThemePref,
    workspace::{Notice, Workspace},
};
use composer::Composer;

actions!(
    agentdesk,
    [
        NewChat,
        ToggleSidebar,
        StopTurn,
        Reconnect,
        Quit,
        OpenPalette,
        OpenSettings,
        FocusComposer,
        CompactChat,
        ReviewChanges,
        AttachFiles,
    ]
);

/// The window's root view, so app-wide actions reach it wherever focus is
/// (GPUI Kit's window root can hold focus above this view).
struct RootHandle(WeakEntity<RootView>);
impl Global for RootHandle {}

/// Runs `handle` on the root view of the active window.
fn forward<A: Action + Clone>(
    cx: &mut App,
    handle: impl Fn(&mut RootView, &mut Window, &mut Context<RootView>) + 'static,
) {
    let handle = std::rc::Rc::new(handle);
    cx.on_action(move |_: &A, cx| {
        let Some(root) = cx.try_global::<RootHandle>().and_then(|h| h.0.upgrade()) else {
            return;
        };
        let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
            return;
        };
        // Actions dispatch while the window is being updated; wait until it is free.
        let handle = handle.clone();
        cx.defer(move |cx| {
            let updated = window.update(cx, |_, window, cx| {
                root.update(cx, |this, cx| handle(this, window, cx))
            });
            if let Err(error) = updated {
                log::warn!("action dropped: {error:#}");
            }
        });
    });
}

/// Registers the app's actions. Call once at startup.
pub fn register_actions(cx: &mut App) {
    forward::<NewChat>(cx, |this, window, cx| this.new_chat(window, cx));
    forward::<ToggleSidebar>(cx, |this, _, cx| {
        this.workspace.update(cx, |ws, cx| {
            let collapsed = !ws.settings.sidebar_collapsed;
            ws.set_sidebar_collapsed(collapsed, cx)
        })
    });
    forward::<StopTurn>(cx, |this, _, cx| {
        this.workspace.update(cx, |ws, cx| ws.stop(cx))
    });
    forward::<Reconnect>(cx, |this, _, cx| {
        this.workspace.update(cx, |ws, cx| ws.connect(cx))
    });
    forward::<OpenPalette>(cx, |this, _, cx| {
        this.overlay = match this.overlay {
            Some(Overlay::Palette) => None,
            _ => Some(Overlay::Palette),
        };
        cx.notify();
    });
    forward::<OpenSettings>(cx, |this, window, cx| this.open_settings(window, cx));
    forward::<FocusComposer>(cx, |this, window, cx| {
        Composer::focus_entity(&this.composer, window, cx)
    });
    forward::<CompactChat>(cx, |this, _, cx| {
        this.workspace.update(cx, |ws, cx| ws.compact(cx))
    });
    forward::<ReviewChanges>(cx, |this, _, cx| {
        this.workspace.update(cx, |ws, cx| ws.review(None, cx))
    });
    forward::<AttachFiles>(cx, |this, _, cx| {
        this.composer
            .update(cx, |composer, cx| composer.pick_files(cx))
    });
}

/// Height of the title bar strip, which on macOS holds the traffic lights.
pub const TITLE_BAR_HEIGHT: f32 = 44.;

/// What floats over the window.
enum Overlay {
    Palette,
    Settings {
        codex_path: Entity<TextInput>,
    },
    Rename {
        thread_id: String,
        field: Entity<TextInput>,
    },
}

pub struct RootView {
    focus: FocusHandle,
    workspace: Entity<Workspace>,
    composer: Entity<Composer>,
    search: Entity<TextInput>,
    toaster: Entity<Toaster>,
    overlay: Option<Overlay>,
    /// The theme last applied, so a change of preference or system appearance re-applies.
    applied: Option<(ThemePref, bool)>,
    _subscriptions: Vec<Subscription>,
}

impl RootView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = cx.new(Workspace::new);
        let composer = cx.new(|cx| Composer::new(workspace.clone(), window, cx));
        let search = cx.new(|cx| TextInput::new(window, cx).placeholder("Search chats"));
        let toaster = cx.new(|_| Toaster::default());
        let subscriptions = vec![
            cx.observe_in(&workspace, window, |this, _, window, cx| {
                this.apply_theme(window, cx);
                cx.notify();
            }),
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.subscribe(&workspace, |this, _, notice: &Notice, cx| {
                this.toast(notice, cx)
            }),
            cx.observe_window_appearance(window, |this, window, cx| this.apply_theme(window, cx)),
        ];
        cx.set_global(RootHandle(cx.entity().downgrade()));
        let focus = cx.focus_handle();
        let mut view = Self {
            focus,
            workspace,
            composer,
            search,
            toaster,
            overlay: None,
            applied: None,
            _subscriptions: subscriptions,
        };
        view.apply_theme(window, cx);
        Composer::focus_entity(&view.composer, window, cx);
        view
    }

    fn toast(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        let toast = match notice {
            Notice::Info(text) => Toast::new(text.clone()),
            Notice::Success(text) => Toast::new(text.clone()).severity(Severity::Success),
            Notice::Error(text) => Toast::new(text.clone()).severity(Severity::Danger),
        };
        self.toaster.update(cx, |toaster, cx| {
            toaster.push(toast, cx);
        });
    }

    /// Applies the theme preference, following the system when asked to.
    fn apply_theme(&mut self, window: &mut Window, cx: &mut App) {
        let pref = self.workspace.read(cx).settings.theme;
        let dark = match pref {
            ThemePref::Light => false,
            ThemePref::Dark => true,
            ThemePref::System => matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        };
        if self.applied == Some((pref, dark)) {
            return;
        }
        self.applied = Some((pref, dark));
        let (ely, kit) = if dark {
            (Mode::Dark, ThemeMode::Dark)
        } else {
            (Mode::Light, ThemeMode::Light)
        };
        Theme::set_mode(ely, cx);
        KitTheme::change(kit, Some(window), cx);
    }

    fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.update(cx, |ws, cx| ws.new_chat(cx));
        Composer::focus_entity(&self.composer, window, cx);
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        Composer::focus_entity(&self.composer, window, cx);
        cx.notify();
    }

    pub fn start_rename(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .workspace
            .read(cx)
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .map(|t| t.title())
            .unwrap_or_default();
        let field = cx.new(|cx| TextInput::new(window, cx).placeholder("Chat name"));
        field.update(cx, |input, cx| input.set_text(title, cx));
        self.overlay = Some(Overlay::Rename { thread_id, field });
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .workspace
            .read(cx)
            .settings
            .codex_binary
            .clone()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let codex_path =
            cx.new(|cx| TextInput::new(window, cx).placeholder("Found on PATH (leave empty)"));
        codex_path.update(cx, |input, cx| input.set_text(current, cx));
        self.overlay = Some(Overlay::Settings { codex_path });
        cx.notify();
    }

    fn palette(&self, cx: &mut Context<Self>) -> AnyElement {
        let ws = self.workspace.read(cx);
        let actions = [
            ("new", "New chat", IconName::SquarePen, "⌘N"),
            ("review", "Review uncommitted changes", IconName::Eye, ""),
            ("compact", "Compact conversation", IconName::Minimize2, ""),
            ("attach", "Attach files…", IconName::Paperclip, ""),
            ("folder", "Choose working folder…", IconName::FolderOpen, ""),
            ("sidebar", "Toggle sidebar", IconName::PanelLeft, "⌘B"),
            ("settings", "Settings", IconName::Settings, "⌘,"),
            ("restart", "Restart Codex", IconName::RefreshCw, ""),
        ]
        .map(|(value, label, icon, keys)| {
            let command = Command::new(value, label).icon(icon);
            if keys.is_empty() {
                command
            } else {
                command.keys(keys)
            }
        });
        let modes = AccessMode::ALL.map(|mode| {
            Command::new(
                format!("mode:{}", mode.sandbox_mode()),
                format!("Access: {}", mode.label()),
            )
            .icon(match mode {
                AccessMode::ReadOnly => IconName::Lock,
                AccessMode::Auto => IconName::Shield,
                AccessMode::FullAccess => IconName::LockOpen,
            })
        });
        let themes = [
            ("theme:system", "Theme: match system", IconName::Monitor),
            ("theme:light", "Theme: light", IconName::Sun),
            ("theme:dark", "Theme: dark", IconName::Moon),
        ]
        .map(|(value, label, icon)| Command::new(value, label).icon(icon));
        let chats: Vec<Command> = ws
            .threads
            .iter()
            .take(50)
            .map(|thread| {
                Command::new(format!("chat:{}", thread.id), thread.title())
                    .icon(IconName::MessageSquare)
            })
            .collect();
        let close = cx.entity();
        let run = cx.entity();
        CommandPalette::new("palette", move |window, cx| {
            close.update(cx, |this, cx| this.close_overlay(window, cx))
        })
        .group("Actions", actions)
        .group("Chats", chats)
        .group("Access", modes)
        .group("Appearance", themes)
        .on_run(move |value, window, cx| {
            let value = value.to_string();
            run.update(cx, |this, cx| this.run_command(&value, window, cx))
        })
        .into_any_element()
    }

    fn run_command(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        let workspace = self.workspace.clone();
        match value {
            "new" => self.new_chat(window, cx),
            "review" => workspace.update(cx, |ws, cx| ws.review(None, cx)),
            "compact" => workspace.update(cx, |ws, cx| ws.compact(cx)),
            "attach" => self
                .composer
                .update(cx, |composer, cx| composer.pick_files(cx)),
            "folder" => composer::choose_folder(workspace, cx),
            "sidebar" => window.dispatch_action(Box::new(ToggleSidebar), cx),
            "settings" => self.open_settings(window, cx),
            "restart" => workspace.update(cx, |ws, cx| ws.connect(cx)),
            other => {
                if let Some(id) = other.strip_prefix("chat:") {
                    let id = id.to_string();
                    workspace.update(cx, |ws, cx| ws.open_thread(id, cx));
                } else if let Some(mode) = other.strip_prefix("mode:") {
                    if let Some(mode) = AccessMode::ALL
                        .into_iter()
                        .find(|m| m.sandbox_mode() == mode)
                    {
                        workspace.update(cx, |ws, cx| ws.set_mode(mode, cx));
                    }
                } else if let Some(theme) = other.strip_prefix("theme:") {
                    let pref = match theme {
                        "light" => ThemePref::Light,
                        "dark" => ThemePref::Dark,
                        _ => ThemePref::System,
                    };
                    workspace.update(cx, |ws, cx| ws.set_theme(pref, cx));
                }
            }
        }
        if !matches!(self.overlay, Some(Overlay::Settings { .. })) {
            Composer::focus_entity(&self.composer, window, cx);
        }
        cx.notify();
    }

    fn settings_dialog(
        &self,
        codex_path: &Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let ws = self.workspace.read(cx);
        let section = |title: &'static str, hint: &'static str, control: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Sm))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(control)
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_muted)
                        .child(hint),
                )
        };
        let appearance = {
            let workspace = self.workspace.clone();
            let selected = match ws.settings.theme {
                ThemePref::System => "system",
                ThemePref::Light => "light",
                ThemePref::Dark => "dark",
            };
            SegmentedControl::new("theme", selected)
                .segment("system", "System", Some(IconName::Monitor))
                .segment("light", "Light", Some(IconName::Sun))
                .segment("dark", "Dark", Some(IconName::Moon))
                .size(ControlSize::Sm)
                .on_change(move |value, _, cx| {
                    let pref = match value.as_ref() {
                        "light" => ThemePref::Light,
                        "dark" => ThemePref::Dark,
                        _ => ThemePref::System,
                    };
                    workspace.update(cx, |ws, cx| ws.set_theme(pref, cx));
                })
        };
        let access = {
            let workspace = self.workspace.clone();
            let mut control = SegmentedControl::new("access", ws.settings.mode.sandbox_mode())
                .size(ControlSize::Sm);
            for mode in AccessMode::ALL {
                control = control.segment(mode.sandbox_mode(), mode.label(), None);
            }
            control.on_change(move |value, _, cx| {
                if let Some(mode) = AccessMode::ALL
                    .into_iter()
                    .find(|m| m.sandbox_mode() == value.as_ref())
                {
                    workspace.update(cx, |ws, cx| ws.set_mode(mode, cx));
                }
            })
        };
        let about = format!(
            "{} · settings in {}",
            ws.backend_label,
            dirs::config_dir()
                .map(|dir| composer::short_path(&dir.join("AgentDesk")))
                .unwrap_or_default()
        );
        let close = cx.entity();
        let save = (cx.entity(), self.workspace.clone(), codex_path.clone());
        Dialog::new("settings", "Settings", move |window, cx| {
            close.update(cx, |this, cx| this.close_overlay(window, cx))
        })
        .detail(about)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_5()
                .w_full()
                .child(section(
                    "Appearance",
                    "Follow macOS, or keep one look.",
                    appearance.into_any_element(),
                ))
                .child(section(
                    "Default access for new chats",
                    ws.settings.mode.description(),
                    access.into_any_element(),
                ))
                .child(section(
                    "Codex CLI",
                    "Path to the codex binary. Saving restarts Codex.",
                    Input::new(codex_path).into_any_element(),
                )),
        )
        .action(|close| {
            Button::new("settings-cancel", "Close")
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, window, cx| close(window, cx))
        })
        .action(move |close| {
            let (root, workspace, field) = save;
            Button::new("settings-save", "Save")
                .primary()
                .on_click(move |_, window, cx| {
                    let text = field.read(cx).text().trim().to_string();
                    let path = (!text.is_empty()).then(|| std::path::PathBuf::from(text));
                    workspace.update(cx, |ws, cx| ws.set_codex_binary(path, cx));
                    root.update(cx, |_, cx| cx.notify());
                    close(window, cx);
                })
        })
        .into_any_element()
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Shortcuts need a focused element to start from: a click on empty
        // space must not leave the window deaf to them.
        if window.focused(cx).is_none() && self.overlay.is_none() {
            Composer::focus_entity(&self.composer, window, cx);
        }
        let colors = cx.theme().colors.clone();
        let collapsed = self.workspace.read(cx).settings.sidebar_collapsed;
        let sidebar = sidebar::render(
            &self.workspace,
            &self.search,
            cx.entity(),
            collapsed,
            window,
            cx,
        );
        let main = chat::render(
            &self.workspace,
            &self.composer,
            cx.entity(),
            collapsed,
            window,
            cx,
        );
        let overlay: Option<AnyElement> = match &self.overlay {
            None => None,
            Some(Overlay::Palette) => Some(self.palette(cx)),
            Some(Overlay::Settings { codex_path }) => {
                let codex_path = codex_path.clone();
                Some(self.settings_dialog(&codex_path, cx))
            }
            Some(Overlay::Rename { thread_id, field }) => {
                let (close, submit) = (cx.entity(), cx.entity());
                let (thread_id, workspace) = (thread_id.clone(), self.workspace.clone());
                Some(
                    PromptDialog::new("rename", "Rename chat", field, move |window, cx| {
                        close.update(cx, |this, cx| this.close_overlay(window, cx))
                    })
                    .submit("Rename")
                    .check(|text| {
                        if text.trim().is_empty() {
                            Err("A chat needs a name.".into())
                        } else {
                            Ok(())
                        }
                    })
                    .on_submit(move |text, window, cx| {
                        let (id, name) = (thread_id.clone(), text.to_string());
                        workspace.update(cx, |ws, cx| ws.rename(id, name, cx));
                        submit.update(cx, |this, cx| this.close_overlay(window, cx));
                    })
                    .into_any_element(),
                )
            }
        };
        FocusScope::new(&self.focus).root().size_full().child(
            div()
                .key_context("AgentDesk")
                .relative()
                .size_full()
                .flex()
                .flex_row()
                .bg(colors.bg)
                .text_color(colors.fg)
                .child(sidebar)
                .child(main)
                .children(overlay)
                .child(ToastViewport::new("toasts", &self.toaster)),
        )
    }
}
