//! Per-user systemd lifecycle. Fixtures use a scripted systemctl and temporary files.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const UNIT: &str = "atrium-node.service";
const HEAD: &str =
    "[Unit]\nDescription=Atrium agent node\nAfter=network.target\n\n[Service]\nType=";
const TAIL: &str = "\nRestart=on-failure\nRestartSec=3\nKillMode=mixed\nTimeoutStopSec=30\n\n[Install]\nWantedBy=default.target\n";

fn render(exe: &Path, config: &Path) -> Result<String> {
    let exe = super::path_text(exe)?;
    // systemd rejects these characters in the executable even after unquoting.
    ensure!(
        !exe.contains(['\\', '"', '\'']),
        "systemd executable path cannot contain quotes or backslashes"
    );
    let config = super::path_text(config)?;
    let exe = exe.replace('%', "%%");
    let config = config
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    // The ':' prefix disables environment expansion, including in argv[0].
    Ok(format!(
        "{HEAD}exec\nExecStart=\":{exe}\" serve --config \"{config}\"{TAIL}"
    ))
}

fn managed(text: &str) -> bool {
    let Some(line) = text.strip_prefix(HEAD).and_then(|s| s.strip_suffix(TAIL)) else {
        return false;
    };
    let (legacy, line) = match line.strip_prefix("exec\nExecStart=\":") {
        Some(line) => (false, line),
        None => match line.strip_prefix("simple\nExecStart=\"") {
            Some(line) => (true, line),
            None => return false,
        },
    };
    let Some((exe, config)) = line
        .strip_suffix('"')
        .and_then(|s| s.split_once("\" serve --config \""))
    else {
        return false;
    };
    let escaped_path = |path: &str| {
        if !path.starts_with('/') || path.chars().any(char::is_control) {
            return false;
        }
        let mut chars = path.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' if matches!(chars.next(), Some('\\' | '"')) => {}
                '%' if chars.next() == Some('%') => {}
                '$' if legacy && chars.next() == Some('$') => {}
                '\\' | '"' | '%' => return false,
                '$' if legacy => return false,
                _ => {}
            }
        }
        true
    };
    escaped_path(exe) && escaped_path(config)
}

struct Definition {
    file: File,
    text: String,
}

impl Definition {
    fn read(path: &Path) -> Result<Option<Self>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).context("open systemd unit without following symlinks"),
        };
        ensure!(
            file.metadata()?.is_file(),
            "systemd unit is not a regular file"
        );
        let mut text = String::new();
        const MAX_UNIT: u64 = 64 * 1024;
        (&file).take(MAX_UNIT + 1).read_to_string(&mut text)?;
        ensure!(text.len() as u64 <= MAX_UNIT, "systemd unit is too large");
        Ok(Some(Self { file, text }))
    }

    fn check(&self, path: &Path) -> Result<()> {
        let Some(other) = Self::read(path)? else {
            bail!("systemd unit disappeared: {}", path.display());
        };
        let a = self.file.metadata()?;
        let b = other.file.metadata()?;
        ensure!(
            (a.dev(), a.ino()) == (b.dev(), b.ino()) && self.text == other.text,
            "systemd unit changed: {}",
            path.display()
        );
        Ok(())
    }

    fn remove(self, path: &Path) -> Result<()> {
        self.check(path)?;
        // Preserve a file that wins the race with rename, even if it cannot be restored.
        let staged = path.with_file_name(format!(".atrium-systemd-{}", uuid::Uuid::new_v4()));
        let _reserved = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)?;
        if let Err(err) = fs::rename(path, &staged) {
            let _ = fs::remove_file(&staged);
            return Err(err).context("stage systemd unit removal");
        }
        self.check(&staged)
            .with_context(|| format!("unit preserved at {}", staged.display()))?;
        fs::remove_file(staged).context("remove unchanged systemd unit")
    }
}

#[derive(Debug, PartialEq)]
enum UnitState {
    Absent,
    Loaded(PathBuf),
}

fn parse_state(text: &str) -> Result<UnitState> {
    let get = |key| {
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find_map(|(name, value)| (name == key).then_some(value))
    };
    ensure!(
        get("DropInPaths") == Some(""),
        "systemd unit has drop-ins or unknown state"
    );
    match (get("LoadState"), get("ActiveState"), get("FragmentPath")) {
        (Some("not-found"), Some("inactive"), Some("")) => Ok(UnitState::Absent),
        (Some("loaded"), Some(_), Some(path)) if Path::new(path).is_absolute() => {
            Ok(UnitState::Loaded(path.into()))
        }
        _ => bail!("systemd unit is masked, invalid, or its state is unknown"),
    }
}

fn state(run: &mut impl FnMut(&[&str]) -> Result<String>) -> Result<UnitState> {
    parse_state(&run(&[
        "show",
        "--all",
        "--property=LoadState,ActiveState,FragmentPath,DropInPaths",
        UNIT,
    ])?)
}

fn loaded_from(fragment: &Path, target: &Path) -> bool {
    target.canonicalize().is_ok_and(|path| path == fragment)
}

