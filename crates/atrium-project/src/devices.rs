//! Reaching other computers' files from the `atrium` tools of a spread
//! project: `device_list`, `device_read` and `device_copy`.
//!
//! The logic (which paths are allowed, who may be written, how much may be
//! copied) lives here and goes through [`Nodes`], so it is tested without a
//! network. The app implements [`Nodes`] over the node of each computer; the
//! scheduler's own files are read from disk.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{
    DIR, Project, Workstream,
    mcp::Devices,
    spread::{self, Spread},
};

/// The most `device_copy` moves in one call.
pub const MAX_COPY_BYTES: u64 = 200 * 1024 * 1024;
/// The largest single file a node serves or takes.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// How much of a text file `device_read` returns.
const READ_CHARS: usize = 100_000;
/// Folders deeper than this, or with more entries in all, are not copied.
const MAX_DEPTH: usize = 24;
const MAX_ENTRIES: usize = 20_000;

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// What a path is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stat {
    pub exists: bool,
    pub dir: bool,
    pub size: u64,
}

/// The files of the computers of a project. Paths are absolute on the
/// computer asked; `device` is a device id. The scheduler itself is not
/// asked through this: its files are read from disk.
pub trait Nodes: Send + Sync {
    /// The scheduler's name for itself, as `device` may name it.
    fn scheduler(&self) -> String;
    /// This computer's Atrium Workspace folder.
    fn here_workspace(&self) -> Option<PathBuf>;
    /// A device's Atrium Workspace folder, as that computer writes it.
    fn workspace_of(&self, device: &str) -> Option<String>;
    fn list(&self, device: &str, path: &str) -> Result<Vec<Entry>>;
    fn stat(&self, device: &str, path: &str) -> Result<Stat>;
    fn read(&self, device: &str, path: &str) -> Result<Vec<u8>>;
    fn write(&self, device: &str, path: &str, bytes: &[u8]) -> Result<()>;
    fn mkdir(&self, device: &str, path: &str) -> Result<()>;
}

/// The `atrium` device tools over [`Nodes`].
pub struct Direct<N: Nodes>(pub N);

impl<N: Nodes> Devices for Direct<N> {
    fn call(&self, project: &Project, me: &str, name: &str, args: &Value) -> Result<String> {
        let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
        let stream = project
            .stream(me)
            .with_context(|| format!("No workstream {me}."))?;
        let ctx = Ctx {
            nodes: &self.0,
            root: &project.root,
            spread: Spread::of(project),
            stream,
        };
        match name {
            "device_list" => ctx.list(&text("device"), &text("path")),
            "device_read" => ctx.read(&text("device"), &text("path")),
            "device_copy" => ctx.copy(&text("device"), &text("path"), &text("to")),
            _ => bail!("No tool {name}."),
        }
    }
}

/// Whether `name` is one of the device tools.
pub fn is_device_tool(name: &str) -> bool {
    matches!(name, "device_list" | "device_read" | "device_copy")
}

/// A project-relative path, with `/`, that stays inside the project and out
/// of `.atrium/`. `""` is the project folder.
pub fn clean(path: &str) -> Result<String> {
    let path = path.trim().replace('\\', "/");
    if path.contains('\0') {
        bail!("Bad path.");
    }
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        bail!("`{path}` is not relative to the project: give a path such as `src/main.rs`.");
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("`{path}` leaves the project."),
            other => parts.push(other),
        }
    }
    if parts.first() == Some(&DIR) {
        bail!(
            "`{DIR}/` is not copied between computers: use the `atrium` tools for tasks, notices and status."
        );
    }
    Ok(parts.join("/"))
}

struct Ctx<'a, N: Nodes> {
    nodes: &'a N,
    root: &'a Path,
    spread: Spread,
    stream: &'a Workstream,
}

/// The ids compare equal when they only differ in case or punctuation.
pub fn same_device(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    !norm(a).is_empty() && norm(a) == norm(b)
}

/// Where a computer's files are read and written.
enum Place {
    /// The scheduler: the project folder here.
    Here,
    Device(String),
}

