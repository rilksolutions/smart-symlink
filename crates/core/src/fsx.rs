//! Folder listing that does not change the folder's "last accessed" date.
//!
//! Plain listing (FindFirstFile / readdir) counts as an access and bumps the date when
//! access-time updates are enabled. DirLinker lists folders constantly (browsing, folder
//! sizes, copying), so it uses this instead:
//! - Windows: open the folder, tell NTFS to not update the access time for this handle
//!   (`SetFileTime` with 0xFFFFFFFF), then enumerate through that handle.
//! - Linux: open the folder with `O_NOATIME` (allowed for the owner / root).
//! Falls back to normal listing when that isn't possible.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DirItem {
    pub name: OsString,
    pub path: PathBuf,
    /// Real folder (not a link to one)
    pub is_dir: bool,
    /// Symbolic link or junction
    pub is_link: bool,
    /// File size in bytes (0 for folders)
    pub len: u64,
}

pub fn list(p: &Path) -> io::Result<Vec<DirItem>> {
    if let Some(v) = imp::list_quiet(p) {
        return Ok(v);
    }
    let mut out = Vec::new();
    for e in fs::read_dir(p)? {
        let e = e?;
        let ft = e.file_type()?;
        let is_link = ft.is_symlink();
        let len = if !is_link && ft.is_file() { e.metadata().map(|m| m.len()).unwrap_or(0) } else { 0 };
        out.push(DirItem { name: e.file_name(), path: e.path(), is_dir: ft.is_dir() && !is_link, is_link, len });
    }
    Ok(out)
}

#[cfg(windows)]
mod imp {
    use super::DirItem;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandleEx, SetFileTime};

    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FULL_DIRECTORY_INFO: i32 = 14;
    const ERROR_NO_MORE_FILES: i32 = 18;

    pub fn list_quiet(p: &Path) -> Option<Vec<DirItem>> {
        let dir = std::fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE)
            .share_mode(7)
            .custom_flags(BACKUP_SEMANTICS)
            .open(p)
            .ok()?;
        let h = dir.as_raw_handle() as _;
        let keep = FILETIME { dwLowDateTime: u32::MAX, dwHighDateTime: u32::MAX };
        // SAFETY: valid handle; 0xFFFFFFFF/0xFFFFFFFF = "don't update last access through this handle".
        if unsafe { SetFileTime(h, std::ptr::null(), &keep, std::ptr::null()) } == 0 {
            return None;
        }
        let mut buf = vec![0u64; 8192]; // 64 KiB, 8-byte aligned
        let mut out = Vec::new();
        loop {
            // SAFETY: buffer is valid for its full length.
            let ok = unsafe { GetFileInformationByHandleEx(h, FILE_FULL_DIRECTORY_INFO, buf.as_mut_ptr() as _, (buf.len() * 8) as u32) };
            if ok == 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                    break;
                }
                return None;
            }
            let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 8) };
            let mut off = 0usize;
            loop {
                let rd32 = |o: usize| u32::from_le_bytes(bytes[off + o..off + o + 4].try_into().unwrap());
                let next = rd32(0) as usize;
                let size = u64::from_le_bytes(bytes[off + 40..off + 48].try_into().unwrap());
                let attrs = rd32(56);
                let name_len = rd32(60) as usize;
                let reparse_tag = rd32(64);
                let name_u16: Vec<u16> = bytes[off + 68..off + 68 + name_len]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let name = OsString::from_wide(&name_u16);
                if name != "." && name != ".." {
                    let is_reparse = attrs & 0x400 != 0;
                    let is_link = is_reparse && reparse_tag & 0x2000_0000 != 0; // name surrogate: symlink / junction
                    let is_dir = attrs & 0x10 != 0 && !is_link;
                    out.push(DirItem {
                        path: p.join(&name),
                        name,
                        is_dir,
                        is_link,
                        len: if is_dir || is_link { 0 } else { size },
                    });
                }
                if next == 0 {
                    break;
                }
                off += next;
            }
        }
        Some(out)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::DirItem;
    use std::ffi::{CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn list_quiet(p: &Path) -> Option<Vec<DirItem>> {
        let c = CString::new(p.as_os_str().as_bytes()).ok()?;
        // SAFETY: plain libc calls; the DIR stream owns the fd and is closed below.
        unsafe {
            let fd = libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOATIME | libc::O_CLOEXEC);
            if fd < 0 {
                return None;
            }
            let dir = libc::fdopendir(fd);
            if dir.is_null() {
                libc::close(fd);
                return None;
            }
            let mut out = Vec::new();
            loop {
                let ent = libc::readdir(dir);
                if ent.is_null() {
                    break;
                }
                let name = CStr::from_ptr((*ent).d_name.as_ptr());
                let bytes = name.to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                let path = p.join(OsStr::from_bytes(bytes));
                let Ok(md) = std::fs::symlink_metadata(&path) else { continue };
                let is_link = md.file_type().is_symlink();
                out.push(DirItem {
                    name: OsStr::from_bytes(bytes).to_os_string(),
                    is_dir: md.is_dir(),
                    is_link,
                    len: if md.is_file() { md.len() } else { 0 },
                    path,
                });
            }
            libc::closedir(dir);
            Some(out)
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod imp {
    pub fn list_quiet(_: &std::path::Path) -> Option<Vec<super::DirItem>> {
        None
    }
}
