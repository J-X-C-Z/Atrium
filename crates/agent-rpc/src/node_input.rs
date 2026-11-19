//! Fail-fast accounting for unfinished, queued and in-flight attachment input.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(super) const MAX_LINE: usize = 64 * 1024 * 1024;
pub(super) const LINK_BYTES: usize = 128 * 1024 * 1024;
pub(super) const LINK_SLOTS: usize = 32;
pub(super) const CLIENT_BYTES: usize = 256 * 1024 * 1024;
pub(super) const CLIENT_SLOTS: usize = 1024;

pub(super) struct Budget {
    pub(super) bytes: AtomicUsize,
    pub(super) slots: AtomicUsize,
    max_bytes: usize,
    max_slots: usize,
}
impl Budget {
    pub(super) fn new(bytes: usize, slots: usize) -> Arc<Self> {
        Arc::new(Self {
            bytes: AtomicUsize::new(0),
            slots: AtomicUsize::new(0),
            max_bytes: bytes,
            max_slots: slots,
        })
    }
    fn reserve(self: &Arc<Self>, bytes: usize, slots: usize) -> Option<Credit> {
        let add = |value: &AtomicUsize, n: usize, max: usize| {
            let mut current = value.load(Ordering::Acquire);
            loop {
                let Some(next) = current.checked_add(n).filter(|v| *v <= max) else {
                    return false;
                };
                match value.compare_exchange_weak(
                    current,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return true,
                    Err(observed) => current = observed,
                }
            }
        };
        if !add(&self.bytes, bytes, self.max_bytes) {
            return None;
        }
        if !add(&self.slots, slots, self.max_slots) {
            self.bytes.fetch_sub(bytes, Ordering::AcqRel);
            return None;
        }
        Some(Credit {
            budget: self.clone(),
            bytes,
            slots,
        })
    }
}
struct Credit {
    budget: Arc<Budget>,
    bytes: usize,
    slots: usize,
}
impl Credit {
    fn empty(budget: Arc<Budget>) -> Self {
        Self {
            budget,
            bytes: 0,
            slots: 0,
        }
    }
    fn merge(&mut self, mut other: Self) {
        debug_assert!(Arc::ptr_eq(&self.budget, &other.budget));
        self.bytes += other.bytes;
        self.slots += other.slots;
        other.bytes = 0;
        other.slots = 0;
    }
    fn split(&mut self, bytes: usize, slots: usize) -> Self {
        assert!(bytes <= self.bytes && slots <= self.slots);
        self.bytes -= bytes;
        self.slots -= slots;
        Self {
            budget: self.budget.clone(),
            bytes,
            slots,
        }
    }
}
impl Drop for Credit {
    fn drop(&mut self) {
        self.budget.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.budget.slots.fetch_sub(self.slots, Ordering::AcqRel);
    }
}

pub(super) struct Credits {
    local: Credit,
    shared: Credit,
}
impl Credits {
    pub(super) fn empty(local: Arc<Budget>, shared: Arc<Budget>) -> Self {
        Self {
            local: Credit::empty(local),
            shared: Credit::empty(shared),
        }
    }
    pub(super) fn acquire(&self, bytes: usize, slots: usize) -> Option<Self> {
        let local = self.local.budget.reserve(bytes, slots)?;
        let shared = self.shared.budget.reserve(bytes, slots)?;
        Some(Self { local, shared })
    }
    pub(super) fn merge(&mut self, other: Self) {
        self.local.merge(other.local);
        self.shared.merge(other.shared);
    }
    pub(super) fn split(&mut self, bytes: usize, slots: usize) -> Self {
        Self {
            local: self.local.split(bytes, slots),
            shared: self.shared.split(bytes, slots),
        }
    }
    pub(super) fn clear(&mut self) {
        let empty = Self::empty(self.local.budget.clone(), self.shared.budget.clone());
        *self = empty;
    }
}

pub(super) struct InputLine {
    pub(super) line: String,
    pub(super) _credits: Credits,
}

/// Exact length of from_utf8_lossy without allocating the expanded string.
pub(super) fn lossy_len(mut bytes: &[u8]) -> usize {
    let mut size = 0;
    loop {
        match std::str::from_utf8(bytes) {
            Ok(text) => return size + text.len(),
            Err(error) => {
                size += error.valid_up_to() + 3;
                let rest = &bytes[error.valid_up_to()..];
                match error.error_len() {
                    Some(n) => bytes = &rest[n..],
                    None => return size,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn length_preflight_matches_lossy_conversion_for_byte_pairs_and_truncated_utf8() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let bytes = [a, b];
                assert_eq!(lossy_len(&bytes), String::from_utf8_lossy(&bytes).len());
            }
        }
        for bytes in [
            b"\xf0\x9f\x92".as_slice(),
            b"\xe2\x82\xac",
            b"ok\xff\xf0\x9f\x92\xa9\xfe",
        ] {
            assert_eq!(lossy_len(bytes), String::from_utf8_lossy(bytes).len());
        }
    }
}
