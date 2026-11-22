//! `atrium memory-mcp`: two tools every agent in Atrium gets, so it can
//! look things up and keep what it learns, whichever agent it is. Two tools
//! only, to keep what each request carries small.
//!
//! - `recall`: entries, past conversations and (when on) Hindsight.
//! - `remember`: one entry, about the user, this project or in general.

use std::{
    io::{BufRead, Write},
    path::PathBuf,
};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::{
    Config, hindsight, project_key, project_root,
    store::{Op, Scope, Store, Target},
};

const PROTOCOL: &str = "2025-03-26";

const INSTRUCTIONS: &str = "Memory shared by every agent this user runs through Atrium. \
Use `recall` before re-discovering something an earlier session may have found (a command, a convention, why something failed). \
Use `remember` when you learn something that will matter in later sessions: a preference the user states, a project convention, \
a fix that took effort. Keep each entry to one or two sentences. Never store secrets.";

/// Where the server works: the memory, the folder the agent works in, and
/// which agent it serves.
pub struct Server {
    pub root: PathBuf,
    pub cwd: Option<PathBuf>,
    pub agent: String,
}

impl Server {
    /// `--root <dir> --cwd <dir> --agent <backend>`; the root defaults to
    /// [`crate::default_root`].
    pub fn from_args(args: &[String]) -> Result<Self> {
        let (mut root, mut cwd, mut agent) = (None, None, None);
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--root" => root = it.next().map(PathBuf::from),
                "--cwd" => cwd = it.next().map(PathBuf::from),
                "--agent" => agent = it.next().cloned(),
                other => return Err(anyhow!("unknown argument {other}")),
            }
        }
        Ok(Self {
            root: root
                .or_else(crate::default_root)
                .ok_or_else(|| anyhow!("no memory folder"))?,
            cwd,
            agent: agent.unwrap_or_else(|| "agent".into()),
        })
    }

    fn project(&self) -> Option<String> {
        self.cwd.as_deref().map(|c| project_key(&project_root(c)))
    }
}

/// Answers requests from `input` on `output` until `input` ends.
pub fn serve(target: &Server, input: impl BufRead, mut output: impl Write) -> Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(target, &message),
            Err(error) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse error: {error}") }
            })),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

pub fn handle(target: &Server, message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or("");
    let params = &message["params"];
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "atrium-memory", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call(
            target,
            params["name"].as_str().unwrap_or(""),
            &params["arguments"],
        )),
        _ => Err(json!({ "code": -32601, "message": format!("no method {method}") })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "recall",
            "description": "Search what earlier sessions learned: memory entries, past conversations with any agent, and long-term memory when it is on.",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string", "description": "What you want to know, in a few words" } },
                "required": ["query"],
            },
        }),
        json!({
            "name": "remember",
            "description": "Keep one short fact for later sessions and other agents.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "One or two sentences" },
                    "about": { "type": "string", "enum": ["user", "project", "general"],
                               "description": "user: the user's preferences; project: this folder's code and conventions; general: anything else" },
                },
                "required": ["text", "about"],
            },
        }),
    ]
}

fn call(target: &Server, name: &str, args: &Value) -> Value {
    let outcome = match name {
        "recall" => recall(target, args["query"].as_str().unwrap_or("")),
        "remember" => remember(
            target,
            args["text"].as_str().unwrap_or(""),
            args["about"].as_str().unwrap_or("general"),
        ),
        other => Err(anyhow!("no tool {other}")),
    };
    match outcome {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Err(error) => {
            json!({ "content": [{ "type": "text", "text": format!("{error:#}") }], "isError": true })
        }
    }
}

fn recall(target: &Server, query: &str) -> Result<String> {
    let store = Store::new(&target.root);
    let config = Config::load(&target.root);
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Err(anyhow!("give a query"));
    }
    let mut scopes = vec![Scope::Global];
    if let Some(key) = target.project() {
        scopes.insert(0, Scope::Project(key));
    }
    let mut out = String::new();
    let mut entries = Vec::new();
    for scope in &scopes {
        for target in [Target::User, Target::Notes] {
            for entry in store.active(scope, target) {
                let lower = entry.text.to_lowercase();
                if words.iter().any(|w| lower.contains(w)) {
                    entries.push(entry.text);
                }
            }
        }
    }
    if !entries.is_empty() {
        out.push_str("Memory entries:\n");
        for e in entries.iter().take(10) {
            out.push_str(&format!("- {e}\n"));
        }
    }
    if let Some(h) = config.hindsight() {
        match hindsight::recall_all(h, target.project().as_deref(), query) {
            Ok(found) if !found.is_empty() => {
                out.push_str("\nLong-term memory:\n");
                for f in found.iter().take(10) {
                    out.push_str(&format!("- {f}\n"));
                }
            }
            Ok(_) => {}
            Err(error) => out.push_str(&format!("\n(Long-term memory did not answer: {error})\n")),
        }
    }
    let sessions = store.search_sessions(query, 5);
    if !sessions.is_empty() {
        out.push_str("\nPast conversations:\n");
        for (header, line) in sessions {
            out.push_str(&format!(
                "- \"{}\" with {} in {}: {}\n  ({})\n",
                header.title,
                header.agent,
                header.cwd,
                line.replace('\n', " "),
                store.session_file(&header.thread).display()
            ));
        }
    }
    if out.is_empty() {
        out = "Nothing found.".into();
    }
    Ok(out)
}

