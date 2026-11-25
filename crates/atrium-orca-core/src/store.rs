//! The few file writes the core makes on its own (run state, team file, task
//! titles); `atrium-project` does the rest. Atomic: a reader never sees half.

use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result};
use atrium_project::{DIR, Project, pool::TaskLock};

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
    fn writes_replace_the_file_and_leave_no_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("f.txt");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_task_file_is_rewritten_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = Project::open(dir.path()).unwrap();
        project.file.streams.push(atrium_project::Workstream {
            id: "boss".into(),
            role: atrium_project::Role::Controller,
            ..Default::default()
        });
        project.save().unwrap();
        let key = project
            .add_task("boss", "Web", "# Web\nstatus: open\n\nBuild it.\n")
            .unwrap();
        rewrite_task(&mut project, &key, |t| t.replace("Web", "Site")).unwrap();
        assert_eq!(project.task(&key).unwrap().title, "Site");
        assert!(!dir.path().join(DIR).join("tasks").join(".lock").exists());
    }
}
