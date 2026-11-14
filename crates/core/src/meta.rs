//! Full-fidelity metadata: capture everything about a file/folder before copying,
//! apply it to the copy, then read it back and compare. Any difference is an error.
//!
//! Windows: created / accessed / modified time, attributes (read-only, hidden, system,
//! archive, not-indexed, compressed, ...), and the complete security descriptor
//! (owner, group, permissions/DACL, and auditing/SACL when running as administrator).
//! File contents incl. alternate data streams and extended attributes are copied by
//! `CopyFileExW` (used by `std::fs::copy`).
//!
//! Unix: accessed / modified time (nanoseconds), mode incl. setuid/setgid/sticky, owner,
//! group, and all extended attributes (which carry POSIX ACLs, SELinux labels, macOS
//! Finder info and quarantine flags). The creation ("birth") time can't be set on Linux
//! by any program; on macOS it is kept by the system copy routine.

use std::io;
use std::path::Path;

pub use imp::{Snapshot, capture, create_dir_like, privileges_note};

/// Compare `dst` with a snapshot; Err lists what differs.
pub fn verify(dst: &Path, snap: &Snapshot) -> Result<(), String> {
    imp::verify(dst, snap)
}

/// Apply `snap` to `dst`, then verify. Errors name the path and what could not be kept.
pub fn apply_and_verify(dst: &Path, snap: &Snapshot) -> Result<(), String> {
    imp::apply(dst, snap).map_err(|e| describe(dst, "could not keep", &e))?;
    imp::verify(dst, snap).map_err(|what| format!("{}: {what} changed while copying — refusing to continue.", crate::util::display_path(dst)))
}

/// Re-apply only the dates (used when another program bumped the "accessed" date).
pub fn reapply_dates(dst: &Path, snap: &Snapshot) -> Result<(), String> {
    imp::set_dates(dst, snap).map_err(|e| format!("{}: could not restore the dates ({e})", crate::util::display_path(dst)))
}

/// Compare two files byte by byte.
pub fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    use std::io::Read;
    let mut fa = io::BufReader::with_capacity(1 << 20, std::fs::File::open(a)?);
    let mut fb = io::BufReader::with_capacity(1 << 20, std::fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
    fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match r.read(&mut buf[n..])? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }
}

fn describe(p: &Path, verb: &str, e: &MetaError) -> String {
    let admin = if cfg!(windows) { "Run Smart Symlink as administrator" } else { "Run Smart Symlink with sudo" };
    match e {
        MetaError::Privilege(what) => format!(
            "{}: {verb} the {what} — this needs elevated rights. {admin} so nothing about the files changes. Nothing was changed.",
            crate::util::display_path(p)
        ),
        MetaError::Io(what, err) => format!("{}: {verb} the {what} ({err}). Nothing was changed.", crate::util::display_path(p)),
    }
}

#[derive(Debug)]
pub enum MetaError {
    /// Needs administrator / root
    Privilege(&'static str),
    Io(&'static str, io::Error),
}

impl MetaError {
    fn from_io(what: &'static str, e: io::Error) -> Self {
        let denied = e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(1307 | 1308 | 1314));
        if denied { MetaError::Privilege(what) } else { MetaError::Io(what, e) }
    }
}

// ====================================================================== Windows

#[cfg(windows)]
mod imp {
    //! Everything goes through file handles on which "last access" updates are switched off
    //! right after opening (SetFileTime with 0xFFFFFFFF), so Smart Symlink's own reads and
    //! writes never count as an access to the file or folder.
    use super::MetaError;
    use std::fs;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GetLastError, HANDLE, LUID, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, GetKernelObjectSecurity, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SetKernelObjectSecurity,
        TOKEN_PRIVILEGES,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateDirectoryExW, FILE_BASIC_INFO, GetFileInformationByHandle, SetFileInformationByHandle,
        SetFileTime,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const OWNER: u32 = 0x1;
    const GROUP: u32 = 0x2;
    const DACL: u32 = 0x4;
    const SACL: u32 = 0x8;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const ACCESS_SYSTEM_SECURITY: u32 = 0x0100_0000;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x800;
    /// Attributes that can be set directly
    const SETTABLE: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x100 | 0x1000 | 0x2000;
    /// Attributes we compare (settable + compression + encryption)
    const COMPARED: u32 = SETTABLE | FILE_ATTRIBUTE_COMPRESSED | 0x4000;
    const FSCTL_SET_COMPRESSION: u32 = 0x0009_C040;
    const FILE_BASIC_INFO_CLASS: i32 = 0;

