//! Checking for a newer Atrium: the latest GitHub release, read with the
//! system's `curl`, compared with this build's version. The release's assets
//! are searched for this platform's installer, so the sidebar links straight
//! to the right download; installing stays a click away.

use anyhow::{Context, Result, ensure};
use std::process::{Command, Stdio};

const RELEASES: &str = "https://api.github.com/repos/J-X-C-Z/Atrium/releases/latest";
const FALLBACK: &str = "https://github.com/J-X-C-Z/Atrium/releases";

/// A release newer than this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    pub url: String,
}

fn parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// Whether `latest` is a newer version than `current`.
pub fn newer(current: &str, latest: &str) -> bool {
    let (a, b) = (parts(current), parts(latest));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return y > x;
        }
    }
    false
}

fn normalized(name: &str) -> String {
    name.to_ascii_lowercase().replace(['-', '.'], "_")
}

/// Whether `needle` is a complete `_`-separated token in a normalized name.
fn has_token(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(index, _)| {
        hay[..index].chars().next_back().is_none_or(|c| c == '_')
            && hay[index + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| c == '_')
    })
}

fn os_aliases(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &["macos", "darwin", "osx"],
        "windows" => &["windows", "win"],
        "linux" => &["linux"],
        _ => &[],
    }
}

fn arch_aliases(arch: &str) -> &'static [&'static str] {
    match arch {
        "aarch64" => &["aarch64", "arm64"],
        "x86_64" => &["x86_64", "x64", "amd64"],
        // Do not accept a bare "x86" token: it is also the first half of
        // "x86_64", and a 32-bit build must not pick the 64-bit installer.
        "x86" => &["i686", "i386"],
        _ => &[],
    }
}

/// Lower is better: the preferred installer kind, then a universal fallback.
fn asset_score(name: &str, os: &str, arch: &str) -> Option<(u8, u8)> {
    let lower = name.to_ascii_lowercase();
    let kind = match os {
        "macos" if lower.ends_with(".dmg") => 0,
        "windows" if lower.ends_with("setup.exe") => 0,
        "windows" if lower.ends_with(".msi") => 1,
        "windows" if lower.ends_with(".exe") => 2,
        "linux" if lower.ends_with(".appimage") => 0,
        "linux" if lower.ends_with(".deb") => 1,
        _ => return None,
    };
    let normalized = normalized(&lower);
    if !os_aliases(os)
        .iter()
        .any(|token| has_token(&normalized, token))
    {
        return None;
    }
    let aliases = arch_aliases(arch);
    let arch_rank = if aliases.is_empty() || aliases.iter().any(|t| has_token(&normalized, t)) {
        0
    } else if has_token(&normalized, "universal") {
        1
    } else {
        return None;
    };
    Some((kind, arch_rank))
}

/// The download URL of the best installer asset for `os` and `arch`.
///
/// Asset names are the ones the release workflow writes, for example
/// `Atrium-0.1.0-macos-arm64.dmg`, `Atrium-0.1.0-windows-x64-setup.exe` and
/// `Atrium-0.1.0-linux-x86_64.AppImage`. Unknown platforms and releases
/// without a matching asset fall back to the release page.
pub fn pick_asset(assets: &[serde_json::Value], os: &str, arch: &str) -> Option<String> {
    assets
        .iter()
        .filter_map(|asset| {
            let name = asset["name"].as_str()?;
            let url = asset["browser_download_url"].as_str()?;
            asset_score(name, os, arch).map(|score| (score, name, url))
        })
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)))
        .map(|(_, _, url)| url.to_string())
}

