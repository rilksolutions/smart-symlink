//! Recursive folder sizes: parallel (rayon), cancellable per "slot" (pane), cached.

use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::ops::Add;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

#[derive(Serialize, Clone, Copy, Default, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SizeInfo {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Folders that could not be read (access denied, ...)
    pub errors: u64,
}

impl Add for SizeInfo {
    type Output = SizeInfo;
    fn add(self, o: SizeInfo) -> SizeInfo {
        SizeInfo {
            bytes: self.bytes + o.bytes,
            files: self.files + o.files,
            dirs: self.dirs + o.dirs,
            errors: self.errors + o.errors,
        }
    }
}

pub const SLOTS: usize = 4;
static EPOCHS: [AtomicU64; SLOTS] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static CACHE: LazyLock<Mutex<HashMap<PathBuf, SizeInfo>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sub-folders up to this depth below a requested folder are cached too,
/// so opening a folder after its parent was measured is instant.
const CACHE_DEPTH: u32 = 3;

type Token = Option<(usize, u64)>;

fn cancelled(tok: &Token) -> bool {
    match tok {
        Some((slot, epoch)) => EPOCHS[*slot].load(Ordering::Relaxed) != *epoch,
        None => false,
    }
}

/// Cancel all running size computations for a slot (e.g. a pane navigated away).
pub fn cancel(slot: usize) {
    EPOCHS[slot % SLOTS].fetch_add(1, Ordering::Relaxed);
}

pub fn cached(path: &Path) -> Option<SizeInfo> {
    CACHE.lock().ok()?.get(path).copied()
}

/// Size of a folder. Returns `None` if cancelled.
pub fn dir_size(path: &Path, slot: Option<usize>) -> Option<SizeInfo> {
    if let Some(c) = cached(path) {
        return Some(c);
    }
    let tok = slot.map(|s| (s % SLOTS, EPOCHS[s % SLOTS].load(Ordering::Relaxed)));
    let r = walk(path, &tok, 0, true)?;
    Some(r)
}

/// Uncached, uncancellable measurement (used to verify copies). Works for files too.
pub fn measure(path: &Path) -> SizeInfo {
    match fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_symlink() => SizeInfo::default(),
        Ok(md) if md.is_dir() => walk(path, &None, 0, false).unwrap_or_default(),
        Ok(md) => SizeInfo { bytes: md.len(), files: 1, ..Default::default() },
        Err(_) => SizeInfo { errors: 1, ..Default::default() },
    }
}

fn walk(p: &Path, tok: &Token, depth: u32, use_cache: bool) -> Option<SizeInfo> {
    if cancelled(tok) {
        return None;
    }
    if use_cache && depth > 0 {
        if let Some(c) = cached(p) {
            return Some(c);
        }
    }
    let rd = match fs::read_dir(p) {
        Ok(r) => r,
        Err(_) => return Some(SizeInfo { errors: 1, ..Default::default() }),
    };
    let entries: Vec<fs::DirEntry> = rd.filter_map(Result::ok).collect();
    let total = entries
        .par_iter()
        .map(|e| -> Option<SizeInfo> {
            let Ok(ft) = e.file_type() else {
                return Some(SizeInfo { errors: 1, ..Default::default() });
            };
            if ft.is_symlink() {
                // never follow links / junctions: their data lives elsewhere
                Some(SizeInfo::default())
            } else if ft.is_dir() {
                walk(&e.path(), tok, depth + 1, use_cache).map(|mut s| {
                    s.dirs += 1;
                    s
                })
            } else {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                Some(SizeInfo { bytes: len, files: 1, ..Default::default() })
            }
        })
        .try_reduce(SizeInfo::default, |a, b| Some(a + b))?;

    if use_cache && depth <= CACHE_DEPTH && !cancelled(tok) {
        if let Ok(mut c) = CACHE.lock() {
            c.insert(p.to_path_buf(), total);
        }
    }
    Some(total)
}

pub fn clear_cache() {
    if let Ok(mut c) = CACHE.lock() {
        c.clear();
    }
}

/// Forget cached sizes for `path`, everything below it and all its ancestors.
pub fn invalidate(path: &Path) {
    if let Ok(mut c) = CACHE.lock() {
        c.retain(|k, _| !(k.starts_with(path) || path.starts_with(k)));
    }
}