fn install(
    run: &mut impl FnMut(&[&str]) -> Result<String>,
    target: &Path,
    content: &str,
) -> Result<()> {
    ensure!(
        state(run)? == UnitState::Absent,
        "systemd unit already exists"
    );
    super::save(target, content).context("publish new systemd unit")?;
    let owned = Definition::read(target)?.context("published systemd unit disappeared")?;
    ensure!(owned.text == content, "published systemd unit changed");
    if let Err(err) = run(&["daemon-reload"]) {
        // No enable/start was attempted. Only remove our file when absence is confirmed.
        if matches!(state(run), Ok(UnitState::Absent)) {
            owned
                .remove(target)
                .context("reload failed and unit cleanup failed")?;
            return Err(err).context("systemd reload failed, installation can be retried");
        }
        return Err(err).context("systemd reload failed, unit preserved");
    }
    match state(run)? {
        UnitState::Loaded(fragment) if loaded_from(&fragment, target) => owned.check(target)?,
        UnitState::Absent => {
            owned.remove(target)?;
            bail!(
                "systemd did not find the unit after reload; check the user manager's config directory"
            );
        }
        _ => bail!("systemd selected a different unit after reload; definitions preserved"),
    }
    run(&["enable", "--now", UNIT]).context(
        "systemd enable/start failed; unit preserved for recovery with atrium-node uninstall",
    )?;
    Ok(())
}

