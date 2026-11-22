//! Skills: how to do something, written down once a way that works is
//! found. Each is a folder with a `SKILL.md` that starts with front matter
//! (`name`, `description`), the format Claude Code, Codex and agentskills.io
//! share. Agents see only the index; they read a skill's file when it
//! applies.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::store::{Scope, Store};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// Global, or the project it belongs to.
    pub scope: Scope,
}

impl Store {
    pub fn skills_dir(&self, scope: &Scope) -> PathBuf {
        match scope {
            Scope::Global => self.root().join("skills"),
            Scope::Project(_) => self.dir(scope).join("skills"),
        }
    }

    /// The skills that apply in `project`: its own first, then the global
    /// ones it does not shadow.
    pub fn skills(&self, project: Option<&str>) -> Vec<Skill> {
        let mut out = Vec::new();
        if let Some(key) = project {
            out.extend(read_dir(
                &self.skills_dir(&Scope::Project(key.into())),
                Scope::Project(key.into()),
            ));
        }
        for skill in read_dir(&self.skills_dir(&Scope::Global), Scope::Global) {
            if !out.iter().any(|s: &Skill| s.name == skill.name) {
                out.push(skill);
            }
        }
        out
    }
}

fn read_dir(dir: &Path, scope: Scope) -> Vec<Skill> {
    let mut out: Vec<Skill> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path().join("SKILL.md");
            let text = fs::read_to_string(&path).ok()?;
            let (name, description) = front_matter(&text);
            Some(Skill {
                name: name.unwrap_or_else(|| e.file_name().to_string_lossy().to_string()),
                description: description.unwrap_or_default(),
                path,
                scope: scope.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `name` and `description` from a `---` block at the top.
pub fn front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
            match key.trim() {
                "name" => name = Some(value),
                "description" => description = Some(value),
                _ => {}
            }
        }
    }
    (name, description)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, description: &str) {
        let d = dir.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"{description}\"\n---\n\n# {name}\n"),
        )
        .unwrap();
    }

    #[test]
    fn a_project_skill_shadows_a_global_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        write(
            &store.skills_dir(&Scope::Global),
            "release",
            "Cut a release",
        );
        write(
            &store.skills_dir(&Scope::Global),
            "triage",
            "Sort new issues",
        );
        write(
            &store.skills_dir(&Scope::Project("app-1".into())),
            "release",
            "This app's release",
        );
        let skills = store.skills(Some("app-1"));
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].description, "This app's release");
        assert_eq!(skills[1].name, "triage");
        assert_eq!(store.skills(None)[0].description, "Cut a release");
    }
}
