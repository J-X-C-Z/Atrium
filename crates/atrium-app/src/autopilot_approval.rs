//! Prove the scope of a local approval. The display command, a tool title,
//! and a cwd alone do not describe the effects of arbitrary shell code.
use std::path::{Component, Path};

use atrium_core::{
    ApprovalDecision, ApprovalKind, ApprovalRequest, ChangeKind, EntryKind, Item, Transcript,
};
use serde_json::Value;

pub(super) fn contained(root: &Path, path: &Path) -> bool {
    path.is_absolute()
        && root
            .canonicalize()
            .ok()
            .zip(path.canonicalize().ok())
            .is_some_and(|(root, path)| path.starts_with(root))
}

fn change_in(dir: &Path, name: &str) -> bool {
    if name.is_empty()
        || Path::new(name)
            .components()
            .any(|c| c == Component::ParentDir)
    {
        return false;
    }
    let path = dir.join(name);
    // A new file can be absent. Never walk through a dangling link or infer
    // the destination of an inaccessible component.
    for candidate in path.ancestors() {
        match candidate.symlink_metadata() {
            Ok(_) => return contained(dir, candidate),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            _ => return false,
        }
    }
    false
}

fn literal_inspection(dir: &Path, cwd: &str, command: &str) -> bool {
    let cwd = Path::new(cwd);
    if !contained(dir, cwd) || !cwd.is_dir() {
        return false;
    }
    if cfg!(windows) {
        // Names resolved through PATH can name a program in the agent's cwd.
        return std::env::var("SystemRoot").is_ok_and(|root| {
            root.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b":\\_-".contains(&b))
                && command.eq_ignore_ascii_case(&format!(r"{root}\System32\cmd.exe /d /c cd"))
        });
    }
    if command == "/bin/pwd" {
        return true;
    }
    // Only system executables with one literal existing operand. Shells,
    // options, redirections, substitutions and interpreters are ambiguous.
    let Some((exe, arg)) = command.split_once(' ') else {
        return false;
    };
    if !matches!(exe, "/bin/cat" | "/bin/ls")
        || arg.is_empty()
        || arg.starts_with('-')
        || !arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    {
        return false;
    }
    let path = cwd.join(arg);
    contained(dir, &path) && path.is_file()
}

pub(super) fn acp_decision(dir: &Path, wire: &Value) -> ApprovalDecision {
    let call = &wire["toolCall"];
    let input = &call["rawInput"];
    let once = wire["options"].as_array().is_some_and(|options| {
        options.iter().any(|o| {
            o["kind"] == "allow_once" && o["optionId"].as_str().is_some_and(|id| !id.is_empty())
        })
    });
    if once
        && call["kind"] == "execute"
        && input.as_object().is_some_and(|fields| {
            fields
                .keys()
                .all(|k| matches!(k.as_str(), "command" | "cwd"))
        })
        && let (Some(cwd), Some(command)) = (input["cwd"].as_str(), input["command"].as_str())
        && literal_inspection(dir, cwd, command)
    {
        ApprovalDecision::Accept
    } else {
        ApprovalDecision::Decline
    }
}

