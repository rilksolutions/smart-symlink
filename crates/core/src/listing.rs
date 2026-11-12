//! Directory listing with link detection.

use crate::fsinfo;
use crate::links::LinkKind;
use crate::util::{display_path, file_name_string, normalize_input};
use serde::Serialize;
use std::fs::{self, Metadata};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    pub kind: LinkKind,
    /// Absolute target path (symlinks / junctions)
    pub target: Option<String>,
    pub target_exists: bool,
    /// Hard links: number of names sharing the data
    pub link_count: Option<u32>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// File size (directories: None, computed separately)
    pub size: Option<u64>,
    /// Milliseconds since Unix epoch
    pub modified: Option<i64>,
    /// OS "hidden" attribute (Windows); always false on Unix
    pub hidden: bool,
    /// Name starts with a dot
    pub dot: bool,
    pub link: Option<LinkInfo>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Crumb {
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub crumbs: Vec<Crumb>,
    pub entries: Vec<Entry>,
    pub fs: String,
    pub free: Option<u64>,
    pub total: Option<u64>,
}

/// Above this many files we skip per-file hard-link counting on Windows (it needs a handle per file).
const HARDLINK_SCAN_LIMIT: usize = 5000;

pub fn list_dir(input: &str) -> Result<Listing, String> {
    let path = normalize_input(input);
    let rd = fs::read_dir(&path).map_err(|e| friendly_io(&e, &path))?;
    let raw: Vec<fs::DirEntry> = rd.filter_map(Result::ok).collect();
    let scan_hardlinks = raw.len() <= HARDLINK_SCAN_LIMIT;

    let entries = raw
        .iter()
        .filter_map(|de| {
            let p = de.path();
            let md = fs::symlink_metadata(&p).ok()?;
            Some(make_entry(&p, &md, scan_hardlinks))
        })
        .collect();

    let mut crumbs: Vec<Crumb> = path
        .ancestors()
        .map(|a| Crumb { name: file_name_string(a), path: display_path(a) })
        .collect();
    crumbs.reverse();

    let drives = fsinfo::list_drives();
    let space = fsinfo::space_for(&path, &drives);
    let fs = fsinfo::vol_info(&path).map(|v| v.label()).unwrap_or_default();

    Ok(Listing {
        path: display_path(&path),
        parent: path.parent().map(display_path),
        crumbs,
        entries,
        fs,
        free: space.map(|s| s.0),
        total: space.map(|s| s.1),
    })
}

pub fn make_entry(p: &Path, md: &Metadata, scan_hardlinks: bool) -> Entry {
    let followed = fs::metadata(p).ok();
    let is_dir = is_dir_like(p, md);
    let link = detect_link(p, md, scan_hardlinks);
    let size = if is_dir { None } else { Some(followed.as_ref().unwrap_or(md).len()) };
    let modified = followed
        .as_ref()
        .unwrap_or(md)
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);
    let name = file_name_string(p);
    Entry {
        dot: name.starts_with('.'),
        name,
        path: display_path(p),
        is_dir,
        size,
        modified,
        hidden: is_hidden(md),
        link,
    }
}

/// Directory, directory symlink or junction (without following broken links).
pub fn is_dir_like(p: &Path, md: &Metadata) -> bool {
    if md.is_dir() {
        return true;
    }
    if md.file_type().is_symlink() {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let _ = p;
            return md.file_attributes() & 0x10 != 0;
        }
        #[cfg(not(windows))]
        {
            return fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false);
        }
    }
    false
}

pub fn is_junction(p: &Path) -> bool {
    #[cfg(windows)]
    {
        junction::exists(p).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        false
    }
}

pub fn detect_link(p: &Path, md: &Metadata, scan_hardlinks: bool) -> Option<LinkInfo> {
    if md.file_type().is_symlink() {
        let kind = if is_junction(p) { LinkKind::Junction } else { LinkKind::Symlink };
        let target = fs::read_link(p).ok().map(|t| {
            let t = crate::util::clean_path(&t);
            if t.is_relative() {
                p.parent().map(|par| par.join(&t)).unwrap_or(t)
            } else {
                t
            }
        });
        return Some(LinkInfo {
            kind,
            target: target.as_deref().map(display_path),
            target_exists: fs::metadata(p).is_ok(),
            link_count: None,
        });
    }
    if md.is_file() && scan_hardlinks {
        if let Some(n) = fsinfo::link_count(p) {
            if n > 1 {
                return Some(LinkInfo {
                    kind: LinkKind::Hardlink,
                    target: None,
                    target_exists: true,
                    link_count: Some(n),
                });
            }
        }
    }
    None
}

fn is_hidden(md: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        md.file_attributes() & (0x2 | 0x4) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = md;
        false
    }
}

pub fn friendly_io(e: &std::io::Error, p: &Path) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        PermissionDenied => format!("Access denied: {}", display_path(p)),
        NotFound => format!("Not found: {}", display_path(p)),
        _ => format!("{}: {e}", display_path(p)),
    }
}
