//! The sidebar's conversation list: each chat shows its agent's icon, its
//! title, and, when another device runs it, a small tag with the device's
//! name. Grouped by day like Ely's `ConversationList`, which has no room for
//! an icon or a tag.

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
use jiff::{Timestamp, civil::Date, tz::TimeZone};

use crate::workspace::CODEX;

/// One chat in the list.
#[derive(Clone, Debug)]
pub struct ChatRow {
    pub key: SharedString,
    pub title: String,
    pub at: Timestamp,
    pub pinned: bool,
    pub status: ConversationStatus,
    /// The agent's id (`codex`, `claude`, ...), for its icon.
    pub agent: String,
    /// The device's name, for chats that run elsewhere.
    pub device: Option<String>,
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

fn group_name(day: Date, today: Date) -> String {
    let days = (today - day).get_days();
    match days {
        ..=0 => word("chat.group.today").to_string(),
        1 => word("chat.group.yesterday").to_string(),
        2..=7 => word("chat.group.week").to_string(),
        8..=30 => word("chat.group.month").to_string(),
        _ if day.year() == today.year() => day.strftime(word("chat.group.month_name")).to_string(),
        _ => day.strftime(word("chat.group.month_year")).to_string(),
    }
}

/// The rows `query` keeps: pinned first, then under their day, newest first.
fn grouped(
    rows: &[ChatRow],
    query: &str,
    today: Date,
    zone: &TimeZone,
) -> Vec<(String, Vec<usize>)> {
    let mut kept: Vec<usize> = (0..rows.len())
        .filter(|ix| matches(&rows[*ix], query))
        .collect();
    kept.sort_by_key(|ix| std::cmp::Reverse(rows[*ix].at));
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let pinned: Vec<usize> = kept.iter().copied().filter(|ix| rows[*ix].pinned).collect();
    if !pinned.is_empty() {
        groups.push((word("chat.group.pinned").to_string(), pinned));
    }
    for ix in kept.into_iter().filter(|ix| !rows[*ix].pinned) {
        let name = group_name(rows[ix].at.to_zoned(zone.clone()).date(), today);
        match groups.last_mut() {
            Some((last, items)) if *last == name => items.push(ix),
            _ => groups.push((name, vec![ix])),
        }
    }
    groups
}

type OnKey = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type ExtraFor = dyn Fn(&SharedString) -> Vec<Extra>;

/// An extra menu entry on one chat: label, icon, action.
pub type Extra = (SharedString, IconName, OnKey);

#[derive(IntoElement)]
pub struct ChatList {
    rows: Vec<ChatRow>,
    active: Option<SharedString>,
    query: String,
    today: Date,
    on_select: OnKey,
    on_rename: OnKey,
    on_pin: OnKey,
    on_delete: OnKey,
    extra: Rc<ExtraFor>,
}

impl ChatList {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rows: Vec<ChatRow>,
        active: Option<SharedString>,
        query: String,
        today: Date,
        on_select: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
        on_rename: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
        on_pin: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
        on_delete: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
        extra: impl Fn(&SharedString) -> Vec<Extra> + 'static,
    ) -> Self {
        Self {
            rows,
            active,
            query,
            today,
            on_select: Rc::new(on_select),
            on_rename: Rc::new(on_rename),
            on_pin: Rc::new(on_pin),
            on_delete: Rc::new(on_delete),
            extra: Rc::new(extra),
        }
    }
}

impl RenderOnce for ChatList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let zone = TimeZone::try_system().unwrap_or(TimeZone::UTC);
        let groups = grouped(&self.rows, &self.query, self.today, &zone);
        let empty = groups.is_empty();
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
            .children(groups.into_iter().map(|(name, items)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .px_2()
                            .pb_1()
                            .text_size(theme.text_size(TextSize::Xs))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.fg_subtle)
                            .child(name),
                    )
                    .children(items.into_iter().map(|ix| {
                        let row = self.rows[ix].clone();
                        let active = self.active.as_ref() == Some(&row.key);
                        let extra = (self.extra)(&row.key);
                        chat_row(
                            row,
                            active,
                            extra,
                            [
                                self.on_select.clone(),
                                self.on_rename.clone(),
                                self.on_pin.clone(),
                                self.on_delete.clone(),
                            ],
                            cx,
                        )
                    }))
            }))
    }
}

fn chat_row(
    row: ChatRow,
    active: bool,
    extra: Vec<Extra>,
    on: [OnKey; 4],
    cx: &App,
) -> impl IntoElement {
    let [select, rename, pin, delete] = on;
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let id: ElementId = format!("chat-row-{}", row.key).into();
    let group = SharedString::from(format!("chat-group-{}", row.key));
    let key = row.key.clone();
    let (a, b, c) = (key.clone(), key.clone(), key.clone());
    let mut menu = Menu::new()
        .item(
            MenuItem::new(word("chat.rename"))
                .icon(IconName::Pencil)
                .on_click(move |window, cx| rename(&a, window, cx)),
        )
        .item(
            match row.pinned {
                true => MenuItem::new(word("chat.unpin")).icon(IconName::PinOff),
                false => MenuItem::new(word("chat.pin")).icon(IconName::Pin),
            }
            .on_click(move |window, cx| pin(&b, window, cx)),
        );
    for (label, icon, action) in extra {
        let k = key.clone();
        menu = menu.item(
            MenuItem::new(label)
                .icon(icon)
                .on_click(move |window, cx| action(&k, window, cx)),
        );
    }
    let menu = menu.separator().item(
        MenuItem::new(word("chat.delete"))
            .icon(IconName::Trash2)
            .on_click(move |window, cx| delete(&c, window, cx)),
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
        .on_click(move |_, window, cx| select(&select_key, window, cx))
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
    use jiff::civil::date;

    use super::*;

    fn row(key: &str, title: &str, device: Option<&str>, pinned: bool, day: i8) -> ChatRow {
        ChatRow {
            key: key.to_string().into(),
            title: title.into(),
            at: date(2026, 9, day)
                .at(12, 0, 0, 0)
                .to_zoned(TimeZone::UTC)
                .expect("noon")
                .timestamp(),
            pinned,
            status: ConversationStatus::Idle,
            agent: "codex".into(),
            device: device.map(str::to_string),
        }
    }

    #[test]
    fn rows_group_by_day_with_pinned_first_and_match_the_device() {
        let rows = vec![
            row("a", "Fix the build", None, false, 26),
            row("b", "Write docs", Some("jxcz-pc"), false, 25),
            row("c", "Old idea", None, true, 1),
        ];
        let today = date(2026, 9, 26);
        let groups = grouped(&rows, "", today, &TimeZone::UTC);
        let ixs: Vec<Vec<usize>> = groups.iter().map(|g| g.1.clone()).collect();
        assert_eq!(ixs, vec![vec![2], vec![0], vec![1]]);
        let found = grouped(&rows, "jxcz", today, &TimeZone::UTC);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, vec![1]);
    }

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
}