fn uninstall(run: &mut impl FnMut(&[&str]) -> Result<String>, target: &Path) -> Result<()> {
    let owned = Definition::read(target)?;
    ensure!(
        owned.as_ref().is_none_or(|def| managed(&def.text)),
        "unrecognized systemd unit preserved"
    );
    match (state(run)?, owned) {
        (UnitState::Absent, None) => Ok(()),
        (UnitState::Absent, Some(owned)) => owned.remove(target),
        (UnitState::Loaded(fragment), Some(owned)) if loaded_from(&fragment, target) => {
            owned.check(target)?;
            run(&["disable", "--now", UNIT])
                .context("systemd disable/stop failed; unit preserved")?;
            owned.remove(target)?;
            run(&["daemon-reload"]).context("unit removed but systemd reload failed")?;
            Ok(())
        }
        _ => bail!("systemd did not load our unit file; refusing to stop or disable it"),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn run(action: &str, target: &Path, config_path: &Path) -> Result<()> {
    use std::process::Command;
    let mut systemctl = |args: &[&str]| {
        let output = Command::new("systemctl")
            .args(["--user", "--no-pager"])
            .args(args)
            .output()
            .context("run systemctl --user")?;
        ensure!(
            output.status.success(),
            "systemctl --user {} failed ({}): {}",
            args[0],
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        String::from_utf8(output.stdout).context("invalid systemctl output")
    };
    match action {
        "install" => {
            let config = config_path
                .canonicalize()
                .context("create a valid config before installing")?;
            let exe = std::env::current_exe()?.canonicalize()?;
            install(&mut systemctl, target, &render(&exe, &config)?)?;
            println!(
                "For operation after logout, enable user lingering explicitly with loginctl enable-linger."
            );
        }
        "uninstall" => uninstall(&mut systemctl, target)?,
        "status" => {
            let status = Command::new("systemctl")
                .args(["--user", "status", UNIT])
                .status()
                .context("run systemctl --user status")?;
            ensure!(
                status.success(),
                "systemctl --user status failed with {status}"
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("atrium-systemd-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn target(&self) -> PathBuf {
            self.0.join(UNIT)
        }

        fn loaded(&self) -> String {
            format!(
                "LoadState=loaded\nActiveState=active\nFragmentPath={}\nDropInPaths=\n",
                self.target().canonicalize().unwrap().display()
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const ABSENT: &str = "LoadState=not-found\nActiveState=inactive\nFragmentPath=\nDropInPaths=\n";

    fn content() -> String {
        render(Path::new("/bin/atrium-node"), Path::new("/tmp/node config")).unwrap()
    }

    fn script<'a>(
        steps: &'a mut VecDeque<(&str, Result<String>)>,
    ) -> impl FnMut(&[&str]) -> Result<String> + 'a {
        |args| {
            let (command, result) = steps.pop_front().expect("unexpected systemctl call");
            assert_eq!(args.join(" "), command);
            result
        }
    }

    const SHOW: &str =
        "show --all --property=LoadState,ActiveState,FragmentPath,DropInPaths atrium-node.service";
    const ENABLE: &str = "enable --now atrium-node.service";
    const DISABLE: &str = "disable --now atrium-node.service";

    #[test]
    fn quotes_paths_without_expanding_dollars_and_preserves_legacy_units() {
        let rendered = render(
            Path::new("/tmp/a %n $node"),
            Path::new("/tmp/${HOME}%n/\\\" config"),
        )
        .unwrap();
        assert!(rendered.contains(
            r#"ExecStart=":/tmp/a %%n $node" serve --config "/tmp/${HOME}%%n/\\\" config""#
        ));
        assert!(managed(&rendered));
        let legacy = format!(
            "{HEAD}simple\nExecStart=\"/tmp/a %%n $$node\" serve --config \"/tmp/$${{HOME}}%%n/\\\\\\\" config\"{TAIL}"
        );
        assert!(managed(&legacy));
        assert!(!managed(&rendered.replace("RestartSec=3", "RestartSec=5")));
        assert!(!managed(&rendered.replace("$node", "\" --unexpected \"")));
        assert!(render(Path::new("/tmp/a\nb"), Path::new("/tmp/config")).is_err());
        assert!(render(Path::new("/tmp/a'b"), Path::new("/tmp/config")).is_err());
    }

    #[test]
    fn unavailable_manager_keeps_install_unpublished() {
        let f = Fixture::new();
        let mut steps = [(SHOW, Err(anyhow::anyhow!("no user bus")))].into();
        assert!(install(&mut script(&mut steps), &f.target(), &content()).is_err());
        assert!(!f.target().exists());
        assert!(steps.is_empty());
    }

    #[test]
    fn failed_reload_rolls_back_when_absent_and_retry_succeeds() {
        let f = Fixture::new();
        let target = f.target();
        let mut rollback = [
            (SHOW, Ok(ABSENT.into())),
            ("daemon-reload", Err(anyhow::anyhow!("reload failed"))),
            (SHOW, Ok(ABSENT.into())),
        ]
        .into();
        assert!(install(&mut script(&mut rollback), &target, &content()).is_err());
        assert!(!target.exists());
        assert!(rollback.is_empty());

        let mut commands = Vec::new();
        install(
            &mut |args| {
                commands.push(args.join(" "));
                match commands.len() {
                    1 => Ok(ABSENT.into()),
                    3 => Ok(f.loaded()),
                    _ => Ok(String::new()),
                }
            },
            &target,
            &content(),
        )
        .unwrap();
        assert_eq!(commands, [SHOW, "daemon-reload", SHOW, ENABLE]);
        assert!(target.exists());
    }

    #[test]
    fn partial_enable_remains_recoverable_and_uninstall_can_be_repeated() {
        let f = Fixture::new();
        let target = f.target();
        let mut calls = Vec::new();
        assert!(
            install(
                &mut |args| {
                    calls.push(args.join(" "));
                    match calls.len() {
                        1 => Ok(ABSENT.into()),
                        3 => Ok(f.loaded()),
                        4 => Err(anyhow::anyhow!("start failed after enable")),
                        _ => Ok(String::new()),
                    }
                },
                &target,
                &content()
            )
            .is_err()
        );
        assert_eq!(calls, [SHOW, "daemon-reload", SHOW, ENABLE]);
        let mut steps = [
            (SHOW, Ok(f.loaded())),
            (DISABLE, Ok(String::new())),
            ("daemon-reload", Ok(String::new())),
            (SHOW, Ok(ABSENT.into())),
        ]
        .into();
        uninstall(&mut script(&mut steps), &target).unwrap();
        uninstall(&mut script(&mut steps), &target).unwrap();
        assert!(!target.exists());
        assert!(steps.is_empty());
    }

    #[test]
    fn uninstall_preserves_custom_files_symlinks_and_foreign_fragments() {
        let f = Fixture::new();
        let target = f.target();
        let real = f.0.join("foreign.service");
        fs::write(&target, "custom unit").unwrap();
        let mut no_calls = |_: &[&str]| panic!("unrecognized unit must not invoke systemctl");
        assert!(uninstall(&mut no_calls, &target).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "custom unit");
        fs::rename(&target, &real).unwrap();
        std::os::unix::fs::symlink(&real, &target).unwrap();
        assert!(uninstall(&mut no_calls, &target).is_err());
        assert!(target.is_symlink());
        fs::remove_file(&target).unwrap();
        fs::write(&target, content()).unwrap();
        let state = f.loaded().replace(UNIT, "foreign.service");
        let mut steps = [(SHOW, Ok(state))].into();
        assert!(uninstall(&mut script(&mut steps), &target).is_err());
        assert!(target.exists());
        assert!(steps.is_empty());
    }

    #[test]
    fn failed_reload_preserves_changed_or_uncertain_units() {
        let f = Fixture::new();
        let target = f.target();
        for changed in [false, true] {
            let mut calls = Vec::new();
            assert!(
                install(
                    &mut |args| {
                        calls.push(args.join(" "));
                        match calls.len() {
                            1 => Ok(ABSENT.into()),
                            2 => {
                                if changed {
                                    fs::write(&target, "replacement").unwrap();
                                }
                                Err(anyhow::anyhow!("reload failed"))
                            }
                            3 if changed => Ok(ABSENT.into()),
                            _ => Err(anyhow::anyhow!("manager became unavailable")),
                        }
                    },
                    &target,
                    &content()
                )
                .is_err()
            );
            assert_eq!(calls, [SHOW, "daemon-reload", SHOW]);
            assert_eq!(
                fs::read_to_string(&target).unwrap(),
                if changed {
                    "replacement".into()
                } else {
                    content()
                }
            );
            fs::remove_file(&target).unwrap();
        }
    }
}
