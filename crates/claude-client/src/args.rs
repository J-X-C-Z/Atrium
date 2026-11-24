// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts (T3 Code v0.1.11, MIT).
// The flag set and ordering follow `@anthropic-ai/claude-agent-sdk` 0.3.321's
// `ProcessTransport.initialize`, which T3's adapter supplies options to.

//! Builds the `claude` command line exactly as the Agent SDK does.
//!
//! The SDK always starts the CLI in bidirectional streaming mode:
//! `--output-format stream-json --verbose --input-format stream-json`.
//! It never passes `--print`: `--input-format stream-json` is what makes the
//! process headless. Values are written as `--flag=value`, as the SDK's `Ye`
//! helper does.

use serde_json::Value;

use crate::events::{Effort, PermissionMode};

/// Which built-in tools are available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tools {
    /// The full Claude Code tool preset (`--tools=default`).
    ClaudeCode,
    /// An explicit list; an empty list disables built-in tools.
    List(Vec<String>),
}

/// Everything this crate passes on the command line. Values that arrive in the
/// `initialize` control request instead (`systemPrompt`, `appendSystemPrompt`,
/// `agents`, `skills`) live on [`crate::ClaudeConfig`].
#[derive(Debug, Clone)]
pub struct LaunchArgs {
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub permission_mode: Option<PermissionMode>,
    pub allow_dangerously_skip_permissions: bool,
    /// Resume an existing session by id (`--resume`).
    pub resume: Option<String>,
    /// Force a session id (`--session-id`).
    pub session_id: Option<String>,
    /// Branch a resumed session instead of continuing it.
    pub fork_session: bool,
    /// Ask for `stream_event` partial frames (`--include-partial-messages`).
    pub include_partial_messages: bool,
    pub tools: Option<Tools>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub additional_directories: Vec<String>,
    /// Implements the SDK's `canUseTool` callback by passing
    /// `--permission-prompt-tool=stdio`.
    pub permission_prompt_tool_stdio: bool,
    /// Extra CLI flags, `(name, value)`. A value of `None` is a boolean flag.
    pub extra_args: Vec<(String, Option<String>)>,
}

impl Default for LaunchArgs {
    fn default() -> Self {
        Self {
            model: None,
            effort: None,
            permission_mode: None,
            allow_dangerously_skip_permissions: false,
            resume: None,
            session_id: None,
            fork_session: false,
            include_partial_messages: true,
            tools: Some(Tools::ClaudeCode),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            max_turns: None,
            additional_directories: Vec::new(),
            permission_prompt_tool_stdio: true,
            extra_args: Vec::new(),
        }
    }
}

/// Builds the argument vector for a CLI process.
pub fn build_args(launch: &LaunchArgs) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--input-format".into(),
        "stream-json".into(),
    ];
    if let Some(effort) = launch.effort.and_then(Effort::launch_arg) {
        args.push(format!("--effort={effort}"));
    }
    if let Some(max_turns) = launch.max_turns {
        args.push(format!("--max-turns={max_turns}"));
    }
    if let Some(model) = &launch.model {
        args.push(format!("--model={model}"));
    }
    if launch.permission_prompt_tool_stdio {
        args.push("--permission-prompt-tool=stdio".into());
    }
    if let Some(resume) = &launch.resume {
        args.push(format!("--resume={resume}"));
    }
    if !launch.allowed_tools.is_empty() {
        args.push(format!("--allowedTools={}", launch.allowed_tools.join(",")));
    }
    if !launch.disallowed_tools.is_empty() {
        args.push(format!(
            "--disallowedTools={}",
            launch.disallowed_tools.join(",")
        ));
    }
    match &launch.tools {
        Some(Tools::ClaudeCode) => args.push("--tools=default".into()),
        Some(Tools::List(tools)) => args.push(format!("--tools={}", tools.join(","))),
        None => {}
    }
    if let Some(mode) = launch.permission_mode {
        args.push(format!("--permission-mode={}", mode.as_str()));
    }
    if launch.allow_dangerously_skip_permissions {
        args.push("--allow-dangerously-skip-permissions".into());
    }
    if launch.include_partial_messages {
        args.push("--include-partial-messages".into());
    }
    for directory in &launch.additional_directories {
        args.push(format!("--add-dir={directory}"));
    }
    if launch.fork_session {
        args.push("--fork-session".into());
    }
    if let Some(session_id) = &launch.session_id {
        args.push(format!("--session-id={session_id}"));
    }
    for (name, value) in &launch.extra_args {
        match value {
            Some(value) if value.len() > 1 && value.starts_with('-') => {
                args.push(format!("--{name}={value}"));
            }
            Some(value) => {
                args.push(format!("--{name}"));
                args.push(value.clone());
            }
            None => args.push(format!("--{name}")),
        }
    }
    args
}

