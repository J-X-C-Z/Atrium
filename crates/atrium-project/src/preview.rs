//! A phase preview runs only with the launch configuration approved on this device.
use crate::{DIR, Project, ProjectNotice, Role};
use anyhow::{Context, Result, bail, ensure};
use atrium_process::ProcessTree;
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub name: String,
    command: String,
    port: u16,
    #[serde(default = "slash")]
    url: String,
    #[serde(default)]
    cwd: PathBuf,
}

fn slash() -> String {
    "/".into()
}

pub fn launches(root: &Path) -> Result<Vec<Launch>> {
    let file = fs::File::open(root.join(DIR).join("launch.json"))?;
    let entries: Vec<Launch> = serde_json::from_reader(file)?;
    ensure!(!entries.is_empty(), "no preview commands configured");
    for e in &entries {
        ensure!(
            !e.command.trim().is_empty() && e.port != 0,
            "empty command or zero port"
        );
        ensure!(
            e.cwd
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
            "cwd must be inside the work directory"
        );
        ensure!(
            e.url.starts_with('/')
                && !e.url.starts_with("//")
                && !e.url.contains('\\')
                && !e.url.chars().any(char::is_control),
            "expected a local URL path"
        );
    }
    Ok(entries)
}

#[derive(Debug, Clone)]
pub struct Preview {
    pub output: PathBuf,
    root: PathBuf,
    work_dir: PathBuf,
    phase: SystemTime,
}

impl Preview {
    pub fn from_notice(project: &Project, n: &ProjectNotice) -> Option<Self> {
        let lead = project.stream(&n.from)?;
        let text = n.text.strip_prefix("Phase ")?;
        if lead.role != Role::Lead
            || !n.is_for(lead.parent.as_deref()?)
            || !(text.ends_with(" done")
                || text.ends_with(" done.")
                || text.contains("\" done (") && text.ends_with(")."))
            || Path::new(&lead.id).file_name()?.to_str()? != lead.id
        {
            return None;
        }
        let phase = n.at.parse::<jiff::Timestamp>().ok().or_else(|| {
            jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", &n.at)
                .ok()?
                .to_zoned(jiff::tz::TimeZone::system())
                .ok()
                .map(|z| z.timestamp())
        })?;
        Some(Self {
            output: project
                .dir()
                .join("previews")
                .join(format!("{}.png", lead.id)),
            root: project.root.clone(),
            work_dir: project.work_dir(&lead.id),
            phase: phase.into(),
        })
    }

