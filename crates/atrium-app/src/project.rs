//! Projects: a folder worked on by several long-running sessions
//! ("workstreams"), each with its own goal and scope, aligning through shared
//! files in the folder's `.atrium/` — the atrium.
//!
//! Everything lives in plain files any agent can read and write with its own
//! tools, so no agent needs to know about Atrium:
//!
//! ```text
//! .atrium/project.json      the workstreams (Atrium's own bookkeeping)
//! .atrium/brief.md          shared conventions and decisions
//! .atrium/contracts/*.md    interfaces between workstreams
//! .atrium/status/<id>.md    each workstream's own status
//! .atrium/notices.md        append-only notices: "- <time> · <from> → <to>: <text>"
//! ```

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub const DIR: &str = ".atrium";
/// Who signs notices Atrium posts itself, such as scope warnings.
pub const ATRIUM: &str = "atrium";
/// Who signs notices the user posts.
pub const YOU: &str = "you";

/// One long-running session of a project.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workstream {
    /// Short and file-safe: names its status file and addresses notices.
    pub id: String,
    pub name: String,
    /// `codex`, `claude`, `deepseek`, `antigravity`.
    pub backend: String,
    /// The session, once started.
    pub thread_id: Option<String>,
    /// What it keeps working toward.
    pub goal: String,
    /// Paths it owns, as globs relative to the project root (`web/**`).
    pub scope: Vec<String>,
    /// How many notices it has been handed.
    pub notices_seen: usize,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectFile {
    pub name: String,
    pub streams: Vec<Workstream>,
}

/// A line of `.atrium/notices.md`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectNotice {
    pub at: String,
    pub from: String,
    /// Workstream ids, or `all`.
    pub to: Vec<String>,
    pub text: String,
}

impl ProjectNotice {
    /// Whether `stream` should get it: addressed to it or to all, and not its own.
    pub fn is_for(&self, stream: &str) -> bool {
        self.from != stream && self.to.iter().any(|to| to == stream || to == "all")
    }

    pub fn line(&self) -> String {
        format!(
            "- {} · {} → {}: {}",
            self.at,
            self.from,
            self.to.join(", "),
            self.text.replace('\n', "\n  ")
        )
    }
}

/// A change one workstream made inside another's scope.
#[derive(Clone, Debug, PartialEq)]
pub struct Crossing {
    pub stream: String,
    pub owner: String,
    pub path: String,
}

/// An open project: its bookkeeping plus the atrium as last read.
#[derive(Clone, Debug, Default)]
pub struct Project {
    pub root: PathBuf,
    pub file: ProjectFile,
    pub brief: String,
    /// `(file name, text)`, sorted by name.
    pub contracts: Vec<(String, String)>,
    /// Status file text by workstream id.
    pub statuses: HashMap<String, String>,
    pub notices: Vec<ProjectNotice>,
    /// Scope crossings seen this session.
    pub crossings: Vec<Crossing>,
    /// The newest change to the atrium's files, to reload only when needed.
    pub stamp: Option<SystemTime>,
}

const BRIEF: &str = "# Brief\n\nShared conventions and decisions every workstream follows. Keep it short.\n\n## Architecture\n\n## Conventions\n\n## Decisions\n";
const CONTRACTS: &str = "# Contracts\n\nOne file per interface between workstreams (an API, a schema, an event).\nThe workstream that changes an interface updates its file and posts a notice.\n";

impl Project {
    pub fn dir(&self) -> PathBuf {
        self.root.join(DIR)
    }