impl<N: Nodes> Ctx<'_, N> {
    fn place(&self, device: &str) -> Result<Place> {
        let device = device.trim();
        if device.is_empty() {
            bail!(
                "Say which computer: a device id, or `{}`.",
                self.nodes.scheduler()
            );
        }
        if same_device(device, &self.nodes.scheduler()) {
            return Ok(Place::Here);
        }
        let known = self
            .spread
            .owners
            .keys()
            .flatten()
            .any(|d| same_device(d, device));
        if !known {
            bail!(
                "No computer `{device}` in this project. Computers: {}.",
                self.computers()
            );
        }
        Ok(Place::Device(device.to_string()))
    }

    fn computers(&self) -> String {
        let mut names = vec![self.nodes.scheduler()];
        names.extend(self.spread.owners.keys().flatten().cloned());
        names.join(", ")
    }

    /// The absolute path of `rel` on a computer.
    fn absolute(&self, place: &Place, rel: &str) -> Result<String> {
        match place {
            Place::Here => Ok(self.root.join(rel).display().to_string()),
            Place::Device(device) => {
                let here = self
                    .nodes
                    .here_workspace()
                    .context("This computer has no Atrium Workspace folder.")?;
                let project = spread::relative(self.root, &here)
                    .context("This project is not inside this computer's Atrium Workspace.")?;
                let workspace = self
                    .nodes
                    .workspace_of(device)
                    .with_context(|| format!("{device} has no Atrium Workspace folder known."))?;
                let rel = if rel.is_empty() {
                    project
                } else {
                    format!("{project}/{rel}")
                };
                Ok(spread::join_on(&workspace, &rel))
            }
        }
    }

    fn stat(&self, place: &Place, abs: &str) -> Result<Stat> {
        match place {
            Place::Here => local_stat(self.root, abs),
            Place::Device(d) => self.nodes.stat(d, abs),
        }
    }

    fn entries(&self, place: &Place, abs: &str) -> Result<Vec<Entry>> {
        match place {
            Place::Here => local_list(self.root, abs),
            Place::Device(d) => self.nodes.list(d, abs),
        }
    }

    fn bytes(&self, place: &Place, abs: &str) -> Result<Vec<u8>> {
        match place {
            Place::Here => local_read(self.root, abs),
            Place::Device(d) => self.nodes.read(d, abs),
        }
    }

    fn list(&self, device: &str, path: &str) -> Result<String> {
        let rel = clean(path)?;
        let place = self.place(device)?;
        let abs = self.absolute(&place, &rel)?;
        let stat = self.stat(&place, &abs)?;
        if !stat.exists {
            bail!("`{}` does not exist on {device}.", shown(&rel));
        }
        if !stat.dir {
            return Ok(format!(
                "`{}` is a file of {} bytes.",
                shown(&rel),
                stat.size
            ));
        }
        let mut entries = self.entries(&place, &abs)?;
        entries.sort_by(|a, b| (!a.dir, &a.name).cmp(&(!b.dir, &b.name)));
        if entries.is_empty() {
            return Ok(format!("`{}` on {device} is empty.", shown(&rel)));
        }
        let mut out = format!("`{}` on {device}:\n", shown(&rel));
        for e in entries {
            if e.dir {
                out.push_str(&format!("- {}/\n", e.name));
            } else {
                out.push_str(&format!("- {} ({} bytes)\n", e.name, e.size));
            }
        }
        Ok(out)
    }

    fn read(&self, device: &str, path: &str) -> Result<String> {
        let rel = clean(path)?;
        if rel.is_empty() {
            bail!("Name a file; `device_list` shows a folder.");
        }
        let place = self.place(device)?;
        let abs = self.absolute(&place, &rel)?;
        let stat = self.stat(&place, &abs)?;
        if !stat.exists {
            bail!("`{rel}` does not exist on {device}.");
        }
        if stat.dir {
            bail!("`{rel}` is a folder: use `device_list`, or `device_copy` to copy it.");
        }
        if stat.size > MAX_FILE_BYTES {
            return Ok(format!(
                "`{rel}` is {} bytes, too large to read here; `device_copy` it if you need it.",
                stat.size
            ));
        }
        let bytes = self.bytes(&place, &abs)?;
        match std::str::from_utf8(&bytes) {
            Ok(text) if !bytes.contains(&0) => {
                let mut shown_text: String = text.chars().take(READ_CHARS).collect();
                if shown_text.len() < text.len() {
                    shown_text.push_str(&format!(
                        "\n\n[cut after {READ_CHARS} characters of {} bytes; `device_copy` it to read all]",
                        bytes.len()
                    ));
                }
                Ok(shown_text)
            }
            _ => Ok(format!(
                "`{rel}` is a binary file of {} bytes. `device_copy` it to use it.",
                bytes.len()
            )),
        }
    }

    fn copy(&self, device: &str, path: &str, to: &str) -> Result<String> {
        let rel = clean(path)?;
        let dest = clean(if to.trim().is_empty() { path } else { to })?;
        if rel.is_empty() || dest.is_empty() {
            bail!("Name what to copy and where; the whole project is not copied.");
        }
        let mine = spread::device_of(&self.stream.backend);
        match self.spread.owner_of(&dest) {
            Some(owner) if owner == mine => {}
            Some(owner) => bail!(
                "`{dest}` belongs to {}; you write only your own scope ({}). Copy into your scope.",
                owner.unwrap_or(&self.nodes.scheduler()),
                crate::scope_label(&self.stream.scope)
            ),
            None => bail!(
                "`{dest}` is in nobody's scope; copy into your own scope ({}).",
                crate::scope_label(&self.stream.scope)
            ),
        }
        let source = self.place(device)?;
        let target = match mine {
            Some(device) => Place::Device(device.to_string()),
            None => Place::Here,
        };
        if matches!((&source, &target), (Place::Here, Place::Here))
            || matches!((&source, &target), (Place::Device(a), Place::Device(b)) if same_device(a, b))
        {
            bail!("That is your own computer: its files are already there.");
        }
        let from = self.absolute(&source, &rel)?;
        let into = self.absolute(&target, &dest)?;
        let stat = self.stat(&source, &from)?;
        if !stat.exists {
            bail!("`{rel}` does not exist on {device}.");
        }
        // Count first, so a too-large copy moves nothing.
        let mut files = Vec::new();
        let mut folders = vec![String::new()];
        if stat.dir {
            self.walk(&source, &from, "", 0, &mut files, &mut folders)?;
        } else {
            files.push((String::new(), stat.size));
        }
        let total: u64 = files.iter().map(|(_, size)| size).sum();
        if total > MAX_COPY_BYTES {
            bail!(
                "That is {} MB; copies are limited to {} MB. Copy a smaller folder.",
                total / 1_048_576,
                MAX_COPY_BYTES / 1_048_576
            );
        }
        if let Some((name, size)) = files.iter().find(|(_, size)| *size > MAX_FILE_BYTES) {
            bail!(
                "`{}` is {} MB; a node moves files up to {} MB.",
                shown(&join_rel(&rel, name)),
                size / 1_048_576,
                MAX_FILE_BYTES / 1_048_576
            );
        }
        if stat.dir {
            for folder in &folders {
                self.make_dir(&target, &child(&into, folder))?;
            }
        }
        for (name, _) in &files {
            let bytes = self.bytes(&source, &child(&from, name))?;
            self.put(&target, &child(&into, name), &bytes)?;
        }
        Ok(format!(
            "Copied {} file(s), {} bytes, from {device}:{rel} to {dest}.",
            files.len(),
            total
        ))
    }

    /// Every file under `abs`, as `(path below it, size)`, and every folder.
    fn walk(
        &self,
        place: &Place,
        abs: &str,
        below: &str,
        depth: usize,
        files: &mut Vec<(String, u64)>,
        folders: &mut Vec<String>,
    ) -> Result<()> {
        if depth > MAX_DEPTH || files.len() + folders.len() > MAX_ENTRIES {
            bail!("That folder is too big to copy.");
        }
        for e in self.entries(place, &child(abs, below))? {
            let name = join_rel(below, &e.name);
            if e.dir {
                folders.push(name.clone());
                self.walk(place, abs, &name, depth + 1, files, folders)?;
            } else {
                files.push((name, e.size));
            }
        }
        Ok(())
    }

    fn make_dir(&self, place: &Place, abs: &str) -> Result<()> {
        match place {
            Place::Here => local_mkdir(self.root, abs),
            Place::Device(d) => self.nodes.mkdir(d, abs),
        }
    }

    fn put(&self, place: &Place, abs: &str, bytes: &[u8]) -> Result<()> {
        match place {
            Place::Here => local_write(self.root, abs, bytes),
            Place::Device(d) => {
                // Parent folders first: a node writes into folders that exist.
                if let Some(parent) = parent_of(abs) {
                    self.nodes.mkdir(d, &parent)?;
                }
                self.nodes.write(d, abs, bytes)
            }
        }
    }
}

