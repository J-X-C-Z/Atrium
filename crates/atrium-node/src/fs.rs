//! Root-scoped file access. Canonicalisation is for routing, not authorization:
//! all actual I/O is relative to a cap-std directory capability, so a symlink
//! swap after routing cannot redirect I/O outside the selected root.
use anyhow::{Result, bail};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use serde_json::{Value, json};
use std::{
    fmt,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
static TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub enum FsError {
    OutsideRoots,
    BadRequest,
    TooLarge,
}
impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutsideRoots => "outside_roots",
            Self::BadRequest => "bad_request",
            Self::TooLarge => "too_large",
        })
    }
}
impl std::error::Error for FsError {}

pub struct Roots {
    paths: Vec<PathBuf>,
    dirs: Vec<Dir>,
}
impl Roots {
    pub fn new(paths: Vec<PathBuf>) -> Result<Self> {
        let mut canonical = Vec::new();
        let mut dirs = Vec::new();
        for path in paths {
            let path = expand(path.to_str().ok_or(FsError::BadRequest)?)?.canonicalize()?;
            if !path.is_dir() {
                bail!(FsError::BadRequest);
            }
            if !canonical.contains(&path) {
                dirs.push(Dir::open_ambient_dir(&path, ambient_authority())?);
                canonical.push(path);
            }
        }
        Ok(Self {
            paths: canonical,
            dirs,
        })
    }
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
    /// Resolve symlinks in the existing ancestor and allow a missing suffix.
    /// This method alone is NOT a sandbox for a subsequently spawned process.
    pub fn resolve(&self, input: &str) -> Result<PathBuf> {
        let absolute = expand(input)?;
        let mut ancestor = absolute.as_path();
        let mut tail = Vec::new();
        let base = loop {
            match ancestor.canonicalize() {
                Ok(p) => break p,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // A dangling symlink must not be interpreted as a new file.
                    if ancestor.symlink_metadata().is_ok() {
                        bail!(FsError::OutsideRoots);
                    }
                    let name = ancestor.file_name().ok_or(FsError::BadRequest)?;
                    tail.push(name.to_os_string());
                    ancestor = ancestor.parent().ok_or(FsError::BadRequest)?;
                }
                Err(e) => return Err(e.into()),
            }
        };
        let mut resolved = base;
        for part in tail.into_iter().rev() {
            resolved.push(part);
        }
        if !self.paths.iter().any(|root| resolved.starts_with(root)) {
            bail!(FsError::OutsideRoots);
        }
        Ok(resolved)
    }
    fn routed(&self, input: &str) -> Result<(&Dir, PathBuf, PathBuf)> {
        let path = self.resolve(input)?;
        let (index, root) = self
            .paths
            .iter()
            .enumerate()
            .filter(|(_, r)| path.starts_with(r))
            .max_by_key(|(_, r)| r.components().count())
            .ok_or(FsError::OutsideRoots)?;
        let relative = path.strip_prefix(root)?.to_path_buf();
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative
        };
        Ok((&self.dirs[index], relative, path))
    }
    pub fn list(&self, path: &str, hidden: bool) -> Result<Value> {
        let (root, relative, path) = self.routed(path)?;
        let dir = root.open_dir(relative)?;
        let mut entries = Vec::new();
        for entry in dir.entries()? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                continue;
            }
            if entries.len() >= MAX_ENTRIES {
                bail!(FsError::TooLarge);
            }
            // Capability resolution rejects a link outside this directory. List
            // such links as links without following or exposing target metadata.
            let meta = if entry.file_type()?.is_symlink() {
                // Do not dereference unknown links just to render a listing.
                entries.push(json!({"name":name,"dir":false,"size":0,"modified":0}));
                continue;
            } else {
                dir.metadata(entry.file_name())?
            };
            entries.push(json!({"name":name,"dir":meta.is_dir(),"size":meta.len(),"modified":modified(&meta)}));
        }
        entries.sort_by(|a, b| {
            b["dir"]
                .as_bool()
                .cmp(&a["dir"].as_bool())
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        });
        Ok(json!({"path":path,"entries":entries}))
    }
    pub fn stat(&self, path: &str) -> Result<Value> {
        let (root, relative, _) = self.routed(path)?;
        match root.metadata(relative) {
            Ok(meta) => Ok(
                json!({"exists":true,"dir":meta.is_dir(),"size":meta.len(),"modified":modified(&meta)}),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(json!({"exists":false,"dir":false,"size":0,"modified":0}))
            }
            Err(e) => Err(e.into()),
        }
    }
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let (root, relative, _) = self.routed(path)?;
        let file = root.open(relative)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            bail!(FsError::BadRequest);
        }
        if meta.len() > MAX_FILE_BYTES as u64 {
            bail!(FsError::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES {
            bail!(FsError::TooLarge);
        }
        Ok(bytes)
    }
    pub fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        if bytes.len() > MAX_FILE_BYTES {
            bail!(FsError::TooLarge);
        }
        let (root, relative, _) = self.routed(path)?;
        if relative == Path::new(".") {
            bail!(FsError::BadRequest);
        }
        let parent = relative
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        root.create_dir_all(parent)?;
        let dir = root.open_dir(parent)?;
        let name = relative.file_name().ok_or(FsError::BadRequest)?;
        #[cfg(unix)]
        let target_permissions = {
            use cap_std::fs::PermissionsExt;
            match dir.metadata(name) {
                Ok(meta) if meta.is_file() => Some(cap_std::fs::Permissions::from_mode(
                    meta.permissions().mode() & 0o777,
                )),
                Ok(_) => None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            }
        };
        // create_new prevents clobbering a guessed temporary path. The temp and
        // destination share a directory handle, so rename is atomic and confined.
        let mut temp = None;
        for _ in 0..32 {
            let n = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let p = format!(".atrium-tmp-{}-{n}", std::process::id());
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match dir.open_with(&p, &options) {
                Ok(file) => {
                    temp = Some((p, file));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        let (temp, mut file) =
            temp.ok_or_else(|| anyhow::anyhow!("temporary name collision limit"))?;
        let result = (|| -> Result<()> {
            file.write_all(bytes)?;
            #[cfg(unix)]
            if let Some(permissions) = target_permissions {
                file.set_permissions(permissions)?;
            }
            file.sync_all()?;
            drop(file);
            dir.rename(&temp, &dir, name)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = dir.remove_file(&temp);
        }
        result
    }
    pub fn mkdir(&self, path: &str) -> Result<()> {
        let (root, relative, _) = self.routed(path)?;
        root.create_dir_all(relative)?;
        Ok(())
    }
    pub fn changes(&self, path: &str, since: u64) -> Result<Value> {
        let (root, relative, path) = self.routed(path)?;
        let now = millis(SystemTime::now());
        let dir = root.open_dir(relative)?;
        let mut changed = Vec::new();
        let mut visited = 0;
        walk(&dir, &path, since, 0, &mut visited, &mut changed)?;
        changed.sort();
        Ok(json!({"now":now,"changed":changed}))
    }
}
fn walk(
    dir: &Dir,
    path: &Path,
    since: u64,
    depth: usize,
    visited: &mut usize,
    changed: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in dir.entries()? {
        let entry = entry?;
        *visited += 1;
        if *visited > MAX_ENTRIES {
            bail!(FsError::TooLarge);
        }
        // Never traverse symlinks in changes: bounds work and prevents loops.
        if entry.file_type()?.is_symlink() {
            continue;
        }
        let meta = dir.metadata(entry.file_name())?;
        let child = path.join(entry.file_name());
        if modified(&meta) > since {
            changed.push(child.clone());
        }
        if meta.is_dir() && depth < 2 {
            let subdir = dir.open_dir(entry.file_name())?;
            walk(&subdir, &child, since, depth + 1, visited, changed)?;
        }
    }
    Ok(())
}
fn modified(meta: &cap_std::fs::Metadata) -> u64 {
    meta.modified().map(|t| millis(t.into_std())).unwrap_or(0)
}
fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn expand(input: &str) -> Result<PathBuf> {
    if input.contains('\0') {
        bail!(FsError::BadRequest);
    }
    let path = if input == "~" {
        dirs::home_dir().ok_or(FsError::BadRequest)?
    } else if let Some(rest) = input.strip_prefix("~/") {
        dirs::home_dir().ok_or(FsError::BadRequest)?.join(rest)
    } else {
        PathBuf::from(input)
    };
    if !path.is_absolute() {
        bail!(FsError::BadRequest);
    }
    // Existing paths retain filesystem semantics for '..'. For missing suffixes
    // reject it rather than interpreting a nonexistent component as a directory.
    if path.components().any(|c| c == Component::ParentDir) && !path.exists() {
        bail!(FsError::BadRequest);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "atrium-fs-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn roots(&self) -> Roots {
            Roots::new(vec![self.0.clone()]).unwrap()
        }
        fn path(&self, p: &str) -> String {
            self.0.join(p).to_string_lossy().into_owned()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn write_read_stat_and_sort() {
        let f = Fixture::new();
        let r = f.roots();
        r.write(&f.path("z.txt"), b"old").unwrap();
        r.write(&f.path("z.txt"), b"new").unwrap();
        r.write(&f.path("a/b.txt"), b"nested").unwrap();
        r.write(&f.path(".hidden"), b"x").unwrap();
        assert_eq!(r.read(&f.path("z.txt")).unwrap(), b"new");
        assert_eq!(r.stat(&f.path("missing")).unwrap()["exists"], false);
        let list = r.list(&f.path(""), false).unwrap();
        assert_eq!(list["entries"][0]["name"], "a");
        assert_eq!(list["entries"].as_array().unwrap().len(), 2);
        assert_eq!(
            r.list(&f.path(""), true).unwrap()["entries"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }
    #[test]
    fn outside_and_relative_rejected() {
        let f = Fixture::new();
        let r = f.roots();
        assert!(r.read("relative").is_err());
        assert!(r.stat("/etc/passwd").is_err());
        assert!(r.write(&f.path("../escape"), b"x").is_err());
    }
    #[test]
    fn size_limit_and_changes_depth() {
        let f = Fixture::new();
        let r = f.roots();
        assert!(
            r.write(&f.path("large"), &vec![0; MAX_FILE_BYTES + 1])
                .is_err()
        );
        let file = std::fs::File::create(f.0.join("large-on-disk")).unwrap();
        file.set_len((MAX_FILE_BYTES + 1) as u64).unwrap();
        assert!(r.read(&f.path("large-on-disk")).is_err());
        r.write(&f.path("a/b/c/hidden.txt"), b"x").unwrap();
        r.write(&f.path("a/b/visible.txt"), b"x").unwrap();
        let result = r.changes(&f.path(""), 0).unwrap();
        let s = result.to_string();
        assert!(s.contains("visible.txt"));
        assert!(!s.contains("hidden.txt"));
    }
    #[cfg(unix)]
    #[test]
    fn symlink_escape_and_dangling_rejected() {
        let f = Fixture::new();
        let outside = Fixture::new();
        let r = f.roots();
        std::os::unix::fs::symlink(&outside.0, f.0.join("escape")).unwrap();
        assert!(r.write(&f.path("escape/new"), b"x").is_err());
        std::os::unix::fs::symlink(outside.0.join("missing"), f.0.join("dangling")).unwrap();
        assert!(r.write(&f.path("dangling"), b"x").is_err());
        assert!(!outside.0.join("new").exists());
        r.write(&f.path("inside/file"), b"safe").unwrap();
        std::os::unix::fs::symlink(f.0.join("inside"), f.0.join("alias")).unwrap();
        assert_eq!(r.read(&f.path("alias/file")).unwrap(), b"safe");
        assert!(r.list(&f.path(""), true).is_ok());
        assert!(r.changes(&f.path(""), 0).is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn capability_rejects_parent_swap_after_routing() {
        let f = Fixture::new();
        let outside = Fixture::new();
        let r = f.roots();
        r.write(&f.path("parent/file"), b"inside").unwrap();
        std::fs::write(outside.0.join("file"), b"outside secret").unwrap();
        let (dir, relative, _) = r.routed(&f.path("parent/file")).unwrap();
        std::fs::rename(f.0.join("parent"), f.0.join("old-parent")).unwrap();
        std::os::unix::fs::symlink(&outside.0, f.0.join("parent")).unwrap();
        assert!(dir.open(relative).is_err());
    }
    #[test]
    fn empty_roots_deny_all_paths() {
        let r = Roots::new(vec![]).unwrap();
        assert!(r.resolve("/tmp").is_err());
        assert!(r.paths().is_empty());
    }
    #[test]
    fn failed_atomic_replace_retains_destination_and_cleans_temp() {
        let f = Fixture::new();
        let r = f.roots();
        r.write(&f.path("directory/keep"), b"unchanged").unwrap();
        assert!(
            r.write(&f.path("directory"), b"cannot replace directory")
                .is_err()
        );
        assert_eq!(r.read(&f.path("directory/keep")).unwrap(), b"unchanged");
        assert!(!std::fs::read_dir(&f.0).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".atrium-tmp-")
        }));
    }
    #[cfg(unix)]
    #[test]
    fn atomic_replace_preserves_private_mode_and_new_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let r = f.roots();
        r.write(&f.path("private"), b"one").unwrap();
        assert_eq!(
            std::fs::metadata(f.0.join("private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::set_permissions(f.0.join("private"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        r.write(&f.path("private"), b"two").unwrap();
        assert_eq!(
            std::fs::metadata(f.0.join("private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
