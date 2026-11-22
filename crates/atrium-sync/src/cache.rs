use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    time::SystemTime,
};

use atrium_core::{ApprovalRequest, Item, UserInputRequest};
use serde::{Deserialize, Serialize};

use crate::store::StoreError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CacheEntry {
    pub items: Vec<Item>,
    #[serde(default)]
    pub approvals: Vec<ApprovalRequest>,
    #[serde(default)]
    pub questions: Vec<UserInputRequest>,
    /// The owner's snapshot cursor this cache was read at.
    pub cursor: u64,
    /// Unix milliseconds.
    pub fetched_at_ms: i64,
}

/// Read-only copies of conversations from other devices.
///
/// The cache is best effort: a missing or unreadable file is a miss, never
/// an error that blocks opening a conversation. It is bounded by bytes and
/// evicts the least recently used conversation first.
pub struct ThreadCache {
    root: PathBuf,
    max_bytes: u64,
}

impl ThreadCache {
    pub fn open(root: impl AsRef<Path>, max_bytes: u64) -> std::io::Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        Ok(Self {
            root: root.as_ref().to_path_buf(),
            max_bytes,
        })
    }

    pub fn path_for(&self, device: &str, backend: &str, thread: &str) -> PathBuf {
        self.root.join(safe_name(device)).join(format!(
            "{}-{}.json",
            safe_name(backend),
            safe_name(thread)
        ))
    }

    pub fn get(&self, device: &str, backend: &str, thread: &str) -> Option<CacheEntry> {
        let path = self.path_for(device, backend, thread);
        let bytes = fs::read(&path).ok()?;
        let entry = serde_json::from_slice(&bytes).ok()?;
        // Reading is a use: bump the file's mtime for LRU without changing
        // its content.
        if let Ok(file) = File::options().append(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        Some(entry)
    }

    pub fn put(
        &self,
        device: &str,
        backend: &str,
        thread: &str,
        entry: &CacheEntry,
    ) -> Result<(), StoreError> {
        let path = self.path_for(device, backend, thread);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        let mut file = File::create(&temp)?;
        serde_json::to_writer(&mut file, entry)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp, &path)?;
        self.prune()?;
        Ok(())
    }

    pub fn size_bytes(&self) -> u64 {
        walk(&self.root)
            .into_iter()
            .filter_map(|path| fs::metadata(path).ok())
            .map(|meta| meta.len())
            .sum()
    }

    /// Evicts the oldest files until the cache fits. Returns how many went.
    pub fn prune(&self) -> Result<usize, StoreError> {
        let mut files: Vec<(SystemTime, u64, PathBuf)> = walk(&self.root)
            .into_iter()
            .filter_map(|path| {
                let meta = fs::metadata(&path).ok()?;
                if !meta.is_file() || path.extension().is_some_and(|ext| ext == "tmp") {
                    return None;
                }
                Some((
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                    path,
                ))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
        if total <= self.max_bytes {
            return Ok(0);
        }
        files.sort_by_key(|(modified, _, _)| *modified);
        let mut removed = 0;
        for (_, size, path) in files {
            if total <= self.max_bytes {
                break;
            }
            if fs::remove_file(path).is_ok() {
                total = total.saturating_sub(size);
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn safe_name(value: &str) -> String {
    let mut name: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        name.push('_');
    }
    if name == "." || name == ".." {
        name = "_".repeat(name.len());
    }
    name
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_core::Item;

    fn entry(text: &str) -> CacheEntry {
        CacheEntry {
            items: vec![Item::Agent {
                id: text.into(),
                text: text.into(),
            }],
            approvals: Vec::new(),
            questions: Vec::new(),
            cursor: 7,
            fetched_at_ms: 1,
        }
    }

    #[test]
    fn entries_round_trip_and_paths_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThreadCache::open(dir.path(), 1_000_000).unwrap();
        cache
            .put("studio/../x", "codex", "a/b", &entry("hello"))
            .unwrap();
        assert_eq!(
            cache
                .get("studio/../x", "codex", "a/b")
                .map(|entry| entry.items),
            Some(entry("hello").items)
        );
        assert!(!cache.path_for("..", "codex", "x").starts_with(".."));
    }

    #[test]
    fn the_cache_stays_under_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ThreadCache::open(dir.path(), 2_000).unwrap();
        for ix in 0..20 {
            cache
                .put(
                    "studio",
                    "codex",
                    &format!("t{ix}"),
                    &entry(&"x".repeat(300)),
                )
                .unwrap();
        }
        assert!(cache.size_bytes() <= 2_000);
        assert!(cache.get("studio", "codex", "t0").is_none());
    }
}
