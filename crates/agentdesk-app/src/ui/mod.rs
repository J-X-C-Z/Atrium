//! The window: a ChatGPT-style sidebar of conversations beside the chat.

mod chat;
mod rows;
mod sidebar;

use ely_gpui_component::{
    forms::TextInput,
    primitives::FocusScope,
    theme::{ActiveTheme, Mode, Theme},
};
use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, WindowAppearance, actions, div, prelude::*,
};
use gpui_kit::component::theme::{Theme as KitTheme, ThemeMode};

use crate::workspace::Workspace;

actions!(
    agentdesk,
    [NewChat, ToggleSidebar, StopTurn, Reconnect, Quit]
);

/// Height of the title bar strip, which on macOS holds the traffic lights.
pub const TITLE_BAR_HEIGHT: f32 = 44.;

pub struct RootView {
    focus: FocusHandle,
    workspace: Entity<Workspace>,
    draft: Entity<TextInput>,
    search: Entity<TextInput>,
    sidebar_collapsed: bool,
    _subscriptions: Vec<Subscription>,
}

impl RootView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = cx.new(Workspace::new);
        let draft = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line(1, 10)
                .placeholder("Message Codex")
        });
        let search = cx.new(|cx| TextInput::new(window, cx).placeholder("Search chats"));
        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe(&draft, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |_, window, cx| follow_appearance(window, cx)),
        ];
        follow_appearance(window, cx);
        let focus = cx.focus_handle();
        let input_focus = draft.focus_handle(cx);
        window.focus(&input_focus, cx);
        Self {
            focus,
            workspace,
            draft,
            search,
            sidebar_collapsed: false,
            _subscriptions: subscriptions,
        }
    }

    /// Sends what is in the composer.
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft.read(cx).text().to_string();
        if text.trim().is_empty() || self.workspace.read(cx).is_running() {
            return;
        }
        self.draft.update(cx, |input, cx| input.set_text("", cx));
        self.workspace.update(cx, |ws, cx| ws.send(text, cx));
        let focus = self.draft.focus_handle(cx);
        window.focus(&focus, cx);
    }

    fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.update(cx, |ws, cx| ws.new_chat(cx));
        let focus = self.draft.focus_handle(cx);
        window.focus(&focus, cx);
    }
}

/// Ely and Kit both follow the system's light or dark appearance.
fn follow_appearance(window: &mut Window, cx: &mut gpui::App) {
    let dark = matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    let (ely, kit) = if dark {
        (Mode::Dark, ThemeMode::Dark)
    } else {
        (Mode::Light, ThemeMode::Light)
    };
    Theme::set_mode(ely, cx);
    KitTheme::change(kit, Some(window), cx);
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let sidebar = (!self.sidebar_collapsed)
            .then(|| sidebar::render(&self.workspace, &self.search, cx.entity(), window, cx));
        let main = chat::render(
            &self.workspace,
            &self.draft,
            cx.entity(),
            self.sidebar_collapsed,
            window,
            cx,
        );
        FocusScope::new(&self.focus).root().size_full().child(
            div()
                .key_context("AgentDesk")
                .on_action(cx.listener(|this, _: &NewChat, window, cx| this.new_chat(window, cx)))
                .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                    this.sidebar_collapsed = !this.sidebar_collapsed;
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &StopTurn, _, cx| {
                    this.workspace.update(cx, |ws, cx| ws.stop(cx))
                }))
                .on_action(cx.listener(|this, _: &Reconnect, _, cx| {
                    this.workspace.update(cx, |ws, cx| ws.connect(cx))
                }))
                .size_full()
                .flex()
                .flex_row()
                .bg(colors.bg)
                .text_color(colors.fg)
                .children(sidebar)
                .child(main),
        )
    }
}
