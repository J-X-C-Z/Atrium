//! The sidebar's conversation list: each chat shows its agent's icon, its
//! title (or the user's nickname for it), and, when another device runs it, a
//! small tag with the device's name. Chats sit under Pinned, Working, Recent
//! and Archived; the last is folded away until asked for. Archive and delete
//! are undoable: the backend hears about a delete only after the undo window.

use std::rc::Rc;

use ely_gpui_component::{
    chat::ConversationStatus,
    i18n::word,
    menus::{Menu, MenuItem, OverflowMenu},
    motion::{Spinner, SpinnerStyle},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
    typography::Ellipsis,
};
use gpui::{
    App, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px, rgb,
    transparent_black,
};

use crate::chat_list_model::{Activity, RowFacts, Section};
use crate::i18n::t;
use crate::workspace::CODEX;

/// One chat in the list.
#[derive(Clone, Debug)]
pub struct ChatRow {
    pub key: SharedString,
    pub title: String,
    /// Unix seconds.
    pub at: i64,
    pub pinned: bool,
    pub archived: bool,
    pub status: ConversationStatus,
    /// The agent's id (`codex`, `claude`, ...), for its icon.
    pub agent: String,
    /// The device's name, for chats that run elsewhere.
    pub device: Option<String>,
}

impl ChatRow {
    fn facts(&self) -> RowFacts {
        RowFacts {
            at: self.at,
            pinned: self.pinned,
            archived: self.archived,
            activity: match self.status {
                ConversationStatus::Idle => Activity::Idle,
                ConversationStatus::Working => Activity::Working,
                ConversationStatus::Waiting => Activity::Waiting,
                ConversationStatus::Unread(n) => Activity::Unread(n),
            },
        }
    }
}

/// The icon and tint for an agent's chats. The component set has no brand
/// marks, so each agent gets a distinct icon and colour.
pub fn agent_icon(agent: &str) -> (IconName, Option<Hsla>) {
    match agent {
        a if a == CODEX => (IconName::Terminal, None),
        "claude" => (IconName::Sparkles, Some(rgb(0xD97757).into())),
        "antigravity" => (IconName::Rocket, Some(rgb(0x4285F4).into())),
        "deepseek" => (IconName::Compass, Some(rgb(0x4D6BFE).into())),
        _ => (IconName::Bot, None),
    }
}

/// Whether every word of `query` is in the row's title or device name.
fn matches(row: &ChatRow, query: &str) -> bool {
    let haystack = format!("{} {}", row.title, row.device.as_deref().unwrap_or("")).to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

type OnKey = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type OnToggle = Rc<dyn Fn(&mut Window, &mut App)>;
type ExtraFor = dyn Fn(&SharedString) -> Vec<Extra>;

/// An extra menu entry on one chat: label, icon, action.
pub type Extra = (SharedString, IconName, OnKey);

/// What the list can do to a chat.
pub struct ChatCallbacks {
    pub select: OnKey,
    pub rename: OnKey,
    pub nickname: OnKey,
    pub pin: OnKey,
    pub archive: OnKey,
    pub unarchive: OnKey,
    pub delete: OnKey,
    pub toggle_archived: OnToggle,
    pub extra: Rc<ExtraFor>,
}

#[derive(IntoElement)]
pub struct ChatList {
    rows: Vec<ChatRow>,
    active: Option<SharedString>,
    query: String,
    archived_open: bool,
    callbacks: ChatCallbacks,
}

impl ChatList {
    pub fn new(
        rows: Vec<ChatRow>,
        active: Option<SharedString>,
        query: String,
        archived_open: bool,
        callbacks: ChatCallbacks,
    ) -> Self {
        Self {
            rows,
            active,
            query,
            archived_open,
            callbacks,
        }
    }
}

fn section_label(section: Section) -> &'static str {
    match section {
        Section::Pinned => t("Pinned"),
        Section::Working => t("Working"),
        Section::Recent => t("Recent"),
        Section::Archived => t("Archived"),
    }
}