    /// Opens the project in `root`, creating the atrium the first time.
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join(DIR);
        fs::create_dir_all(dir.join("contracts"))
            .and_then(|()| fs::create_dir_all(dir.join("status")))
            .with_context(|| format!("could not create {}", dir.display()))?;
        let write_new = |path: PathBuf, text: &str| -> Result<()> {
            if !path.exists() {
                fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            }
            Ok(())
        };
        write_new(dir.join("brief.md"), BRIEF)?;
        write_new(dir.join("contracts").join("README.md"), CONTRACTS)?;
        write_new(dir.join("notices.md"), "# Notices\n\n")?;
        let file = match fs::read_to_string(dir.join("project.json")) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("reading {}", dir.join("project.json").display()))?,
            Err(_) => ProjectFile {
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Project".into()),
                streams: Vec::new(),
            },
        };
        let mut project = Self {
            root: root.to_path_buf(),
            file,
            ..Self::default()
        };
        project.save()?;
        project.reload();
        Ok(project)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.dir().join("project.json");
        let text = serde_json::to_string_pretty(&self.file)?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// When the atrium's files last changed.
    pub fn latest_change(&self) -> Option<SystemTime> {
        let dir = self.dir();
        let mut paths = vec![dir.join("brief.md"), dir.join("notices.md")];
        for sub in ["contracts", "status"] {
            paths.push(dir.join(sub));
            if let Ok(entries) = fs::read_dir(dir.join(sub)) {
                paths.extend(entries.flatten().map(|e| e.path()));
            }
        }
        paths
            .iter()
            .filter_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
            .max()
    }

    /// Reads the atrium again. Returns whether anything changed on disk.
    pub fn reload(&mut self) -> bool {
        let stamp = self.latest_change();
        if stamp.is_some() && stamp == self.stamp {
            return false;
        }
        self.stamp = stamp;
        let dir = self.dir();
        self.brief = fs::read_to_string(dir.join("brief.md")).unwrap_or_default();
        self.notices =
            parse_notices(&fs::read_to_string(dir.join("notices.md")).unwrap_or_default());
        self.contracts = read_dir_texts(&dir.join("contracts"));
        self.statuses = read_dir_texts(&dir.join("status"))
            .into_iter()
            .map(|(name, text)| (name.trim_end_matches(".md").to_string(), text))
            .collect();
        true
    }

    /// Appends a notice and reads it back.
    pub fn post(&mut self, from: &str, to: Vec<String>, text: &str) -> Result<()> {
        let notice = ProjectNotice {
            at: now_label(),
            from: from.to_string(),
            to,
            text: text.trim().to_string(),
        };
        let path = self.dir().join("notices.md");
        let mut body = fs::read_to_string(&path).unwrap_or_default();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&notice.line());
        body.push('\n');
        fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        self.stamp = None;
        self.reload();
        Ok(())
    }

    pub fn stream(&self, id: &str) -> Option<&Workstream> {
        self.file.streams.iter().find(|s| s.id == id)
    }

    pub fn stream_mut(&mut self, id: &str) -> Option<&mut Workstream> {
        self.file.streams.iter_mut().find(|s| s.id == id)
    }

    /// A new workstream id from its name, unique in the project.
    pub fn new_id(&self, name: &str) -> String {
        let base: String = name
            .trim()
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let base = if base.is_empty() || base == "all" || base == ATRIUM || base == YOU {
            format!("stream-{}", self.file.streams.len() + 1)
        } else {
            base
        };
        let mut id = base.clone();
        let mut n = 2;
        while self.stream(&id).is_some() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// Notices `stream` has not been handed yet.
    pub fn unread(&self, stream: &str) -> Vec<&ProjectNotice> {
        let seen = self.stream(stream).map_or(0, |s| s.notices_seen);
        self.notices
            .iter()
            .skip(seen)
            .filter(|n| n.is_for(stream))
            .collect()
    }

    /// Marks every notice so far as handed to `stream`.
    pub fn mark_read(&mut self, stream: &str) {
        let count = self.notices.len();
        if let Some(s) = self.stream_mut(stream) {
            s.notices_seen = count;
        }
    }

    /// Who owns `path` (absolute, or relative to the root) when it is not
    /// `stream` itself: a scope crossing.
    pub fn crossing(&self, stream: &str, path: &str) -> Option<Crossing> {
        let relative = Path::new(path)
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.trim_start_matches("./").to_string());
        let own = self.stream(stream)?;
        if own.scope.iter().any(|glob| glob_match(glob, &relative)) {
            return None;
        }
        let owner =
            self.file.streams.iter().find(|s| {
                s.id != stream && s.scope.iter().any(|glob| glob_match(glob, &relative))
            })?;
        Some(Crossing {
            stream: stream.to_string(),
            owner: owner.id.clone(),
            path: relative,
        })
    }

    /// What a workstream is told when it starts.
    pub fn kickoff(&self, stream: &Workstream) -> String {
        let others: Vec<String> = self
            .file
            .streams
            .iter()
            .filter(|s| s.id != stream.id)
            .map(|s| {
                format!(
                    "- {} (`{}`): {} — scope {}",
                    s.name,
                    s.id,
                    first_line(&s.goal),
                    scope_label(&s.scope)
                )
            })
            .collect();
        let others = if others.is_empty() {
            "None yet; others may join later.".to_string()
        } else {
            others.join("\n")
        };
        format!(
            "You are the \"{name}\" workstream (id `{id}`) of the project \"{project}\": one of several long-lived sessions working on this repository side by side, each toward its own goal.\n\n\
             Your goal:\n{goal}\n\n\
             Your scope: {scope}. Change files outside it only when you must, and tell their owner with a notice.\n\n\
             The other workstreams:\n{others}\n\n\
             The atrium, `{dir}/` in the repository root, is how workstreams stay aligned:\n\
             - `{dir}/brief.md`: shared conventions and decisions. Read it first and follow it.\n\
             - `{dir}/contracts/`: interfaces between workstreams. Read the ones you depend on; if you change one, update its file and post a notice.\n\
             - `{dir}/status/{id}.md`: your status. Keep it current with the headings Focus, Done, Next, Touching (files you are changing) and Questions.\n\
             - `{dir}/notices.md`: append-only. To tell others something, append one line: `- YYYY-MM-DD HH:MM · {id} → <workstream id or all>: <message>`.\n\
             New notices for you are added to your next message, so there is no need to check for them.\n\n\
             Read the brief and the contracts, write your status file, then work toward your goal on your own. Stop to ask only when a decision really needs me.",
            name = stream.name,
            id = stream.id,
            project = self.file.name,
            goal = stream.goal.trim(),
            scope = scope_label(&stream.scope),
            dir = DIR,
        )
    }
}

