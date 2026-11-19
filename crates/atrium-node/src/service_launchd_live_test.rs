use super::{Launchctl, Runner, State, install, same_path, uninstall};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, DirBuilder},
    net::{SocketAddr, TcpStream},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    thread::sleep,
    time::{Duration, Instant},
};

struct Job {
    root: PathBuf,
    domain: String,
    label: String,
    target: String,
    cleanup_armed: bool,
}

impl Job {
    fn plist(&self) -> PathBuf {
        self.root.join("node.plist")
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.cleanup_armed {
            uninstall(&mut Launchctl, &self.domain, &self.target, &self.plist())?;
            ensure!(
                Launchctl.state(&self.domain, &self.target) == State::Absent,
                "launchd absence was not confirmed"
            );
            self.cleanup_armed = false;
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!(
                "cleanup of {} failed: {error:#}; temporary files preserved",
                self.target
            );
        } else if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!("remove {}: {error}", self.root.display());
        }
    }
}

fn wait_for_node(log: &Path, token: &str, prior: Option<&str>) -> Result<(SocketAddr, String)> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let http = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .timeout(Duration::from_secs(1))
        .build();
    while Instant::now() < deadline {
        if let Some(addr) = fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix("listening on "))
            .and_then(|addr| addr.parse::<SocketAddr>().ok())
        {
            ensure!(addr.ip().is_loopback(), "unexpected non-loopback listener");
            if let Ok(response) = http
                .get(&format!("http://{addr}/v1/node"))
                .set("Authorization", &format!("Bearer {token}"))
                .call()
            {
                let info: serde_json::Value = serde_json::from_reader(response.into_reader())?;
                let instance = info["instance_id"]
                    .as_str()
                    .context("missing node identity")?;
                ensure!(info["protocol"] == 1, "unexpected node protocol");
                if Some(instance) != prior {
                    return Ok((addr, instance.to_owned()));
                }
            }
        }
        sleep(Duration::from_millis(100));
    }
    bail!(
        "node readiness timed out; log: {}",
        fs::read_to_string(log).unwrap_or_default()
    )
}

#[test]
#[ignore = "registers a temporary job in the current macOS GUI launchd domain"]
fn live_launchd_install_restart_uninstall() -> Result<()> {
    let source = std::env::var_os("ATRIUM_LAUNCHD_TEST_NODE")
        .context("set ATRIUM_LAUNCHD_TEST_NODE to the built atrium-node executable")?;
    let label = format!("org.atrium.acceptance.{}", uuid::Uuid::new_v4());
    let uid = unsafe { libc::getuid() };
    ensure!(
        uid != 0 && uid == unsafe { libc::geteuid() },
        "use the GUI user without sudo"
    );
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{label}");
    let mut launchctl = Launchctl;
    ensure!(
        launchctl.state(&domain, &target) == State::Absent,
        "could not confirm {target} absent in a reachable GUI domain"
    );
    let root = std::env::temp_dir().canonicalize()?.join(&label);
    DirBuilder::new().mode(0o700).create(&root)?;
    let mut job = Job {
        root,
        domain,
        label,
        target,
        cleanup_armed: false,
    };
    let node = job.root.join("atrium-node");
    fs::copy(source, &node)?;
    let home = job.root.join("home");
    fs::create_dir(&home)?;
    let wrapper = job.root.join("run-node");
    fs::write(
        &wrapper,
        "#!/bin/sh\nbase=${0%/*}\nexec /usr/bin/env -i HOME=\"$base/home\" SHELL=/usr/bin/false PATH=/usr/bin:/bin \"$base/atrium-node\" \"$@\"\n",
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
    let config = job.root.join("node.toml");
    let token = uuid::Uuid::new_v4().to_string();
    let mut contents = format!(
        "port = 0\nloopback_only = true\ndiscover_magpie = false\nroots = []\ntoken = \"{token}\"\n"
    );
    let missing = toml::Value::String(job.root.join("no-agent").display().to_string());
    for (id, protocol) in [
        ("codex", "codex-app-server"),
        ("claude", "acp"),
        ("deepseek", "acp"),
        ("antigravity", "acp"),
    ] {
        contents.push_str(&format!(
            "[agents.{id}]\ncommand = [{missing}]\nprotocol = \"{protocol}\"\n"
        ));
    }
    fs::write(&config, contents)?;
    let plist = job.plist();
    let log = job.root.join("node.log");
    let template = super::super::render_launchd(&job.label, &wrapper, &config, &log)?;

    println!("launchd target: {} ({})", job.target, plist.display());
    job.cleanup_armed = true;
    let tested = (|| -> Result<_> {
        install(&mut launchctl, &job.domain, &job.target, &plist, &template)?;
        let (first_addr, first) = wait_for_node(&log, &token, None)?;
        ensure!(
            matches!(launchctl.state(&job.domain, &job.target), State::Loaded(Some(p)) if same_path(&p, &plist)),
            "refusing to signal a job without its verified temporary definition"
        );
        let stopped = Command::new("/bin/launchctl")
            .env("LC_ALL", "C")
            .args(["kill", "SIGTERM", &job.target])
            .status()?;
        ensure!(stopped.success(), "launchctl kill returned {stopped}");
        let (second_addr, second) = wait_for_node(&log, &token, Some(&first))?;
        println!("KeepAlive restarted the node ({first} -> {second})");
        Ok((first_addr, second_addr))
    })();
    let cleanup = job.cleanup().context("uninstall temporary launchd service");
    let (first_addr, second_addr) = match (tested, cleanup) {
        (Ok(addrs), Ok(())) => addrs,
        (Err(test), Err(cleanup)) => bail!("{test:#}; cleanup: {cleanup:#}"),
        (Err(error), _) | (_, Err(error)) => return Err(error),
    };
    ensure!(
        !plist.exists(),
        "owned definition still present after uninstall"
    );
    for addr in [first_addr, second_addr] {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(100))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
            {
                break;
            }
            ensure!(Instant::now() < deadline, "listener {addr} did not close");
            sleep(Duration::from_millis(100));
        }
    }
    println!("launchd job absent, owned plist removed and loopback listeners closed");
    Ok(())
}