    #[derive(Clone, Debug)]
    pub struct Snapshot {
        created: u64,
        accessed: u64,
        written: u64,
        attrs: u32,
        is_link: bool,
        /// self-relative security descriptor + which parts it contains
        sd: Option<(Vec<u8>, u32)>,
    }

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    fn last_err() -> io::Error {
        io::Error::last_os_error()
    }

    /// Enable backup/restore/security privileges if the token has them (i.e. running as admin).
    /// Returns (can set any owner, can read+write auditing SACL).
    fn privileges() -> (bool, bool) {
        static P: OnceLock<(bool, bool)> = OnceLock::new();
        *P.get_or_init(|| {
            let mut token: HANDLE = std::ptr::null_mut();
            // SAFETY: plain Win32 calls with valid out-pointers.
            unsafe {
                if OpenProcessToken(GetCurrentProcess(), 0x0020 | 0x0008, &mut token) == 0 {
                    return (false, false);
                }
                let enable = |name: &str| -> bool {
                    let n: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                    let mut luid = LUID { LowPart: 0, HighPart: 0 };
                    if LookupPrivilegeValueW(std::ptr::null(), n.as_ptr(), &mut luid) == 0 {
                        return false;
                    }
                    let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: 0x2 }] };
                    AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut()) != 0 && GetLastError() == 0
                };
                let backup = enable("SeBackupPrivilege");
                let restore = enable("SeRestorePrivilege");
                let security = enable("SeSecurityPrivilege");
                CloseHandle(token);
                (backup && restore, security)
            }
        })
    }

    pub fn privileges_note() -> Option<&'static str> {
        if privileges().1 { None } else { Some("Auditing entries (SACL) can only be read and kept when running as administrator.") }
    }

    fn sec_info() -> u32 {
        OWNER | GROUP | DACL | if privileges().1 { SACL } else { 0 }
    }

    /// Open with the given access and immediately stop "last access" updates through this handle.
    /// Falls back to an un-frozen handle without FILE_WRITE_ATTRIBUTES if that right isn't granted.
    fn open_quiet(p: &Path, access: u32) -> io::Result<fs::File> {
        let open = |a: u32| fs::OpenOptions::new().access_mode(a).share_mode(7).custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT).open(p);
        match open(access | FILE_WRITE_ATTRIBUTES) {
            Ok(f) => {
                let keep = FILETIME { dwLowDateTime: u32::MAX, dwHighDateTime: u32::MAX };
                // SAFETY: valid handle.
                unsafe { SetFileTime(f.as_raw_handle() as _, std::ptr::null(), &keep, std::ptr::null()) };
                Ok(f)
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && access & FILE_WRITE_ATTRIBUTES == 0 => open(access),
            Err(e) => Err(e),
        }
    }

    fn handle_info(f: &fs::File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        // SAFETY: zeroed POD out-struct, valid handle.
        let mut i: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut i) } == 0 {
            return Err(last_err());
        }
        Ok(i)
    }

    fn get_sd(f: &fs::File, info: u32) -> io::Result<Vec<u8>> {
        let h = f.as_raw_handle() as _;
        let mut need = 0u32;
        // SAFETY: size query, then fill a buffer of the reported size.
        unsafe {
            GetKernelObjectSecurity(h, info, std::ptr::null_mut(), 0, &mut need);
            if need == 0 {
                return Err(last_err());
            }
            let mut buf = vec![0u8; need as usize];
            if GetKernelObjectSecurity(h, info, buf.as_mut_ptr() as _, need, &mut need) == 0 {
                return Err(last_err());
            }
            Ok(buf)
        }
    }

    fn set_sd(f: &fs::File, info: u32, sd: &[u8]) -> io::Result<()> {
        // SAFETY: sd is a valid self-relative security descriptor.
        if unsafe { SetKernelObjectSecurity(f.as_raw_handle() as _, info, sd.as_ptr() as _) } == 0 {
            return Err(last_err());
        }
        Ok(())
    }

    fn sddl(sd: &[u8], info: u32) -> String {
        let mut out: *mut u16 = std::ptr::null_mut();
        // SAFETY: sd is a valid self-relative descriptor; out is freed with LocalFree.
        unsafe {
            if ConvertSecurityDescriptorToStringSecurityDescriptorW(sd.as_ptr() as _, 1, info, &mut out, std::ptr::null_mut()) == 0 {
                return String::new();
            }
            let mut len = 0;
            while *out.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, len));
            LocalFree(out as _);
            s
        }
    }

    fn ft64(t: FILETIME) -> u64 {
        ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
    }

    fn read(p: &Path, with_sd: bool) -> io::Result<(BY_HANDLE_FILE_INFORMATION, Option<(Vec<u8>, u32)>)> {
        let info = sec_info();
        let mut access = FILE_READ_ATTRIBUTES;
        if with_sd {
            access |= READ_CONTROL | if info & SACL != 0 { ACCESS_SYSTEM_SECURITY } else { 0 };
        }
        let f = open_quiet(p, access)?;
        let i = handle_info(&f)?;
        let sd = if with_sd { Some((get_sd(&f, info)?, info)) } else { None };
        Ok((i, sd))
    }

    pub fn capture(p: &Path) -> io::Result<Snapshot> {
        let is_link = fs::symlink_metadata(p)?.file_type().is_symlink();
        let (i, sd) = read(p, !is_link)?;
        Ok(Snapshot {
            created: ft64(i.ftCreationTime),
            accessed: ft64(i.ftLastAccessTime),
            written: ft64(i.ftLastWriteTime),
            attrs: i.dwFileAttributes,
            is_link,
            sd,
        })
    }

    /// Create `dst` as a directory with the same attributes (incl. compression) as `src`.
    pub fn create_dir_like(src: &Path, dst: &Path) -> io::Result<()> {
        let (s, d) = (wide(src), wide(dst));
        // SAFETY: valid null-terminated paths; default security (the real descriptor is applied later).
        if unsafe { CreateDirectoryExW(s.as_ptr(), d.as_ptr(), std::ptr::null()) } == 0 {
            return Err(last_err());
        }
        Ok(())
    }

    fn set_compression(p: &Path, on: bool) -> io::Result<()> {
        let f = open_quiet(p, 0x8000_0000 | 0x4000_0000)?; // GENERIC_READ | GENERIC_WRITE
        let format: u16 = if on { 1 } else { 0 };
        let mut ret = 0u32;
        // SAFETY: valid handle and in-buffer.
        let ok = unsafe {
            DeviceIoControl(f.as_raw_handle() as _, FSCTL_SET_COMPRESSION, &format as *const u16 as _, 2, std::ptr::null_mut(), 0, &mut ret, std::ptr::null_mut())
        };
        if ok == 0 { Err(last_err()) } else { Ok(()) }
    }

    fn set_basic(f: &fs::File, s: &Snapshot, attrs: u32) -> io::Result<()> {
        let a = attrs & SETTABLE;
        let bi = FILE_BASIC_INFO {
            CreationTime: s.created as i64,
            LastAccessTime: s.accessed as i64,
            LastWriteTime: s.written as i64,
            ChangeTime: 0, // 0 = leave unchanged
            FileAttributes: if a == 0 { FILE_ATTRIBUTE_NORMAL } else { a },
        };
        // SAFETY: valid handle and correctly sized struct.
        let ok = unsafe {
            SetFileInformationByHandle(f.as_raw_handle() as _, FILE_BASIC_INFO_CLASS, &bi as *const _ as _, std::mem::size_of::<FILE_BASIC_INFO>() as u32)
        };
        if ok == 0 { Err(last_err()) } else { Ok(()) }
    }

    pub fn apply(dst: &Path, s: &Snapshot) -> Result<(), MetaError> {
        privileges();
        // compression state (not settable as a plain attribute); needs the read-only flag cleared first
        if !s.is_link {
            let cur = read(dst, false).map_err(|e| MetaError::Io("attributes", e))?.0.dwFileAttributes;
            if (cur ^ s.attrs) & FILE_ATTRIBUTE_COMPRESSED != 0 {
                let f = open_quiet(dst, FILE_WRITE_ATTRIBUTES).map_err(|e| MetaError::from_io("attributes", e))?;
                set_basic(&f, s, 0).map_err(|e| MetaError::from_io("attributes", e))?;
                drop(f);
                set_compression(dst, s.attrs & FILE_ATTRIBUTE_COMPRESSED != 0).map_err(|e| MetaError::from_io("compression", e))?;
            }
        }

        // one handle for permissions + dates + attributes. Access rights are checked when opening,
        // so this handle keeps them even if the permissions we apply would deny them.
        let (info, sd) = match &s.sd {
            Some((sd, info)) => (*info, Some(sd)),
            None => (0, None),
        };
        let mut access = FILE_WRITE_ATTRIBUTES | FILE_READ_ATTRIBUTES;
        if sd.is_some() {
            access |= READ_CONTROL | WRITE_DAC | if info & SACL != 0 { ACCESS_SYSTEM_SECURITY } else { 0 };
        }
        let f = open_quiet(dst, access).map_err(|e| MetaError::from_io("dates", e))?;

        if let Some(sd) = sd {
            // 1. permissions (+ auditing) first: the original ACL is what grants us the right to change the owner
            // Auditing entries (only readable as administrator): a new file already carries the same
            // "auto-inherited, no entries" state as most originals. Setting an empty list again would
            // drop that flag, and it can't be put back without entries, so leave it when it already matches.
            let mut acl_info = info & DACL;
            if info & SACL != 0 && !get_sd(&f, SACL).is_ok_and(|now| sddl(&now, SACL) == sddl(sd, SACL)) {
                acl_info |= SACL;
            }
            set_sd(&f, acl_info, sd).map_err(|e| MetaError::from_io("permissions (ACL)", e))?;
            // The plain set drops the "auto-inherited" (AI) flag; setting the identical ACL again with
            // the matching "auto-inherit request" bit restores it (the ACEs are already identical).
            if let Some(req) = with_auto_inherit_request(sd, acl_info) {
                set_sd(&f, acl_info, &req).map_err(|e| MetaError::from_io("permissions (ACL)", e))?;
            }
            // 2. owner + group, only if they differ from what the copy already has
            let og = OWNER | GROUP;
            let differs = get_sd(&f, og).map(|now| sddl(&now, og) != sddl(sd, og)).unwrap_or(true);
            if differs {
                let fo = open_quiet(dst, WRITE_OWNER | READ_CONTROL).map_err(|e| MetaError::from_io("owner", e))?;
                set_sd(&fo, og, sd).map_err(|e| MetaError::from_io("owner", e))?;
            }
        }
        // 3. dates + attributes last (in one call)
        set_basic(&f, s, s.attrs).map_err(|e| MetaError::from_io("dates", e))?;
        Ok(())
    }

    pub fn set_dates(dst: &Path, s: &Snapshot) -> io::Result<()> {
        let f = open_quiet(dst, FILE_WRITE_ATTRIBUTES)?;
        set_basic(&f, s, s.attrs)
    }

    pub fn verify(dst: &Path, s: &Snapshot) -> Result<(), String> {
        let (i, sd) = read(dst, s.sd.is_some()).map_err(|e| e.to_string())?;
        let mut diff = Vec::new();
        if ft64(i.ftCreationTime) != s.created {
            diff.push("created date");
        }
        if ft64(i.ftLastWriteTime) != s.written {
            diff.push("modified date");
        }
        if ft64(i.ftLastAccessTime) != s.accessed {
            diff.push("accessed date");
        }
        if i.dwFileAttributes & COMPARED != s.attrs & COMPARED {
            diff.push("attributes");
        }
        let mut detail = String::new();
        if let (Some((want, info)), Some((now, _))) = (&s.sd, &sd) {
            for (part, name) in [(OWNER | GROUP, "owner / group"), (DACL, "permissions"), (SACL, "auditing entries")] {
                if info & part != 0 && sddl(now, part) != sddl(want, part) {
                    diff.push(name);
                    if std::env::var_os("SMART_SYMLINK_VERBOSE").is_some() {
                        detail += &format!(" [{name}: expected {} but found {}]", sddl(want, part), sddl(now, part));
                    }
                }
            }
        }
        if diff.is_empty() { Ok(()) } else { Err(diff.join(", ") + &detail) }
    }

    /// Copy of a self-relative SD whose AI flags are turned into "auto-inherit request" bits
    /// (control word at offset 2). None if the SD has no AI flags.
    fn with_auto_inherit_request(sd: &[u8], info: u32) -> Option<Vec<u8>> {
        const DACL_AI_REQ: u16 = 0x0100;
        const SACL_AI_REQ: u16 = 0x0200;
        const DACL_AI: u16 = 0x0400;
        const SACL_AI: u16 = 0x0800;
        if sd.len() < 4 {
            return None;
        }
        let control = u16::from_le_bytes([sd[2], sd[3]]);
        let mut add = 0;
        if info & DACL != 0 && control & DACL_AI != 0 {
            add |= DACL_AI_REQ;
        }
        if info & SACL != 0 && control & SACL_AI != 0 {
            add |= SACL_AI_REQ;
        }
        if add == 0 {
            return None;
        }
        let mut v = sd.to_vec();
        v[2..4].copy_from_slice(&(control | add).to_le_bytes());
        Some(v)
    }
}

