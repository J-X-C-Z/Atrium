//! Per-user service integration. No registration happens on `serve` or at
//! build/test time: only an explicit install/uninstall CLI action changes it.
#[cfg(all(unix, any(target_os = "macos", test)))]
#[path = "service_launchd.rs"]
mod launchd;
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
#[cfg(any(target_os = "macos", test))]
const LABEL: &str = "dev.atrium.node";
#[cfg(windows)]
const TASK: &str = "AtriumNode";
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
#[cfg(any(target_os = "linux", test))]
fn systemd_arg(s: &str) -> String {
    // ExecStart is not a shell. Escape systemd specifiers and environment
    // expansion in addition to its double-quoted argument grammar.
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}
#[cfg(any(target_os = "linux", test))]
pub fn render_systemd(exe: &Path, config: &Path) -> Result<String> {
    let e = systemd_arg(path_text(exe)?);
    let c = systemd_arg(path_text(config)?);
    Ok(format!(
        "[Unit]\nDescription=Atrium agent node\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={e} serve --config {c}\nRestart=on-failure\nRestartSec=3\nKillMode=mixed\nTimeoutStopSec=30\n\n[Install]\nWantedBy=default.target\n"
    ))
}
/// Quote one Windows CommandLineToArgvW argument without a command shell.
#[cfg(any(windows, test))]
fn windows_arg(s: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in s.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            out.push_str(&"\\".repeat(slashes * 2 + 1));
            out.push('"');
        } else {
            out.push_str(&"\\".repeat(slashes));
            out.push(c);
        }
        slashes = 0;
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}
#[cfg(any(windows, test))]
pub fn render_windows(exe: &Path, config: &Path) -> Result<String> {
    let e = xml(path_text(exe)?);
    let c = xml(&windows_arg(path_text(config)?));
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<RegistrationInfo><Description>Atrium per-user agent node</Description></RegistrationInfo>
<Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>
<Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Enabled>true</Enabled></Settings>
<Actions Context="Author"><Exec><Command>{e}</Command><Arguments>serve --config {c}</Arguments></Exec></Actions>
</Task>
"#
    ))
}
fn invoke(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} failed with {status}");
    }
    Ok(())
}
#[cfg(any(target_os = "linux", windows, test))]
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
pub fn definition_path() -> Result<PathBuf> {
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
    #[cfg(windows)]
    {
        return Ok(dirs::config_dir()
            .context("APPDATA unavailable")?
            .join("atrium-node/task.xml"));
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
    let target = definition_path()?;
    let t = path_text(&target)?;
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
    {
        match action {
            "install" => {
                let config = config_path
                    .canonicalize()
                    .context("create a valid config before installing")?;
                let exe = std::env::current_exe()?.canonicalize()?;
                save(&target, &render_systemd(&exe, &config)?)?;
                invoke("systemctl", &["--user", "daemon-reload"])?;
                invoke(
                    "systemctl",
                    &["--user", "enable", "--now", "atrium-node.service"],
                )?;
                println!(
                    "For operation after logout, enable user lingering explicitly with loginctl enable-linger."
                );
            }
            "uninstall" => {
                invoke(
                    "systemctl",
                    &["--user", "disable", "--now", "atrium-node.service"],
                )?;
                if target.exists() {
                    std::fs::remove_file(&target)?;
                }
                invoke("systemctl", &["--user", "daemon-reload"])?;
            }
            "status" => invoke("systemctl", &["--user", "status", "atrium-node.service"])?,
            _ => unreachable!(),
        }
    }
    #[cfg(windows)]
    {
        match action {
            "install" => {
                let config = config_path
                    .canonicalize()
                    .context("create a valid config before installing")?;
                let exe = std::env::current_exe()?.canonicalize()?;
                save(&target, &render_windows(&exe, &config)?)?;
                invoke("schtasks", &["/Create", "/TN", TASK, "/XML", t])?;
                invoke("schtasks", &["/Run", "/TN", TASK])?;
            }
            "uninstall" => {
                invoke("schtasks", &["/End", "/TN", TASK])?;
                invoke("schtasks", &["/Delete", "/TN", TASK, "/F"])?;
                if target.exists() {
                    std::fs::remove_file(&target)?;
                }
            }
            "status" => invoke("schtasks", &["/Query", "/TN", TASK, "/V", "/FO", "LIST"])?,
            _ => unreachable!(),
        }
    }
    let _ = (t, config_path);
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
    fn systemd_escapes_expansion() {
        let config = std::env::temp_dir().join("config space");
        let p = render_systemd(&std::env::temp_dir().join("a%b$"), &config).unwrap();
        assert!(p.contains("a%%b$$"));
        assert!(p.contains(&systemd_arg(config.to_str().unwrap())));
    }
    #[test]
    fn rejects_control_paths() {
        assert!(
            render_systemd(
                Path::new("/tmp/a\nExecStart=evil"),
                Path::new("/tmp/config")
            )
            .is_err()
        );
    }
    #[test]
    fn windows_quote_backslashes_and_quotes() {
        assert_eq!(windows_arg("C:\\a b\\"), "\"C:\\a b\\\\\"");
        assert_eq!(windows_arg("a\"b"), "\"a\\\"b\"");
    }
    #[test]
    fn windows_task_is_unprivileged_and_escapes_arguments() {
        // Use host-native absolute paths: this tests XML and command-line
        // generation, not Windows task registration on another OS.
        let base = std::env::temp_dir();
        let exe = base.join("node & helper.exe");
        let config = base.join("settings space & quote.toml");
        let output = render_windows(&exe, &config).unwrap();
        assert!(output.contains("node &amp; helper.exe</Command>"));
        assert!(output.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(output.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(output.contains("<Arguments>serve --config &quot;"));
        assert!(output.contains("settings space &amp; quote.toml&quot;</Arguments>"));
        assert!(!output.contains("cmd.exe"));
        assert!(!output.contains("HighestAvailable"));
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
