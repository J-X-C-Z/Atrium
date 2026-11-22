//! Rendered files on disk: one folder per file version.
//!
//! `cache_dir/<key>/doc.pdf` holds the converted PDF and
//! `<page>-<width>.png` the pages drawn from it. `warnings.txt` keeps the
//! conversion warnings, so asking again does not convert again.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::Refused;

/// Bump whenever the output changes: new renderer, new layout, new fonts.
pub const RENDER_VERSION: u32 = 3;
/// The whole cache is kept under this size.
pub const CACHE_LIMIT: u64 = 500 * 1024 * 1024;
/// The most a single source file may be.
pub const INPUT_LIMIT: u64 = 100 * 1024 * 1024;
/// The most an OOXML/ODF package may expand to.
pub const UNPACKED_LIMIT: u64 = 500 * 1024 * 1024;

pub(crate) struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The folder for this file version: SHA-256 of the real path, its size
    /// and modification time, and [`RENDER_VERSION`].
    pub fn key(path: &Path) -> Result<String, Refused> {
        let meta = fs::metadata(path).map_err(|_| Refused::Failed("the file is gone".into()))?;
        let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()))
            .unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(real.to_string_lossy().as_bytes());
        hasher.update(meta.len().to_le_bytes());
        hasher.update(modified.0.to_le_bytes());
        hasher.update(modified.1.to_le_bytes());
        hasher.update(RENDER_VERSION.to_le_bytes());
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn entry(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    /// The converted PDF, if it is already cached.
    pub fn pdf(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.entry(key).join("doc.pdf");
        let bytes = fs::read(&path).ok()?;
        if bytes.len() < 5 {
            return None;
        }
        self.touch(key);
        Some(bytes)
    }

    pub fn put_pdf(&self, key: &str, pdf: &[u8]) -> io::Result<()> {
        let dir = self.entry(key);
        fs::create_dir_all(&dir)?;
        write_atomic(&dir.join("doc.pdf"), pdf)?;
        self.touch(key);
        self.trim();
        Ok(())
    }

    pub fn warnings(&self, key: &str) -> Vec<String> {
        fs::read_to_string(self.entry(key).join("warnings.txt"))
            .map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn put_warnings(&self, key: &str, warnings: &[String]) {
        let dir = self.entry(key);
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        let text = warnings
            .iter()
            .map(|w| w.replace(['\n', '\r'], " "))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = write_atomic(&dir.join("warnings.txt"), text.as_bytes());
    }

    pub fn page(&self, key: &str, index: usize, width: u32) -> Option<PathBuf> {
        let path = self.page_path(key, index, width);
        if path.is_file() {
            self.touch(key);
            Some(path)
        } else {
            None
        }
    }

    pub fn put_page(&self, key: &str, index: usize, width: u32, png: &[u8]) -> io::Result<PathBuf> {
        let dir = self.entry(key);
        fs::create_dir_all(&dir)?;
        let path = self.page_path(key, index, width);
        write_atomic(&path, png)?;
        self.touch(key);
        self.trim();
        Ok(path)
    }

    fn page_path(&self, key: &str, index: usize, width: u32) -> PathBuf {
        self.entry(key).join(format!("{index}-{width}.png"))
    }

    /// Mark a folder recently used. Directory mtimes drive eviction, and a
    /// short-lived marker file is how a directory date is bumped without a
    /// platform-specific call.
    pub fn touch(&self, key: &str) {
        let dir = self.entry(key);
        if !dir.is_dir() {
            return;
        }
        let mut hasher = DefaultHasher::new();
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        let marker = dir.join(format!(".used-{:x}", hasher.finish()));
        if fs::write(&marker, b"").is_ok() {
            let _ = fs::remove_file(marker);
        }
    }

    /// Keep the folder under [`CACHE_LIMIT`], dropping the least recently
    /// used file versions first.
    pub fn trim(&self) {
        self.trim_to(CACHE_LIMIT);
    }

    fn trim_to(&self, limit: u64) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        let mut dirs: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let mut total = 0u64;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let size = folder_size(&path);
            let used = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            total += size;
            dirs.push((used, size, path));
        }
        if total <= limit {
            return;
        }
        dirs.sort_by_key(|(used, _, _)| *used);
        for (_, size, path) in dirs {
            if total <= limit {
                break;
            }
            if fs::remove_dir_all(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atrium-preview-cache-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn eviction_drops_the_least_recently_used_folder() {
        let dir = root("evict");
        let cache = Cache::new(dir.clone());
        let page = vec![7u8; 100];
        cache.put_pdf("a", &page).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        cache.put_pdf("b", &page).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        cache.put_pdf("c", &page).unwrap();

        cache.trim_to(250);
        assert!(
            !cache.entry("a").exists(),
            "oldest folder should be evicted"
        );
        assert!(cache.entry("b").exists());
        assert!(cache.entry("c").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn touching_a_folder_keeps_it() {
        let dir = root("touch");
        let cache = Cache::new(dir.clone());
        let page = vec![7u8; 100];
        cache.put_pdf("a", &page).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        cache.put_pdf("b", &page).unwrap();
        cache.touch("a");

        cache.trim_to(150);
        assert!(cache.entry("a").exists(), "touched folder should stay");
        assert!(!cache.entry("b").exists(), "staler folder should go");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn key_changes_with_size_and_mtime() {
        let dir = root("key");
        let path = dir.join("note.pdf");
        fs::write(&path, b"%PDF-1.4 first").unwrap();
        let first = Cache::key(&path).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(later))
            .unwrap();
        let second = Cache::key(&path).unwrap();
        assert_ne!(first, second);
        fs::remove_dir_all(dir).unwrap();
    }
}

fn folder_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let temp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    fs::write(&temp, bytes)?;
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(err)
        }
    }
}

/// A hidden hook so tests can see whether a file was converted again.
#[doc(hidden)]
pub mod probe {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub static CONVERSIONS: AtomicUsize = AtomicUsize::new(0);
    static BY_FILE: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    #[allow(dead_code)]
    pub fn reset() {
        CONVERSIONS.store(0, Ordering::SeqCst);
        if let Ok(mut map) = BY_FILE.lock() {
            *map = Some(HashMap::new());
        }
    }

    #[allow(dead_code)]
    pub fn conversions() -> usize {
        CONVERSIONS.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn conversions_of(path: &Path) -> usize {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        BY_FILE
            .lock()
            .ok()
            .and_then(|map| map.as_ref().and_then(|map| map.get(&real).copied()))
            .unwrap_or(0)
    }

    pub(crate) fn converted(path: &Path) {
        CONVERSIONS.fetch_add(1, Ordering::SeqCst);
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Ok(mut map) = BY_FILE.lock() {
            *map.get_or_insert_with(HashMap::new)
                .entry(real)
                .or_insert(0) += 1;
        }
    }
}
