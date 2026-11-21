use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    clock::{Hlc, HybridClock},
    model::{ModelError, Op, Register, TOMBSTONE_RETENTION_MS, Vector, validate_value},
};

const COMPACT_AFTER_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sync store: {0}")]
    Io(#[from] std::io::Error),
    #[error("sync store: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("sync store state has an unsupported version: {0}")]
    Version(u32),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default = "sync_format_version")]
    version: u32,
    /// Ops older than this are ignored; tombstones below it were purged.
    #[serde(default)]
    floor_ms: i64,
    #[serde(default)]
    registers: BTreeMap<String, Register>,
    #[serde(default)]
    versions: BTreeMap<String, Hlc>,
}

fn sync_format_version() -> u32 {
    1
}

/// The append-only overlay store: one instance per device.
pub struct Store {
    dir: PathBuf,
    clock: HybridClock,
    registers: BTreeMap<String, Register>,
    versions: BTreeMap<String, Hlc>,
    floor_ms: i64,
    log_bytes: u64,
}

impl Store {
    pub fn open(dir: impl AsRef<Path>, device: impl Into<String>) -> Result<Self, StoreError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let state_path = dir.join("state.json");
        let persisted = match fs::read_to_string(&state_path) {
            Ok(text) => serde_json::from_str::<Persisted>(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Persisted::default(),
            Err(error) => return Err(error.into()),
        };
        if persisted.version > sync_format_version() {
            return Err(StoreError::Version(persisted.version));
        }
        let mut store = Self {
            dir,
            clock: HybridClock::new(device, Hlc::default()),
            registers: persisted.registers,
            versions: persisted.versions,
            floor_ms: persisted.floor_ms,
            log_bytes: 0,
        };
        for register in store.registers.values() {
            store.clock.observe(&register.clock);
        }
        store.replay_log()?;
        if let Some(last) = store.versions.values().max() {
            store.clock.observe(last);
        }
        store.refresh_log_bytes()?;
        Ok(store)
    }

    pub fn device(&self) -> &str {
        self.clock.device()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn register(&self, key: &str) -> Option<&Register> {
        self.registers.get(key)
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.registers.get(key)?.value.as_ref()
    }

    pub fn registers(&self) -> &BTreeMap<String, Register> {
        &self.registers
    }

    pub fn vector(&self) -> Vector {
        self.versions.clone()
    }

    /// Writes a local register and returns the op to replicate.
    pub fn set(&mut self, key: impl Into<String>, value: Value) -> Result<Op, StoreError> {
        let now = now_ms();
        let op = Op::set(key, value, self.clock.tick(now))?;
        self.accept(&op)?;
        self.append(&op)?;
        Ok(op)
    }

    /// Deletes a local register with a tombstone.
    pub fn delete(&mut self, key: impl Into<String>) -> Result<Op, StoreError> {
        let now = now_ms();
        let op = Op::delete(key, self.clock.tick(now))?;
        self.accept(&op)?;
        self.append(&op)?;
        Ok(op)
    }

    /// Merges ops from a peer. Returns the ops that actually changed state.
    pub fn merge(&mut self, ops: impl IntoIterator<Item = Op>) -> Result<Vec<Op>, StoreError> {
        let mut applied = Vec::new();
        for op in ops {
            if self.accept(&op)? {
                self.append(&op)?;
                applied.push(op);
            }
        }
        Ok(applied)
    }

    /// The current registers a peer at `remote` does not have.
    ///
    /// Sending current state instead of history also covers a peer that was
    /// away long enough for compaction to drop the old log.
    pub fn missing_for(&self, _remote: &Vector) -> Vec<Op> {
        // The version vector is a hint, not a proof that every earlier key
        // arrived. Sending the current registers is small (the overlay is
        // metadata) and cannot lose an op that arrived out of order.
        self.registers
            .iter()
            .map(|(key, register)| Op {
                key: key.clone(),
                value: register.value.clone(),
                clock: register.clock.clone(),
            })
            .collect()
    }

    /// Drops tombstones older than the retention window. New peers get the
    /// remaining state, and stale ops cannot resurrect a purged key because
    /// their clocks fall below the floor.
    pub fn purge_tombstones(&mut self, now_ms: i64) -> Result<usize, StoreError> {
        let floor = now_ms.saturating_sub(TOMBSTONE_RETENTION_MS);
        let before = self.registers.len();
        self.registers
            .retain(|_, register| register.value.is_some() || register.clock.wall_ms >= floor);
        let removed = before - self.registers.len();
        if removed > 0 {
            self.floor_ms = self.floor_ms.max(floor);
            self.save_state()?;
        }
        Ok(removed)
    }

    /// Folds the whole log into `state.json` and empties the log.
    pub fn compact(&mut self) -> Result<(), StoreError> {
        self.save_state()?;
        let path = self.dir.join("ops.jsonl");
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        self.log_bytes = 0;
        Ok(())
    }

    fn accept(&mut self, op: &Op) -> Result<bool, StoreError> {
        if op.clock.wall_ms < self.floor_ms {
            return Ok(false);
        }
        if let Some(value) = &op.value {
            validate_value(&op.key, value)?;
        }
        // Compare clocks per key. A device's high-water mark from another
        // key must not suppress this op: out-of-order delivery is normal.
        let known = self
            .registers
            .get(&op.key)
            .is_some_and(|register| register.clock >= op.clock);
        self.versions
            .entry(op.clock.device.clone())
            .and_modify(|clock| {
                if op.clock > *clock {
                    *clock = op.clock.clone();
                }
            })
            .or_insert_with(|| op.clock.clone());
        self.clock.observe(&op.clock);
        if known {
            return Ok(false);
        }
        self.registers.insert(
            op.key.clone(),
            Register {
                value: op.value.clone(),
                clock: op.clock.clone(),
            },
        );
        Ok(true)
    }

    fn replay_log(&mut self) -> Result<(), StoreError> {
        let path = self.dir.join("ops.jsonl");
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Op>(&line) {
                Ok(op) => {
                    self.accept(&op)?;
                }
                Err(error) => {
                    // A torn last line from a crash is expected; anything
                    // else is corruption worth surfacing.
                    log::warn!("sync: skipping unreadable op: {error}");
                }
            }
        }
        Ok(())
    }

