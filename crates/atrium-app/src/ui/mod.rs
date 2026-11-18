//! The window: a ChatGPT-style sidebar of conversations beside the chat,
//! with a command palette, settings and toasts over it.

mod chat;
mod composer;
mod context;
mod rows;
mod sidebar;

use atrium_core::{Account, plan_label};
use codex_client::AccessMode;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    data_display::{Avatar, Badge, Tone},
    feedback::{Toast, ToastViewport, Toaster},
    forms::{Input, Switch, TextInput},
    motion::ProgressBar,
    navigation::{Command, CommandPalette},
    overlays::{Dialog, PromptDialog},
    primitives::{FocusScope, IconName, Severity},
    theme::{ActiveTheme, ControlSize, Mode, TextSize, Theme},
};
use gpui::{
    Action, AnyElement, App, Context, Entity, FocusHandle, Global, InteractiveElement as _,
    IntoElement, ParentElement, Render, StatefulInteractiveElement as _, Styled, Subscription,
    WeakEntity, Window, WindowAppearance, actions, div, prelude::*,
};
use gpui_kit::component::theme::{Theme as KitTheme, ThemeMode};

use crate::{
    settings::ThemePref,
    workspace::{Fill, Notice, Workspace},
};
pub use composer::AcceptPrediction;
use composer::Composer;

