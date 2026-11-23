//! The folder dialog for new chats: breadcrumbs for where you are, the
//! folders in it, a way up, a way to make a new folder, and recent folders to
//! jump to. It works on this computer (through the file system) and on
//! another device (through its node).

use std::path::{Path, PathBuf};

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Input, Switch, TextInput},
    overlays::Dialog,
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::{Overlay, RootView};
use crate::i18n::{t, tf};

/// What the open folder dialog shows.
pub struct FolderPicker {
    /// The device being browsed; `None` for this computer.
    pub device: Option<String>,
    pub path: String,
    /// The folders in `path` when it is on this computer; a device's are
    /// kept by the workspace as they arrive.
    pub entries: Vec<String>,
    pub name: gpui::Entity<TextInput>,
    pub error: Option<String>,
}

fn separator(path: &str) -> char {
    if path.contains('\\') && !path.contains('/') {
        '\\'
    } else {
        '/'
    }
}

/// Each step of `path` with the path that reaches it: `/a/b` is `/`, `/a`,
/// `/a/b`. Works for Windows paths too.
pub fn crumbs(path: &str) -> Vec<(String, String)> {
    let sep = separator(path);
    let mut out = Vec::new();
    let mut acc = String::new();
    if path.starts_with(sep) {
        acc.push(sep);
        out.push((sep.to_string(), acc.clone()));
    }
    for (ix, part) in path.split(sep).filter(|p| !p.is_empty()).enumerate() {
        if !acc.is_empty() && !acc.ends_with(sep) {
            acc.push(sep);
        }
        acc.push_str(part);
        let target = if ix == 0 && part.ends_with(':') {
            format!("{acc}{sep}")
        } else {
            acc.clone()
        };
        out.push((part.to_string(), target));
    }
    out
}

/// The folder above `path`, if there is one.
pub fn parent_of(path: &str) -> Option<String> {
    let steps = crumbs(path);
    (steps.len() > 1).then(|| steps[steps.len() - 2].1.clone())
}

/// `name` inside `path`.
pub fn join(path: &str, name: &str) -> String {
    let sep = separator(path);
    format!("{}{sep}{name}", path.trim_end_matches(sep))
}

/// Whether `path` is `root` or inside it.
pub fn within(path: &str, root: &str) -> bool {
    let sep = separator(root);
    let (path, root) = (path.trim_end_matches(sep), root.trim_end_matches(sep));
    path == root || path.starts_with(&format!("{root}{sep}"))
}

/// Why `name` cannot be a new folder's name.
fn bad_name(name: &str) -> Option<String> {
    if name.is_empty() {
        Some(t("Type a name for the new folder.").into())
    } else if name == "." || name == ".." || name.contains(['/', '\\']) {
        Some(t("A folder name cannot hold a slash or be . or ..").into())
    } else {
        None
    }
}