fn shown(rel: &str) -> &str {
    if rel.is_empty() { "." } else { rel }
}

fn join_rel(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if name.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{name}")
    }
}

/// `abs` with `below` (`/` separated) added, using `abs`'s own separator.
fn child(abs: &str, below: &str) -> String {
    if below.is_empty() {
        abs.to_string()
    } else {
        spread::join_on(abs, below)
    }
}

fn parent_of(abs: &str) -> Option<String> {
    let cut = abs.trim_end_matches(['/', '\\']).rfind(['/', '\\'])?;
    (cut > 0).then(|| abs[..cut].to_string())
}

/// Files of the scheduler stay inside the project, links included.
fn inside(root: &Path, abs: &str) -> Result<PathBuf> {
    let root = root.canonicalize().context("The project folder is gone.")?;
    let path = PathBuf::from(abs);
    // The nearest part that exists decides: a link there may lead out.
    let mut probe = path.as_path();
    while !probe.exists() {
        probe = probe.parent().context("Bad path.")?;
    }
    let real = probe.canonicalize()?;
    if !real.starts_with(&root) {
        bail!("That path leaves the project.");
    }
    Ok(path)
}

fn local_stat(root: &Path, abs: &str) -> Result<Stat> {
    let path = inside(root, abs)?;
    Ok(match fs::metadata(&path) {
        Ok(m) => Stat {
            exists: true,
            dir: m.is_dir(),
            size: m.len(),
        },
        Err(_) => Stat::default(),
    })
}