/// `text` with the notices `stream` has not seen in front of it.
pub fn with_notices(notices: &[&ProjectNotice], text: &str) -> String {
    if notices.is_empty() {
        return text.to_string();
    }
    let lines: Vec<String> = notices
        .iter()
        .map(|n| format!("- {} · from {}: {}", n.at, n.from, n.text))
        .collect();
    format!(
        "[Atrium] New notices for you:\n{}\n\n{}",
        lines.join("\n"),
        text
    )
}

pub fn scope_label(scope: &[String]) -> String {
    if scope.is_empty() {
        "the whole repository".into()
    } else {
        scope
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn now_label() -> String {
    let now = jiff::Zoned::now();
    now.strftime("%Y-%m-%d %H:%M").to_string()
}

fn read_dir_texts(dir: &Path) -> Vec<(String, String)> {
    let mut texts: Vec<(String, String)> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                .filter_map(|e| {
                    let text = fs::read_to_string(e.path()).ok()?;
                    Some((e.file_name().to_string_lossy().into_owned(), text))
                })
                .collect()
        })
        .unwrap_or_default();
    texts.sort();
    texts
}

/// Reads notice lines, leniently: agents write them by hand. Indented lines
/// continue the notice above.
pub fn parse_notices(text: &str) -> Vec<ProjectNotice> {
    let mut notices: Vec<ProjectNotice> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            if let Some(notice) = parse_notice(rest) {
                notices.push(notice);
            }
        } else if (line.starts_with("  ") || line.starts_with('\t'))
            && let Some(last) = notices.last_mut()
        {
            last.text.push('\n');
            last.text.push_str(line.trim());
        }
    }
    notices
}

fn parse_notice(line: &str) -> Option<ProjectNotice> {
    let (at, rest) = line.split_once('·')?;
    let (route, text) = rest.split_once(':')?;
    let (from, to) = route.split_once('→').or_else(|| route.split_once("->"))?;
    let to: Vec<String> = to
        .split(',')
        .map(|t| t.trim().trim_matches('`').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let from = from.trim().trim_matches('`').to_lowercase();
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some(ProjectNotice {
        at: at.trim().to_string(),
        from,
        to,
        text: text.trim().to_string(),
    })
}

/// Matches a path against a glob: `*` within a segment, `**` across them,
/// `?` for one character. A pattern without wildcards also matches anything
/// under it, so `web` means `web/**`.
pub fn glob_match(glob: &str, path: &str) -> bool {
    let glob = glob.trim().trim_start_matches("./").trim_end_matches('/');
    let path = path.trim_start_matches("./");
    if glob.is_empty() {
        return false;
    }
    if !glob.contains(['*', '?']) {
        return path == glob || path.starts_with(&format!("{glob}/"));
    }
    let glob: Vec<&str> = glob.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&glob, &path)
}

