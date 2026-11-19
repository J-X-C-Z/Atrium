//! Conservative launchd recovery. Real commands are only wired on macOS;
//! ordinary tests use a scripted runner and temporary files.
use anyhow::{Context, Result, bail};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const LIMIT: u64 = 64 * 1024;
pub(super) const MARKER: &str = "<!-- atrium-node managed service v1 -->";
#[derive(Clone, Debug, PartialEq)]
pub(super) enum State {
    Absent,
    Loaded(Option<PathBuf>),
    Unknown,
}
pub(super) trait Runner {
    fn state(&mut self, domain: &str, label: &str) -> State;
    fn bootstrap(&mut self, domain: &str, path: &Path) -> Result<()>;
    fn bootout(&mut self, label: &str) -> Result<()>;
}

struct Definition {
    file: File,
    bytes: Vec<u8>,
}
impl Definition {
    fn read(path: &Path) -> Result<Option<Self>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("open service definition without following symlinks"),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > LIMIT {
            bail!("service definition is not a bounded regular file");
        }
        let mut bytes = Vec::new();
        (&file).take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            bail!("service definition too large");
        }
        Ok(Some(Self { file, bytes }))
    }
    fn matches(&self, path: &Path) -> Result<bool> {
        let Some(other) = Self::read(path)? else {
            return Ok(false);
        };
        let a = self.file.metadata()?;
        let b = other.file.metadata()?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino() && self.bytes == other.bytes)
    }
    fn remove_unchanged(&self, path: &Path) -> Result<()> {
        if path
            .symlink_metadata()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        if !self.matches(path)? {
            bail!("service definition changed; preserved {}", path.display());
        }
        // Stage before deletion, then check identity and content again. If a
        // replacement won the rename race, preserve it under the staging name
        // rather than deleting it or overwriting a new destination to restore it.
        let staged =
            path.with_file_name(format!(".atrium-service-cleanup-{}", uuid::Uuid::new_v4()));
        let reservation = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)?;
        if let Err(e) = std::fs::rename(path, &staged) {
            let _ = std::fs::remove_file(&staged);
            return Err(e).context("stage service definition for cleanup");
        }
        drop(reservation);
        if !self.matches(&staged).with_context(|| {
            format!(
                "cleanup verification failed; file preserved at {}",
                staged.display()
            )
        })? {
            bail!(
                "definition changed during cleanup; preserved at {}",
                staged.display()
            );
        }
        std::fs::remove_file(&staged).context("remove unchanged service definition")
    }
}
fn publish(path: &Path, content: &str) -> Result<Definition> {
    let parent = path.parent().context("service path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".atrium-service-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, path)
            .context("refusing to replace an existing service definition")?;
        Ok(())
    })();
    if let Err(e) = std::fs::remove_file(&temporary) {
        eprintln!(
            "could not clean service temporary {}: {e}",
            temporary.display()
        );
    }
    result?;
    Ok(Definition {
        file,
        bytes: content.as_bytes().to_vec(),
    })
}
fn job_label<'a>(domain: &str, job: &'a str) -> Option<&'a str> {
    let label = job.strip_prefix(domain)?.strip_prefix('/')?;
    (!label.is_empty() && !label.contains('/')).then_some(label)
}

fn managed(bytes: &[u8], label: &str) -> bool {
    fn validate(bytes: &[u8], label: &str) -> Option<()> {
        let text = std::str::from_utf8(bytes).ok()?;
        const EXE: &str = "/__ATRIUM_EXE__";
        const CONFIG: &str = "/__ATRIUM_CONFIG__";
        const LOG: &str = "/__ATRIUM_LOG__";
        let current =
            super::render_launchd(label, Path::new(EXE), Path::new(CONFIG), Path::new(LOG)).ok()?;
        let legacy = !text.starts_with(&format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{MARKER}\n"
        ));
        let template = if legacy {
            current.replacen(&format!("{MARKER}\n"), "", 1)
        } else {
            current
        };
        let (prefix, rest) = template.split_once(EXE)?;
        let (between_exe_config, rest) = rest.split_once(CONFIG)?;
        let (between_config_log, rest) = rest.split_once(LOG)?;
        let (between_logs, suffix) = rest.split_once(LOG)?;
        let rest = text.strip_prefix(prefix)?;
        let (exe, rest) = rest.split_once(between_exe_config)?;
        let (config, rest) = rest.split_once(between_config_log)?;
        let (log, rest) = rest.split_once(between_logs)?;
        if rest.strip_suffix(suffix)? != log {
            return None;
        }
        let decode = |s: &str| {
            s.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&")
        };
        // Re-rendering validates all literal structure, escaped path values,
        // duplicate log paths, and excludes extra keys or comment decoys.
        let expected = super::render_launchd(
            label,
            Path::new(&decode(exe)),
            Path::new(&decode(config)),
            Path::new(&decode(log)),
        )
        .ok()?;
        let expected = if legacy {
            expected.replacen(&format!("{MARKER}\n"), "", 1)
        } else {
            expected
        };
        (expected == text).then_some(())
    }
    validate(bytes, label).is_some()
}

