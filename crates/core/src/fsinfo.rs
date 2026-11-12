//! Drives, volumes and filesystem capabilities.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DriveInfo {
    /// Volume label (Windows) or device name (Unix)
    pub name: String,
    /// Mount point, e.g. `C:\` or `/home`
    pub mount: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub removable: bool,
    /// "ssd" | "hdd" | "unknown"
    pub kind: String,
}

/// Filesystem types that are pseudo / system mounts and not interesting to browse.
#[cfg(unix)]
const HIDDEN_FS: &[&str] = &[
    "squashfs", "overlay", "tmpfs", "devtmpfs", "proc", "sysfs", "efivarfs", "autofs", "devfs", "nullfs",
];

pub fn list_drives() -> Vec<DriveInfo> {
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else {
        return Vec::new();
    };
    let mut out: Vec<DriveInfo> = Vec::new();
    for d in disks.iter() {
        let mount = d.mount_point().to_string_lossy().to_string();
        let fs = d.file_system().to_string_lossy().to_string();
        #[cfg(unix)]
        {
            if HIDDEN_FS.contains(&fs.as_str()) || mount.starts_with("/snap") || mount.starts_with("/var/lib/") {
                continue;
            }
            if cfg!(target_os = "macos") && mount.starts_with("/System/Volumes/") && mount != "/System/Volumes/Data" {
                continue;
            }
        }
        if out.iter().any(|o| o.mount == mount) {
            continue;
        }
        let kind = match d.kind() {
            sysinfo::DiskKind::SSD => "ssd",
            sysinfo::DiskKind::HDD => "hdd",
            _ => "unknown",
        };
        out.push(DriveInfo {
            name: d.name().to_string_lossy().to_string(),
            mount,
            fs,
            total: d.total_space(),
            free: d.available_space(),
            removable: d.is_removable(),
            kind: kind.into(),
        });
    }
    out.sort_by(|a, b| a.mount.to_lowercase().cmp(&b.mount.to_lowercase()));
    out
}

/// Free / total space of the drive containing `path` (longest mount-point match).
pub fn space_for(path: &Path, drives: &[DriveInfo]) -> Option<(u64, u64)> {
    let p = crate::util::display_path(path);
    drives
        .iter()
        .filter(|d| path_starts_with(&p, &d.mount))
        .max_by_key(|d| d.mount.len())
        .map(|d| (d.free, d.total))
}

fn path_starts_with(path: &str, mount: &str) -> bool {
    #[cfg(windows)]
    {
        path.to_lowercase().starts_with(&mount.to_lowercase())
    }
    #[cfg(not(windows))]
    {
        Path::new(path).starts_with(mount)
    }
}

/// Everything we need to know about the volume a path lives on.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct VolInfo {
    /// Filesystem name, e.g. NTFS, ReFS, exFAT, ext4, btrfs, apfs
    pub fs: String,
    /// Volume root / mount point
    pub root: String,
    /// Volume serial (Windows) or device id (Unix)
    pub id: u64,
    pub remote: bool,
    pub symlinks: bool,
    pub hardlinks: bool,
    pub block_clone: bool,
}

impl VolInfo {
    pub fn same_volume(&self, other: &VolInfo) -> bool {
        #[cfg(windows)]
        {
            self.id == other.id && self.root.eq_ignore_ascii_case(&other.root)
        }
        #[cfg(not(windows))]
        {
            self.id == other.id
        }
    }

    pub fn label(&self) -> String {
        if self.fs.is_empty() {
            "unknown".into()
        } else if self.remote {
            format!("{} (network)", self.fs)
        } else {
            self.fs.clone()
        }
    }
}

/// Walk up until an existing path is found (so we can inspect not-yet-created targets).
fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut p = Some(path);
    while let Some(cur) = p {
        if std::fs::symlink_metadata(cur).is_ok() {
            return Some(cur.to_path_buf());
        }
        p = cur.parent();
    }
    None
}