impl RenderOnce for ChatList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        // Search filters first, then the remaining rows are grouped, so search
        // matches keep their places in the sections.
        let kept: Vec<usize> = (0..self.rows.len())
            .filter(|ix| matches(&self.rows[*ix], &self.query))
            .collect();
        let facts: Vec<RowFacts> = kept.iter().map(|&ix| self.rows[ix].facts()).collect();
        let groups = crate::chat_list_model::grouped(&facts);
        let empty = groups.is_empty();
        // Searching opens the archive so a match is never hidden.
        let archived_open = self.archived_open || !self.query.trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_2()
            .when(empty, |list| {
                list.child(
                    div()
                        .px_2()
                        .text_size(theme.text_size(TextSize::Sm))
                        .text_color(colors.fg_subtle)
                        .child(if self.query.trim().is_empty() {
                            word("chat.empty")
                        } else {
                            word("chat.no_match")
                        }),
                )
            })
            .children(groups.into_iter().map(|(section, items)| {
                let archived = section == Section::Archived;
                let folded = archived && !archived_open;
                let toggle = self.callbacks.toggle_archived.clone();
                let header = div()
                    .id(SharedString::from(format!("chat-section-{section:?}")))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pb_1()
                    .text_size(theme.text_size(TextSize::Xs))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.fg_subtle)
                    .when(archived, |header| {
                        header
                            .cursor_pointer()
                            .rounded(theme.radius(Radius::Sm))
                            .hover(|header| header.bg(colors.hover))
                            .tooltip(ely_gpui_component::primitives::Tooltip::text(if folded {
                                t("Show archived conversations")
                            } else {
                                t("Hide archived conversations")
                            }))
                            .on_click(move |_, window, cx| toggle(window, cx))
                            .child(
                                Icon::new(if folded {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(IconSize::Xs)
                                .color(colors.fg_subtle),
                            )
                    })
                    .child(section_label(section))
                    .when(archived, |header| {
                        header.child(format!("· {}", items.len()))
                    });
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(header)
                    .when(!folded, |column| {
                        column.children(items.into_iter().map(|ix| {
                            let row = self.rows[kept[ix]].clone();
                            let active = self.active.as_ref() == Some(&row.key);
                            chat_row(row, active, &self.callbacks, cx)
                        }))
                    })
            }))
    }
}