fn same_path(loaded: &Path, target: &Path) -> bool {
    match (loaded.canonicalize(), target.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
pub(super) fn install(
    runner: &mut impl Runner,
    domain: &str,
    label: &str,
    path: &Path,
    content: &str,
) -> Result<()> {
    match runner.state(domain, label) {
        State::Absent => {}
        State::Loaded(_) => bail!("launchd job already loaded; no definition was changed"),
        State::Unknown => bail!("launchd state is uncertain; no definition was changed"),
    }
    let owned = publish(path, content)?;
    if !owned.matches(path)? {
        bail!("service definition changed before bootstrap; preserved without launching");
    }
    let started = runner.bootstrap(domain, path);
    match runner.state(domain, label) {
        State::Loaded(Some(loaded))
            if started.is_ok() && same_path(&loaded, path) && owned.matches(path)? =>
        {
            Ok(())
        }
        State::Absent => {
            owned
                .remove_unchanged(path)
                .context("job absent, but definition cleanup could not be completed")?;
            match started {
                Err(e) => Err(e).context(
                    "bootstrap failed; unchanged definition removed, install may be retried",
                ),
                Ok(()) => bail!(
                    "bootstrap returned success but job is absent; unchanged definition removed"
                ),
            }
        }
        _ => match started {
            Err(e) => Err(e)
                .context("bootstrap failed; loaded or uncertain job state, definition preserved"),
            Ok(()) => bail!(
                "bootstrap returned success but ownership/state is uncertain; definition preserved"
            ),
        },
    }
}
pub(super) fn uninstall(
    runner: &mut impl Runner,
    domain: &str,
    label: &str,
    path: &Path,
) -> Result<()> {
    let service = job_label(domain, label).context("invalid launchd service target")?;
    let owned = Definition::read(path)?;
    if owned.as_ref().is_some_and(|d| !managed(&d.bytes, service)) {
        bail!("unrecognized service definition preserved; refusing to unload by label alone");
    }
    match runner.state(domain, label) {
        State::Absent => {}
        State::Loaded(Some(loaded)) if owned.is_some() && same_path(&loaded, path) => {
            if !owned.as_ref().expect("checked definition").matches(path)? {
                bail!("service definition changed before bootout; preserved without unloading");
            }
            let stopped = runner.bootout(label);
            if runner.state(domain, label) != State::Absent {
                return match stopped {
                    Err(e) => Err(e)
                        .context("bootout failed and absence is unconfirmed; definition preserved"),
                    Ok(()) => anyhow::bail!(
                        "bootout returned success but absence is unconfirmed; definition preserved"
                    ),
                };
            }
            // A nonzero bootout can mean a concurrent unload won. Confirmed
            // absence, not its exit status alone, authorizes file cleanup.
        }
        State::Loaded(_) => bail!("loaded job definition ownership is unconfirmed; preserved"),
        State::Unknown => bail!("launchd state is uncertain; definition preserved"),
    }
    if let Some(owned) = owned {
        owned.remove_unchanged(path)?;
    }
    Ok(())
}

// launchctl print is not a stable structured API. Fail closed for new/localized
// diagnostics. Never equate an arbitrary failed print with an absent job.
#[cfg(any(target_os = "macos", test))]
fn classify(
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
    domain_ok: bool,
    domain: &str,
    job: &str,
) -> State {
    let Some(label) = job_label(domain, job) else {
        return State::Unknown;
    };
    if code == Some(0) {
        let path = stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix("path = "))
            .map(PathBuf::from);
        return State::Loaded(path);
    }
    let Some(uid) = domain
        .strip_prefix("gui/")
        .and_then(|v| v.parse::<u32>().ok())
    else {
        return State::Unknown;
    };
    let expected = format!("Could not find service \"{label}\" in domain for user gui: {uid}");
    let missing = stderr.lines().any(|line| line.trim() == expected);
    if code == Some(113) && missing && domain_ok {
        State::Absent
    } else {
        State::Unknown
    }
}

#[cfg(target_os = "macos")]
pub(super) struct Launchctl;
#[cfg(target_os = "macos")]
impl Runner for Launchctl {
    fn state(&mut self, domain: &str, label: &str) -> State {
        let Ok(out) = std::process::Command::new("/bin/launchctl")
            .env("LC_ALL", "C")
            .args(["print", label])
            .output()
        else {
            return State::Unknown;
        };
        let domain_ok = !out.status.success()
            && std::process::Command::new("/bin/launchctl")
                .env("LC_ALL", "C")
                .args(["print", domain])
                .output()
                .is_ok_and(|o| o.status.success());
        classify(
            out.status.code(),
            std::str::from_utf8(&out.stdout).unwrap_or(""),
            std::str::from_utf8(&out.stderr).unwrap_or(""),
            domain_ok,
            domain,
            label,
        )
    }
    fn bootstrap(&mut self, domain: &str, path: &Path) -> Result<()> {
        let status = std::process::Command::new("/bin/launchctl")
            .env("LC_ALL", "C")
            .args(["bootstrap", domain, super::path_text(path)?])
            .status()?;
        if !status.success() {
            bail!("launchctl bootstrap failed with {status}");
        }
        Ok(())
    }
    fn bootout(&mut self, label: &str) -> Result<()> {
        let status = std::process::Command::new("/bin/launchctl")
            .env("LC_ALL", "C")
            .args(["bootout", label])
            .status()?;
        if !status.success() {
            bail!("launchctl bootout failed with {status}");
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "service_launchd_live_test.rs"]
mod live_test;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("atrium-launchd-fixture-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> PathBuf {
            self.0.join("node.plist")
        }
        fn content(&self) -> String {
            super::super::render_launchd(
                super::super::LABEL,
                &self.0.join("node"),
                &self.0.join("config"),
                &self.0.join("log"),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    type Hook = Box<dyn FnOnce()>;
    struct Fake {
        states: VecDeque<State>,
        fail: bool,
        calls: usize,
        hook: Option<Hook>,
        probe_hook: Option<Hook>,
    }
    impl Fake {
        fn new(states: Vec<State>, fail: bool) -> Self {
            Self {
                states: states.into(),
                fail,
                calls: 0,
                hook: None,
                probe_hook: None,
            }
        }
        fn mutation(&mut self) -> Result<()> {
            self.calls += 1;
            if let Some(h) = self.hook.take() {
                h();
            }
            if self.fail {
                bail!("scripted command failure")
            }
            Ok(())
        }
    }
    impl Runner for Fake {
        fn state(&mut self, _: &str, _: &str) -> State {
            let state = self.states.pop_front().expect("unexpected probe");
            if let Some(h) = self.probe_hook.take() {
                h();
            }
            state
        }
        fn bootstrap(&mut self, _: &str, _: &Path) -> Result<()> {
            self.mutation()
        }
        fn bootout(&mut self, _: &str) -> Result<()> {
            self.mutation()
        }
    }
    const DOMAIN: &str = "gui/123";
    const JOB: &str = "gui/123/dev.atrium.node";
    #[test]
    fn failed_bootstrap_absent_rolls_back_and_retry_works() {
        let f = Fixture::new();
        let p = f.path();
        let mut r = Fake::new(vec![State::Absent, State::Absent], true);
        assert!(install(&mut r, DOMAIN, JOB, &p, &f.content()).is_err());
        assert!(!p.exists());
        let mut r = Fake::new(vec![State::Absent, State::Loaded(Some(p.clone()))], false);
        install(&mut r, DOMAIN, JOB, &p, &f.content()).unwrap();
        assert!(p.exists());
    }
    #[test]
    fn install_never_overwrites_original() {
        let f = Fixture::new();
        let p = f.path();
        std::fs::write(&p, "original").unwrap();
        let mut r = Fake::new(vec![State::Absent], false);
        assert!(install(&mut r, DOMAIN, JOB, &p, &f.content()).is_err());
        assert_eq!(std::fs::read_to_string(p).unwrap(), "original");
        assert_eq!(r.calls, 0);
    }
    #[test]
    fn loaded_and_unknown_preflight_do_not_write() {
        for state in [State::Loaded(None), State::Unknown] {
            let f = Fixture::new();
            let mut r = Fake::new(vec![state], false);
            assert!(install(&mut r, DOMAIN, JOB, &f.path(), &f.content()).is_err());
            assert!(!f.path().exists());
            assert_eq!(r.calls, 0);
        }
    }
    #[test]
    fn loaded_or_uncertain_failure_preserves_definition() {
        for state in [State::Loaded(None), State::Unknown] {
            let f = Fixture::new();
            let mut r = Fake::new(vec![State::Absent, state], true);
            assert!(install(&mut r, DOMAIN, JOB, &f.path(), &f.content()).is_err());
            assert_eq!(std::fs::read_to_string(f.path()).unwrap(), f.content());
        }
    }
    #[test]
    fn rollback_preserves_replacement_and_in_place_edit() {
        for replace in [true, false] {
            let f = Fixture::new();
            let p = f.path();
            let q = p.clone();
            let mut r = Fake::new(vec![State::Absent, State::Absent], true);
            r.hook = Some(Box::new(move || {
                if replace {
                    std::fs::remove_file(&q).unwrap();
                }
                std::fs::write(q, "changed").unwrap();
            }));
            assert!(install(&mut r, DOMAIN, JOB, &p, &f.content()).is_err());
            assert_eq!(std::fs::read_to_string(p).unwrap(), "changed");
        }
    }
    #[test]
    fn bootout_failure_with_confirmed_absence_cleans_and_is_idempotent() {
        let f = Fixture::new();
        let p = f.path();
        std::fs::write(&p, f.content()).unwrap();
        let mut r = Fake::new(vec![State::Loaded(Some(p.clone())), State::Absent], true);
        uninstall(&mut r, DOMAIN, JOB, &p).unwrap();
        assert!(!p.exists());
        let mut r = Fake::new(vec![State::Absent], false);
        uninstall(&mut r, DOMAIN, JOB, &p).unwrap();
        assert_eq!(r.calls, 0);
    }
    #[test]
    fn uninstall_preserves_loaded_or_unknown_after_failure() {
        for state in [State::Loaded(None), State::Unknown] {
            let f = Fixture::new();
            let p = f.path();
            std::fs::write(&p, f.content()).unwrap();
            let mut r = Fake::new(vec![State::Loaded(Some(p.clone())), state], true);
            assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
            assert!(p.exists());
        }
    }
    #[test]
    fn uninstall_absent_legacy_definition_needs_no_bootout() {
        let f = Fixture::new();
        let p = f.path();
        let legacy = f.content().replace(&format!("{MARKER}\n"), "");
        std::fs::write(&p, legacy).unwrap();
        let mut r = Fake::new(vec![State::Absent], true);
        uninstall(&mut r, DOMAIN, JOB, &p).unwrap();
        assert!(!p.exists());
        assert_eq!(r.calls, 0);
    }
    #[test]
    fn uninstall_protects_foreign_missing_or_wrong_loaded_path() {
        let f = Fixture::new();
        let p = f.path();
        std::fs::write(&p, "foreign").unwrap();
        let mut r = Fake::new(vec![], false);
        assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "foreign");
        std::fs::remove_file(&p).unwrap();
        let mut r = Fake::new(vec![State::Loaded(Some(p.clone()))], false);
        assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
        assert_eq!(r.calls, 0);
        std::fs::write(&p, f.content()).unwrap();
        let mut r = Fake::new(vec![State::Loaded(Some(f.0.join("other")))], false);
        assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
        assert!(p.exists());
        assert_eq!(r.calls, 0);
    }
    #[test]
    fn uninstall_keeps_replaced_plist() {
        let f = Fixture::new();
        let p = f.path();
        std::fs::write(&p, f.content()).unwrap();
        let q = p.clone();
        let mut r = Fake::new(vec![State::Loaded(Some(p.clone())), State::Absent], false);
        r.hook = Some(Box::new(move || {
            std::fs::remove_file(&q).unwrap();
            std::fs::write(q, "replacement").unwrap();
        }));
        assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
        assert_eq!(std::fs::read_to_string(p).unwrap(), "replacement");
    }
    #[test]
    fn symlink_definition_is_preserved() {
        let f = Fixture::new();
        let other = f.0.join("other");
        std::fs::write(&other, f.content()).unwrap();
        std::os::unix::fs::symlink(&other, f.path()).unwrap();
        let mut r = Fake::new(vec![], false);
        assert!(uninstall(&mut r, DOMAIN, JOB, &f.path()).is_err());
        assert!(
            f.path()
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(other).unwrap(), f.content());
    }
    #[test]
    fn absence_requires_specific_diagnostic_and_live_domain() {
        let missing = "Bad request.\nCould not find service \"dev.atrium.node\" in domain for user gui: 123\n";
        assert_eq!(
            classify(Some(113), "", missing, true, DOMAIN, JOB),
            State::Absent
        );
        assert_eq!(
            classify(Some(113), "", missing, false, DOMAIN, JOB),
            State::Unknown
        );
        assert_eq!(
            classify(Some(1), "", "Permission denied", true, DOMAIN, JOB),
            State::Unknown
        );
        assert_eq!(
            classify(
                Some(113),
                "",
                "Could not find service \"other\" in domain x",
                true,
                DOMAIN,
                JOB,
            ),
            State::Unknown
        );
        assert_eq!(
            classify(
                Some(0),
                "\tpath = /tmp/node.plist\n",
                "",
                false,
                DOMAIN,
                JOB
            ),
            State::Loaded(Some(PathBuf::from("/tmp/node.plist")))
        );
    }
    #[test]
    fn rollback_preserves_same_bytes_new_inode_and_missing_target() {
        for disappear in [false, true] {
            let f = Fixture::new();
            let p = f.path();
            let q = p.clone();
            let content = f.content();
            let replacement = content.clone();
            let mut r = Fake::new(vec![State::Absent, State::Absent], true);
            r.hook = Some(Box::new(move || {
                std::fs::remove_file(&q).unwrap();
                if !disappear {
                    std::fs::write(q, replacement).unwrap();
                }
            }));
            let error = install(&mut r, DOMAIN, JOB, &p, &content).unwrap_err();
            if disappear {
                assert!(!p.exists());
                assert!(format!("{error:#}").contains("scripted command failure"));
            } else {
                assert_eq!(std::fs::read_to_string(p).unwrap(), content);
            }
        }
    }
    #[test]
    fn successful_commands_do_not_override_uncertain_postconditions() {
        let f = Fixture::new();
        let p = f.path();
        let mut r = Fake::new(vec![State::Absent, State::Unknown], false);
        assert!(install(&mut r, DOMAIN, JOB, &p, &f.content()).is_err());
        assert!(p.exists());
        let mut r = Fake::new(vec![State::Loaded(Some(p.clone())), State::Unknown], false);
        assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
        assert!(p.exists());
        let mut r = Fake::new(vec![State::Unknown], false);
        assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
        assert_eq!(r.calls, 0);
    }
    #[test]
    fn query_time_replacement_is_not_unloaded() {
        let f = Fixture::new();
        let p = f.path();
        std::fs::write(&p, f.content()).unwrap();
        let q = p.clone();
        let content = f.content();
        let mut r = Fake::new(vec![State::Loaded(Some(p.clone()))], false);
        r.probe_hook = Some(Box::new(move || {
            std::fs::remove_file(&q).unwrap();
            std::fs::write(q, content).unwrap();
        }));
        assert!(uninstall(&mut r, DOMAIN, JOB, &p).is_err());
        assert_eq!(r.calls, 0);
        assert!(p.exists());
    }
    #[test]
    fn comment_decoys_duplicate_keys_and_extra_commands_are_foreign() {
        let f = Fixture::new();
        let valid = f.content();
        assert!(managed(valid.as_bytes(), super::super::LABEL));
        assert!(!managed(valid.as_bytes(), "org.atrium.acceptance.other"));
        for invalid in [
            format!("<!--{valid}-->"),
            valid.replace("</dict>", "<key>Label</key><string>other</string></dict>"),
            valid.replace(
                "<string>serve</string>",
                "<string>evil</string><string>serve</string>",
            ),
            valid.replace(
                "<key>StandardErrorPath</key><string>",
                "<key>StandardErrorPath</key><string>/other",
            ),
        ] {
            assert!(!managed(invalid.as_bytes(), super::super::LABEL));
        }
        let missing = "Could not find service \"dev.atrium.node\" in domain for user gui: 123";
        assert_eq!(
            classify(Some(1), "", missing, true, DOMAIN, JOB),
            State::Unknown
        );
        assert_eq!(
            classify(None, "", missing, true, DOMAIN, JOB),
            State::Unknown
        );
        assert_eq!(
            classify(
                Some(113),
                "",
                missing,
                true,
                "gui/456",
                "gui/456/dev.atrium.node"
            ),
            State::Unknown
        );
    }
}
