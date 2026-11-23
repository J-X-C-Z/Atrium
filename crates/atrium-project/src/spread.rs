//! Projects spread over several computers.
//!
//! Every computer has an **Atrium Workspace** folder, wherever it likes
//! (`~/Atrium Workspace`, `D:\Atrium Workspace`). A project sits at the same
//! path inside it on every computer, such as `Atrium Workspace/photo-engine`,
//! but each computer keeps only the folders it works on. A folder is written
//! by one computer only: the one whose agent owns it (its workstream's
//! `scope`, with the device from its backend, `codex@win`). Others read it
//! over the network. The `.atrium/` files stay with the computer that runs
//! the project's scheduler.
//!
//! This module only computes: which computer owns a path, where a project
//! is on another computer, and what an agent is told about it. Starting
//! agents there and reaching files is the app's (see
//! `docs/spread-projects-implementation.md`).

use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

use crate::{Project, Workstream, glob_match, scope_label};

/// The folder's name, the same on every computer.
pub const WORKSPACE_DIR: &str = "Atrium Workspace";

/// The computer a backend runs on: `win` for `codex@win`, `None` for this one.
pub fn device_of(backend: &str) -> Option<&str> {
    backend.split_once('@').map(|(_, device)| device)
}

/// The project's path inside the workspace, with `/` between parts, when it
/// is inside it: `photo-engine` for `~/Atrium Workspace/photo-engine`.
pub fn relative(project_root: &Path, workspace: &Path) -> Option<String> {
    let rest = project_root.strip_prefix(workspace).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|c| match c {
            Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The workspace folder of a computer, from its node's roots: the root
/// named [`WORKSPACE_DIR`], else none (the user sets it in settings).
pub fn workspace_in_roots(roots: &[String]) -> Option<String> {
    roots
        .iter()
        .find(|r| {
            r.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(WORKSPACE_DIR))
        })
        .cloned()
}

/// `relative` (with `/`) joined onto a workspace folder of another
/// computer, with that computer's separator: a folder written with `\` or
/// a drive letter is Windows.
pub fn join_on(workspace: &str, relative: &str) -> String {
    let windows = workspace.contains('\\') || workspace.as_bytes().get(1) == Some(&b':');
    let sep = if windows { '\\' } else { '/' };
    let mut out = workspace.trim_end_matches(['/', '\\']).to_string();
    for part in relative.split('/').filter(|p| !p.is_empty()) {
        out.push(sep);
        out.push_str(part);
    }
    out
}

/// Which computer writes what, from the workstreams.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spread {
    /// Globs each computer writes, by device; `None` is the scheduler's.
    pub owners: BTreeMap<Option<String>, Vec<String>>,
}

impl Spread {
    pub fn of(project: &Project) -> Self {
        let mut owners: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
        for s in project.file.streams.iter().filter(|s| !s.retired) {
            let device = device_of(&s.backend).map(str::to_string);
            let globs = owners.entry(device).or_default();
            for glob in &s.scope {
                if !globs.contains(glob) {
                    globs.push(glob.clone());
                }
            }
        }
        Spread { owners }
    }

    /// Whether any workstream runs on another computer.
    pub fn is_spread(&self) -> bool {
        self.owners.keys().any(Option::is_some)
    }

    /// The computer that writes `path` (relative to the project, `/`
    /// separated): `Some(None)` for the scheduler's, `None` when no scope
    /// covers it. `.atrium/` is always the scheduler's.
    pub fn owner_of(&self, path: &str) -> Option<Option<&str>> {
        if path == crate::DIR || path.starts_with(&format!("{}/", crate::DIR)) {
            return Some(None);
        }
        self.owners
            .iter()
            .find(|(_, globs)| globs.iter().any(|g| glob_match(g, path)))
            .map(|(device, _)| device.as_deref())
    }

    /// Paths of a change that `stream` writes but another computer owns.
    pub fn foreign<'a>(&self, stream: &Workstream, paths: &'a [String]) -> Vec<&'a str> {
        let mine = device_of(&stream.backend);
        paths
            .iter()
            .map(String::as_str)
            .filter(|p| matches!(self.owner_of(p), Some(owner) if owner != mine))
            .collect()
    }
}