    fn append(&mut self, op: &Op) -> Result<(), StoreError> {
        let path = self.dir.join("ops.jsonl");
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let line = serde_json::to_string(op)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        self.log_bytes = self.log_bytes.saturating_add(line.len() as u64 + 1);
        if self.log_bytes > COMPACT_AFTER_BYTES {
            self.compact()?;
        }
        Ok(())
    }

    fn refresh_log_bytes(&mut self) -> Result<(), StoreError> {
        self.log_bytes = match fs::metadata(self.dir.join("ops.jsonl")) {
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        if self.log_bytes > COMPACT_AFTER_BYTES {
            self.compact()?;
        }
        Ok(())
    }

    fn save_state(&self) -> Result<(), StoreError> {
        let persisted = Persisted {
            version: sync_format_version(),
            floor_ms: self.floor_ms,
            registers: self.registers.clone(),
            versions: self.versions.clone(),
        };
        let text = serde_json::to_string_pretty(&persisted)?;
        let target = self.dir.join("state.json");
        let temp = self.dir.join("state.json.tmp");
        let mut file = File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp, target)?;
        Ok(())
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_store_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = Store::open(dir.path(), "a").unwrap();
            store
                .set("thread/a/codex/t/title", json!("A title"))
                .unwrap();
            store.set("thread/a/codex/t/pinned", json!(true)).unwrap();
        }
        let store = Store::open(dir.path(), "a").unwrap();
        assert_eq!(
            store.value("thread/a/codex/t/title"),
            Some(&json!("A title"))
        );
        assert_eq!(store.value("thread/a/codex/t/pinned"), Some(&json!(true)));
    }

    #[test]
    fn last_writer_wins_and_deletes_stick() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), "a").unwrap();
        store.set("k", json!("old")).unwrap();
        store.set("k", json!("new")).unwrap();
        assert_eq!(store.value("k"), Some(&json!("new")));
        store.delete("k").unwrap();
        assert_eq!(store.value("k"), None);
        let vector = Vector::new();
        let missing = store.missing_for(&vector);
        assert!(missing.iter().any(|op| op.key == "k" && op.is_tombstone()));
    }

    #[test]
    fn duplicate_ops_do_not_apply_twice() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), "a").unwrap();
        let op = Op::set("k", json!(1), Hlc::new(10, 0, "b")).unwrap();
        assert_eq!(store.merge([op.clone()]).unwrap().len(), 1);
        assert_eq!(store.merge([op]).unwrap().len(), 0);
    }

    #[test]
    fn a_purged_tombstone_cannot_be_resurrected() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), "a").unwrap();
        let old = Op::delete("k", Hlc::new(1, 0, "a")).unwrap();
        store.merge([old.clone()]).unwrap();
        let later = TOMBSTONE_RETENTION_MS + 10;
        assert_eq!(store.purge_tombstones(later).unwrap(), 1);
        assert_eq!(store.merge([old]).unwrap().len(), 0);
        assert_eq!(store.value("k"), None);
    }

    #[test]
    fn compaction_keeps_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), "a").unwrap();
        store.set("k", json!("v")).unwrap();
        store.compact().unwrap();
        assert_eq!(store.value("k"), Some(&json!("v")));
        let reopened = Store::open(dir.path(), "a").unwrap();
        assert_eq!(reopened.value("k"), Some(&json!("v")));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config { cases: 4, ..Default::default() })]
        #[test]
        fn three_replicas_converge(
            writes in proptest::collection::vec(
                (0usize..8, 0u8..4, proptest::option::of("[a-z]{1,8}")),
                1..12,
            )
        ) {
            let dirs = [
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
            ];
            let mut stores: Vec<Store> = dirs
                .iter()
                .enumerate()
                .map(|(ix, dir)| Store::open(dir.path(), format!("d{ix}")).unwrap())
                .collect();
            let mut ops = Vec::new();
            for (device, (key_ix, kind, text)) in writes.into_iter().enumerate() {
                let store = &mut stores[device % 3];
                let key = format!("thread/d{}/{}/t/{kind}", device % 3, key_ix);
                let op = match text {
                    Some(text) if kind != 3 => store.set(key, json!(text)).unwrap(),
                    _ => store.delete(key).unwrap(),
                };
                ops.push(op);
            }
            // Every op reaches every replica once, twice, and out of order.
            for store in &mut stores {
                let mut shuffled = ops.clone();
                shuffled.reverse();
                store.merge(shuffled).unwrap();
                store.merge(ops.clone()).unwrap();
            }
            let first = stores[0].registers().clone();
            for store in &stores[1..] {
                proptest::prop_assert_eq!(store.registers(), &first);
            }
        }
    }
}