fn match_segments(glob: &[&str], path: &[&str]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((segment, tail)) => match_one(first, segment) && match_segments(rest, tail),
            None => false,
        },
    }
}

fn match_one(glob: &str, text: &str) -> bool {
    let (g, t): (Vec<char>, Vec<char>) = (glob.chars().collect(), text.chars().collect());
    fn go(g: &[char], t: &[char]) -> bool {
        match g.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) => (0..=t.len()).any(|skip| go(rest, &t[skip..])),
            Some(('?', rest)) => !t.is_empty() && go(rest, &t[1..]),
            Some((c, rest)) => t.first() == Some(c) && go(rest, &t[1..]),
        }
    }
    go(&g, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("web/**", "web/src/app.tsx"));
        assert!(glob_match("web", "web/src/app.tsx"));
        assert!(!glob_match("web", "website/index.html"));
        assert!(glob_match("server/**/*.rs", "server/api/render.rs"));
        assert!(glob_match("server/**/*.rs", "server/main.rs"));
        assert!(!glob_match("server/**/*.rs", "server/README.md"));
        assert!(glob_match("*.md", "README.md"));
        assert!(!glob_match("*.md", "docs/README.md"));
    }

    #[test]
    fn notices_parse_leniently() {
        let text = "# Notices\n\n\
            - 2026-11-18 10:42 · backend → frontend: `/render` now returns `progress`\n  \
              (0 to 1, polled every second)\n\
            - 2026-11-18 11:00 · you -> all: use zod for validation\n\
            - just a note\n\
            * 2026-11-18 11:05 · Frontend → backend, core: thanks\n";
        let notices = parse_notices(text);
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[0].from, "backend");
        assert_eq!(notices[0].to, vec!["frontend"]);
        assert!(notices[0].text.ends_with("polled every second)"));
        assert_eq!(notices[1].to, vec!["all"]);
        assert_eq!(notices[2].to, vec!["backend", "core"]);
        assert!(notices[0].is_for("frontend") && !notices[0].is_for("backend"));
        assert!(notices[1].is_for("backend"));
    }

    fn stream(id: &str, scope: &[&str]) -> Workstream {
        Workstream {
            id: id.into(),
            name: id.into(),
            backend: "codex".into(),
            goal: format!("Build the {id}"),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_project_round_trips_and_tracks_notices_and_crossings() {
        let root = std::env::temp_dir().join(format!("atrium-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut project = Project::open(&root).unwrap();
        assert!(root.join(".atrium/brief.md").exists());
        project.file.streams.push(stream("frontend", &["web/**"]));
        project.file.streams.push(stream("backend", &["server/**"]));
        project.save().unwrap();
        assert_eq!(project.new_id("Frontend"), "frontend-2");
        assert_eq!(project.new_id("后端 优化"), "后端-优化");

        project
            .post(
                "backend",
                vec!["frontend".into()],
                "render returns progress",
            )
            .unwrap();
        project.post(YOU, vec!["all".into()], "use zod").unwrap();
        assert_eq!(project.unread("frontend").len(), 2);
        assert_eq!(project.unread("backend").len(), 1);
        project.mark_read("frontend");
        assert!(project.unread("frontend").is_empty());

        let crossing = project
            .crossing(
                "frontend",
                &root.join("server/api.rs").display().to_string(),
            )
            .unwrap();
        assert_eq!(crossing.owner, "backend");
        assert_eq!(crossing.path, "server/api.rs");
        assert!(project.crossing("frontend", "web/app.tsx").is_none());
        assert!(project.crossing("frontend", "README.md").is_none());

        let kickoff = project.kickoff(project.stream("frontend").unwrap());
        assert!(kickoff.contains("Build the frontend") && kickoff.contains("`backend`"));

        let reopened = Project::open(&root).unwrap();
        assert_eq!(reopened.file.streams.len(), 2);
        assert_eq!(
            reopened.file.streams[0].notices_seen, 0,
            "marks are saved by the caller"
        );
        assert_eq!(reopened.notices.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }
}
