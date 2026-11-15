//! Main column: title bar with model and folder, the conversation, the composer.

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    chat::{ChatContainer, ErrorMessage, MessageList, PromptInput, SuggestionChips, WelcomeScreen},
    forms::TextInput,
    menus::{DropdownMenu, Menu, MenuItem},
    primitives::IconName,
    shell::drag_region,
    theme::{ActiveTheme, ControlSize, TextSize},
};
use gpui::{
    AnyElement, App, Entity, Focusable, IntoElement, ParentElement, PathPromptOptions, Styled,
    Window, div, prelude::*, px,
};
use jiff::{Timestamp, tz::TimeZone};

use super::{
    NewChat, RootView, TITLE_BAR_HEIGHT, ToggleSidebar, rows, sidebar::traffic_light_room,
};
use crate::workspace::{Status, View, Workspace};

const STARTERS: [(&str, IconName); 3] = [
    ("Explain how this project is organized", IconName::FileText),
    ("Find and fix a failing test", IconName::Terminal),
    ("Review my latest changes", IconName::GitBranch),
];

pub fn render(
    workspace: &Entity<Workspace>,
    draft: &Entity<TextInput>,
    root: Entity<RootView>,
    sidebar_collapsed: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors.clone();
    let header = header(workspace, sidebar_collapsed, window, cx);
    let body = body(workspace, draft, root, window, cx);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(colors.bg)
        .child(header)
        .child(div().flex_1().min_h_0().child(body))
        .into_any_element()
}

fn header(
    workspace: &Entity<Workspace>,
    sidebar_collapsed: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let bar = drag_region("chat-title", window, cx);
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let title = ws
        .active_thread()
        .map(|thread| thread.title())
        .unwrap_or_else(|| "New chat".into());
    let thread_id = ws.active_thread().map(|thread| thread.id.clone());
    let (folder, folder_fixed) = match ws.active_thread() {
        Some(thread) => (thread.cwd.clone(), true),
        None => (ws.settings.working_dir().display().to_string(), false),
    };
    let folder_name = folder
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("/")
        .to_string();
    let usage = ws.transcript().and_then(|t| t.usage()).map(|usage| {
        let tokens = format_tokens(usage.total_tokens);
        match usage.context_window {
            Some(window) if window > 0 => {
                let share = usage.last_turn_tokens as f64 * 100. / window as f64;
                if share < 1. {
                    format!("{tokens} tokens · <1% of context")
                } else {
                    format!("{tokens} tokens · {share:.0}% of context")
                }
            }
            _ => format!("{tokens} tokens"),
        }
    });

    let mut menu = Menu::new().item(
        MenuItem::radio("Codex default", ws.settings.model.is_none()).on_click({
            let workspace = workspace.clone();
            move |_, cx| workspace.update(cx, |ws, cx| ws.set_model(None, cx))
        }),
    );
    for model in &ws.models {
        let id = model.id.clone();
        let workspace = workspace.clone();
        menu = menu.item(
            MenuItem::radio(
                model.display_name.clone(),
                ws.settings.model.as_deref() == Some(&model.id),
            )
            .on_click(move |_, cx| {
                workspace.update(cx, |ws, cx| ws.set_model(Some(id.clone()), cx))
            }),
        );
    }
    let model_label = ws.model_label();

    let pick_folder = workspace.clone();
    let archive = workspace.clone();
    bar.flex_none()
        .h(px(TITLE_BAR_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(colors.border)
        .when(sidebar_collapsed, |bar| {
            bar.pl(traffic_light_room())
                .child(
                    IconButton::new("show-sidebar", IconName::PanelLeft)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("Show sidebar (⌘B)")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleSidebar), cx)
                        }),
                )
                .child(
                    IconButton::new("new-chat-collapsed", IconName::SquarePen)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("New chat (⌘N)")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewChat), cx)),
                )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(theme.text_size(TextSize::Sm))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(title),
        )
        .children(usage.map(|usage| {
            div()
                .flex_none()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_muted)
                .child(usage)
        }))
        .child(
            DropdownMenu::new("model", model_label, menu)
                .icon(IconName::Cpu)
                .variant(ButtonVariant::Ghost),
        )
        .child(
            Button::new("cwd", folder_name)
                .icon(IconName::Folder)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .disabled(folder_fixed)
                .on_click(move |_, _, cx| choose_folder(pick_folder.clone(), cx)),
        )
        .children(thread_id.map(|id| {
            IconButton::new("archive", IconName::Archive)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .tooltip("Archive this chat")
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    archive.update(cx, |ws, cx| ws.archive(id, cx))
                })
        }))
        .into_any_element()
}

