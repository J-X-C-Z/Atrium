//! Finding and starting `codex`.
//!
//! A macOS app started from Finder or the Dock does not inherit the user's
//! shell `PATH`: it gets `/usr/bin:/bin:/usr/sbin:/sbin`. Codex installed
//! with npm, Homebrew, nvm or Volta lives elsewhere, and the npm build is a
//! Node script that also needs `node` on `PATH`. So we ask the user's login
//! shell for its `PATH` once, and search that plus the usual install places.

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

/// The `PATH` the user's login shell sets up, joined with this process's own.
pub fn effective_path() -> OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(shell_path) = login_shell_path() {
            dirs.extend(env::split_paths(&shell_path));
        }
        if let Some(own) = env::var_os("PATH") {
            dirs.extend(env::split_paths(&own));
        }
        dirs.extend(common_dirs());
        let mut seen = std::collections::HashSet::new();
        dirs.retain(|dir| seen.insert(dir.clone()));
        env::join_paths(dirs).unwrap_or_else(|_| env::var_os("PATH").unwrap_or_default())
    })
    .clone()
}

/// Where `codex` is usually installed, beyond `PATH`.
fn common_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        for sub in [
            ".local/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".volta/bin",
            ".cargo/bin",
            "Library/pnpm",
        ] {
            dirs.push(home.join(sub));
        }
    }
    dirs
}

/// Asks the login shell for its `PATH`, with a deadline so a slow or
/// interactive profile cannot hang startup.
fn login_shell_path() -> Option<OsString> {
    if cfg!(windows) {
        return None;
    }
    let shell = env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    const MARK: &str = "__AGENTDESK_PATH__";
    let mut child = Command::new(&shell)
        .args(["-l", "-c", &format!("printf '{MARK}%s{MARK}' \"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                log::warn!("login shell did not report PATH in time");
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let start = text.find(MARK)? + MARK.len();
    let end = start + text[start..].find(MARK)?;
    Some(OsString::from(&text[start..end]))
}

/// The `codex` binary to run: `explicit` if given, else `$CODEX_BIN`, else
/// the first `codex` on the effective `PATH`.
pub fn find_codex(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(format!("{} does not exist", path.display()))
        };
    }
    if let Some(path) = env::var_os("CODEX_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
        log::warn!("CODEX_BIN={} is not a file; searching PATH", path.display());
    }
    let name = if cfg!(windows) { "codex.exe" } else { "codex" };
    env::split_paths(&effective_path())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(|| {
            "Codex CLI not found. Install it with `npm install -g @openai/codex` or \
             `brew install codex`, or set CODEX_BIN to its path."
                .to_string()
        })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}