pub(super) fn decision(
    dir: &Path,
    request: &ApprovalRequest,
    wire: &Value,
    transcript: &Transcript,
) -> ApprovalDecision {
    use ApprovalDecision::{Accept, AcceptForSession, Decline};
    let Some(fields) = wire.as_object() else {
        return Decline;
    };
    // Unknown permission extensions may widen the request beyond its displayed action.
    if fields.iter().any(|(k, v)| {
        !v.is_null()
            && !matches!(
                k.as_str(),
                "threadId"
                    | "turnId"
                    | "itemId"
                    | "approvalId"
                    | "reason"
                    | "command"
                    | "cwd"
                    | "commandActions"
                    | "availableDecisions"
                    | "grantRoot"
            )
    }) {
        return Decline;
    }
    match &request.kind {
        ApprovalKind::Command { command, cwd, .. }
            if wire["command"].as_str() == Some(command)
                && literal_inspection(dir, cwd, command) =>
        {
            Accept
        }
        ApprovalKind::FileChange { grant_root, .. } => {
            if grant_root
                .as_ref()
                .is_some_and(|r| !contained(dir, Path::new(r)))
            {
                return Decline;
            }
            let permitted = grant_root.as_deref().map(Path::new).unwrap_or(dir);
            let changes = transcript
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match &entry.kind {
                    EntryKind::Item(Item::FileChange { id, changes, .. })
                        if entry.turn_id == request.turn_id && id == &request.item_id =>
                    {
                        Some(changes)
                    }
                    _ => None,
                });
            if !changes.is_some_and(|changes| {
                !changes.is_empty()
                    && changes.iter().all(|c| {
                        let check =
                            |name: &str| change_in(permitted, &dir.join(name).to_string_lossy());
                        !c.path.is_empty()
                            && check(&c.path)
                            && match &c.kind {
                                ChangeKind::Update {
                                    move_to: Some(dest),
                                } => !dest.is_empty() && check(dest),
                                _ => true,
                            }
                    })
            }) {
                return Decline;
            }
            if grant_root.is_some() {
                AcceptForSession
            } else {
                Accept
            }
        }
        _ => Decline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::{AgentEvent, FileChange, ItemStatus};
    #[test]
    fn proves_cwd_and_each_operand_and_move_destination() {
        let parent = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let dir = parent.join("project");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source"), "").unwrap();
        std::fs::write(parent.join("outside"), "").unwrap();
        let cwd = dir.to_str().unwrap();
        #[cfg(unix)]
        {
            assert!(literal_inspection(&dir, cwd, "/bin/pwd"));
            assert!(literal_inspection(&dir, cwd, "/bin/cat source"));
        }
        for cmd in [
            "pwd",
            "/bin/cat ../outside",
            "pwd; echo x",
            "python script.py",
            "/bin/cat $(pwd)",
        ] {
            assert!(!literal_inspection(&dir, cwd, cmd));
        }
        assert!(!literal_inspection(&dir, parent.to_str().unwrap(), "pwd"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&parent, dir.join("escape")).unwrap();
            assert!(!change_in(&dir, "escape/new"));
        }
        #[cfg(unix)]
        {
            let mut acp = serde_json::json!({"toolCall":{"kind":"execute","rawInput":{"cwd":cwd,"command":"/bin/pwd"}},
                "options":[{"optionId":"once","kind":"allow_once"}]});
            assert_eq!(acp_decision(&dir, &acp), ApprovalDecision::Accept);
            acp["toolCall"]["rawInput"]["command"] = "pwd; exit".into();
            assert_eq!(acp_decision(&dir, &acp), ApprovalDecision::Decline);
            acp["toolCall"]["rawInput"]["command"] = "/bin/pwd".into();
            acp["options"][0]["kind"] = "allow_always".into();
            assert_eq!(acp_decision(&dir, &acp), ApprovalDecision::Decline);
        }
        let req = ApprovalRequest {
            thread_id: "s".into(),
            turn_id: "t".into(),
            item_id: "i".into(),
            request_id: 1.into(),
            kind: ApprovalKind::FileChange {
                reason: None,
                grant_root: Some(cwd.into()),
            },
        };
        for (dest, expected) in [
            ("new", ApprovalDecision::AcceptForSession),
            ("../outside", ApprovalDecision::Decline),
        ] {
            let mut transcript = Transcript::new();
            transcript.apply(&AgentEvent::ItemStarted {
                thread_id: "s".into(),
                turn_id: "t".into(),
                item: Item::FileChange {
                    id: "i".into(),
                    status: ItemStatus::InProgress,
                    changes: vec![FileChange {
                        path: "source".into(),
                        kind: ChangeKind::Update {
                            move_to: Some(dest.into()),
                        },
                        diff: String::new(),
                    }],
                },
            });
            assert_eq!(
                decision(
                    &dir,
                    &req,
                    &serde_json::json!({"grantRoot": cwd}),
                    &transcript
                ),
                expected
            );
            assert_eq!(
                decision(
                    &dir,
                    &req,
                    &serde_json::json!({"additionalPermissions": {}}),
                    &transcript
                ),
                ApprovalDecision::Decline
            );
        }
        std::fs::remove_dir_all(parent).unwrap();
    }
}