    fn stale(&self) -> io::Result<bool> {
        match fs::metadata(&self.output) {
            Ok(m) => Ok(m.modified()? < self.phase),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e),
        }
    }

    pub fn capture(&self, approved: &[Launch], path: &OsStr, cancel: &AtomicBool) -> Result<bool> {
        self.capture_with(approved, path, cancel, Duration::from_secs(120), || {
            browser(path)
        })
    }

    fn capture_with(
        &self,
        approved: &[Launch],
        path: &OsStr,
        cancel: &AtomicBool,
        limit: Duration,
        browser: impl FnOnce() -> Result<PathBuf>,
    ) -> Result<bool> {
        let deadline = Instant::now() + limit;
        let check = || -> Result<()> {
            ensure!(!cancel.load(Ordering::Acquire), "preview cancelled");
            ensure!(Instant::now() < deadline, "preview timed out");
            Ok(())
        };
        check()?;
        if approved.is_empty() || !self.stale()? {
            return Ok(false);
        }
        let entries = launches(&self.root)?;
        ensure!(
            entries == approved,
            "preview commands changed; review them in the project menu"
        );
        let root = self.root.canonicalize()?;
        let work = self.work_dir.canonicalize()?;
        ensure!(work.starts_with(&root), "work directory is outside project");
        let Some((entry, cwd)) = entries.iter().find_map(|e| {
            let dir = work.join(&e.cwd).canonicalize().ok()?;
            (dir.starts_with(&work) && dir.is_dir()).then_some((e, dir))
        }) else {
            return Ok(false);
        };

        let previews = self.output.parent().context("missing preview directory")?;
        ensure!(
            previews
                .parent()
                .context("missing project directory")?
                .canonicalize()?
                .starts_with(&root),
            "atrium directory is outside project"
        );
        fs::create_dir_all(previews)?;
        let previews = previews.canonicalize()?;
        ensure!(
            previews.starts_with(&root),
            "preview directory is outside project"
        );
        // The OS releases the lock on exit, including when another Atrium instance crashes.
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(previews.join(".capture.lock"))?;
        loop {
            check()?;
            match lock.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) => std::thread::sleep(POLL),
                Err(fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
        if !self.stale()? {
            return Ok(false);
        }
        let chromium = browser()?;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, entry.port));
        ensure!(
            !ready(addr),
            "preview port {} is already in use",
            entry.port
        );
        let profile = tempfile::tempdir()?;
        let staging = tempfile::tempdir_in(previews)?;
        let png = staging.path().join("capture.png");
        let mut launch = shell(&entry.command);
        launch.current_dir(cwd).env("PATH", path);
        check()?;
        let mut server = quiet_spawn(&mut launch).context("start preview command")?;
        loop {
            check()?;
            ensure!(
                !server.exited()?,
                "preview server exited before becoming ready"
            );
            if ready(addr) {
                break;
            }
            std::thread::sleep(POLL);
        }
        let mut cmd = Command::new(chromium);
        cmd.args([
            "--headless",
            "--disable-gpu",
            "--disable-breakpad",
            "--disable-crash-reporter",
            "--no-first-run",
            "--no-default-browser-check",
            "--window-size=1280,800",
            "--force-device-scale-factor=1",
        ])
        .arg(flag("--user-data-dir=", profile.path()))
        .arg(flag("--screenshot=", &png))
        .arg(format!("http://{addr}{}", entry.url));
        check()?;
        // Inherit Chromium diagnostics in the test harness for native smoke failures.
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        #[cfg(not(test))]
        cmd.stderr(Stdio::null());
        let mut camera = ProcessTree::spawn(&mut cmd).context("start headless browser")?;
        while !camera.exited()? {
            check()?;
            ensure!(!server.exited()?, "preview server stopped during capture");
            std::thread::sleep(POLL);
        }
        let status = camera.finish()?;
        ensure!(status.success(), "headless screenshot failed ({status})");
        drop(server);
        check()?;
        let mut signature = [0; 8];
        fs::File::open(&png)?.read_exact(&mut signature)?;
        ensure!(
            &signature == b"\x89PNG\r\n\x1a\n",
            "browser did not produce a PNG"
        );
        if !self.stale()? {
            return Ok(false);
        }
        fs::rename(png, &self.output)?;
        Ok(true)
    }
}

const POLL: Duration = Duration::from_millis(50);

fn ready(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, POLL).is_ok()
}

fn quiet_spawn(cmd: &mut Command) -> Result<ProcessTree> {
    ProcessTree::spawn(
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
}

fn flag(prefix: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(prefix);
    arg.push(path);
    arg
}

fn shell(line: &str) -> Command {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", line]);
        cmd
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new(std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into()));
        cmd.args(["/d", "/s", "/c"]).raw_arg(format!("\"{line}\""));
        cmd
    }
}

