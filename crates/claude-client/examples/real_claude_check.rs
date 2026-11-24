//! One real `claude` turn for the owner's acceptance check.
//!
//! Run through `scripts/real-claude-check.sh`. It starts a session, sends one
//! text turn, and writes a sanitized JSONL transcript: event kinds, assistant
//! text, terminal status, token counts and model names. It never writes the
//! environment, headers, credentials or raw frames, and it denies any
//! permission prompt so the check cannot touch the workspace.

use std::{env, fs::File, io::Write, path::PathBuf};

use anyhow::Result;
use claude_client::{
    ClaudeClient, ClaudeConfig, ContentBlock, Incoming, LaunchArgs, PermissionDecision,
    TurnOptions, UserContentBlock,
    events::{ClaudeEvent, Delta, StreamEvent},
};
use futures::{StreamExt, executor::block_on};
use serde_json::json;

fn main() -> Result<()> {
    let binary = env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
    let output = env::var("OUT").unwrap_or_else(|_| "claude-check-transcript.jsonl".into());
    let model = env::var("CLAUDE_MODEL").ok();
    let mut file = File::create(&output)?;

    let config = ClaudeConfig {
        binary: PathBuf::from(binary),
        launch: LaunchArgs {
            model: model.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    let (client, mut events) = block_on(ClaudeClient::start(config))?;

    client.send_turn(
        vec![UserContentBlock::text(
            "Reply with exactly: atrium claude check ok",
        )],
        &TurnOptions {
            model,
            ..Default::default()
        },
    )?;

    let mut answer = String::new();
    while let Some(frame) = block_on(events.next()) {
        match frame {
            Incoming::Event(ClaudeEvent::Stream(StreamEvent::ContentBlockDelta {
                delta: Delta::Text { text },
                ..
            })) => {
                answer.push_str(&text);
                writeln!(file, "{}", json!({"kind": "delta", "text": text}))?;
            }
            Incoming::Event(ClaudeEvent::Assistant(message)) => {
                for block in message.content {
                    if let ContentBlock::Text { text } = block {
                        writeln!(file, "{}", json!({"kind": "assistant", "text": text}))?;
                    }
                }
            }
            Incoming::Event(ClaudeEvent::Result(result)) => {
                writeln!(
                    file,
                    "{}",
                    json!({
                        "kind": "result",
                        "status": format!("{:?}", result.terminal_status()),
                        "is_error": result.is_error,
                        "result": result.result,
                        "usage": result.usage,
                        "models": result.model_usage.keys().collect::<Vec<_>>(),
                        "model_tokens": result
                            .model_usage
                            .iter()
                            .map(|(name, usage)| (name.clone(), usage.output_tokens))
                            .collect::<std::collections::BTreeMap<_, _>>(),
                    })
                )?;
                eprintln!("status: {:?}", result.terminal_status());
                if !answer.is_empty() {
                    eprintln!("assistant: {answer}");
                }
                break;
            }
            Incoming::Permission(request) => {
                // The check sends one text turn only; never let a tool run.
                client.answer_permission(
                    &request.request_id,
                    &PermissionDecision::Deny {
                        message: "real-claude-check only sends one text turn".into(),
                        interrupt: true,
                    },
                )?;
            }
            Incoming::Event(ClaudeEvent::RateLimit(info)) => {
                writeln!(
                    file,
                    "{}",
                    json!({"kind": "rate_limit", "status": info.status, "reset_at": info.reset_at_iso()})
                )?;
            }
            Incoming::Malformed { error, .. } => {
                eprintln!("skipped a malformed frame: {error}");
            }
            Incoming::Closed => break,
            _ => {}
        }
    }
    eprintln!("sanitized transcript: {output}");
    Ok(())
}
