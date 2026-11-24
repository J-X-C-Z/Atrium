//! Checking for a newer Atrium: the latest GitHub release, read with the
//! system's `curl`, compared with this build's version. Atrium only says a
//! new one is out and links to it; installing stays a click away.

use anyhow::{Context, Result, ensure};
use std::process::{Command, Stdio};

const RELEASES: &str = "https://api.github.com/repos/J-X-C-Z/Atrium/releases/latest";

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

/// The latest release, when it is newer than this build.
pub fn check() -> Result<Option<Update>> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsSL",
        "-m",
        "15",
        "-H",
        "Accept: application/vnd.github+json",
        "-A",
        "Atrium",
        RELEASES,
    ])
    .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().context("curl is not available")?;
    ensure!(out.status.success(), "no release found");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let tag = json["tag_name"].as_str().context("no tag")?;
    let url = json["html_url"]
        .as_str()
        .unwrap_or("https://github.com/J-X-C-Z/Atrium/releases");
    Ok(newer(env!("CARGO_PKG_VERSION"), tag).then(|| Update {
        version: tag.trim_start_matches('v').to_string(),
        url: url.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.1.0", "v0.2.0"));
        assert!(newer("0.9.0", "0.10.0"));
        assert!(newer("0.1", "0.1.1"));
        assert!(!newer("0.2.0", "v0.2.0"));
        assert!(!newer("1.0.0", "0.9.9"));
        assert!(!newer("0.2.0", "0.2.0-beta"));
    }
}