actions!(
    atrium,
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
        ShowContext,
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
    forward::<ShowContext>(cx, |this, _, cx| {
        if matches!(this.overlay, Some(Overlay::Context)) {
            this.overlay = None;
            cx.notify();
        } else {
            this.open_context(cx);
        }
    });
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
    Account,
    Context,
    Goal {
        objective: Entity<TextInput>,
        budget: Entity<TextInput>,
    },
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
            cx.observe(&search, |this, search, cx| {
                let query = search.read(cx).text().to_string();
                this.workspace
                    .update(cx, |ws, cx| ws.search_chats(query, cx));
                cx.notify()
            }),
            cx.subscribe(&workspace, |this, _, notice: &Notice, cx| {
                this.toast(notice, cx)
            }),
            cx.subscribe_in(&workspace, window, |this, _, fill: &Fill, window, cx| {
                let composed = fill.0.clone();
                this.composer
                    .update(cx, |composer, cx| composer.fill(composed, window, cx));
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

    pub fn open_account(&mut self, cx: &mut Context<Self>) {
        self.workspace.update(cx, |ws, cx| ws.refresh_usage(cx));
        self.overlay = Some(Overlay::Account);
        cx.notify();
    }

    pub fn open_goal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.workspace.read(cx).goal().cloned();
        let objective = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line(2, 5)
                .placeholder("What should Codex keep working toward?")
        });
        let budget = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder("Token budget (optional), such as 500000")
                .filter(|c| c.is_ascii_digit())
        });
        if let Some(goal) = current {
            objective.update(cx, |input, cx| input.set_text(goal.objective, cx));
            if let Some(tokens) = goal.token_budget {
                budget.update(cx, |input, cx| input.set_text(tokens.to_string(), cx));
            }
        }
        self.overlay = Some(Overlay::Goal { objective, budget });
        cx.notify();
    }

    fn goal_dialog(
        &self,
        objective: &Entity<TextInput>,
        budget: &Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let close = cx.entity();
        let start = (
            cx.entity(),
            self.workspace.clone(),
            objective.clone(),
            budget.clone(),
        );
        Dialog::new("goal", "Set a goal", move |window, cx| {
            close.update(cx, |this, cx| this.close_overlay(window, cx))
        })
        .detail("Codex keeps working toward the goal across turns until it is done, blocked, or out of budget.")
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .child(Input::new(objective))
                .child(Input::new(budget))
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_muted)
                        .child("Long goals use your plan's usage. Pause or clear the goal from the banner at any time."),
                ),
        )
        .action(|close| {
            Button::new("goal-cancel", "Cancel")
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, window, cx| close(window, cx))
        })
        .action(move |close| {
            let (root, workspace, objective, budget) = start;
            Button::new("goal-start", "Start working")
                .primary()
                .on_click(move |_, window, cx| {
                    let text = objective.read(cx).text().trim().to_string();
                    if text.is_empty() {
                        return;
                    }
                    let tokens = budget.read(cx).text().trim().parse::<u64>().ok();
                    workspace.update(cx, |ws, cx| ws.set_goal(text, tokens, cx));
                    root.update(cx, |_, cx| cx.notify());
                    close(window, cx);
                })
        })
        .into_any_element()
    }

    fn account_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let ws = self.workspace.read(cx);
        let close = cx.entity();
        let mut body = div().w_full().flex().flex_col().gap_4();
        let signed_in = ws.account.is_some();
        match &ws.account {
            Some(Account::ChatGpt {
                email,
                plan,
                workspace,
            }) => {
                let who = email.clone().unwrap_or_else(|| "ChatGPT account".into());
                body = body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(Avatar::new("account-avatar", who.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().font_weight(gpui::FontWeight::MEDIUM).child(who))
                                .children(workspace.clone().map(|w| {
                                    div()
                                        .text_size(theme.text_size(TextSize::Xs))
                                        .text_color(colors.fg_muted)
                                        .child(w)
                                })),
                        )
                        .child(Badge::new(plan_label(plan)).tone(Tone::Accent)),
                );
            }
            Some(Account::ApiKey) => {
                body = body.child(div().child(
                    "Signed in with an API key: usage is billed per token, with no plan limits.",
                ));
            }
            Some(Account::Other(kind)) => {
                body = body.child(div().child(format!("Signed in through {kind}.")));
            }
            None => {
                body =
                    body.child(div().text_color(colors.fg_muted).child(
                        "Sign in with ChatGPT to use your Plus or Pro subscription in Codex.",
                    ));
            }
        }
        if let Some(limits) = &ws.rate_limits {
            let mut usage = div().flex().flex_col().gap_3();
            for (ix, window) in [limits.primary, limits.secondary]
                .into_iter()
                .flatten()
                .enumerate()
            {
                let reset = window.resets_at.map(resets_in).unwrap_or_default();
                usage = usage.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_size(theme.text_size(TextSize::Sm))
                                .child(window.label())
                                .child(
                                    div()
                                        .text_color(colors.fg_muted)
                                        .child(format!("{:.0}% used{reset}", window.used_percent)),
                                ),
                        )
                        .child(ProgressBar::new(
                            ("usage-bar", ix),
                            (window.used_percent / 100.) as f32,
                        )),
                );
            }
            if let Some(credits) = &limits.credits {
                let said = if credits.unlimited {
                    "Unlimited credits".to_string()
                } else {
                    match &credits.balance {
                        Some(balance) => format!("{balance} credits left"),
                        None if credits.has_credits => "Credits available".into(),
                        None => "No credits".into(),
                    }
                };
                usage = usage.child(
                    div()
                        .text_size(theme.text_size(TextSize::Sm))
                        .text_color(colors.fg_muted)
                        .child(said),
                );
            }
            if limits.reached {
                usage = usage.child(
                    div()
                        .text_size(theme.text_size(TextSize::Sm))
                        .text_color(colors.danger)
                        .child("A usage limit is reached; Codex waits until it resets."),
                );
            }
            body = body.child(usage);
        }
        if let Some((remembered, ready)) = ws.memory {
            body = body.child(
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(if ready {
                        format!("Memory: {remembered} chats remembered. Turn it off per chat from the chat menu.")
                    } else {
                        "Memory is getting ready.".to_string()
                    }),
            );
        }
        let pending = ws.login.is_some();
        let workspace = self.workspace.clone();
        Dialog::new("account", "Account & usage", move |window, cx| {
            close.update(cx, |this, cx| this.close_overlay(window, cx))
        })
        .child(body)
        .action(move |close| {
            let workspace = workspace.clone();
            match (signed_in, pending) {
                (true, _) => Button::new("sign-out", "Sign out")
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| {
                        workspace.update(cx, |ws, cx| ws.sign_out(cx));
                        close(window, cx);
                    }),
                (false, true) => Button::new("cancel-sign-in", "Cancel sign-in")
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, _, cx| workspace.update(cx, |ws, cx| ws.cancel_sign_in(cx))),
                (false, false) => Button::new("sign-in", "Sign in with ChatGPT")
                    .primary()
                    .on_click(move |_, _, cx| workspace.update(cx, |ws, cx| ws.sign_in(cx))),
            }
        })
        .action(|close| {
            Button::new("account-close", "Done").on_click(move |_, window, cx| close(window, cx))
        })
        .into_any_element()
    }

    /// Each agent, whether it runs, and how to get it when it does not.
    fn agents_list(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::workspace::{AgentStatus, Status};
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let ws = self.workspace.read(cx);
        let row = |name: String, dot: gpui::Hsla, said: String| {
            div()
                .flex()
                .items_start()
                .gap_2()
                .text_size(theme.text_size(TextSize::Sm))
                .child(
                    div()
                        .mt_1p5()
                        .size(gpui::px(7.))
                        .flex_none()
                        .rounded_full()
                        .bg(dot),
                )
                .child(div().w(gpui::px(130.)).flex_none().child(name))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(colors.fg_muted)
                        .child(said),
                )
        };
        let codex = match &ws.status {
            Status::Ready => (colors.success, ws.backend_label.clone()),
            Status::Connecting => (colors.warning, "Starting…".into()),
            Status::Failed(reason) => (colors.danger, reason.clone()),
        };
        let mut list =
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(row("Codex".into(), codex.0, codex.1));
        for agent in &ws.agents {
            let (dot, said) = match &agent.status {
                AgentStatus::Ready => (
                    colors.success,
                    agent
                        .info
                        .as_ref()
                        .map(|i| {
                            format!(
                                "{} {}",
                                if i.title.is_empty() {
                                    &i.name
                                } else {
                                    &i.title
                                },
                                i.version
                            )
                        })
                        .unwrap_or_else(|| "Ready".into()),
                ),
                AgentStatus::Starting => (colors.warning, "Starting…".into()),
                AgentStatus::NotInstalled => (colors.fg_subtle, agent.profile.install.to_string()),
                AgentStatus::Failed(reason) => (colors.danger, reason.clone()),
            };
            list = list.child(row(agent.profile.name.to_string(), dot, said));
        }
        list.into_any_element()
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
            (
                "goal",
                "Set a goal for this chat…",
                IconName::ListChecks,
                "",
            ),
            ("branch", "Branch this chat", IconName::GitBranch, ""),
            (
                "memory",
                "Toggle memory for this chat",
                IconName::History,
                "",
            ),
            ("usage", "Account & usage", IconName::Gauge, ""),
            ("context", "Context & speed", IconName::Zap, "⌘I"),
            ("fast", "Toggle Fast mode", IconName::Zap, ""),
            ("plan", "Toggle Plan mode", IconName::ListChecks, ""),
            (
                "predict",
                "Toggle next-message predictions",
                IconName::Sparkles,
                "",
            ),
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
        let agents: Vec<Command> = std::iter::once(("codex", "Codex"))
            .chain(
                ws.agents
                    .iter()
                    .filter(|a| a.status != crate::workspace::AgentStatus::NotInstalled)
                    .map(|a| (a.profile.id, a.profile.name)),
            )
            .map(|(id, name)| {
                Command::new(format!("agent:{id}"), format!("New chat with {name}"))
                    .icon(IconName::Bot)
            })
            .collect();
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
        .group("Agents", agents)
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
            "goal" => self.open_goal(window, cx),
            "branch" => workspace.update(cx, |ws, cx| ws.branch(None, cx)),
            "memory" => workspace.update(cx, |ws, cx| ws.toggle_memory(cx)),
            "usage" => self.open_account(cx),
            "context" => self.open_context(cx),
            "fast" | "plan" | "predict" => workspace.update(cx, |ws, cx| {
                match value {
                    "fast" => ws.settings.fast = !ws.settings.fast,
                    "plan" => ws.settings.plan_mode = !ws.settings.plan_mode,
                    _ => ws.settings.no_predictions = !ws.settings.no_predictions,
                }
                ws.settings.save();
                cx.notify();
            }),
            other => {
                if let Some(agent) = other.strip_prefix("agent:") {
                    let agent = agent.to_string();
                    workspace.update(cx, |ws, cx| {
                        ws.set_draft_backend(&agent, cx);
                        ws.new_chat(cx);
                    });
                } else if let Some(id) = other.strip_prefix("chat:") {
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
        let agents = self.agents_list(cx);
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
                .map(|dir| composer::short_path(&dir.join("Atrium")))
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
                .id("settings-body")
                .max_h(gpui::px(480.))
                .overflow_y_scroll()
                .pr_1()
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
                    "Suggestions",
                    "After each answer Codex guesses your next message; Tab takes it. Each guess uses a little of your plan.",
                    Switch::new("predict", !ws.settings.no_predictions)
                        .label("Predict my next message")
                        .on_change({
                            let workspace = self.workspace.clone();
                            move |on, _, cx| {
                                workspace.update(cx, |ws, cx| {
                                    ws.settings.no_predictions = !on;
                                    ws.settings.save();
                                    cx.notify();
                                })
                            }
                        })
                        .into_any_element(),
                ))
                .child(section(
                    "Agents",
                    "Claude Code, DeepSeek Harness and Antigravity run over the Agent Client Protocol. Set a command per agent with `agent_commands` in settings.json.",
                    agents,
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
            Some(Overlay::Account) => Some(self.account_dialog(cx)),
            Some(Overlay::Context) => Some(self.context_dialog(cx)),
            Some(Overlay::Goal { objective, budget }) => {
                let (objective, budget) = (objective.clone(), budget.clone());
                Some(self.goal_dialog(&objective, &budget, cx))
            }
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
                .key_context("Atrium")
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

/// " · resets in 2h 17m" for a reset time in Unix seconds.
fn resets_in(at: i64) -> String {
    let left = at - jiff::Timestamp::now().as_second();
    if left <= 0 {
        return " · resets now".into();
    }
    let (days, hours, mins) = (left / 86400, left % 86400 / 3600, left % 3600 / 60);
    match (days, hours) {
        (0, 0) => format!(" · resets in {mins}m"),
        (0, h) => format!(" · resets in {h}h {mins}m"),
        (d, h) => format!(" · resets in {d}d {h}h"),
    }
}
