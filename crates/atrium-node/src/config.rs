use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub command: Vec<String>,
    pub protocol: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub port: u16,
    pub loopback_only: bool,
    pub discover_magpie: bool,
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub allow_users: Vec<String>,
    pub allow_tags: Vec<String>,
    pub token: String,
    pub keep_output_lines: usize,
    pub keep_output_bytes: usize,
    pub total_output_bytes: usize,
    pub total_input_bytes: usize,
    pub max_ws_connections: usize,
    pub max_ws_connections_per_process: usize,
    pub detached_timeout_secs: u64,
    pub agents: BTreeMap<String, Agent>,
    #[serde(skip)]
    pub auto_claude: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            port: 7381,
            loopback_only: false,
            discover_magpie: true,
            name: hostname(),
            roots: vec![],
            allow_users: vec![],
            allow_tags: vec![],
            token: String::new(),
            keep_output_lines: 20_000,
            keep_output_bytes: 128 * 1024 * 1024,
            total_output_bytes: 128 * 1024 * 1024,
            total_input_bytes: 256 * 1024 * 1024,
            max_ws_connections: 256,
            max_ws_connections_per_process: 2,
            detached_timeout_secs: 86400,
            agents: BTreeMap::new(),
            auto_claude: true,
        }
    }
}
pub fn path() -> PathBuf {
    #[cfg(windows)]
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    #[cfg(not(windows))]
    let base = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config");
    base.join("atrium-node/config.toml")
}
impl Config {
    fn validate_admission(&self) -> Result<()> {
        if self.total_input_bytes < crate::procs::MAX_LINE_BYTES {
            bail!("total input budget must allow one 64 MiB frame");
        }
        for limit in [self.max_ws_connections, self.max_ws_connections_per_process] {
            if !(2..=tokio::sync::Semaphore::MAX_PERMITS).contains(&limit) {
                bail!("WebSocket limits must be between 2 and Semaphore::MAX_PERMITS");
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut c: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).context("invalid node config")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error).context("cannot read node config"),
        };
        if c.keep_output_lines == 0 || c.keep_output_bytes < 64 * 1024 * 1024 {
            bail!("output limits must retain at least one 64 MiB frame");
        }
        if c.total_output_bytes < 64 * 1024 * 1024 + 128 {
            bail!("total output budget must retain at least one 64 MiB frame with metadata");
        }
        c.validate_admission()?;
        if c.detached_timeout_secs == 0 {
            bail!("detached timeout must be positive");
        }
        // No roots by default: file/process APIs stay closed until explicitly configured.
        c.auto_claude = !c.agents.contains_key("claude");
        for (id, command, protocol) in [
            ("codex", vec!["codex", "app-server"], "codex-app-server"),
            ("claude", vec!["claude-agent-acp"], "acp"),
            ("deepseek", vec!["dsh", "--profile", "acp"], "acp"),
            ("antigravity", vec!["agy", "acp"], "acp"),
        ] {
            c.agents.entry(id.into()).or_insert_with(|| Agent {
                command: command.into_iter().map(String::from).collect(),
                protocol: protocol.into(),
            });
        }
        for (id, a) in &c.agents {
            if a.command.is_empty()
                || a.command[0].is_empty()
                || a.command.iter().any(|arg| arg.contains('\0'))
            {
                bail!("empty command for agent {id}");
            }
        }
        Ok(c)
    }
}
pub fn apply_fallback(c: &mut Config, path: &str) {
    if c.auto_claude
        && find_binary("claude-agent-acp", path).is_none()
        && find_binary("claude", path).is_some()
        && find_binary("npx", path).is_some()
    {
        c.agents.insert(
            "claude".into(),
            Agent {
                command: vec![
                    "npx".into(),
                    "-y".into(),
                    "@agentclientprotocol/claude-agent-acp".into(),
                ],
                protocol: "acp".into(),
            },
        );
    }
}

pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..len]).into_owned();
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "atrium-device".into())
}
pub async fn login_path() -> String {
    let value = std::env::var("PATH").unwrap_or_default();
    #[cfg(unix)]
    let value = {
        let mut value = value;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = tokio::process::Command::new(shell);
        cmd.args(["-lc", "printf '%s' \"$PATH\""])
            .kill_on_drop(true);
        if let Ok(Ok(o)) =
            tokio::time::timeout(std::time::Duration::from_secs(3), cmd.output()).await
            && o.status.success()
        {
            value = String::from_utf8_lossy(&o.stdout).into_owned();
        }
        value
    };
    let mut paths: Vec<PathBuf> = std::env::split_paths(&value).collect();
    #[cfg(unix)]
    paths.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    if let Some(h) = dirs::home_dir() {
        for p in [".npm-global/bin", ".volta/bin", ".local/bin", ".cargo/bin"] {
            paths.push(h.join(p));
        }
    }
    std::env::join_paths(paths)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(value)
}
fn executable(p: &Path) -> bool {
    let Ok(m) = p.metadata() else { return false };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
pub fn find_binary(name: &str, path: &str) -> Option<PathBuf> {
    let p = PathBuf::from(name);
    let bases: Vec<PathBuf> = if p.is_absolute() {
        vec![p]
    } else {
        std::env::split_paths(path).map(|p| p.join(name)).collect()
    };
    #[cfg(not(windows))]
    {
        bases.into_iter().find(|p| executable(p))
    }
    #[cfg(windows)]
    {
        for p in bases {
            if executable(&p) {
                return Some(p);
            }
            if p.extension().is_none() {
                for ext in std::env::var("PATHEXT")
                    .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
                    .split(';')
                {
                    let candidate = p.with_extension(ext.trim_start_matches('.'));
                    if executable(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_fail_closed() {
        let c = Config::default();
        assert!(c.roots.is_empty());
        assert_eq!(c.port, 7381);
        assert!(c.keep_output_bytes >= 64 * 1024 * 1024);
        assert_eq!(c.total_output_bytes, 128 * 1024 * 1024);
        // Existing configurations omit the new field and keep their old cap.
        let legacy: Config = toml::from_str("keep_output_bytes=268435456").unwrap();
        assert_eq!(legacy.keep_output_bytes, 256 * 1024 * 1024);
        assert_eq!(legacy.total_output_bytes, 128 * 1024 * 1024);
    }
    #[test]
    fn admission_limits_validate_without_allocating_payloads_or_semaphores() {
        let mut config = Config::default();
        assert_eq!(config.total_input_bytes, 256 * 1024 * 1024);
        assert_eq!(config.max_ws_connections, 256);
        config.total_input_bytes = crate::procs::MAX_LINE_BYTES;
        assert!(config.validate_admission().is_ok());
        config.total_input_bytes -= 1;
        assert!(config.validate_admission().is_err());
        config.total_input_bytes += 1;
        for limit in [0, 1, tokio::sync::Semaphore::MAX_PERMITS + 1] {
            config.max_ws_connections = limit;
            assert!(config.validate_admission().is_err());
        }
        config.max_ws_connections = 2;
        for limit in [0, 1, tokio::sync::Semaphore::MAX_PERMITS + 1] {
            config.max_ws_connections_per_process = limit;
            assert!(config.validate_admission().is_err());
        }
        config.max_ws_connections_per_process = 2;
        assert!(config.validate_admission().is_ok());
    }

    #[test]
    fn unknown_security_keys_are_not_silently_ignored() {
        assert!(toml::from_str::<Config>("tokne = 'secret'").is_err());
        assert!(
            toml::from_str::<Config>(
                "[agents.codex]\ncommand=['codex']\nprotocol='codex-app-server'\ncommmand=['evil']"
            )
            .is_err()
        );
    }
    #[test]
    fn unreadable_config_is_not_defaulted() {
        // A directory exists but cannot be read as a configuration file.
        assert!(Config::load(&std::env::temp_dir()).is_err());
    }
}