/// What an agent in a spread project is told about where it is and where
/// the rest lives. Empty when the project is not spread.
pub fn briefing(project: &Project, stream: &Workstream, scheduler: &str) -> String {
    let spread = Spread::of(project);
    if !spread.is_spread() {
        return String::new();
    }
    let here = device_of(&stream.backend).unwrap_or(scheduler);
    let mut lines = vec![format!(
        "## Computers\n\nThis project is spread over several computers, each with the \
project at the same path inside its `{WORKSPACE_DIR}` folder, each keeping only the \
folders it works on. You run on `{here}`. Write only your own scope ({scope}); a \
folder another computer owns may be missing or stale here.\n",
        scope = scope_label(&stream.scope),
    )];
    for (device, globs) in &spread.owners {
        let name = device.as_deref().unwrap_or(scheduler);
        lines.push(format!(
            "- `{name}`{mark} writes {}",
            scope_label(globs),
            mark = if name == here { " (you)" } else { "" }
        ));
    }
    lines.push(format!(
        "- `{scheduler}` keeps `{}/` (tasks, notices, status) for everyone.\n\n\
To use another computer's files, read or copy them with the `atrium` tools \
`device_list`, `device_read` and `device_copy`. To change them, ask their owner with a \
notice.",
        crate::DIR
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Role, Workstream};
    use std::path::PathBuf;

    fn s(id: &str, backend: &str, scope: &[&str]) -> Workstream {
        Workstream {
            id: id.into(),
            name: id.into(),
            backend: backend.into(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            role: Role::Lead,
            ..Default::default()
        }
    }

    fn project(streams: Vec<Workstream>) -> Project {
        let mut p = Project {
            root: PathBuf::from("/w/Atrium Workspace/photo"),
            ..Default::default()
        };
        p.file.streams = streams;
        p
    }

    #[test]
    fn paths_map_by_their_place_in_the_workspace() {
        let ws = Path::new("/home/me/Atrium Workspace");
        assert_eq!(
            relative(Path::new("/home/me/Atrium Workspace/photo/engine"), ws),
            Some("photo/engine".into())
        );
        assert_eq!(relative(Path::new("/home/me/code/photo"), ws), None);
        assert_eq!(relative(ws, ws), None);
        assert_eq!(
            join_on(r"D:\Atrium Workspace\", "photo/engine"),
            r"D:\Atrium Workspace\photo\engine"
        );
        assert_eq!(
            join_on("/Users/me/Atrium Workspace", "photo"),
            "/Users/me/Atrium Workspace/photo"
        );
        assert_eq!(
            workspace_in_roots(&["~/code".into(), r"D:\Atrium Workspace".into()]),
            Some(r"D:\Atrium Workspace".into())
        );
        assert_eq!(workspace_in_roots(&["~/code".into()]), None);
        assert_eq!(device_of("codex@win"), Some("win"));
        assert_eq!(device_of("claude"), None);
    }

    #[test]
    fn each_folder_has_one_writer() {
        let p = project(vec![
            s("ui", "claude", &["ui/**"]),
            s("algo", "codex@win", &["analysis/**"]),
            s("perf", "codex@win", &["perf/core/**", "perf/win/**"]),
            s("perf-mac", "codex@mac", &["perf/mac/**"]),
        ]);
        let spread = Spread::of(&p);
        assert!(spread.is_spread());
        assert_eq!(spread.owner_of("analysis/fit.py"), Some(Some("win")));
        assert_eq!(spread.owner_of("perf/mac/metal.rs"), Some(Some("mac")));
        assert_eq!(spread.owner_of("ui/app.tsx"), Some(None));
        assert_eq!(spread.owner_of(".atrium/tasks/x.md"), Some(None));
        assert_eq!(spread.owner_of("README.md"), None);
        let mac = &p.file.streams[3];
        let paths = vec![
            "perf/mac/a.rs".to_string(),
            "perf/core/b.rs".to_string(),
            "README.md".to_string(),
        ];
        assert_eq!(spread.foreign(mac, &paths), vec!["perf/core/b.rs"]);
    }

    #[test]
    fn agents_are_told_where_things_are() {
        let p = project(vec![
            s("ui", "claude", &["ui/**"]),
            s("perf-mac", "codex@mac", &["perf/mac/**"]),
        ]);
        let text = briefing(&p, &p.file.streams[1], "linux");
        assert!(text.contains("You run on `mac`"), "{text}");
        assert!(text.contains("- `mac` (you) writes"), "{text}");
        assert!(text.contains("- `linux` writes"), "{text}");
        assert!(text.contains("device_read"), "{text}");
        let alone = project(vec![s("ui", "claude", &["ui/**"])]);
        assert_eq!(briefing(&alone, &alone.file.streams[0], "linux"), "");
    }
}