// ====================================================================== Unix

#[cfg(unix)]
mod imp {
    use super::MetaError;
    use filetime::FileTime;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    #[derive(Clone, Debug)]
    pub struct Snapshot {
        atime: FileTime,
        mtime: FileTime,
        mode: u32,
        uid: u32,
        gid: u32,
        xattrs: Vec<(OsString, Vec<u8>)>,
        is_link: bool,
    }

    fn read_xattrs(p: &Path) -> io::Result<Vec<(OsString, Vec<u8>)>> {
        let names = match xattr::list(p) {
            Ok(n) => n,
            // filesystem without xattr support: nothing to keep
            Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut v = Vec::new();
        for n in names {
            let val = xattr::get(p, &n)?.unwrap_or_default();
            v.push((n, val));
        }
        v.sort();
        Ok(v)
    }

    pub fn privileges_note() -> Option<&'static str> {
        None
    }

    pub fn capture(p: &Path) -> io::Result<Snapshot> {
        let md = fs::symlink_metadata(p)?;
        Ok(Snapshot {
            atime: FileTime::from_last_access_time(&md),
            mtime: FileTime::from_last_modification_time(&md),
            mode: md.mode() & 0o7777,
            uid: md.uid(),
            gid: md.gid(),
            xattrs: read_xattrs(p)?,
            is_link: md.file_type().is_symlink(),
        })
    }

