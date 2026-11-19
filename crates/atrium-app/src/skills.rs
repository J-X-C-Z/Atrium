//! Skills across agents: the ones installed for Claude Code
//! (`~/.claude/skills/<name>/SKILL.md`) and for Codex
//! (`~/.codex/skills/<name>/SKILL.md`), and copying one to the other so every
//! agent has the same.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

/// The agents whose skill folders Atrium manages, with their folder under home.
pub const AGENTS: [(&str, &str); 2] = [("Claude Code", ".claude"), ("Codex", ".codex")];

/// One skill and which agents have it.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Per entry of [`AGENTS`].
    pub installed: [bool; 2],
}

fn skills_dir(home: &Path, agent: usize) -> PathBuf {
    home.join(AGENTS[agent].1).join("skills")
}

/// The `description:` of a SKILL.md's front matter, first sentence.
fn description(text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("description:"))
        .map(|d| d.trim().trim_matches('"').to_string())
        .map(|d| match d.find(". ") {
            Some(i) => d[..=i].to_string(),
            None => d,
        })
        .unwrap_or_default()
}

/// Every skill any agent has, by name.
pub fn list(home: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for agent in 0..AGENTS.len() {
        let Ok(entries) = fs::read_dir(skills_dir(home, agent)) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.path().join("SKILL.md");
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            match skills.iter_mut().find(|s| s.name == name) {
                Some(skill) => skill.installed[agent] = true,
                None => {
                    let mut installed = [false; 2];
                    installed[agent] = true;
                    skills.push(Skill {
                        name,
                        description: description(&text),
                        installed,
                    });
                }
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Copies skill `name` to every agent that lacks it, from one that has it.
pub fn share(home: &Path, name: &str) -> Result<usize> {
    let from = (0..AGENTS.len())
        .map(|a| skills_dir(home, a).join(name))
        .find(|dir| dir.join("SKILL.md").is_file())
        .with_context(|| format!("no agent has the skill {name}"))?;
    let mut copied = 0;
    for agent in 0..AGENTS.len() {
        let to = skills_dir(home, agent).join(name);
        if to.join("SKILL.md").is_file() {
            continue;
        }
        copy_dir(&from, &to)?;
        copied += 1;
    }
    Ok(copied)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(from)?.flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)
                .with_context(|| format!("copying to {}", target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_are_listed_per_agent_and_shared() {
        let home = std::env::temp_dir().join(format!("atrium-skills-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let review = home.join(".claude/skills/review");
        fs::create_dir_all(review.join("scripts")).unwrap();
        fs::write(
            review.join("SKILL.md"),
            "---\nname: review\ndescription: Review a diff. Use when asked.\n---\n",
        )
        .unwrap();
        fs::write(review.join("scripts/run.sh"), "echo").unwrap();
        fs::create_dir_all(home.join(".codex/skills/notes")).unwrap();
        fs::write(
            home.join(".codex/skills/notes/SKILL.md"),
            "description: Notes",
        )
        .unwrap();

        let skills = list(&home);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[1].name, "review");
        assert_eq!(skills[1].description, "Review a diff.");
        assert_eq!(skills[1].installed, [true, false]);

        assert_eq!(share(&home, "review").unwrap(), 1);
        assert!(home.join(".codex/skills/review/scripts/run.sh").is_file());
        assert_eq!(list(&home)[1].installed, [true, true]);
        assert_eq!(share(&home, "review").unwrap(), 0);
        let _ = fs::remove_dir_all(&home);
    }
}
