//! The side panel's Run view: the folder's dev server (or any command) in a
//! terminal of its own, and its local address opened in the browser.

use std::path::Path;

use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::Input,
    primitives::IconName,
    terminal::{Launch, Terminal},
    theme::{ActiveTheme, ControlSize, TextSize},
};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, prelude::*};

use super::RootView;
use crate::i18n::{t, tf};
use crate::workspace::Notice;

/// What probably starts the project in `dir`.
pub fn guess_command(dir: &Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(dir.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
    {
        let runner = if dir.join("pnpm-lock.yaml").exists() {
            "pnpm"
        } else if dir.join("yarn.lock").exists() {
            "yarn"
        } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
            "bun run"
        } else {
            "npm run"
        };
        for script in ["dev", "start", "serve", "preview"] {
            if json["scripts"][script].is_string() {
                return Some(format!("{runner} {script}"));
            }
        }
    }
    if dir.join("Cargo.toml").exists() {
        return Some("cargo run".into());
    }
    if dir.join("manage.py").exists() {
        return Some("python3 manage.py runserver".into());
    }
    if dir.join("go.mod").exists() {
        return Some("go run .".into());
    }
    if dir.join("index.html").exists() {
        return Some("python3 -m http.server 8000".into());
    }
    None
}

/// The first local web address in a command's output.
pub fn find_url(text: &str) -> Option<String> {
    for word in text
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | ','))
    {
        let word = word.trim_end_matches(['.', ';']);
        let rest = word
            .strip_prefix("http://")
            .or_else(|| word.strip_prefix("https://"));
        let Some(rest) = rest else { continue };
        let host = rest.split(['/', ':']).next().unwrap_or_default();
        if matches!(
            host,
            "localhost" | "127.0.0.1" | "0.0.0.0" | "[::]" | "[::1]"
        ) {
            return Some(
                word.replacen("0.0.0.0", "localhost", 1)
                    .replacen("[::]", "localhost", 1),
            );
        }
    }
    None
}

fn launch(dir: &Path, command: &str) -> Launch {
    let program = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), command.to_string()],
        )
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        (shell, vec!["-lc".to_string(), command.to_string()])
    };
    Launch {
        program: Some(program),
        cwd: Some(dir.to_path_buf()),
        env: Vec::new(),
    }
}

impl RootView {
    /// The command the Run view starts for the open conversation's folder.
    fn dev_command(&self, dir: &Path, cx: &gpui::App) -> String {
        let text = self.tools.run_input.read(cx).text().trim().to_string();
        if !text.is_empty() {
            return text;
        }
        self.workspace
            .read(cx)
            .settings
            .run_commands
            .get(&dir.display().to_string())
            .cloned()
            .or_else(|| guess_command(dir))
            .unwrap_or_default()
    }

    /// Fills the command box when the Run view opens on a new folder.
    pub(super) fn prepare_run(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if self.tools.run_dir.as_deref() == Some(dir) {
            return;
        }
        self.tools.run_dir = Some(dir.to_path_buf());
        let command = self
            .workspace
            .read(cx)
            .settings
            .run_commands
            .get(&dir.display().to_string())
            .cloned()
            .or_else(|| guess_command(dir))
            .unwrap_or_default();
        self.tools
            .run_input
            .update(cx, |input, cx| input.set_text(command, cx));
    }

    fn start_run(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.chat_dir(cx) else {
            return;
        };
        let command = self.dev_command(&dir, cx);
        if command.is_empty() {
            self.toast(
                &Notice::Info(t("Write the command that starts the project.").into()),
                cx,
            );
            return;
        }
        self.workspace.update(cx, |ws, _| {
            ws.settings
                .run_commands
                .insert(dir.display().to_string(), command.clone());
            ws.settings.save();
        });
        let mut failed = None;
        let term = cx.new(|cx| match Terminal::spawn(launch(&dir, &command), cx) {
            Ok(term) => term,
            Err(e) => {
                failed = Some(e);
                Terminal::replay(b"", 80, 24, cx)
            }
        });
        if let Some(e) = failed {
            self.toast(&Notice::Error(format!("{e:#}")), cx);
            return;
        }
        // Look at its output each second for a while, to notice the address
        // it prints (the terminal repaints itself without telling us).
        let watched = term.downgrade();
        cx.spawn(async move |this, cx| {
            for _ in 0..120 {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let Some(term) = watched.upgrade() else {
                    return;
                };
                let found = term.read_with(cx, |t, _| find_url(&t.text()).is_some());
                if this.update(cx, |_, cx| cx.notify()).is_err() || found {
                    return;
                }
            }
        })
        .detach();
        self.tools.runs.insert(dir, term);
        cx.notify();
    }