    pub fn create_dir_like(_src: &Path, dst: &Path) -> io::Result<()> {
        fs::create_dir(dst)
    }

    pub fn apply(dst: &Path, s: &Snapshot) -> Result<(), MetaError> {
        let md = fs::symlink_metadata(dst).map_err(|e| MetaError::Io("owner", e))?;
        if md.uid() != s.uid || md.gid() != s.gid {
            std::os::unix::fs::lchown(dst, Some(s.uid), Some(s.gid)).map_err(|e| MetaError::from_io("owner", e))?;
        }
        for (k, v) in &s.xattrs {
            xattr::set(dst, k, v).map_err(|e| MetaError::from_io("extended attributes / ACL", e))?;
        }
        if !s.is_link {
            fs::set_permissions(dst, fs::Permissions::from_mode(s.mode)).map_err(|e| MetaError::from_io("permissions", e))?;
        }
        filetime::set_symlink_file_times(dst, s.atime, s.mtime).map_err(|e| MetaError::from_io("dates", e))?;
        Ok(())
    }

    pub fn set_dates(dst: &Path, s: &Snapshot) -> io::Result<()> {
        filetime::set_symlink_file_times(dst, s.atime, s.mtime)
    }

    pub fn verify(dst: &Path, s: &Snapshot) -> Result<(), String> {
        let now = capture(dst).map_err(|e| e.to_string())?;
        let mut diff = Vec::new();
        if now.mtime != s.mtime {
            diff.push("modified date");
        }
        if now.atime != s.atime {
            diff.push("accessed date");
        }
        if !s.is_link && now.mode != s.mode {
            diff.push("permissions");
        }
        if now.uid != s.uid || now.gid != s.gid {
            diff.push("owner / group");
        }
        if now.xattrs != s.xattrs {
            diff.push("extended attributes / ACL");
        }
        if diff.is_empty() { Ok(()) } else { Err(diff.join(", ")) }
    }
}

/// Restart this program elevated (Windows "Run as administrator"). Returns Ok(true) if a new
/// elevated instance was started (the caller should then exit).
pub fn restart_elevated() -> Result<bool, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let w = |s: &std::ffi::OsStr| s.encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (verb, file) = (w(std::ffi::OsStr::new("runas")), w(exe.as_os_str()));
        // SAFETY: valid null-terminated strings. Returns > 32 on success.
        let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
        Ok(r as isize > 32)
    }
    #[cfg(not(windows))]
    {
        Err("Start Smart Symlink with sudo to keep owners of other users' files.".into())
    }
}