/// The `initialize` control request payload. T3's adapter puts prompt and
/// skill configuration here rather than on the command line.
pub fn initialize_request(
    system_prompt: Option<&str>,
    append_system_prompt: Option<&str>,
    agents: Option<&Value>,
    skills: Option<&[String]>,
) -> Value {
    let mut request = serde_json::json!({ "subtype": "initialize" });
    if let Some(prompt) = system_prompt {
        request["systemPrompt"] = Value::String(prompt.to_owned());
    }
    if let Some(prompt) = append_system_prompt {
        request["appendSystemPrompt"] = Value::String(prompt.to_owned());
    }
    if let Some(agents) = agents {
        request["agents"] = agents.clone();
    }
    if let Some(skills) = skills {
        request["skills"] = Value::Array(
            skills
                .iter()
                .map(|skill| Value::String(skill.clone()))
                .collect(),
        );
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_command_is_bidirectional_streaming_json() {
        let args = build_args(&LaunchArgs {
            permission_prompt_tool_stdio: false,
            ..Default::default()
        });
        assert_eq!(
            args,
            [
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--tools=default",
                "--include-partial-messages",
            ]
        );
    }

    #[test]
    fn model_effort_resume_and_permissions_use_the_sdk_order() {
        let args = build_args(&LaunchArgs {
            model: Some("claude-sonnet-4-6".into()),
            effort: Some(Effort::XHigh),
            permission_mode: Some(PermissionMode::Plan),
            allow_dangerously_skip_permissions: true,
            resume: Some("session-1".into()),
            session_id: Some("session-2".into()),
            include_partial_messages: false,
            tools: Some(Tools::List(vec!["Read".into(), "Glob".into()])),
            allowed_tools: vec!["Read".into()],
            max_turns: Some(3),
            ..Default::default()
        });
        assert_eq!(
            args,
            [
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--effort=xhigh",
                "--max-turns=3",
                "--model=claude-sonnet-4-6",
                "--permission-prompt-tool=stdio",
                "--resume=session-1",
                "--allowedTools=Read",
                "--tools=Read,Glob",
                "--permission-mode=plan",
                "--allow-dangerously-skip-permissions",
                "--session-id=session-2",
            ]
        );
    }

    #[test]
    fn ultrathink_is_never_passed_to_effort() {
        let args = build_args(&LaunchArgs {
            effort: Some(Effort::UltraThink),
            permission_prompt_tool_stdio: false,
            ..Default::default()
        });
        assert!(!args.iter().any(|arg| arg.starts_with("--effort")));
    }

    #[test]
    fn initialize_carries_prompts_agents_and_skills() {
        let agents = serde_json::json!({"reviewer": {"description": "reviews"}});
        let skills = vec!["review".to_owned()];
        let request = initialize_request(
            Some("be brief"),
            Some("repo rules"),
            Some(&agents),
            Some(&skills),
        );
        assert_eq!(request["subtype"], "initialize");
        assert_eq!(request["systemPrompt"], "be brief");
        assert_eq!(request["appendSystemPrompt"], "repo rules");
        assert_eq!(request["agents"]["reviewer"]["description"], "reviews");
        assert_eq!(request["skills"][0], "review");
    }
}
