//! The `atrium` skill: how to work in a project, for agents that load skills
//! (Claude Code from `~/.claude/skills`, Codex from `~/.codex/skills`).

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

/// `skills/atrium/SKILL.md` in this repository.
pub const SKILL: &str = include_str!("../../../skills/atrium/SKILL.md");

/// Where the skill goes under `home`, one folder per agent.
pub fn targets(home: &Path) -> Vec<PathBuf> {
    [".claude", ".codex"]
        .iter()
        .map(|agent| {
            home.join(agent)
                .join("skills")
                .join("atrium")
                .join("SKILL.md")
        })
        .collect()
}

/// Writes the skill for every agent under `home`; returns the files written.
pub fn install(home: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for path in targets(home) {
        fs::create_dir_all(path.parent().expect("skill folder"))
            .with_context(|| format!("creating {}", path.display()))?;
        fs::write(&path, SKILL).with_context(|| format!("writing {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_names_itself_and_the_tools() {
        assert!(SKILL.starts_with("---\nname: atrium\ndescription: "));
        for tool in [
            "whoami",
            "create_task",
            "check_step",
            "finish_task",
            "notify",
        ] {
            assert!(SKILL.contains(&format!("`{tool}`")), "{tool}");
        }
        let home = std::env::temp_dir().join(format!("atrium-skill-{}", std::process::id()));
        let written = install(&home).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(fs::read_to_string(&written[0]).unwrap(), SKILL);
        let _ = fs::remove_dir_all(&home);
    }
}