fn remember(target: &Server, text: &str, about: &str) -> Result<String> {
    let store = Store::new(&target.root);
    let config = Config::load(&target.root);
    let (scope, kind) = match about {
        "user" => (Scope::Global, Target::User),
        "project" => match target.cwd.as_deref() {
            Some(cwd) => {
                let root = project_root(cwd);
                let key = project_key(&root);
                let _ = store.register_project(&key, &root);
                (Scope::Project(key), Target::Notes)
            }
            None => (Scope::Global, Target::Notes),
        },
        _ => (Scope::Global, Target::Notes),
    };
    let author = format!("agent:{}", target.agent);
    let change = store
        .apply(
            &scope,
            &[Op::Add {
                content: text.into(),
                target: kind,
            }],
            &author,
            config.landing(),
        )
        .map_err(|e| anyhow!("{e}"))?;
    if let Some(h) = config.hindsight().filter(|h| h.retain) {
        let banks = h.banks(match &scope {
            Scope::Project(key) => Some(key.as_str()),
            Scope::Global => None,
        });
        let _ = hindsight::Hindsight::new(h).retain(
            &banks[0],
            text,
            &format!("note-{}", crate::now_ms()),
            "A note an agent kept",
            &[],
        );
    }
    Ok(match (change, config.confirm_learned) {
        (None, _) => "Already kept.".into(),
        (Some(_), true) => "Kept; the user will look at it before it is used.".into(),
        (Some(_), false) => "Kept.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_tool(target: &Server, name: &str, args: Value) -> Value {
        handle(
            target,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}),
        )
        .unwrap()["result"]
            .clone()
    }

    #[test]
    fn an_agent_remembers_and_another_recalls() {
        let dir = tempfile::tempdir().unwrap();
        let code = dir.path().join("app");
        std::fs::create_dir_all(code.join(".git")).unwrap();
        let codex = Server {
            root: dir.path().join("memory"),
            cwd: Some(code.clone()),
            agent: "codex".into(),
        };
        let listed = handle(
            &codex,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .unwrap();
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 2);

        let kept = call_tool(
            &codex,
            "remember",
            json!({"text":"Integration tests need DATABASE_URL set","about":"project"}),
        );
        assert_eq!(kept["content"][0]["text"], "Kept.");

        let claude = Server {
            root: dir.path().join("memory"),
            cwd: Some(code.join("src")),
            agent: "claude".into(),
        };
        let found = call_tool(&claude, "recall", json!({"query":"database tests"}));
        let text = found["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("DATABASE_URL"), "{text}");

        // The project entry is the project's, written by the agent.
        let store = Store::new(dir.path().join("memory"));
        let key = project_key(&project_root(&code));
        assert_eq!(
            store.active(&Scope::Project(key), Target::Notes)[0].author,
            "agent:codex"
        );
    }

    #[test]
    fn with_confirmation_on_an_agents_note_waits() {
        let dir = tempfile::tempdir().unwrap();
        Config {
            confirm_learned: true,
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let t = Server {
            root: dir.path().into(),
            cwd: None,
            agent: "claude".into(),
        };
        let kept = call_tool(
            &t,
            "remember",
            json!({"text":"Prefers pnpm","about":"user"}),
        );
        assert!(
            kept["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("look at it")
        );
        assert!(
            Store::new(dir.path())
                .active(&Scope::Global, Target::User)
                .is_empty()
        );
    }

    #[test]
    fn refused_notes_come_back_as_errors() {
        let dir = tempfile::tempdir().unwrap();
        let t = Server {
            root: dir.path().into(),
            cwd: None,
            agent: "x".into(),
        };
        let refused = call_tool(
            &t,
            "remember",
            json!({"text":"token ghp_abcdefghijklmnopqrstuvwxyz0123456789","about":"general"}),
        );
        assert_eq!(refused["isError"], true);
    }
}
