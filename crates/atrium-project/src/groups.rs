//! Groups for notices: a notice to `@leads`, `@<lead-id>` or a group the
//! controller made reaches every member except the sender.
//!
//! The automatic groups need no setup: `leads` (the controller and the
//! leads) and one per lead, named by its id (that lead and its workers).
//! Retired streams are not members. Custom groups live in
//! `.atrium/groups.json` as `{"<name>": ["id", ...]}`.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};

use crate::{Project, ProjectNotice, Role, atomic_write};

/// Every group of a project with its current members, automatic first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Groups(pub Vec<(String, Vec<String>)>);

/// Reads `groups.json`; a missing or broken file is no groups.
pub(crate) fn read_custom(path: &Path) -> BTreeMap<String, Vec<String>> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Whether `text` mentions `@id` as a whole word.
pub fn mentions(text: &str, id: &str) -> bool {
    let needle = format!("@{id}");
    text.match_indices(&needle).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + needle.len()..].chars().next();
        let word = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

impl Groups {
    pub fn members(&self, name: &str) -> Option<&[String]> {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, m)| m.as_slice())
    }

    /// The groups `stream` belongs to.
    pub fn of<'a>(&'a self, stream: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |(_, m)| m.iter().any(|s| s == stream))
            .map(|(n, _)| n.as_str())
    }

    /// Whether `notice` reaches `stream`: addressed to it, to a group it is
    /// in, or (with `with_all`) to everyone; never its own.
    pub fn reaches(&self, notice: &ProjectNotice, stream: &str, with_all: bool) -> bool {
        notice.from != stream
            && notice.to.iter().any(|to| match to.strip_prefix('@') {
                Some(group) => self
                    .members(group)
                    .is_some_and(|m| m.iter().any(|s| s == stream)),
                None => to == stream || (with_all && to == "all"),
            })
    }
}

impl Project {
    /// All groups: `leads`, one per lead, then the controller's.
    pub fn groups(&self) -> Groups {
        let live = |s: &&crate::Workstream| !s.retired;
        let streams = &self.file.streams;
        let mut leads: Vec<String> = streams
            .iter()
            .filter(live)
            .filter(|s| s.role == Role::Controller)
            .map(|s| s.id.clone())
            .collect();
        leads.extend(
            streams
                .iter()
                .filter(live)
                .filter(|s| s.role == Role::Lead)
                .map(|s| s.id.clone()),
        );
        let mut out = vec![("leads".to_string(), leads)];
        for lead in streams.iter().filter(live).filter(|s| s.role == Role::Lead) {
            let mut members = vec![lead.id.clone()];
            members.extend(
                streams
                    .iter()
                    .filter(live)
                    .filter(|s| s.role == Role::Worker && s.parent.as_deref() == Some(&lead.id))
                    .map(|s| s.id.clone()),
            );
            out.push((lead.id.clone(), members));
        }
        for (name, members) in &self.custom_groups {
            if out.iter().all(|(n, _)| n != name) {
                let members = members
                    .iter()
                    .filter(|m| self.stream(m).is_some_and(|s| !s.retired))
                    .cloned()
                    .collect();
                out.push((name.clone(), members));
            }
        }
        Groups(out)
    }

    /// Streams `notice` reaches, in project order.
    pub fn recipients(&self, notice: &ProjectNotice) -> Vec<String> {
        let groups = self.groups();
        self.file
            .streams
            .iter()
            .filter(|s| groups.reaches(notice, &s.id, true))
            .map(|s| s.id.clone())
            .collect()
    }

    /// [`ProjectNotice::is_for`], with groups.
    pub fn notice_is_for(&self, notice: &ProjectNotice, stream: &str) -> bool {
        self.groups().reaches(notice, stream, true)
    }

    /// Whether `stream` is addressed directly: by its id, or by `@<id>` in
    /// the text of a notice that reaches it. Only these wake an idle stream;
    /// a plain group message waits for its next message.
    pub fn notice_wakes(&self, notice: &ProjectNotice, stream: &str) -> bool {
        notice.from != stream
            && (notice.to.iter().any(|to| to == stream)
                || (self.notice_is_for(notice, stream) && mentions(&notice.text, stream)))
    }

    /// The groups `me` is in, one line each, with members.
    pub fn groups_text(&self, me: &str) -> String {
        let groups = self.groups();
        let out: String = groups
            .0
            .iter()
            .filter(|(_, m)| m.iter().any(|s| s == me))
            .map(|(n, m)| format!("- `@{n}`: {}\n", m.join(", ")))
            .collect();
        if out.is_empty() {
            "You are in no group.".into()
        } else {
            out
        }
    }

    /// The kickoff line naming `me`'s groups, with its blank line; empty
    /// when it has none.
    pub(crate) fn groups_line(&self, me: &str) -> String {
        let groups = self.groups();
        let names: Vec<String> = groups.of(me).map(|n| format!("`@{n}`")).collect();
        if names.is_empty() {
            String::new()
        } else {
            format!(
                "Your groups: {}. Post there what others in the group should align on.\n\n",
                names.join(", ")
            )
        }
    }

    /// Every notice addressed to `@group`, for a member to catch up.
    pub fn group_history(&self, me: &str, group: &str) -> Result<String> {
        self.me(me)?;
        let group = group.trim().trim_start_matches('@');
        let groups = self.groups();
        let Some(members) = groups.members(group) else {
            bail!("No group `@{group}`. Yours:\n{}", self.groups_text(me));
        };
        if !members.iter().any(|m| m == me) {
            bail!("You are not in `@{group}`.");
        }
        let tag = format!("@{group}");
        let notices: Vec<&ProjectNotice> = self
            .notices
            .iter()
            .filter(|n| n.to.contains(&tag))
            .collect();
        if notices.is_empty() {
            return Ok(format!("No notices to `@{group}` yet."));
        }
        Ok(notices.iter().map(|n| n.line() + "\n").collect())
    }

    /// Makes or replaces a custom group (the controller only).
    pub fn create_group(&mut self, me: &str, name: &str, members: &[String]) -> Result<String> {
        if self.me(me)?.role != Role::Controller {
            bail!("Only the controller makes groups.");
        }
        let name = name.trim().trim_start_matches('@').to_lowercase();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("A group name is letters, digits, `-` or `_`.");
        }
        if matches!(name.as_str(), "all" | "leads" | crate::YOU | crate::ATRIUM)
            || self.stream(&name).is_some()
        {
            bail!("`{name}` is taken by a workstream or an automatic group; pick another name.");
        }
        let mut list: Vec<String> = Vec::new();
        for m in members.iter().map(|m| m.trim().trim_start_matches('@')) {
            if self.stream(m).is_none() {
                bail!("No workstream `{m}`.");
            }
            if !list.iter().any(|l| l == m) {
                list.push(m.to_string());
            }
        }
        if list.is_empty() {
            bail!("A group needs members.");
        }
        let mut next = self.custom_groups.clone();
        next.insert(name.clone(), list.clone());
        let path = self.dir().join("groups.json");
        atomic_write(&path, serde_json::to_string_pretty(&next)?.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        self.custom_groups = next;
        self.stamp = None;
        Ok(format!(
            "Group `@{name}` has {}. Post to it with `notify`.",
            list.join(", ")
        ))
    }
}

#[cfg(test)]
#[path = "groups_test.rs"]
mod tests;