fn choose_folder(workspace: Entity<Workspace>, cx: &mut App) {
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

fn composer(
    workspace: &Entity<Workspace>,
    draft: &Entity<TextInput>,
    root: Entity<RootView>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);
    let running = ws.is_running();
    let notice = match &ws.status {
        Status::Connecting => Some("Starting Codex…".to_string()),
        Status::Failed(_) => Some("Codex is not running. Restart it from the sidebar.".to_string()),
        // A retry in progress says more than a standing sign-in hint.
        Status::Ready => ws
            .transcript()
            .and_then(|t| t.notice())
            .map(str::to_owned)
            .or_else(|| {
                (ws.signed_in == Some(false))
                    .then(|| "Codex is not signed in: run `codex login` in a terminal.".to_string())
            }),
    };
    let mut input = PromptInput::new("composer", draft, move |window, cx| {
        root.update(cx, |this, cx| this.send(window, cx))
    });
    if running {
        let stop = workspace.clone();
        input = input.busy(move |_, cx| stop.update(cx, |ws, cx| ws.stop(cx)));
    }
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .children(notice.map(|notice| {
            div()
                .px_2()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_muted)
                .child(notice)
        }))
        .child(input)
        .into_any_element()
}

fn body(
    workspace: &Entity<Workspace>,
    draft: &Entity<TextInput>,
    root: Entity<RootView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let ws = workspace.read(cx);

    if let Status::Failed(reason) = &ws.status
        && ws.transcript().is_none()
    {
        let retry = workspace.clone();
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                div().max_w(px(520.)).child(
                    ErrorMessage::new("codex-failed", "Could not start Codex")
                        .detail(reason.clone())
                        .on_retry(move |_, cx| retry.update(cx, |ws, cx| ws.connect(cx))),
                ),
            )
            .into_any_element();
    }

    if ws.is_loading() {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(colors.fg_muted)
            .text_size(theme.text_size(TextSize::Sm))
            .child("Loading chat…")
            .into_any_element();
    }

    let Some(transcript) = ws.transcript() else {
        let hour = Timestamp::now()
            .to_zoned(TimeZone::try_system().unwrap_or(TimeZone::UTC))
            .hour() as u8;
        let fill = draft.clone();
        return WelcomeScreen::new("welcome", hour)
            .child(composer(workspace, draft, root, cx))
            .child(SuggestionChips::new(
                "starters",
                STARTERS,
                move |ix, window, cx| {
                    fill.update(cx, |input, cx| input.set_text(STARTERS[ix].0, cx));
                    let focus = fill.focus_handle(cx);
                    window.focus(&focus, cx);
                },
            ))
            .into_any_element();
    };

    let count = transcript.len();
    let list_id = match &ws.view {
        View::Thread(id) => format!("messages-{id}"),
        View::Draft => "messages-draft".to_string(),
    };
    let rows_from = workspace.clone();
    let messages = MessageList::new(list_id, count, move |ix, _, cx| {
        let ws = rows_from.read(cx);
        let thread = match &ws.view {
            View::Thread(id) => Some(id.clone()),
            View::Draft => None,
        };
        let Some(entry) = ws.transcript().and_then(|t| t.entries().get(ix)).cloned() else {
            return div().into_any_element();
        };
        rows::entry(&rows_from, thread.as_deref(), ix, &entry, cx)
    });
    ChatContainer::new(messages)
        .composer(composer(workspace, draft, root, cx))
        .into_any_element()
}

fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_read_short() {
        assert_eq!(super::format_tokens(950), "950");
        assert_eq!(super::format_tokens(12_345), "12.3k");
        assert_eq!(super::format_tokens(2_500_000), "2.5M");
    }
}