/// The visible folders in `path`, sorted without regard to case.
pub fn list_local(path: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

impl RootView {
    /// Opens the folder dialog on the folder new chats would use.
    pub fn open_folder_picker(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let ws = self.workspace.read(cx);
        let backend = ws.view_backend().to_string();
        let device = crate::agents::split_backend(&backend)
            .1
            .map(str::to_string)
            .filter(|_| ws.is_remote_view());
        let path = ws.folder_for(&backend);
        let name = cx.new(|cx| TextInput::new(window, cx).placeholder(t("New folder name")));
        self.overlay = Some(Overlay::Folder(FolderPicker {
            device,
            path: path.clone(),
            entries: Vec::new(),
            name,
            error: None,
        }));
        self.folder_go(path, cx);
    }

    fn picker_mut(&mut self) -> Option<&mut FolderPicker> {
        match &mut self.overlay {
            Some(Overlay::Folder(picker)) => Some(picker),
            _ => None,
        }
    }

    /// Shows the folders in `path`.
    fn folder_go(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(picker) = self.picker_mut() else {
            return;
        };
        picker.error = None;
        picker.path = path.clone();
        match picker.device.clone() {
            None => picker.entries = list_local(&path),
            Some(device) => self
                .workspace
                .update(cx, |ws, cx| ws.fetch_children(&device, path, cx)),
        }
        cx.notify();
    }

    fn folder_error(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(picker) = self.picker_mut() {
            picker.error = Some(error);
        }
        cx.notify();
    }

    /// Makes the folder named in the dialog's field inside the shown one, and goes into it.
    fn folder_make(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.picker_mut() else {
            return;
        };
        let (device, parent, field) = (
            picker.device.clone(),
            picker.path.clone(),
            picker.name.clone(),
        );
        let name = field.read(cx).text().trim().to_string();
        if let Some(error) = bad_name(&name) {
            return self.folder_error(error, cx);
        }
        let full = join(&parent, &name);
        match device {
            None => match std::fs::create_dir(&full) {
                Ok(()) => {
                    field.update(cx, |input, cx| input.set_text("", cx));
                    self.folder_go(full, cx);
                }
                Err(error) => self.folder_error(error.to_string(), cx),
            },
            Some(device) => {
                let Some(node) = self
                    .workspace
                    .read(cx)
                    .device(&device)
                    .map(|d| d.node.clone())
                else {
                    return self.folder_error(t("That device is not reachable.").into(), cx);
                };
                let target = full.clone();
                cx.spawn(async move |this, cx| {
                    let made = cx
                        .background_executor()
                        .spawn(async move { node.mkdir(&target) })
                        .await;
                    let _ = this.update(cx, |this, cx| match made {
                        Ok(()) => {
                            field.update(cx, |input, cx| input.set_text("", cx));
                            this.workspace
                                .update(cx, |ws, _| ws.forget_children(&device, &parent));
                            this.folder_go(full, cx);
                        }
                        Err(error) => this.folder_error(error.to_string(), cx),
                    });
                })
                .detach();
            }
        }
    }

    /// Takes the shown folder as where new chats run.
    fn folder_use(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.picker_mut() else {
            return;
        };
        let (device, path) = (picker.device.clone(), picker.path.clone());
        self.workspace.update(cx, |ws, cx| match device {
            None => ws.set_working_dir(PathBuf::from(path), cx),
            Some(device) => ws.set_device_folder(&device, path, cx),
        });
        self.close_overlay(window, cx);
    }

    pub(super) fn folder_dialog(
        &self,
        picker: &FolderPicker,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let (small, tiny) = (theme.text_size(TextSize::Sm), theme.text_size(TextSize::Xs));
        let ws = self.workspace.read(cx);
        let root = cx.entity();
        let device = picker.device.clone();
        let roots: Vec<String> = device
            .as_deref()
            .and_then(|d| ws.device(d))
            .map(|d| d.info.roots.clone())
            .unwrap_or_default();
        // A device's node only shows what is inside its shared roots.
        let reachable = {
            let (remote, roots) = (device.is_some(), roots.clone());
            move |path: &str| !remote || roots.iter().any(|r| within(path, r))
        };
        let (names, loading) = match &device {
            None => (picker.entries.clone(), false),
            Some(d) => match ws.remote_children(d, &picker.path) {
                Some(names) => (names.clone(), false),
                None => (Vec::new(), true),
            },
        };
        // Where to jump: recent folders here, a device's shared roots there.
        let quick: Vec<(String, String)> = match &device {
            None => ws
                .settings
                .recent_folders
                .iter()
                .filter(|dir| dir.is_dir())
                .take(5)
                .map(|dir| (dir.display().to_string(), super::composer::short_path(dir)))
                .collect(),
            Some(_) => roots
                .iter()
                .map(|r| (r.clone(), super::composer::folder_name(Path::new(r))))
                .collect(),
        };
        let isolated = ws.settings.chat_worktrees;
        let go = |root: &gpui::Entity<RootView>, path: String| {
            let root = root.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                let path = path.clone();
                root.update(cx, |this, cx| this.folder_go(path, cx))
            }
        };

        let mut body = div().w_full().flex().flex_col().gap_3();
        if !quick.is_empty() {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_size(tiny).text_color(colors.fg_subtle).child(
                        if device.is_some() {
                            t("Shared folders")
                        } else {
                            t("Recent")
                        },
                    ))
                    .child(div().flex().flex_wrap().gap_1().children(
                        quick.into_iter().enumerate().map(|(ix, (path, label))| {
                            let on = path == picker.path;
                            div()
                                .id(("folder-quick", ix))
                                .flex()
                                .items_center()
                                .gap_1()
                                .px_2()
                                .py_0p5()
                                .rounded_full()
                                .border_1()
                                .border_color(if on {
                                    colors.border_strong
                                } else {
                                    colors.border
                                })
                                .text_size(tiny)
                                .cursor_pointer()
                                .when(on, |c| c.bg(colors.active).text_color(colors.fg))
                                .when(!on, |c| {
                                    c.text_color(colors.fg_muted).hover(|c| c.bg(colors.hover))
                                })
                                .child(Icon::new(IconName::Folder).size(IconSize::Xs))
                                .child(label)
                                .on_click(go(&root, path))
                        }),
                    )),
            );
        }

        // Where you are, step by step.
        let steps = crumbs(&picker.path);
        let last = steps.len().saturating_sub(1);
        body = body.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_0p5()
                .px_2()
                .py_1()
                .rounded(theme.radius(Radius::Md))
                .bg(colors.sunken)
                .text_size(small)
                .children(
                    steps
                        .into_iter()
                        .enumerate()
                        .flat_map(|(ix, (label, path))| {
                            let clickable = ix != last && reachable(&path);
                            let mut parts: Vec<AnyElement> = Vec::new();
                            if ix > 0 {
                                parts.push(
                                    Icon::new(IconName::ChevronRight)
                                        .size(IconSize::Xs)
                                        .color(colors.fg_subtle)
                                        .into_any_element(),
                                );
                            }
                            parts.push(
                                div()
                                    .id(("crumb", ix))
                                    .px_1()
                                    .rounded(theme.radius(Radius::Sm))
                                    .text_color(if ix == last {
                                        colors.fg
                                    } else {
                                        colors.fg_muted
                                    })
                                    .when(clickable, |c| {
                                        c.cursor_pointer()
                                            .hover(|c| c.bg(colors.hover))
                                            .on_click(go(&root, path))
                                    })
                                    .child(label)
                                    .into_any_element(),
                            );
                            parts
                        }),
                ),
        );

        // The folders here.
        let up = parent_of(&picker.path).filter(|p| reachable(p));
        let up_button = {
            let button = Button::new("folder-up", t("Up"))
                .icon(IconName::ArrowUp)
                .size(ControlSize::Sm)
                .variant(ButtonVariant::Ghost)
                .disabled(up.is_none());
            match up {
                Some(parent) => button.on_click(go(&root, parent)),
                None => button,
            }
        };
        let list = div()
            .id("folder-list")
            .h(px(220.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .rounded(theme.radius(Radius::Md))
            .border_1()
            .border_color(colors.border)
            .when(names.is_empty(), |list| {
                list.child(
                    div()
                        .p_3()
                        .text_size(small)
                        .text_color(colors.fg_subtle)
                        .child(if loading {
                            t("Loading…")
                        } else {
                            t("No subfolders here.")
                        }),
                )
            })
            .children(names.into_iter().enumerate().map(|(ix, name)| {
                let path = join(&picker.path, &name);
                div()
                    .id(("folder-entry", ix))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .text_size(small)
                    .cursor_pointer()
                    .hover(|row| row.bg(colors.hover))
                    .child(
                        Icon::new(IconName::Folder)
                            .size(IconSize::Sm)
                            .color(colors.fg_muted),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(IconSize::Xs)
                            .color(colors.fg_subtle),
                    )
                    .on_click(go(&root, path))
            }));
        body = body
            .child(div().flex().items_center().child(up_button))
            .child(list);

        // A new folder, made here.
        let make = root.clone();
        body = body
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&picker.name)))
                    .child(
                        Button::new("folder-new", t("New folder"))
                            .icon(IconName::FolderPlus)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                make.update(cx, |this, cx| this.folder_make(cx))
                            }),
                    ),
            )
            .children(
                picker
                    .error
                    .clone()
                    .map(|error| div().text_size(tiny).text_color(colors.danger).child(error)),
            );
        if device.is_none() {
            let toggle = self.workspace.clone();
            body = body.child(
                Switch::new("folder-worktree", isolated)
                    .label(t("Start in its own worktree"))
                    .on_change(move |on, _, cx| {
                        toggle.update(cx, |ws, cx| {
                            ws.settings.chat_worktrees = on;
                            ws.settings.save();
                            cx.notify();
                        })
                    }),
            );
        }

        let (close, choose, pick) = (root.clone(), self.workspace.clone(), root.clone());
        let local = device.is_none();
        let title: SharedString = match &device {
            Some(d) => tf(
                "Choose a folder on {device}",
                &[("device", &ws.device_label(d))],
            )
            .into(),
            None => t("Choose a folder").into(),
        };
        Dialog::new("folder-picker", title, move |window, cx| {
            close.update(cx, |this, cx| this.close_overlay(window, cx))
        })
        .child(body)
        .when(local, |dialog| {
            dialog.action(move |close| {
                Button::new("folder-browse", t("Choose a folder…"))
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| {
                        super::composer::choose_folder(choose.clone(), cx);
                        close(window, cx);
                    })
            })
        })
        .action(|close| {
            Button::new("folder-cancel", t("Cancel"))
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, window, cx| close(window, cx))
        })
        .action(move |_| {
            Button::new("folder-use", t("Use this folder"))
                .primary()
                .on_click(move |_, window, cx| {
                    pick.update(cx, |this, cx| this.folder_use(window, cx))
                })
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(path: &str) -> Vec<String> {
        crumbs(path).into_iter().map(|c| c.1).collect()
    }

    #[test]
    fn breadcrumbs_reach_each_step() {
        assert_eq!(
            paths("/home/me/code"),
            ["/", "/home", "/home/me", "/home/me/code"]
        );
        assert_eq!(paths("/"), ["/"]);
        assert_eq!(paths(r"C:\Users\me"), [r"C:\", r"C:\Users", r"C:\Users\me"]);
    }

    #[test]
    fn up_stops_at_the_top() {
        assert_eq!(parent_of("/home/me").as_deref(), Some("/home"));
        assert_eq!(parent_of("/home").as_deref(), Some("/"));
        assert_eq!(parent_of("/"), None);
        assert_eq!(parent_of(r"C:\Users").as_deref(), Some(r"C:\"));
    }

    #[test]
    fn roots_hold_what_is_inside() {
        assert!(within("/srv/data/a", "/srv/data"));
        assert!(within("/srv/data", "/srv/data/"));
        assert!(!within("/srv/database", "/srv/data"));
        assert!(!within("/srv", "/srv/data"));
        assert_eq!(join("/srv/data/", "x"), "/srv/data/x");
    }

    #[test]
    fn new_folder_names_are_checked() {
        assert!(bad_name("").is_some());
        assert!(bad_name("..").is_some());
        assert!(bad_name("a/b").is_some());
        assert!(bad_name("notes").is_none());
    }

    #[test]
    fn local_folders_list_sorted_without_hidden_ones() {
        let dir = std::env::temp_dir().join(format!("atrium-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["b", "A", ".hidden"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("file.txt"), "x").unwrap();
        assert_eq!(list_local(&dir.display().to_string()), ["A", "b"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