fn local_list(root: &Path, abs: &str) -> Result<Vec<Entry>> {
    let path = inside(root, abs)?;
    let mut out = Vec::new();
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // As a node does: hidden entries are left out, and so are links.
        if name.starts_with('.') || entry.file_type()?.is_symlink() {
            continue;
        }
        let meta = entry.metadata()?;
        out.push(Entry {
            name,
            dir: meta.is_dir(),
            size: meta.len(),
        });
    }
    Ok(out)
}

fn local_read(root: &Path, abs: &str) -> Result<Vec<u8>> {
    Ok(fs::read(inside(root, abs)?)?)
}

fn local_mkdir(root: &Path, abs: &str) -> Result<()> {
    let path = inside(root, abs)?;
    Ok(fs::create_dir_all(path)?)
}

fn local_write(root: &Path, abs: &str, bytes: &[u8]) -> Result<()> {
    let path = inside(root, abs)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(fs::write(path, bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Role, Workstream};
    use serde_json::json;
    use std::{collections::BTreeMap, sync::Mutex};

    /// Fake computers: a path to bytes, by device.
    #[derive(Default)]
    struct Fake {
        files: Mutex<BTreeMap<(String, String), Vec<u8>>>,
        here: PathBuf,
    }

    impl Fake {
        fn put(&self, device: &str, path: &str, bytes: &[u8]) {
            self.files
                .lock()
                .unwrap()
                .insert((device.into(), path.into()), bytes.to_vec());
        }
        fn get(&self, device: &str, path: &str) -> Option<Vec<u8>> {
            self.files
                .lock()
                .unwrap()
                .get(&(device.to_string(), path.to_string()))
                .cloned()
        }
    }

    impl Nodes for &Fake {
        fn scheduler(&self) -> String {
            "linux".into()
        }
        fn here_workspace(&self) -> Option<PathBuf> {
            Some(self.here.clone())
        }
        fn workspace_of(&self, device: &str) -> Option<String> {
            Some(match device {
                "win" => r"D:\Atrium Workspace".into(),
                _ => format!("/{device}/Atrium Workspace"),
            })
        }
        fn list(&self, device: &str, path: &str) -> Result<Vec<Entry>> {
            let prefix = format!("{}{}", path, if path.contains('\\') { '\\' } else { '/' });
            let mut seen: BTreeMap<String, Entry> = BTreeMap::new();
            for ((d, p), bytes) in self.files.lock().unwrap().iter() {
                if d != device {
                    continue;
                }
                if let Some(rest) = p.strip_prefix(&prefix) {
                    let (name, dir) = match rest.split_once(['/', '\\']) {
                        Some((n, _)) => (n, true),
                        None => (rest, false),
                    };
                    seen.insert(
                        name.into(),
                        Entry {
                            name: name.into(),
                            dir,
                            size: bytes.len() as u64,
                        },
                    );
                }
            }
            Ok(seen.into_values().collect())
        }
        fn stat(&self, device: &str, path: &str) -> Result<Stat> {
            if let Some(b) = self.get(device, path) {
                return Ok(Stat {
                    exists: true,
                    dir: false,
                    size: b.len() as u64,
                });
            }
            let dir = !self.list(device, path)?.is_empty();
            Ok(Stat {
                exists: dir,
                dir,
                size: 0,
            })
        }
        fn read(&self, device: &str, path: &str) -> Result<Vec<u8>> {
            self.get(device, path).context("missing")
        }
        fn write(&self, device: &str, path: &str, bytes: &[u8]) -> Result<()> {
            self.put(device, path, bytes);
            Ok(())
        }
        fn mkdir(&self, _device: &str, _path: &str) -> Result<()> {
            Ok(())
        }
    }

    fn stream(id: &str, backend: &str, scope: &[&str]) -> Workstream {
        Workstream {
            id: id.into(),
            name: id.into(),
            backend: backend.into(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            role: Role::Lead,
            ..Default::default()
        }
    }

    fn setup(name: &str) -> (Project, PathBuf) {
        let base =
            std::env::temp_dir().join(format!("atrium-devices-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("Atrium Workspace/photo");
        fs::create_dir_all(root.join("ui")).unwrap();
        let mut project = Project::open(&root).unwrap();
        project.file.streams = vec![
            stream("ui", "claude", &["ui/**"]),
            stream("perf", "codex@win", &["perf/**"]),
        ];
        project.save().unwrap();
        (project, base.join("Atrium Workspace"))
    }

    fn run(fake: &Fake, project: &Project, me: &str, tool: &str, args: Value) -> Result<String> {
        Direct(fake).call(project, me, tool, &args)
    }

    #[test]
    fn paths_stay_inside_the_project() {
        assert_eq!(clean("a/b.rs").unwrap(), "a/b.rs");
        assert_eq!(clean(r"a\b.rs").unwrap(), "a/b.rs");
        assert_eq!(clean("").unwrap(), "");
        assert!(clean("../x").is_err());
        assert!(clean("a/../../x").is_err());
        assert!(clean("/etc/passwd").is_err());
        assert!(clean(r"C:\x").is_err());
        assert!(clean(".atrium/tasks").is_err());
    }

    #[test]
    fn another_computers_files_are_listed_and_read() {
        let (project, workspace) = setup("read");
        let fake = Fake {
            here: workspace,
            ..Default::default()
        };
        fake.put("win", r"D:\Atrium Workspace\photo\perf\a.txt", b"hello");
        fake.put("win", r"D:\Atrium Workspace\photo\perf\img.bin", &[0, 1, 2]);
        let listed = run(
            &fake,
            &project,
            "ui",
            "device_list",
            json!({"device":"win","path":"perf"}),
        )
        .unwrap();
        assert!(listed.contains("a.txt (5 bytes)"), "{listed}");
        let read = run(
            &fake,
            &project,
            "ui",
            "device_read",
            json!({"device":"win","path":"perf/a.txt"}),
        )
        .unwrap();
        assert_eq!(read, "hello");
        let binary = run(
            &fake,
            &project,
            "ui",
            "device_read",
            json!({"device":"win","path":"perf/img.bin"}),
        )
        .unwrap();
        assert!(binary.contains("binary file of 3 bytes"), "{binary}");
        // Outside the project, in `.atrium/`, or on a computer that is not in it.
        for (device, path) in [
            ("win", "../other"),
            ("win", ".atrium/tasks"),
            ("mac", "perf"),
        ] {
            assert!(
                run(
                    &fake,
                    &project,
                    "ui",
                    "device_read",
                    json!({"device":device,"path":path})
                )
                .is_err(),
                "{device}:{path}"
            );
        }
        // The scheduler's own files, by its name.
        fs::write(project.root.join("ui/app.txt"), "mine").unwrap();
        let own = run(
            &fake,
            &project,
            "perf",
            "device_read",
            json!({"device":"Linux","path":"ui/app.txt"}),
        )
        .unwrap();
        assert_eq!(own, "mine");
        let _ = fs::remove_dir_all(project.root.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_copy_lands_in_the_callers_own_scope_only() {
        let (project, workspace) = setup("copy");
        let fake = Fake {
            here: workspace,
            ..Default::default()
        };
        fake.put("win", r"D:\Atrium Workspace\photo\perf\r\a.csv", b"1,2");
        fake.put("win", r"D:\Atrium Workspace\photo\perf\r\b\c.csv", b"3");
        // The scheduler's lead copies a Windows folder into its own scope.
        let said = run(
            &fake,
            &project,
            "ui",
            "device_copy",
            json!({"device":"win","path":"perf/r","to":"ui/data"}),
        )
        .unwrap();
        assert!(said.contains("Copied 2 file(s)"), "{said}");
        assert_eq!(
            fs::read(project.root.join("ui/data/a.csv")).unwrap(),
            b"1,2"
        );
        assert_eq!(
            fs::read(project.root.join("ui/data/b/c.csv")).unwrap(),
            b"3"
        );
        // Into a scope that is not its own: refused, nothing written.
        let error = run(
            &fake,
            &project,
            "ui",
            "device_copy",
            json!({"device":"win","path":"perf/r","to":"perf/r"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("belongs to win"), "{error}");
        assert!(
            run(
                &fake,
                &project,
                "ui",
                "device_copy",
                json!({"device":"win","path":"perf/r","to":"README.md"})
            )
            .is_err()
        );
        // The Windows lead copies the scheduler's file onto Windows.
        fs::write(project.root.join("ui/spec.md"), "spec").unwrap();
        run(
            &fake,
            &project,
            "perf",
            "device_copy",
            json!({"device":"linux","path":"ui/spec.md","to":"perf/spec.md"}),
        )
        .unwrap();
        assert_eq!(
            fake.get("win", r"D:\Atrium Workspace\photo\perf\spec.md")
                .unwrap(),
            b"spec"
        );
        let _ = fs::remove_dir_all(project.root.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn a_copy_over_the_limit_moves_nothing() {
        let (project, workspace) = setup("big");
        let fake = Fake {
            here: workspace,
            ..Default::default()
        };
        // Sizes come from the listing, so no bytes are needed: 13 files of 16 MB.
        struct Big<'a>(&'a Fake);
        impl Nodes for &Big<'_> {
            fn scheduler(&self) -> String {
                self.0.scheduler()
            }
            fn here_workspace(&self) -> Option<PathBuf> {
                self.0.here_workspace()
            }
            fn workspace_of(&self, d: &str) -> Option<String> {
                Nodes::workspace_of(&self.0, d)
            }
            fn list(&self, _: &str, _: &str) -> Result<Vec<Entry>> {
                Ok((0..13)
                    .map(|i| Entry {
                        name: format!("f{i}"),
                        dir: false,
                        size: MAX_FILE_BYTES,
                    })
                    .collect())
            }
            fn stat(&self, _: &str, _: &str) -> Result<Stat> {
                Ok(Stat {
                    exists: true,
                    dir: true,
                    size: 0,
                })
            }
            fn read(&self, _: &str, _: &str) -> Result<Vec<u8>> {
                panic!("nothing is read")
            }
            fn write(&self, _: &str, _: &str, _: &[u8]) -> Result<()> {
                panic!("nothing is written")
            }
            fn mkdir(&self, _: &str, _: &str) -> Result<()> {
                panic!("nothing is made")
            }
        }
        let big = Big(&fake);
        let error = Direct(&big)
            .call(
                &project,
                "ui",
                "device_copy",
                &json!({"device":"win","path":"perf/r","to":"ui/r"}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("limited to 200 MB"), "{error}");
        assert!(!project.root.join("ui/r").exists());
        let _ = fs::remove_dir_all(project.root.parent().unwrap().parent().unwrap());
    }
}
