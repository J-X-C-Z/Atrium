//! Reading and writing a project's `.atrium/` files the way `atrium-project`
//! does, without the parts WASI lacks.
//!
//! `atrium-project`'s own atomic write names its temporary file after the
//! process id, which panics under WASI, and copies permissions, which WASI
//! cannot. The few writes the core makes on its own (the project file, a
//! task's status) go through [`write_atomic`] here instead, producing the
//! same bytes. Everything else is `atrium-project`'s.

use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result};
use atrium_project::{DIR, Project, ProjectFile, TaskStatus, pool::TaskLock, with_status};

/// What `Project::open` writes for a new project's brief.
pub const BRIEF: &str = "# Brief\n\nShared conventions and decisions every workstream follows. Keep it short.\n\n## Architecture\n\n## Conventions\n\n## Decisions\n";
/// What `Project::open` writes for a new project's contracts folder.
pub const CONTRACTS: &str = "# Contracts\n\nOne file per interface between workstreams (an API, a schema, an event).\nThe workstream that changes an interface updates its file and posts a notice.\n";
const NOTICES: &str = "# Notices\n\n";

/// Whether `atrium-project`'s own writes (pool claims, task reports, start
/// failures) work here. Under WASI they panic, so the core skips them.
pub fn scheduler_writes() -> bool {
    !cfg!(target_os = "wasi")
}

/// Writes through a temporary file and a rename, so a reader never sees half.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension(format!(
        "tmp-orca-{}-{}",
        jiff::Timestamp::now().as_nanosecond(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.with_context(|| format!("writing {}", path.display()))
}

/// Whether `root` holds a project.
pub fn is_project(root: &Path) -> bool {
    root.join(DIR).join("project.json").is_file()
}

/// Opens the project in `root` without writing anything.
pub fn load(root: &Path) -> Result<Project> {
    let path = root.join(DIR).join("project.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("{} is not an Atrium project", root.display()))?;
    let file: ProjectFile =
        serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    let mut project = Project {
        root: root.to_path_buf(),
        file,
        ..Project::default()
    };
    project.reload();
    Ok(project)
}

/// Creates the atrium in `root` like `Project::open` does, then opens it.
pub fn init(root: &Path) -> Result<Project> {
    let dir = root.join(DIR);
    for sub in ["contracts", "status", "tasks"] {
        fs::create_dir_all(dir.join(sub))
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    for (path, text) in [
        (dir.join("brief.md"), BRIEF),
        (dir.join("contracts").join("README.md"), CONTRACTS),
        (dir.join("notices.md"), NOTICES),
    ] {
        if !path.exists() {
            fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    if !is_project(root) {
        let mut file = ProjectFile::default();
        file.name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Project".into());
        let project = Project {
            root: root.to_path_buf(),
            file,
            ..Project::default()
        };
        save(&project)?;
    }
    load(root)
}

/// `Project::save`: the project file, as pretty JSON.
pub fn save(project: &Project) -> Result<()> {
    let text = serde_json::to_string_pretty(&project.file)?;
    write_atomic(&project.dir().join("project.json"), text.as_bytes())
}

/// `Project::set_task_status`: the task file's `status:` line, under the
/// task lock, then the project read again.
pub fn set_task_status(project: &mut Project, key: &str, status: TaskStatus) -> Result<()> {
    let _lock = TaskLock::take(project)?;
    let task = project
        .task(key)
        .ok_or_else(|| anyhow::anyhow!("task {key} is missing or unreadable"))?;
    let path = project.root.join(task.rel_path());
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    write_atomic(&path, with_status(&text, status).as_bytes())?;
    project.stamp = None;
    project.reload();
    Ok(())
}

/// Rewrites a task file through `change`, under the task lock.
pub fn rewrite_task(
    project: &mut Project,
    key: &str,
    change: impl FnOnce(&str) -> String,
) -> Result<()> {
    let _lock = TaskLock::take(project)?;
    let task = project
        .task(key)
        .ok_or_else(|| anyhow::anyhow!("no task {key}"))?;
    let path = project.root.join(task.rel_path());
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    write_atomic(&path, change(&text).as_bytes())?;
    project.stamp = None;
    project.reload();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_what_project_open_writes() {
        let ours = tempfile::tempdir().unwrap();
        let theirs = tempfile::tempdir().unwrap();
        let a = init(ours.path()).unwrap();
        let b = Project::open(theirs.path()).unwrap();
        for file in ["brief.md", "contracts/README.md", "notices.md"] {
            assert_eq!(
                fs::read_to_string(ours.path().join(DIR).join(file)).unwrap(),
                fs::read_to_string(theirs.path().join(DIR).join(file)).unwrap(),
                "{file}"
            );
        }
        let name = |p: &Project| p.root.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(a.file.name, name(&a));
        assert_eq!(b.file.name, name(&b));
        let mut file = b.file.clone();
        file.name = a.file.name.clone();
        assert_eq!(
            serde_json::to_string_pretty(&a.file).unwrap(),
            serde_json::to_string_pretty(&file).unwrap()
        );
        // The app reads what the core wrote.
        assert!(Project::open(ours.path()).is_ok());
    }

    #[test]
    fn a_task_status_is_written_like_the_app_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = init(dir.path()).unwrap();
        project.file.streams.push(atrium_project::Workstream {
            id: "boss".into(),
            role: atrium_project::Role::Controller,
            ..Default::default()
        });
        save(&project).unwrap();
        let key = project
            .add_task("boss", "Web", "# Web\nstatus: open\n\nBuild it.\n")
            .unwrap();
        set_task_status(&mut project, &key, TaskStatus::Running).unwrap();
        assert_eq!(project.task(&key).unwrap().status, TaskStatus::Running);
        let again = load(dir.path()).unwrap();
        assert_eq!(again.task(&key).unwrap().status, TaskStatus::Running);
        assert_eq!(again.controller().unwrap().id, "boss");
    }
}
