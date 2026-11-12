//! Which link types are possible for a selection, and why (not).

use crate::fsinfo::{self, VolInfo};
use crate::listing::is_dir_like;
use crate::util::{display_path, file_name_string};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Junction,
    Symlink,
    Hardlink,
    Clone,
    Bind,
    Snapshot,
}

/// `Move`: move the data into the destination and leave a link at the original place.
/// `Link`: leave the data where it is and create a link inside the destination.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Move,
    Link,
}

pub struct KindText {
    pub title: &'static str,
    pub short: &'static str,
    pub details: &'static str,
    pub use_when: &'static str,
}

pub fn text(kind: LinkKind) -> KindText {
    match kind {
        LinkKind::Junction => KindText {
            title: "Junction",
            short: "Folder redirect. Windows only, no admin needed.",
            details: "An NTFS directory junction transparently redirects a folder to another folder on a local drive. \
                      Programs keep using the old path and never notice the data lives elsewhere. Folders only; \
                      the target must be on a local drive (no network shares).",
            use_when: "Moving games, caches, Steam libraries or user folders from C: to another drive. \
                       The most compatible choice on Windows.",
        },
        LinkKind::Symlink => KindText {
            title: "Symbolic link",
            short: "Universal pointer for files and folders.",
            details: "A symbolic (soft) link stores the path of its target. Works for files and folders, across \
                      drives and to network shares. If the target is moved or deleted, the link breaks. On Windows \
                      it needs Developer Mode or administrator rights to create.",
            use_when: "Linking files, network locations, or anything on Linux/macOS — it's the standard link type there.",
        },
        LinkKind::Hardlink => KindText {
            title: "Hard link",
            short: "A second name for the same file data.",
            details: "A hard link is an additional name for exactly the same data: both names are equal, there is no \
                      'original'. The data lives until the last name is deleted. Files only, and both names must be \
                      on the same drive, so it can't move data elsewhere. Editors that save by replacing the file \
                      break the connection.",
            use_when: "Making the same file appear in several folders on one drive without using extra space.",
        },
        LinkKind::Clone => KindText {
            title: "Copy-on-write clone",
            short: "Instant copy that shares storage until changed.",
            details: "A clone (reflink / block clone / APFS clone) is an independent copy that initially shares all \
                      data blocks with the source: instant, and no extra space. Only changed blocks take new space. \
                      Unlike links, changes are NOT shared. Needs ReFS (Dev Drive), Btrfs, XFS or APFS and the same \
                      volume.",
            use_when: "Duplicating large folders or VM images instantly to experiment without touching the original.",
        },
        LinkKind::Bind => KindText {
            title: "Bind mount",
            short: "Mount a folder at a second path (Linux, root).",
            details: "The kernel makes a directory visible at a second location. Even programs that refuse to follow \
                      symlinks see a real folder. Works across filesystems. Requires root and is not persistent across \
                      reboots unless added to /etc/fstab (the line is shown after creation).",
            use_when: "Containers, chroots, or software that rejects symlinks.",
        },
        LinkKind::Snapshot => KindText {
            title: "Btrfs snapshot",
            short: "Instant writable snapshot of a subvolume.",
            details: "Creates a writable snapshot of a Btrfs subvolume: instant and space-sharing like a clone, but \
                      for a whole subvolume. The source must be a subvolume and the destination on the same Btrfs \
                      filesystem. Changes are not shared.",
            use_when: "Backups before an upgrade, or branching a whole project/data subvolume.",
        },
    }
}

/// Link kinds offered on the current OS, in display order.
pub fn kinds_for_os() -> &'static [LinkKind] {
    #[cfg(windows)]
    {
        &[LinkKind::Junction, LinkKind::Symlink, LinkKind::Hardlink, LinkKind::Clone]
    }
    #[cfg(target_os = "linux")]
    {
        &[LinkKind::Symlink, LinkKind::Hardlink, LinkKind::Clone, LinkKind::Bind, LinkKind::Snapshot]
    }
    #[cfg(all(not(windows), not(target_os = "linux")))]
    {
        &[LinkKind::Symlink, LinkKind::Hardlink, LinkKind::Clone]
    }
}

