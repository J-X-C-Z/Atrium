// Ported from apps/server/src/provider/Drivers/ClaudeExecutable.ts (T3 Code v0.1.11, MIT).

//! Resolves the executable the SDK may spawn on Windows.
//!
//! The SDK skips shell and `PATH`/`PATHEXT` lookup on Windows, so a bare
//! `claude` or an npm `.cmd`/`.bat`/`.ps1` launcher cannot be spawned. T3
//! follows the shim to the packaged `claude.exe` or `cli.js`. On other
//! platforms the configured path is returned unchanged.

use std::path::Path;

/// The small part of the host platform this resolver cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Other,
}

/// Windows launcher extensions that the SDK cannot spawn directly.
const WINDOWS_SHIM_EXTENSIONS: &[&str] = &[".cmd", ".bat", ".ps1"];

/// npm: newer packages ship `bin/claude.exe`; older ones need a JS runtime for
/// `cli.js`. WinGet: `Links\claude.cmd` forwards to the package's native exe.
const PACKAGE_ENTRY_CANDIDATES: &[&[&str]] = &[
    &[
        "node_modules",
        "@anthropic-ai",
        "claude-code",
        "bin",
        "claude.exe",
    ],
    &["node_modules", "@anthropic-ai", "claude-code", "cli.js"],
    &[
        "..",
        "Packages",
        "Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe",
        "claude.exe",
    ],
];

/// Resolves `binary_path` to something the SDK can spawn.
///
/// `resolve_command` mirrors a `PATH`/`PATHEXT` lookup (returning the path it
/// found, or `None`), and `is_file` reports whether a candidate exists. Both
/// are injected so the Windows cases are testable on any host.
pub fn resolve_claude_sdk_executable_path(
    binary_path: &str,
    platform: Platform,
    resolve_command: impl Fn(&str) -> Option<String>,
    is_file: impl Fn(&Path) -> bool,
) -> String {
    if platform != Platform::Windows {
        return binary_path.to_owned();
    }
    let resolved = resolve_command(binary_path).unwrap_or_else(|| binary_path.to_owned());
    if !is_windows_shim(&resolved) {
        return resolved;
    }
    let Some(shim_directory) = windows_parent(&resolved) else {
        return binary_path.to_owned();
    };
    for entry in PACKAGE_ENTRY_CANDIDATES {
        let candidate = windows_join(&shim_directory, entry);
        if is_file(Path::new(&candidate)) {
            return candidate;
        }
    }
    // No known package entry: the SDK cannot spawn the shim. Return the
    // configured path so the caller's spawn error names what it tried.
    binary_path.to_owned()
}

fn is_windows_shim(path: &str) -> bool {
    let extension = windows_extension(path);
    WINDOWS_SHIM_EXTENSIONS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&extension))
}

fn windows_extension(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, extension)) => format!(".{extension}"),
        None => String::new(),
    }
}

fn windows_parent(path: &str) -> Option<String> {
    let index = path.rfind(['\\', '/'])?;
    Some(path[..index].to_owned())
}

fn windows_join(base: &str, segments: &[&str]) -> String {
    let mut path = base.trim_end_matches(['\\', '/']).to_owned();
    for segment in segments {
        path.push('\\');
        path.push_str(segment);
    }
    // The WinGet candidate climbs from `Links` to `Packages`; resolve it so
    // callers comparing paths see the real file.
    normalize_windows(&path)
}

fn normalize_windows(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split(['\\', '/']) {
        match segment {
            "" | "." => {}
            ".." => {
                // Keep a drive ("C:") or the first component.
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NPM_DIR: &str = "C:\\Users\\dev\\AppData\\Roaming\\npm";
    const NPM_PACKAGE_EXE: &str = "C:\\Users\\dev\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe";
    const NPM_PACKAGE_CLI: &str =
        "C:\\Users\\dev\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js";
    const WINGET_EXE: &str = "C:\\Users\\dev\\AppData\\Local\\Microsoft\\WinGet\\Packages\\Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe\\claude.exe";

    fn resolve(shim: &str, existing: &[&str]) -> String {
        let existing: Vec<String> = existing.iter().map(|path| (*path).to_owned()).collect();
        resolve_claude_sdk_executable_path(
            "claude",
            Platform::Windows,
            |_| Some(shim.to_owned()),
            |path| {
                existing
                    .iter()
                    .any(|candidate| Path::new(candidate) == path)
            },
        )
    }

    #[test]
    fn follows_an_npm_launcher_to_the_packaged_binary() {
        assert_eq!(
            resolve(
                &format!("{NPM_DIR}\\claude.cmd"),
                &[NPM_PACKAGE_EXE, NPM_PACKAGE_CLI]
            ),
            NPM_PACKAGE_EXE
        );
    }

    #[test]
    fn follows_the_winget_launcher_to_the_package_binary() {
        assert_eq!(
            resolve(
                "C:\\Users\\dev\\AppData\\Local\\Microsoft\\WinGet\\Links\\claude.cmd",
                &[WINGET_EXE]
            ),
            WINGET_EXE
        );
    }

    #[test]
    fn follows_bat_and_ps1_shims_the_same_way() {
        for shim in [
            format!("{NPM_DIR}\\claude.bat"),
            format!("{NPM_DIR}\\claude.ps1"),
        ] {
            assert_eq!(resolve(&shim, &[NPM_PACKAGE_EXE]), NPM_PACKAGE_EXE);
        }
    }

    #[test]
    fn matches_shim_extensions_case_insensitively() {
        assert_eq!(
            resolve(&format!("{NPM_DIR}\\claude.CMD"), &[NPM_PACKAGE_EXE]),
            NPM_PACKAGE_EXE
        );
    }

    #[test]
    fn falls_back_to_cli_js_when_there_is_no_native_binary() {
        assert_eq!(
            resolve(&format!("{NPM_DIR}\\claude.cmd"), &[NPM_PACKAGE_CLI]),
            NPM_PACKAGE_CLI
        );
    }

    #[test]
    fn returns_the_configured_path_when_no_known_entry_exists() {
        assert_eq!(resolve(&format!("{NPM_DIR}\\claude.cmd"), &[]), "claude");
    }

    #[test]
    fn other_platforms_pass_the_path_through() {
        assert_eq!(
            resolve_claude_sdk_executable_path(
                "claude",
                Platform::Other,
                |_| panic!("no lookup off Windows"),
                |_| panic!("no file checks off Windows"),
            ),
            "claude"
        );
    }
}
