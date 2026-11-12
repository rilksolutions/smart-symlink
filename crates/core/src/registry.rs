//! Persistent store: links created by the app, and Quick Access favorites.
//! Shared by GUI and TUI. Lives in the user's config dir as `state.json`.

use crate::fsinfo;
use crate::links::LinkKind;
use crate::listing::{self, Entry};
use crate::util::{clean_path, display_path};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Settings folder name (Linux convention: lowercase).
pub const APP_DIR: &str = if cfg!(any(windows, target_os = "macos")) { "Smart Symlink" } else { "smart-symlink" };
/// Folder used before the app was renamed; its state is copied over once.
const LEGACY_APP_DIR: &str = "DirLinker";
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SavedLink {
    /// Where the link lives
    pub link: String,
    /// Where the real data lives
    pub target: String,
    pub kind: LinkKind,
    /// Milliseconds since Unix epoch
    pub created: i64,
    /// true = data was moved ("Move & link back"), false = link-only
    pub moved: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    #[serde(default)]
    pub links: Vec<SavedLink>,
    #[serde(default)]
    pub favorites: Vec<String>,
}

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SMART_SYMLINK_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    config_base().join(APP_DIR)
}

fn config_base() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(std::env::temp_dir)
}

fn store_path() -> PathBuf {
    let p = config_dir().join("state.json");
    // one-time migration from the old app name (saved links + favorites)
    if std::env::var_os("SMART_SYMLINK_CONFIG_DIR").is_none() && !p.exists() {
        let legacy = config_base().join(LEGACY_APP_DIR).join("state.json");
        if legacy.exists() && fs::create_dir_all(config_dir()).is_ok() {
            let _ = fs::copy(&legacy, &p);
        }
    }
    p
}

pub fn load() -> Store {
    fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(s: &Store) -> Result<(), String> {
    let p = store_path();
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(s).unwrap()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

fn update(f: impl FnOnce(&mut Store)) -> Result<Store, String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load();
    f(&mut s);
    save(&s)?;
    Ok(s)
}

/// Path comparison (case-insensitive on Windows).
pub fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_end_matches(['\\', '/']), b.trim_end_matches(['\\', '/']));
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

pub fn add_link(l: SavedLink) {
    let _ = update(|s| {
        s.links.retain(|x| !same_path(&x.link, &l.link));
        s.links.push(l);
    });
}

pub fn forget_link(link: &str) -> Result<Store, String> {
    update(|s| s.links.retain(|x| !same_path(&x.link, link)))
}

/// Keep the store in sync when a path is renamed or moved (affects link and target paths below it).
pub fn path_moved(from: &str, to: &str) {
    let fix = |p: &mut String| {
        if same_path(p, from) {
            *p = to.to_string();
        } else if let Some(rest) = strip_prefix_ci(p, from) {
            *p = format!("{}{}", to.trim_end_matches(['\\', '/']), rest);
        }
    };
    let _ = update(|s| {
        for l in &mut s.links {
            fix(&mut l.link);
        }
        for f in &mut s.favorites {
            fix(f);
        }
    });
}

fn strip_prefix_ci<'a>(p: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.trim_end_matches(['\\', '/']);
    if p.len() > prefix.len() && p.get(..prefix.len()).is_some_and(|h| same_path(h, prefix)) {
        let rest = p.get(prefix.len()..)?;
        if rest.starts_with(['\\', '/']) {
            return Some(rest);
        }
    }
    None
}

pub fn set_favorite(path: &str, on: bool) -> Result<Store, String> {
    update(|s| {
        s.favorites.retain(|f| !same_path(f, path));
        if on {
            s.favorites.push(path.to_string());
        }
    })
}

/// Links whose real data lives at (or below) `path` — used to warn before rename/delete/move.
pub fn links_into(path: &str) -> Vec<SavedLink> {
    load()
        .links
        .into_iter()
        .filter(|l| same_path(&l.target, path) || strip_prefix_ci(&l.target, path).is_some())
        .collect()
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    /// Link exists and points to the saved target, which exists
    Ok,
    /// Link exists but its target is gone
    Broken,
    /// The link itself no longer exists
    Missing,
    /// Something else is at the link path now (not a link, or it points elsewhere)
    Changed,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SavedLinkView {
    pub saved: SavedLink,
    pub status: LinkStatus,
    /// Live entry for the link path (if it exists)
    pub entry: Option<Entry>,
}

pub fn status(l: &SavedLink) -> (LinkStatus, Option<Entry>) {
    let lp = Path::new(&l.link);
    let Ok(md) = fs::symlink_metadata(lp) else {
        return (LinkStatus::Missing, None);
    };
    let entry = listing::make_entry(lp, &md, true);
    let st = if l.kind == LinkKind::Hardlink {
        match (fsinfo::file_id(lp), fsinfo::file_id(Path::new(&l.target))) {
            (Some(a), Some(b)) if a == b => LinkStatus::Ok,
            (Some(_), None) => LinkStatus::Broken,
            _ => LinkStatus::Changed,
        }
    } else if l.kind == LinkKind::Bind {
        if Path::new(&l.target).exists() { LinkStatus::Ok } else { LinkStatus::Broken }
    } else if md.file_type().is_symlink() {
        let points = fs::read_link(lp).map(|t| display_path(&clean_path(&t))).unwrap_or_default();
        if !same_path(&points, &l.target) {
            LinkStatus::Changed
        } else if Path::new(&l.target).exists() {
            LinkStatus::Ok
        } else {
            LinkStatus::Broken
        }
    } else {
        LinkStatus::Changed
    };
    (st, Some(entry))
}

pub fn list_links() -> Vec<SavedLinkView> {
    let mut v: Vec<SavedLinkView> = load()
        .links
        .into_iter()
        .map(|saved| {
            let (status, entry) = status(&saved);
            SavedLinkView { saved, status, entry }
        })
        .collect();
    v.sort_by(|a, b| b.saved.created.cmp(&a.saved.created));
    v
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