    fn stop_run(&mut self, cx: &mut Context<Self>) {
        if let Some(dir) = self.chat_dir(cx) {
            // Dropping the terminal ends its process.
            self.tools.runs.remove(&dir);
            cx.notify();
        }
    }

    pub(super) fn run_body(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let Some(dir) = self.chat_dir(cx) else {
            return div()
                .p_3()
                .text_size(small)
                .text_color(colors.fg_muted)
                .child(t("Run works in a conversation's folder on this computer."))
                .into_any_element();
        };
        let root = cx.entity();
        let running = self.tools.runs.get(&dir).cloned();
        let exited = running.as_ref().and_then(|t| t.read(cx).exit_status());
        let url = running.as_ref().and_then(|t| find_url(&t.read(cx).text()));
        let (go, stop) = (root.clone(), root.clone());
        let mut bar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().child(Input::new(&self.tools.run_input)));
        bar = match (&running, exited) {
            (Some(_), None) => bar
                .child(
                    Button::new("run-restart", t("Restart"))
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .icon(IconName::RotateCw)
                        .on_click(move |_, _, cx| {
                            go.update(cx, |r, cx| {
                                r.stop_run(cx);
                                r.start_run(cx);
                            })
                        }),
                )
                .child(
                    Button::new("run-stop", t("Stop"))
                        .size(ControlSize::Sm)
                        .icon(IconName::CircleStop)
                        .on_click(move |_, _, cx| stop.update(cx, |r, cx| r.stop_run(cx))),
                ),
            _ => bar.child(
                Button::new("run-start", t("Run"))
                    .primary()
                    .size(ControlSize::Sm)
                    .icon(IconName::Play)
                    .on_click(move |_, _, cx| {
                        go.update(cx, |r, cx| {
                            r.stop_run(cx);
                            r.start_run(cx);
                        })
                    }),
            ),
        };
        let mut body = div().size_full().flex().flex_col().gap_2().p_2().child(bar);
        if let Some(url) = url {
            let open = url.clone();
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(colors.success.opacity(0.10))
                    .text_size(small)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(tf("Serving at {url}", &[("url", &url)])),
                    )
                    .child(
                        Button::new("run-preview", t("Open preview"))
                            .size(ControlSize::Sm)
                            .icon(IconName::Globe)
                            .on_click(move |_, _, cx| cx.open_url(&open)),
                    ),
            );
        }
        if let Some(status) = exited {
            body = body.child(
                div()
                    .text_size(small)
                    .text_color(if status.success() {
                        colors.fg_muted
                    } else {
                        colors.danger
                    })
                    .child(match status.code() {
                        Some(code) => tf("Stopped with exit code {code}", &[("code", &code)]),
                        None => t("Stopped").to_string(),
                    }),
            );
        }
        match running {
            Some(term) => body = body.child(div().flex_1().min_h_0().child(term)),
            None => {
                body = body.child(
                    div()
                        .text_size(small)
                        .text_color(colors.fg_muted)
                        .child(t("Starts the project here, such as its dev server, and opens the address it prints. The command is remembered for this folder.")),
                )
            }
        }
        body.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_found_in_output() {
        assert_eq!(
            find_url("  ➜  Local:   http://localhost:5173/\n"),
            Some("http://localhost:5173/".into())
        );
        assert_eq!(
            find_url("Serving HTTP on 0.0.0.0 port 8000 (http://0.0.0.0:8000/) ..."),
            Some("http://localhost:8000/".into())
        );
        assert_eq!(find_url("see https://example.com"), None);
    }

    #[test]
    fn commands_are_guessed_from_the_project() {
        let dir = std::env::temp_dir().join(format!("atrium-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"build":"x","dev":"vite"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(guess_command(&dir).as_deref(), Some("pnpm dev"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
