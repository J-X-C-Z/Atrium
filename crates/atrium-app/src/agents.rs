//! The agents Atrium can drive besides Codex, all over the Agent Client
//! Protocol, and how to find each on this machine.

use std::path::PathBuf;

use agent_rpc::process;

/// An ACP agent Atrium knows how to start.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentProfile {
    /// Tags its threads: `claude`, `deepseek`, `antigravity`; on another
    /// device, `claude@studio`.
    pub id: String,
    pub name: String,
    /// A few letters for tight places, such as the sidebar.
    pub short: String,
    /// What to run, when found here.
    pub command: Option<(PathBuf, Vec<String>)>,
    /// How to get it when it is missing.
    pub install: String,
}

/// The ACP agents, with the command each would run here.
///
/// A command can be forced per agent with `ATRIUM_<ID>_ACP` (such as
/// `ATRIUM_CLAUDE_ACP="claude-agent-acp"`), or `agent_commands` in the
/// settings file; otherwise the usual binaries are looked up on `PATH`.
pub fn detect(overrides: &std::collections::HashMap<String, String>) -> Vec<AgentProfile> {
    let forced = |id: &str| -> Option<(PathBuf, Vec<String>)> {
        let text = std::env::var(format!("ATRIUM_{}_ACP", id.to_uppercase()))
            .ok()
            .or_else(|| overrides.get(id).cloned())?;
        let mut parts = shell_words(&text).into_iter();
        let program = PathBuf::from(parts.next()?);
        let program = if program.is_absolute() {
            program
        } else {
            process::which(&program.to_string_lossy()).unwrap_or(program)
        };
        Some((program, parts.collect()))
    };
    vec![
        AgentProfile {
            id: "claude".into(),
            name: "Claude Code".into(),
            short: "Claude".into(),
            command: forced("claude").or_else(|| {
                // The official ACP adapter; with only Claude Code installed,
                // npx fetches the adapter on first use.
                process::which("claude-agent-acp")
                    .map(|path| (path, Vec::new()))
                    .or_else(|| {
                        process::which("claude")?;
                        let npx = process::which("npx")?;
                        Some((
                            npx,
                            vec!["-y".into(), "@agentclientprotocol/claude-agent-acp".into()],
                        ))
                    })
            }),
            install: "npm install -g @anthropic-ai/claude-code @agentclientprotocol/claude-agent-acp"
                .into(),
        },
        AgentProfile {
            id: "deepseek".into(),
            name: "DeepSeek Harness".into(),
            short: "DeepSeek".into(),
            command: forced("deepseek").or_else(|| {
                process::which("dsh").map(|path| (path, vec!["--profile".into(), "acp".into()]))
            }),
            install: "npm install -g @deepseek-ai/dsh".into(),
        },
        AgentProfile {
            id: "antigravity".into(),
            name: "Antigravity".into(),
            short: "Antigravity".into(),
            command: forced("antigravity")
                .or_else(|| process::which("agy").map(|path| (path, vec!["acp".into()]))),
            install: "Install the Antigravity CLI (agy 1.5 or later) from antigravity.google, then run `agy acp --login`".into(),
        },
    ]
}

/// Splits a command line on spaces, honoring double quotes.
fn shell_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// A backend id's agent and, for another device's agent, the device:
/// `claude@studio` is `("claude", Some("studio"))`.
pub fn split_backend(id: &str) -> (&str, Option<&str>) {
    match id.split_once('@') {
        Some((agent, device)) => (agent, Some(device)),
        None => (id, None),
    }
}

/// The backend id of `agent` on `device` (this one when `None`).
pub fn backend_id(agent: &str, device: Option<&str>) -> String {
    match device {
        Some(device) => format!("{agent}@{device}"),
        None => agent.to_string(),
    }
}

/// Whether a backend is Codex, here or on another device.
pub fn is_codex(id: &str) -> bool {
    split_backend(id).0 == "codex"
}

/// "Claude Code", or "Claude Code · studio" on another device.
pub fn backend_label(id: &str) -> String {
    match split_backend(id) {
        (agent, Some(device)) => format!("{} · {device}", backend_name(agent)),
        (agent, None) => backend_name(agent).to_string(),
    }
}

/// The display name for a backend id's agent.
pub fn backend_name(id: &str) -> &'static str {
    match split_backend(id).0 {
        "codex" => "Codex",
        "claude" => "Claude Code",
        "deepseek" => "DeepSeek Harness",
        "antigravity" => "Antigravity",
        _ => "Agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_like_a_shell() {
        assert_eq!(
            shell_words(r#"dsh --profile acp"#),
            vec!["dsh", "--profile", "acp"]
        );
        assert_eq!(
            shell_words(r#""/Apps/My Agent/agy" acp"#),
            vec!["/Apps/My Agent/agy", "acp"]
        );
    }

    #[test]
    fn backend_ids_name_the_device() {
        assert_eq!(split_backend("claude@studio"), ("claude", Some("studio")));
        assert_eq!(split_backend("codex"), ("codex", None));
        assert!(is_codex("codex@nas") && !is_codex("claude"));
        assert_eq!(backend_label("deepseek@nas"), "DeepSeek Harness · nas");
        assert_eq!(backend_id("codex", Some("nas")), "codex@nas");
    }

    #[test]
    fn a_forced_command_wins() {
        let overrides = [("deepseek".to_string(), "/opt/dsh --profile acp".to_string())].into();
        let deepseek = detect(&overrides)
            .into_iter()
            .find(|a| a.id == "deepseek")
            .unwrap();
        assert_eq!(
            deepseek.command,
            Some((
                PathBuf::from("/opt/dsh"),
                vec!["--profile".into(), "acp".into()]
            ))
        );
    }
}