fn browser(path: &OsStr) -> Result<PathBuf> {
    let dirs: Vec<_> = std::env::split_paths(path)
        .filter(|p| p.is_absolute())
        .collect();
    for name in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "msedge",
        "microsoft-edge",
        "chrome",
    ] {
        let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        if let Some(file) = dirs.iter().map(|dir| dir.join(&exe)).find(|p| p.is_file()) {
            return Ok(file);
        }
    }
    #[cfg(target_os = "macos")]
    for name in ["Google Chrome", "Chromium", "Microsoft Edge"] {
        let exe = PathBuf::from(format!("/Applications/{name}.app/Contents/MacOS/{name}"));
        if exe.is_file() {
            return Ok(exe);
        }
    }
    #[cfg(windows)]
    for dir in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
    {
        for relative in [
            "Google/Chrome/Application/chrome.exe",
            "Microsoft/Edge/Application/msedge.exe",
            "Chromium/Application/chrome.exe",
        ] {
            let exe = PathBuf::from(&dir).join(relative);
            if exe.is_file() {
                return Ok(exe);
            }
        }
    }
    bail!("no Chromium browser found for local previews")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workstream;
    use serde_json::json;
    use std::{io::Write, net::TcpListener, time::UNIX_EPOCH};

    fn fixture() -> (tempfile::TempDir, Project, ProjectNotice) {
        let tmp = tempfile::tempdir().unwrap();
        let mut project = Project::open(tmp.path()).unwrap();
        project.file.streams.push(Workstream {
            id: "web".into(),
            role: Role::Lead,
            parent: Some("boss".into()),
            ..Default::default()
        });
        let notice = ProjectNotice {
            from: "web".into(),
            to: vec!["boss".into()],
            text: "Phase \"Skeleton\" done (1/2 phases of .atrium/tasks/boss/web.md).".into(),
            at: "2026-01-02T03:04:05.123456Z".into(),
        };
        (tmp, project, notice)
    }

    #[test]
    fn phase_notice_and_staleness() {
        let (_tmp, mut project, mut n) = fixture();
        let req = Preview::from_notice(&project, &n).unwrap();
        assert!(req.stale().unwrap());
        fs::create_dir_all(req.output.parent().unwrap()).unwrap();
        let file = fs::File::create(&req.output).unwrap();
        for (mtime, stale) in [
            (UNIX_EPOCH, true),
            (req.phase, false),
            (req.phase + Duration::from_secs(1), false),
        ] {
            file.set_modified(mtime).unwrap();
            assert_eq!(req.stale().unwrap(), stale);
        }
        n.at = "2026-01-02 03:04".into();
        assert!(Preview::from_notice(&project, &n).is_some());
        n.text = "Phase still in progress".into();
        assert!(Preview::from_notice(&project, &n).is_none());
        n.text = "Phase 2 done".into();
        project.file.streams.last_mut().unwrap().role = Role::Worker;
        assert!(Preview::from_notice(&project, &n).is_none());
    }

    #[test]
    fn parses_launch_array_and_requires_exact_local_approval() {
        let (_tmp, p, n) = fixture();
        let config = p.dir().join("launch.json");
        let data = json!([{"name":"web","port":5173,"command":"npm run dev","cwd":"web"}]);
        fs::write(&config, data.to_string()).unwrap();
        let approved = launches(&p.root).unwrap();
        assert_eq!(approved[0].url, "/");
        let req = Preview::from_notice(&p, &n).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(!req.capture(&[], OsStr::new(""), &cancel).unwrap());
        let mut changed = data.clone();
        changed[0]["command"] = json!("other");
        fs::write(&config, changed.to_string()).unwrap();
        assert!(
            req.capture(&approved, OsStr::new(""), &cancel)
                .unwrap_err()
                .to_string()
                .contains("commands changed")
        );
        for (key, value) in [
            ("url", json!("//example.org")),
            ("cwd", json!("../outside")),
            ("port", json!(0)),
            ("port", json!(70000)),
            ("command", json!(" ")),
        ] {
            let mut data = data.clone();
            data[0][key] = value;
            fs::write(&config, data.to_string()).unwrap();
            assert!(launches(&p.root).is_err());
        }
    }

    // Invoked as a shell grandchild in the selected cwd. Holding the port
    // makes a successful re-bind after capture proof that it was stopped.
    #[test]
    #[ignore = "subprocess fixture"]
    fn preview_server_fixture() {
        let port: u16 = fs::read_to_string("fixture-port").unwrap().parse().unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
        fs::write("started", b"yes").unwrap();
        for stream in listener.incoming() {
            let mut s = stream.unwrap();
            let mut buf = [0; 2048];
            if let Ok(n) = s.read(&mut buf)
                && n > 0
            {
                if buf[..n].starts_with(b"GET /preview ") {
                    fs::write("requested", b"yes").unwrap();
                }
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n<h1>Hello</h1>");
            }
        }
    }

    fn server_config(p: &mut Project, n: &ProjectNotice) -> (Preview, Vec<Launch>, u16, PathBuf) {
        p.file.worktrees = true;
        let work = p.work_dir("web").join("site");
        fs::create_dir_all(&work).unwrap();
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(work.join("fixture-port"), port.to_string()).unwrap();
        let exe = std::env::current_exe().unwrap();
        let command = format!(
            "\"{}\" --ignored --exact preview::tests::preview_server_fixture",
            exe.display()
        );
        #[cfg(unix)]
        let command = format!("{command} & wait");
        let entries = json!([{"name":"other","command":"exit 1","port":port,"cwd":"absent"},
            {"name":"site","command":command,"port":port,"cwd":"site","url":"/preview"}]);
        fs::write(p.dir().join("launch.json"), entries.to_string()).unwrap();
        (
            Preview::from_notice(p, n).unwrap(),
            launches(&p.root).unwrap(),
            port,
            work,
        )
    }

    fn check_stopped(port: u16) {
        let until = Instant::now() + Duration::from_secs(2);
        while TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            assert!(Instant::now() < until, "preview listener survived cleanup");
            std::thread::sleep(POLL);
        }
    }

    #[cfg(unix)]
    #[test]
    fn capture_timeout_stops_server_and_browser_descendants() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, mut p, n) = fixture();
        let (req, approved, port, work) = server_config(&mut p, &n);
        let camera = tmp.path().join("camera");
        let pidfile = tmp.path().join("camera-child");
        fs::write(
            &camera,
            format!(
                "#!/bin/sh\nsleep 60 &\necho $! > '{}'\nwait\n",
                pidfile.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&camera, fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::var_os("PATH").unwrap();
        let start = Instant::now();
        let err = req
            .capture_with(
                &approved,
                &path,
                &AtomicBool::new(false),
                Duration::from_secs(3),
                || Ok(camera),
            )
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err:#}");
        assert!(start.elapsed() < Duration::from_secs(6));
        assert!(work.join("started").exists() && !req.output.exists());
        check_stopped(port);
        let pid = fs::read_to_string(pidfile).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(Instant::now() < deadline, "browser child survived");
            std::thread::sleep(POLL);
        }
    }

    #[test]
    fn a_busy_project_can_cancel_before_starting_any_command() {
        let (_tmp, mut p, n) = fixture();
        let (req, approved, _, work) = server_config(&mut p, &n);
        let previews = req.output.parent().unwrap();
        fs::create_dir_all(previews).unwrap();
        let lock = fs::File::create(previews.join(".capture.lock")).unwrap();
        lock.lock().unwrap();
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                req.capture_with(
                    &approved,
                    OsStr::new(""),
                    &cancel,
                    Duration::from_secs(5),
                    || panic!("project is already capturing"),
                )
            });
            std::thread::sleep(POLL * 2);
            cancel.store(true, Ordering::Release);
            assert!(
                job.join()
                    .unwrap()
                    .unwrap_err()
                    .to_string()
                    .contains("cancelled")
            );
        });
        assert!(!req.output.exists() && !work.join("started").exists());
    }

    #[test]
    #[ignore = "needs an installed Chromium browser and a local listener"]
    fn real_browser_captures_phase_and_stops_server() {
        let (_tmp, mut p, n) = fixture();
        let (req, approved, port, work) = server_config(&mut p, &n);
        let path = std::env::var_os("PATH").unwrap();
        assert!(
            req.capture(&approved, &path, &AtomicBool::new(false))
                .unwrap()
        );
        assert!(work.join("requested").exists());
        let image = fs::read(&req.output).unwrap();
        assert_eq!(&image[12..16], b"IHDR");
        assert_eq!(&image[16..24], &[0, 0, 5, 0, 0, 0, 3, 32]);
        assert!(
            !req.capture(&approved, &path, &AtomicBool::new(false))
                .unwrap()
        );
        check_stopped(port);
    }
}
