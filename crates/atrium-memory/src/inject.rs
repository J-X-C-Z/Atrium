//! What goes into an agent's session: the entries in use, what Hindsight
//! recalled for this conversation and the skill index, within a token
//! budget. It is made once, when the session starts, so later turns reuse
//! the agent's prompt cache.

use std::path::Path;

use crate::{
    Config, estimate_tokens, project_key, project_root,
    skills::Skill,
    store::{Scope, Store, Target},
};

/// The block to put in front of a session, and what it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Injection {
    pub text: String,
    pub tokens: usize,
    /// Entries, recalled memories and skills that fit.
    pub entries: usize,
    pub recalled: usize,
    pub skills: usize,
    /// Pieces left out to stay within the budget.
    pub dropped: usize,
}

impl Injection {
    pub fn is_empty(&self) -> bool {
        self.entries + self.recalled + self.skills == 0
    }
}

/// A part of the block, in the order parts are given room.
struct Part {
    heading: String,
    lead: Option<&'static str>,
    items: Vec<String>,
}

/// The block for a session in `cwd`, with `recalled` from Hindsight.
/// Room goes first to what the user is like, then to this project, then to
/// general notes, then to recalled memories, then to skills; within a part,
/// pinned and earlier entries first.
pub fn injection(
    store: &Store,
    config: &Config,
    cwd: Option<&Path>,
    recalled: &[String],
) -> Injection {
    let project = cwd.map(project_root);
    let key = project.as_deref().map(project_key);
    let project_scope = key.clone().map(Scope::Project);
    let texts = |scope: &Scope, target: Target| -> Vec<String> {
        store
            .active(scope, target)
            .into_iter()
            .map(|e| e.text)
            .collect()
    };
    let mut parts = vec![Part {
        heading: "About the user".into(),
        lead: None,
        items: texts(&Scope::Global, Target::User),
    }];
    if let (Some(scope), Some(root)) = (&project_scope, &project) {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut items = texts(scope, Target::Notes);
        items.extend(texts(scope, Target::User));
        parts.push(Part {
            heading: format!("This project ({name})"),
            lead: None,
            items,
        });
    }
    parts.push(Part {
        heading: "Notes".into(),
        lead: None,
        items: texts(&Scope::Global, Target::Notes),
    });
    parts.push(Part {
        heading: "Recalled for this conversation".into(),
        lead: None,
        items: recalled.to_vec(),
    });
    let skills: Vec<Skill> = store.skills(key.as_deref());
    parts.push(Part {
        heading: "Skills".into(),
        lead: Some("When one applies, read its file first and follow it."),
        items: skills
            .iter()
            .map(|s| format!("{} — {} ({})", s.name, s.description, s.path.display()))
            .collect(),
    });

    const HEAD: &str = "# Memory (from Atrium)\n\nWhat you and other agents learned in earlier sessions with this user. It is background: the user's messages come first, and anything here may be out of date.\n";
    let mut used = estimate_tokens(HEAD);
    let mut out = Injection::default();
    let mut body = String::new();
    for (ix, part) in parts.iter().enumerate() {
        let mut kept = Vec::new();
        let header = format!(
            "\n## {}\n{}",
            part.heading,
            part.lead.map(|l| format!("{l}\n")).unwrap_or_default()
        );
        let mut header_cost = estimate_tokens(&header);
        for item in &part.items {
            let line = bullet(item);
            let cost = estimate_tokens(&line) + header_cost;
            if used + cost > config.budget_tokens {
                out.dropped += 1;
                continue;
            }
            used += cost;
            header_cost = 0;
            kept.push(line);
        }
        if kept.is_empty() {
            continue;
        }
        match ix {
            _ if part.heading == "Skills" => out.skills += kept.len(),
            _ if part.heading.starts_with("Recalled") => out.recalled += kept.len(),
            _ => out.entries += kept.len(),
        }
        body.push_str(&header);
        for line in kept {
            body.push_str(&line);
        }
    }
    if out.is_empty() {
        return out;
    }
    out.text = format!("{HEAD}{body}");
    out.tokens = estimate_tokens(&out.text);
    out
}

fn bullet(text: &str) -> String {
    let mut out = String::new();
    for (n, line) in text.lines().enumerate() {
        out.push_str(if n == 0 { "- " } else { "  " });
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The first prompt of an agent that takes no instructions of its own
/// (ACP agents): the memory block, then the user's message.
pub fn with_memory(block: &str, message: &str) -> String {
    if block.is_empty() {
        return message.to_string();
    }
    format!("<atrium-memory>\n{block}</atrium-memory>\n\n{message}")
}

/// The user's message from a first prompt made by [`with_memory`], to show
/// it as they wrote it.
pub fn without_memory(prompt: &str) -> &str {
    match prompt.strip_prefix("<atrium-memory>\n") {
        Some(rest) => rest
            .split_once("</atrium-memory>\n\n")
            .map(|(_, message)| message)
            .unwrap_or(prompt),
        None => prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Landing, Op};

    fn add(store: &Store, scope: &Scope, target: Target, text: &str) {
        store
            .apply(
                scope,
                &[Op::Add {
                    content: text.into(),
                    target,
                }],
                "user",
                Landing::Active,
            )
            .unwrap();
    }

    #[test]
    fn the_block_holds_the_user_the_project_and_the_skills() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("memory"));
        let project = dir.path().join("app");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let key = project_key(&project_root(&project));
        add(
            &store,
            &Scope::Global,
            Target::User,
            "Prefers short answers in Chinese",
        );
        add(
            &store,
            &Scope::Project(key.clone()),
            Target::Notes,
            "Run tests with cargo test --workspace",
        );
        add(
            &store,
            &Scope::Project("other-1".into()),
            Target::Notes,
            "Not this one",
        );
        let skill = store.skills_dir(&Scope::Global).join("release");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: release\ndescription: Cut a release\n---\n",
        )
        .unwrap();

        let block = injection(
            &store,
            &Config::default(),
            Some(&project),
            &["Fixed the linker error last week".into()],
        );
        assert_eq!((block.entries, block.recalled, block.skills), (2, 1, 1));
        assert!(
            block
                .text
                .contains("## About the user\n- Prefers short answers")
        );
        assert!(block.text.contains("## This project (app)\n- Run tests"));
        assert!(block.text.contains("release — Cut a release"));
        assert!(!block.text.contains("Not this one"));
        assert!(block.tokens < 200);
    }

    #[test]
    fn the_budget_drops_the_least_important_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        add(&store, &Scope::Global, Target::User, "Writes Rust");
        let recalled: Vec<String> = (0..50)
            .map(|n| format!("recalled fact number {n} with some words"))
            .collect();
        let config = Config {
            budget_tokens: 120,
            ..Default::default()
        };
        let block = injection(&store, &config, None, &recalled);
        assert_eq!(block.entries, 1);
        assert!(block.recalled < 50 && block.dropped > 0);
        assert!(block.tokens <= 130, "{}", block.tokens);
    }

    #[test]
    fn nothing_to_say_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let block = injection(&Store::new(dir.path()), &Config::default(), None, &[]);
        assert!(block.is_empty() && block.text.is_empty());
    }

    #[test]
    fn the_first_prompt_carries_the_block_and_gives_the_message_back() {
        let prompt = with_memory("# Memory\n- a\n", "Fix the build");
        assert!(prompt.starts_with("<atrium-memory>"));
        assert_eq!(without_memory(&prompt), "Fix the build");
        assert_eq!(without_memory("plain"), "plain");
        assert_eq!(with_memory("", "plain"), "plain");
    }
}