fn chat_row(row: ChatRow, active: bool, callbacks: &ChatCallbacks, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let id: ElementId = format!("chat-row-{}", row.key).into();
    let group = SharedString::from(format!("chat-group-{}", row.key));
    let key = row.key.clone();
    let (a, b, c, d, e, f) = (
        key.clone(),
        key.clone(),
        key.clone(),
        key.clone(),
        key.clone(),
        key.clone(),
    );
    let (select_cb, rename_cb, nickname_cb, pin_cb, archive_cb, unarchive_cb, delete_cb) = (
        callbacks.select.clone(),
        callbacks.rename.clone(),
        callbacks.nickname.clone(),
        callbacks.pin.clone(),
        callbacks.archive.clone(),
        callbacks.unarchive.clone(),
        callbacks.delete.clone(),
    );
    let mut menu = Menu::new()
        .item(
            MenuItem::new(word("chat.rename"))
                .icon(IconName::Pencil)
                .on_click(move |window, cx| rename_cb(&a, window, cx)),
        )
        .item(
            MenuItem::new(t("Nickname…"))
                .icon(IconName::Tag)
                .on_click(move |window, cx| nickname_cb(&b, window, cx)),
        )
        .item(
            match row.pinned {
                true => MenuItem::new(word("chat.unpin")).icon(IconName::PinOff),
                false => MenuItem::new(word("chat.pin")).icon(IconName::Pin),
            }
            .on_click(move |window, cx| pin_cb(&c, window, cx)),
        );
    for (label, icon, action) in (callbacks.extra)(&row.key) {
        let k = key.clone();
        menu = menu.item(
            MenuItem::new(label)
                .icon(icon)
                .on_click(move |window, cx| action(&k, window, cx)),
        );
    }
    menu = menu.separator().item(if row.archived {
        MenuItem::new(t("Unarchive"))
            .icon(IconName::Inbox)
            .on_click(move |window, cx| unarchive_cb(&d, window, cx))
    } else {
        MenuItem::new(t("Archive chat"))
            .icon(IconName::Archive)
            .on_click(move |window, cx| archive_cb(&e, window, cx))
    });
    let menu = menu.item(
        MenuItem::new(t("Delete chat"))
            .icon(IconName::Trash2)
            .on_click(move |window, cx| delete_cb(&f, window, cx)),
    );
    let (icon, tint) = agent_icon(&row.agent);
    let select_key = key.clone();
    div()
        .id(id.clone())
        .group(group.clone())
        .relative()
        .flex()
        .items_center()
        .gap_1p5()
        .h(theme.control_height(ControlSize::Md))
        .pl_2()
        .pr_0p5()
        .rounded(theme.radius(Radius::Md))
        .border_1()
        .border_color(transparent_black())
        .text_size(theme.text_size(TextSize::Sm))
        .text_color(if active { colors.fg } else { colors.fg_muted })
        .when(active, |r| r.bg(colors.active))
        .when(!active, |r| r.hover(|r| r.bg(colors.hover)))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| select_cb(&select_key, window, cx))
        .child(
            div().flex_none().child(
                Icon::new(icon)
                    .size(IconSize::Sm)
                    .color(tint.unwrap_or(colors.fg_subtle)),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Ellipsis::new(SharedString::from(row.title.clone()))),
        )
        .when(row.pinned, |r| {
            r.child(
                Icon::new(IconName::Pin)
                    .size(IconSize::Xs)
                    .color(colors.fg_subtle),
            )
        })
        .when(row.archived, |r| {
            r.child(
                Icon::new(IconName::Archive)
                    .size(IconSize::Xs)
                    .color(colors.fg_subtle),
            )
        })
        .children(row.device.clone().map(|device| {
            div()
                .flex_none()
                .max_w(px(84.))
                .truncate()
                .px_1p5()
                .rounded_full()
                .bg(colors.sunken)
                .border_1()
                .border_color(colors.border)
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(colors.fg_muted)
                .child(device)
        }))
        .child(match row.status {
            ConversationStatus::Idle => div().into_any_element(),
            ConversationStatus::Working => div()
                .flex_none()
                .px_1()
                .child(
                    Spinner::new((id.clone(), "working"))
                        .style(SpinnerStyle::Ring)
                        .size(IconSize::Xs)
                        .color(colors.fg_muted),
                )
                .into_any_element(),
            ConversationStatus::Waiting => div()
                .flex_none()
                .mx_1()
                .size(px(7.))
                .rounded_full()
                .bg(colors.warning)
                .into_any_element(),
            ConversationStatus::Unread(n) => div()
                .flex_none()
                .min_w(px(18.))
                .h(px(18.))
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(colors.info.opacity(0.16))
                .text_color(colors.info)
                .text_size(theme.text_size(TextSize::Xs))
                .font_weight(FontWeight::MEDIUM)
                .child(if n > 99 {
                    "99+".to_string()
                } else {
                    n.to_string()
                })
                .into_any_element(),
        })
        // The menu floats over the row's end on hover, so it takes no room
        // from the title.
        .child(
            div()
                .absolute()
                .right_0p5()
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .pl_1()
                .rounded(theme.radius(Radius::Md))
                .bg(if active { colors.active } else { colors.hover })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .invisible()
                .group_hover(group, |style| style.visible())
                .child(OverflowMenu::new((id, "menu"), menu).tooltip(word("chat.actions"))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_has_its_own_icon() {
        let icons: Vec<IconName> = ["codex", "claude", "antigravity", "deepseek"]
            .iter()
            .map(|a| agent_icon(a).0)
            .collect();
        for (i, a) in icons.iter().enumerate() {
            assert!(!icons[..i].contains(a));
        }
    }

    #[test]
    fn search_matches_nickname_and_device() {
        let row = ChatRow {
            key: "a".into(),
            title: "Daily driver".into(),
            at: 1,
            pinned: false,
            archived: false,
            status: ConversationStatus::Idle,
            agent: "codex".into(),
            device: Some("studio".into()),
        };
        assert!(matches(&row, "daily"));
        assert!(matches(&row, "STUDIO daily"));
        assert!(!matches(&row, "build"));
    }
}
