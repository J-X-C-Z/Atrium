//! Opening a conversation's files elsewhere: in a code editor, or shown in
//! the system's file manager.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// An editor Atrium can hand a file to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    pub name: &'static str,
    /// Its command-line launcher, which takes `file:line` with `-g`.
    pub command: &'static str,
    /// The macOS app, for `open -a` when the launcher is not installed.
    pub app: &'static str,
}

pub const EDITORS: &[Editor] = &[
    Editor {
        name: "VS Code",
        command: "code",
        app: "Visual Studio Code",
    },
    Editor {
        name: "Cursor",
        command: "cursor",
        app: "Cursor",
    },
    Editor {
        name: "Zed",
        command: "zed",
        app: "Zed",
    },
    Editor {
        name: "Windsurf",
        command: "windsurf",
        app: "Windsurf",
    },
    Editor {
        name: "Sublime Text",
        command: "subl",
        app: "Sublime Text",
    },
];

fn on_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let names: &[String] = if cfg!(windows) {
            &[format!("{command}.cmd"), format!("{command}.exe")]
        } else {
            &[command.to_string()]
        };
        names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
    })
}

fn mac_app(app: &str) -> bool {
    cfg!(target_os = "macos")
        && [PathBuf::from("/Applications"), dirs::home_dir().unwrap_or_default().join("Applications")]
            .iter()
            .any(|dir| dir.join(format!("{app}.app")).exists())
}

/// The editors installed here, in the order of [`EDITORS`].
pub fn installed() -> Vec<Editor> {
    EDITORS
        .iter()
        .filter(|e| on_path(e.command).is_some() || mac_app(e.app))
        .cloned()
        .collect()
}

/// The editor to use: the one chosen in settings when installed, else the
/// first installed one.
pub fn preferred(chosen: Option<&str>) -> Option<Editor> {
    let all = installed();
    chosen
        .and_then(|c| all.iter().find(|e| e.command == c).cloned())
        .or_else(|| all.into_iter().next())
}

/// The arguments that open `path` at `line` with an editor's launcher.
pub fn editor_args(editor: &Editor, path: &Path, line: Option<u32>) -> Vec<String> {
    let target = match line {
        Some(n) if path.is_file() || !path.exists() => format!("{}:{n}", path.display()),
        _ => path.display().to_string(),
    };
    match editor.command {
        // Zed and Sublime take `file:line` as is.
        "zed" | "subl" => vec![target],
        _ if line.is_some() => vec!["-g".into(), target],
        _ => vec![target],
    }
}

/// Opens `path` (a file or a folder) in `editor`, at `line` when given.
pub fn open_in(editor: &Editor, path: &Path, line: Option<u32>) -> Result<()> {
    let mut cmd = if on_path(editor.command).is_some() {
        let mut cmd = Command::new(on_path(editor.command).unwrap_or_else(|| editor.command.into()));
        cmd.args(editor_args(editor, path, line));
        cmd
    } else {
        let mut cmd = Command::new("open");
        cmd.args(["-a", editor.app]).arg(path);
        cmd
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn()
        .with_context(|| format!("could not start {}", editor.name))?;
    Ok(())
}

/// Shows `path` in Finder, Explorer or the file manager.
pub fn reveal(path: &Path) -> Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut cmd = Command::new("open");
        cmd.arg("-R").arg(path);
        cmd
    } else if cfg!(windows) {
        let mut cmd = Command::new("explorer");
        cmd.arg(format!("/select,{}", path.display()));
        cmd
    } else {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        });
        cmd
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn().context("could not open the file manager")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editors_get_the_line_their_way() {
        let file = Path::new("/nowhere/src/main.rs");
        let code = &EDITORS[0];
        assert_eq!(
            editor_args(code, file, Some(12)),
            vec!["-g".to_string(), "/nowhere/src/main.rs:12".into()]
        );
        assert_eq!(editor_args(code, file, None), vec!["/nowhere/src/main.rs".to_string()]);
        let zed = EDITORS.iter().find(|e| e.command == "zed").unwrap();
        assert_eq!(editor_args(zed, file, Some(3)), vec!["/nowhere/src/main.rs:3".to_string()]);
    }
}
