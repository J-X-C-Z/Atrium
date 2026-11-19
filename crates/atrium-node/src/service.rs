//! Per-user service integration. No registration happens on `serve` or at
//! build/test time: only an explicit install/uninstall CLI action changes it.
#[cfg(all(unix, any(target_os = "macos", test)))]
#[path = "service_launchd.rs"]
mod launchd;
#[cfg(any(target_os = "linux", all(unix, test)))]
#[path = "service_systemd.rs"]
mod systemd;
#[cfg(any(windows, test))]
#[path = "service_windows.rs"]
mod windows;
use anyhow::{Context, Result, bail};
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;
#[cfg(any(target_os = "macos", test))]
const LABEL: &str = "dev.atrium.node";
#[cfg(any(target_os = "macos", windows, test))]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn path_text(p: &Path) -> Result<&str> {
    let s = p.to_str().context("service paths must be UTF-8")?;
    if !p.is_absolute() || s.chars().any(|c| c.is_control()) {
        bail!("service paths must be absolute and contain no control characters");
    }
    Ok(s)
}
#[cfg(any(target_os = "macos", test))]
fn render_launchd(label: &str, exe: &Path, config: &Path, log: &Path) -> Result<String> {
    let marker = "<!-- atrium-node managed service v1 -->";
    let label = xml(label);
    let e = xml(path_text(exe)?);
    let c = xml(path_text(config)?);
    let l = xml(path_text(log)?);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
{marker}
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{e}</string><string>serve</string><string>--config</string><string>{c}</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>StandardOutPath</key><string>{l}</string><key>StandardErrorPath</key><string>{l}</string>
</dict></plist>
"#
    ))
}
#[cfg(target_os = "macos")]
fn invoke(program: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} failed with {status}");
    }
    Ok(())
}
#[cfg(any(target_os = "linux", test))]
fn save(path: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("service path has no parent")?;
    std::fs::create_dir_all(parent)?;
    // Avoid following a pre-existing symlink for the service definition.
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        bail!("refusing symlink service definition");
    }
    let temp = parent.join(format!(".atrium-service-{}.tmp", uuid::Uuid::new_v4()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&temp)?;
    let result = (|| -> Result<()> {
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        drop(f);
        // Install must not replace an existing service, including a concurrent
        // registration. Hard-link publication is atomic and fails if it exists.
        std::fs::hard_link(&temp, path)?;
        std::fs::remove_file(&temp)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
#[cfg(not(windows))]
fn definition_path() -> Result<PathBuf> {
    #[cfg(not(windows))]
    let home = dirs::home_dir().context("home unavailable")?;
    #[cfg(target_os = "macos")]
    {
        return Ok(home.join("Library/LaunchAgents/dev.atrium.node.plist"));
    }
    #[cfg(target_os = "linux")]
    {
        return Ok(dirs::config_dir()
            .unwrap_or(home.join(".config"))
            .join("systemd/user/atrium-node.service"));
    }
    #[allow(unreachable_code)]
    {
        #[cfg(not(windows))]
        let _ = home;
        bail!("service integration unsupported on this platform")
    }
}
pub fn run(action: &str, config_path: &Path) -> Result<()> {
    if !matches!(action, "install" | "uninstall" | "status") {
        bail!("unknown service action");
    }
    if action == "install" {
        // Reject invalid configuration before writing a service definition or
        // invoking the service manager. Explicit install still requires an
        // existing file (canonicalized in each platform branch below).
        crate::config::Config::load(config_path)?;
    }
    #[cfg(not(windows))]
    let target = definition_path()?;
    #[cfg(not(windows))]
    path_text(&target)?;
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe { libc::getuid() };
        let domain = format!("gui/{uid}");
        let label = format!("{domain}/{LABEL}");
        match action {
            "install" => {
                let config = config_path
                    .canonicalize()
                    .context("create a valid config before installing")?;
                let exe = std::env::current_exe()?.canonicalize()?;
                let log = dirs::home_dir()
                    .context("home unavailable")?
                    .join("Library/Logs/atrium-node.log");
                std::fs::create_dir_all(log.parent().unwrap())?;
                launchd::install(
                    &mut launchd::Launchctl,
                    &domain,
                    &label,
                    &target,
                    &render_launchd(LABEL, &exe, &config, &log)?,
                )?;
            }
            "uninstall" => {
                launchd::uninstall(&mut launchd::Launchctl, &domain, &label, &target)?;
            }
            "status" => invoke("/bin/launchctl", &["print", &label])?,
            _ => unreachable!(),
        }
    }
    #[cfg(target_os = "linux")]
    systemd::run(action, &target, config_path)?;
    #[cfg(windows)]
    windows::run(action, config_path)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plist_escapes_and_has_separate_arguments() {
        let p = render_launchd(
            LABEL,
            &std::env::temp_dir().join("a & b"),
            &std::env::temp_dir().join("c<d"),
            &std::env::temp_dir().join("log"),
        )
        .unwrap();
        assert!(p.contains("a &amp; b"));
        assert!(p.contains("c&lt;d"));
        assert!(p.contains("<string>--config</string>"));
    }
    #[test]
    fn install_definition_never_clobbers_existing_file() {
        let dir =
            std::env::temp_dir().join(format!("atrium-service-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let target = dir.join("service");
        save(&target, "original").unwrap();
        assert!(save(&target, "replacement").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