/// Kinds that create something new in the destination and always keep the source untouched.
pub fn keeps_source(kind: LinkKind) -> bool {
    matches!(kind, LinkKind::Hardlink | LinkKind::Clone | LinkKind::Snapshot)
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LinkOption {
    pub kind: LinkKind,
    pub title: String,
    pub short: String,
    pub details: String,
    pub use_when: String,
    pub available: bool,
    pub reason: Option<String>,
    pub needs_admin: bool,
    pub recommended: bool,
    pub keeps_source: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub options: Vec<LinkOption>,
    pub src_fs: String,
    pub dst_fs: String,
    pub same_volume: bool,
    pub all_dirs: bool,
    pub all_files: bool,
    pub recommended: Option<LinkKind>,
    /// Problems that make any operation impossible
    pub blocking: Vec<String>,
    pub warnings: Vec<String>,
    /// `dest/<name>` for each source, in order
    pub targets: Vec<String>,
}

pub fn analyze(sources: &[String], dest: &str, mode: Mode) -> Analysis {
    let srcs: Vec<PathBuf> = sources.iter().map(|s| crate::util::normalize_input(s)).collect();
    let dest_p = crate::util::normalize_input(dest);
    let mut blocking = Vec::new();
    let mut warnings = Vec::new();

    if srcs.is_empty() {
        blocking.push("Select one or more folders or files on the left.".to_string());
    }
    if dest.trim().is_empty() {
        blocking.push("Open a destination folder on the right.".to_string());
    } else if !dest_p.is_dir() {
        blocking.push(format!("Destination {} is not an accessible folder.", display_path(&dest_p)));
    }

    let (mut all_dirs, mut all_files, mut any_link) = (true, true, false);
    let mut targets = Vec::new();
    for s in &srcs {
        let name = file_name_string(s);
        match fs::symlink_metadata(s) {
            Ok(md) => {
                if is_dir_like(s, &md) {
                    all_files = false;
                } else {
                    all_dirs = false;
                }
                any_link |= md.file_type().is_symlink();
            }
            Err(_) => blocking.push(format!("{} no longer exists.", display_path(s))),
        }
        if s.parent().is_none() {
            blocking.push(format!("{} is a drive root and can't be linked.", display_path(s)));
            continue;
        }
        if !dest.trim().is_empty() {
            if dest_p.starts_with(s) {
                blocking.push(format!("The destination is inside '{name}'."));
            }
            if s.parent() == Some(dest_p.as_path()) {
                blocking.push(format!("'{name}' is already in the destination folder."));
            }
            let t = dest_p.join(s.file_name().unwrap_or_default());
            if fs::symlink_metadata(&t).is_ok() && s.parent() != Some(dest_p.as_path()) {
                blocking.push(format!("'{name}' already exists in the destination."));
            }
            targets.push(display_path(&t));
        }
    }
    if srcs.is_empty() {
        all_dirs = false;
        all_files = false;
    }

    let dvol = fsinfo::vol_info(&dest_p).unwrap_or_default();
    let svols: Vec<VolInfo> = srcs.iter().filter_map(|s| fsinfo::vol_info(s.parent().unwrap_or(s))).collect();
    let src_fs = svols.first().map(|v| v.label()).unwrap_or_default();
    let same_volume = !svols.is_empty() && !dest.trim().is_empty() && svols.iter().all(|v| v.same_volume(&dvol));

    // The link itself lives on the "host" volume; the data on the other one.
    let (host_symlinks, host_remote, host_fs, data_remote) = match mode {
        Mode::Move => (
            svols.iter().all(|v| v.symlinks),
            svols.iter().any(|v| v.remote),
            src_fs.clone(),
            dvol.remote,
        ),
        Mode::Link => (dvol.symlinks, dvol.remote, dvol.label(), svols.iter().any(|v| v.remote)),
    };
    let can_symlink = fsinfo::can_symlink();
    let root = fsinfo::is_root();

    let mut options: Vec<LinkOption> = Vec::new();
    for &kind in kinds_for_os() {
        let t = text(kind);
        let mut needs_admin = false;
        let moves_data = mode == Mode::Move && !keeps_source(kind);
        let reason: Option<String> = match kind {
            LinkKind::Junction => {
                if !all_dirs {
                    Some("Junctions work for folders only.".into())
                } else if !host_symlinks {
                    Some(format!("{host_fs} can't store junctions."))
                } else if host_remote {
                    Some("A junction must be created on a local drive.".into())
                } else if data_remote {
                    Some("Junctions can't point to network locations — use a symbolic link.".into())
                } else {
                    None
                }
            }
            LinkKind::Symlink => {
                if !host_symlinks {
                    Some(format!("{host_fs} doesn't support symbolic links."))
                } else if !can_symlink {
                    needs_admin = true;
                    Some("Needs Developer Mode (Settings › System › For developers) or running as administrator.".into())
                } else {
                    None
                }
            }
            LinkKind::Hardlink => {
                if !all_files {
                    Some("Hard links work for files only.".into())
                } else if !same_volume {
                    Some("Source and destination must be on the same drive.".into())
                } else if !dvol.hardlinks {
                    Some(format!("{} doesn't support hard links.", dvol.label()))
                } else {
                    None
                }
            }
            LinkKind::Clone => {
                if !same_volume {
                    Some("Source and destination must be on the same volume.".into())
                } else if !dvol.block_clone {
                    Some(format!("{} can't clone (needs ReFS, Btrfs, XFS or APFS).", dvol.label()))
                } else {
                    None
                }
            }
            LinkKind::Bind => {
                if !all_dirs {
                    Some("Bind mounts work for folders only.".into())
                } else if !root {
                    needs_admin = true;
                    Some("Needs root — start Smart Symlink with sudo.".into())
                } else {
                    None
                }
            }
            LinkKind::Snapshot => {
                if !dvol.fs.eq_ignore_ascii_case("btrfs") {
                    Some("Only available on Btrfs.".into())
                } else if !same_volume {
                    Some("Source and destination must be on the same Btrfs filesystem.".into())
                } else if !srcs.iter().all(|s| fsinfo::is_btrfs_subvolume(s)) {
                    Some("The selected folders are not Btrfs subvolumes.".into())
                } else {
                    None
                }
            }
        };
        let reason = reason.or_else(|| {
            (moves_data && any_link).then(|| "A selected item is already a link — it can't be moved.".to_string())
        });
        options.push(LinkOption {
            kind,
            title: t.title.into(),
            short: t.short.into(),
            details: t.details.into(),
            use_when: t.use_when.into(),
            available: reason.is_none(),
            reason,
            needs_admin,
            recommended: false,
            keeps_source: keeps_source(kind),
        });
    }

    let preference: &[LinkKind] = if cfg!(windows) {
        if all_dirs {
            &[LinkKind::Junction, LinkKind::Symlink]
        } else {
            &[LinkKind::Symlink, LinkKind::Hardlink]
        }
    } else {
        &[LinkKind::Symlink, LinkKind::Bind]
    };
    let recommended = preference
        .iter()
        .copied()
        .find(|k| options.iter().any(|o| o.kind == *k && o.available))
        .or_else(|| options.iter().find(|o| o.available).map(|o| o.kind));
    for o in &mut options {
        o.recommended = Some(o.kind) == recommended;
    }

    if blocking.is_empty() && mode == Mode::Move {
        if same_volume {
            warnings.push("Same drive: moving is instant (just a rename).".into());
        } else {
            warnings.push(format!(
                "Different drives: data is copied to {}, verified, and only then removed from the original place.",
                if dvol.root.is_empty() { "the destination".to_string() } else { dvol.root.clone() }
            ));
        }
    }

    Analysis {
        options,
        src_fs,
        dst_fs: dvol.label(),
        same_volume,
        all_dirs,
        all_files,
        recommended,
        blocking,
        warnings,
        targets,
    }
}