#[cfg(windows)]
pub fn vol_info(path: &Path) -> Option<VolInfo> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW};

    const DRIVE_REMOTE: u32 = 4;
    const FILE_SUPPORTS_REPARSE_POINTS: u32 = 0x0000_0080;
    const FILE_SUPPORTS_HARD_LINKS: u32 = 0x0040_0000;
    const FILE_SUPPORTS_BLOCK_REFCOUNTING: u32 = 0x0800_0000;

    let path = existing_ancestor(path)?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut root = [0u16; 1024];
    // SAFETY: buffers are valid and sized as declared.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let mut serial = 0u32;
    let mut max_len = 0u32;
    let mut flags = 0u32;
    let mut fs_name = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            &mut max_len,
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let remote = unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOTE;
    let fs = from_wide(&fs_name);
    Some(VolInfo {
        root: crate::util::clean(&from_wide(&root)),
        id: serial as u64,
        remote,
        symlinks: flags & FILE_SUPPORTS_REPARSE_POINTS != 0,
        hardlinks: flags & FILE_SUPPORTS_HARD_LINKS != 0,
        block_clone: flags & FILE_SUPPORTS_BLOCK_REFCOUNTING != 0,
        fs,
    })
}

#[cfg(windows)]
fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(unix)]
pub fn vol_info(path: &Path) -> Option<VolInfo> {
    use std::os::unix::fs::MetadataExt;
    let path = existing_ancestor(path)?;
    let md = std::fs::metadata(&path).or_else(|_| std::fs::symlink_metadata(&path)).ok()?;
    let canon = std::fs::canonicalize(&path).unwrap_or(path.clone());
    let (fs, root) = unix_fs_type(&canon).unwrap_or_else(|| ("unknown".into(), "/".into()));
    let f = fs.as_str();
    let remote = matches!(
        f,
        "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "fuse.sshfs" | "sshfs" | "9p" | "afpfs" | "webdav" | "davfs"
    );
    let no_links = matches!(f, "vfat" | "msdos" | "exfat" | "fat" | "fat32" | "fuseblk" | "fuse.exfat");
    let block_clone = matches!(f, "btrfs" | "xfs" | "apfs" | "bcachefs" | "ocfs2");
    Some(VolInfo {
        fs,
        root,
        id: md.dev(),
        remote,
        symlinks: !no_links,
        hardlinks: !no_links,
        block_clone,
    })
}

/// Linux: read `/proc/self/mounts` and pick the longest matching mount point.
#[cfg(target_os = "linux")]
fn unix_fs_type(path: &Path) -> Option<(String, String)> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let mut best: Option<(String, String)> = None;
    for line in mounts.lines() {
        let mut it = line.split_whitespace();
        let (_dev, mnt, fs) = (it.next()?, it.next()?, it.next()?);
        let mnt = mnt.replace("\\040", " ").replace("\\011", "\t");
        if path.starts_with(&mnt) && best.as_ref().map_or(true, |(_, b)| mnt.len() >= b.len()) {
            best = Some((fs.to_string(), mnt));
        }
    }
    best
}

/// macOS / other Unix: use the disk list.
#[cfg(all(unix, not(target_os = "linux")))]
fn unix_fs_type(path: &Path) -> Option<(String, String)> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| {
            (
                d.file_system().to_string_lossy().to_lowercase(),
                d.mount_point().to_string_lossy().to_string(),
            )
        })
}

/// Can this process create symbolic links? (Windows: needs Developer Mode or admin.)
pub fn can_symlink() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        #[cfg(windows)]
        {
            let probe = std::env::temp_dir().join(format!("dirlinker-probe-{}", std::process::id()));
            let _ = std::fs::remove_file(&probe);
            let ok = std::os::windows::fs::symlink_file("dirlinker-probe-target", &probe).is_ok();
            let _ = std::fs::remove_file(&probe);
            ok
        }
        #[cfg(not(windows))]
        {
            true
        }
    })
}

pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(windows)]
fn handle_info(path: &Path) -> Option<windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let f = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    // SAFETY: zeroed POD struct, valid handle owned by `f`.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    if ok == 0 { None } else { Some(info) }
}

/// Number of hard links pointing at a file's data.
#[cfg(windows)]
pub fn link_count(path: &Path) -> Option<u32> {
    handle_info(path).map(|i| i.nNumberOfLinks)
}

/// Identity of the underlying file data: (volume, file index). Equal ids = same data (hard links).
#[cfg(windows)]
pub fn file_id(path: &Path) -> Option<(u64, u64)> {
    handle_info(path).map(|i| (i.dwVolumeSerialNumber as u64, ((i.nFileIndexHigh as u64) << 32) | i.nFileIndexLow as u64))
}

#[cfg(unix)]
pub fn link_count(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| m.nlink() as u32)
}

#[cfg(unix)]
pub fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Btrfs subvolumes always have inode number 256.
pub fn is_btrfs_subvolume(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).map(|m| m.ino() == 256 && m.is_dir()).unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}