/// The latest release, when it is newer than this build.
pub fn check() -> Result<Option<Update>> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "-m",
            "15",
            "-H",
            "Accept: application/vnd.github+json",
            "-A",
            "Atrium",
            RELEASES,
        ])
        .stdin(Stdio::null())
        .output()
        .context("curl is not available")?;
    ensure!(out.status.success(), "no release found");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let tag = json["tag_name"].as_str().context("no tag")?;
    let fallback = json["html_url"].as_str().unwrap_or(FALLBACK);
    let url = json["assets"]
        .as_array()
        .and_then(|assets| pick_asset(assets, std::env::consts::OS, std::env::consts::ARCH))
        .unwrap_or_else(|| fallback.to_string());
    Ok(newer(env!("CARGO_PKG_VERSION"), tag).then(|| Update {
        version: tag.trim_start_matches('v').to_string(),
        url,
    }))
}

#[cfg(test)]
mod tests {
    use super::{newer, pick_asset};
    use serde_json::json;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.1.0", "v0.2.0"));
        assert!(newer("0.9.0", "0.10.0"));
        assert!(newer("0.1", "0.1.1"));
        assert!(!newer("0.2.0", "v0.2.0"));
        assert!(!newer("1.0.0", "0.9.9"));
        assert!(!newer("0.2.0", "0.2.0-beta"));
    }

    fn assets() -> Vec<serde_json::Value> {
        vec![
            json!({"name": "atrium-node-0.2.0-linux-x86_64.tar.gz",
                   "browser_download_url": "node-tar"}),
            json!({"name": "Atrium-0.2.0-macos-arm64.dmg",
                   "browser_download_url": "dmg-arm"}),
            json!({"name": "Atrium-0.2.0-macos-x86_64.dmg",
                   "browser_download_url": "dmg-x64"}),
            json!({"name": "Atrium-0.2.0-windows-x64-setup.exe",
                   "browser_download_url": "setup-exe"}),
            json!({"name": "Atrium-0.2.0-windows-x64.msi",
                   "browser_download_url": "msi"}),
            json!({"name": "Atrium-0.2.0-linux-x86_64.AppImage",
                   "browser_download_url": "appimage"}),
            json!({"name": "Atrium-0.2.0-linux-x86_64.deb",
                   "browser_download_url": "deb"}),
            json!({"name": "Atrium-0.2.0-source.tar.gz",
                   "browser_download_url": "source"}),
        ]
    }

    #[test]
    fn picks_the_platform_installer_and_ignores_other_assets() {
        let assets = assets();
        assert_eq!(
            pick_asset(&assets, "macos", "aarch64").as_deref(),
            Some("dmg-arm")
        );
        assert_eq!(
            pick_asset(&assets, "macos", "x86_64").as_deref(),
            Some("dmg-x64")
        );
        assert_eq!(
            pick_asset(&assets, "windows", "x86_64").as_deref(),
            Some("setup-exe")
        );
        assert_eq!(
            pick_asset(&assets, "linux", "x86_64").as_deref(),
            Some("appimage")
        );
    }

    #[test]
    fn falls_back_within_the_platform() {
        let assets = vec![
            json!({"name": "Atrium-0.2.0-windows-x64.msi",
                   "browser_download_url": "msi"}),
            json!({"name": "Atrium-0.2.0-linux-x86_64.deb",
                   "browser_download_url": "deb"}),
            json!({"name": "Atrium-0.2.0-macos-universal.dmg",
                   "browser_download_url": "universal"}),
        ];
        assert_eq!(
            pick_asset(&assets, "windows", "x86_64").as_deref(),
            Some("msi")
        );
        assert_eq!(
            pick_asset(&assets, "linux", "x86_64").as_deref(),
            Some("deb")
        );
        // A universal build is used only when the exact architecture is absent.
        assert_eq!(
            pick_asset(&assets, "macos", "aarch64").as_deref(),
            Some("universal")
        );
    }

    #[test]
    fn unknown_platform_or_no_asset_returns_none() {
        let assets = assets();
        assert_eq!(pick_asset(&assets, "freebsd", "x86_64"), None);
        assert_eq!(
            pick_asset(&assets[..1], "macos", "aarch64"),
            None,
            "a node archive is not a desktop installer"
        );
    }
}
