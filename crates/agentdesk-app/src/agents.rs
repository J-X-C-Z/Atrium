//! The agents AgentDesk can drive besides Codex, all over the Agent Client
//! Protocol, and how to find each on this machine.

use std::path::PathBuf;

use agent_rpc::process;

/// An ACP agent AgentDesk knows how to start.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentProfile {
    /// Tags its threads: `claude`, `deepseek`, `antigravity`.
    pub id: &'static str,
    pub name: &'static str,
    /// A few letters for tight places, such as the sidebar.
    pub short: &'static str,
    /// What to run, when found.
    pub command: Option<(PathBuf, Vec<String>)>,
    /// How to get it when it is missing.
    pub install: &'static str,
}

/// The ACP agents, with the command each would run here.
///
/// A command can be forced per agent with `AGENTDESK_<ID>_ACP` (such as
/// `AGENTDESK_CLAUDE_ACP="claude-agent-acp"`), or `agent_commands` in the
/// settings file; otherwise the usual binaries are looked up on `PATH`.
pub fn detect(overrides: &std::collections::HashMap<String, String>) -> Vec<AgentProfile> {
    let forced = |id: &str| -> Option<(PathBuf, Vec<String>)> {
        let text = std::env::var(format!("AGENTDESK_{}_ACP", id.to_uppercase()))
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
            id: "claude",
            name: "Claude Code",
            short: "Claude",
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
            install: "npm install -g @anthropic-ai/claude-code @agentclientprotocol/claude-agent-acp",
        },
        AgentProfile {
            id: "deepseek",
            name: "DeepSeek Harness",
            short: "DeepSeek",
            command: forced("deepseek").or_else(|| {
                process::which("dsh").map(|path| (path, vec!["--profile".into(), "acp".into()]))
            }),
            install: "npm install -g @deepseek-ai/dsh",
        },
        AgentProfile {
            id: "antigravity",
            name: "Antigravity",
            short: "Antigravity",
            command: forced("antigravity")
                .or_else(|| process::which("agy").map(|path| (path, vec!["acp".into()]))),
            install: "Install the Antigravity CLI (agy 1.5 or later) from antigravity.google, then run `agy acp --login`",
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

/// The display name for a backend id.
pub fn backend_name(id: &str) -> &'static str {
    match id {
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
