//! The actual operations. Moving between drives is done as:
//!
//! 1. copy source -> target
//! 2. verify (file count + bytes)
//! 3. rename source -> `<name>.dirlinker-old` (fails if something holds files open -> abort, nothing changed)
//! 4. create the link at the original path
//! 5. delete the renamed original
//!
//! Every step before 5 rolls back on failure.

use crate::links::{LinkKind, Mode};
use crate::listing::is_dir_like;
use crate::size;
use crate::util::{clean_path, display_path, fmt_bytes};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub sources: Vec<String>,
    pub dest: String,
    pub kind: LinkKind,
    pub mode: Mode,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ItemResult {
    pub source: String,
    pub target: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub item_index: usize,
    pub item_count: usize,
    pub item: String,
    /// scanning | copying | verifying | moving | linking | cloning | cleaning
    pub phase: String,
    pub file: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

pub fn execute(req: &Request, cb: &dyn Fn(&Progress)) -> Vec<ItemResult> {
    let dest = crate::util::normalize_input(&req.dest);
    let n = req.sources.len();
    let mut out = Vec::with_capacity(n);
    for (i, s) in req.sources.iter().enumerate() {
        let src = crate::util::normalize_input(s);
        let Some(name) = src.file_name() else {
            out.push(ItemResult {
                source: s.clone(),
                target: String::new(),
                ok: false,
                message: "Drive roots can't be linked.".into(),
            });
            continue;
        };
        let target = dest.join(name);
        let item = display_path(&src);
        let mut report = |phase: &str, file: Option<&Path>, done: u64, total: u64| {
            cb(&Progress {
                item_index: i,
                item_count: n,
                item: item.clone(),
                phase: phase.into(),
                file: file.map(display_path),
                bytes_done: done,
                bytes_total: total,
            })
        };
        let r = run_item(&src, &target, req.kind, req.mode, &mut report);
        size::invalidate(&src);
        size::invalidate(&target);
        out.push(ItemResult {
            source: display_path(&src),
            target: display_path(&target),
            ok: r.is_ok(),
            message: r.unwrap_or_else(|e| e),
        });
    }
    out
}

type Report<'a> = dyn FnMut(&str, Option<&Path>, u64, u64) + 'a;

fn run_item(src: &Path, target: &Path, kind: LinkKind, mode: Mode, rep: &mut Report) -> Result<String, String> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(format!("{} already exists.", display_path(target)));
    }
    let md = fs::symlink_metadata(src).map_err(|e| format!("Can't read source: {e}"))?;
    let is_dir = is_dir_like(src, &md);

    match kind {
        LinkKind::Hardlink => {
            if is_dir {
                return Err("Hard links work for files only.".into());
            }
            rep("linking", Some(src), 0, 0);
            fs::hard_link(src, target).map_err(|e| format!("Hard link failed: {e}"))?;
            Ok("Hard link created — both names now share the same data.".into())
        }
        LinkKind::Clone => {
            let total = size::measure(src).bytes;
            let mut done = 0u64;
            rep("cloning", Some(src), 0, total);
            if let Err(e) = clone_tree(src, target, &mut done, &mut |f, d| rep("cloning", Some(f), d, total)) {
                let _ = remove_tree(target);
                return Err(format!("Clone failed: {e}"));
            }
            Ok(format!("Cloned {} (copy-on-write, no extra space used yet).", fmt_bytes(total)))
        }
        LinkKind::Snapshot => {
            rep("linking", Some(src), 0, 0);
            run_cmd("btrfs", &[&display_path(src), &display_path(target)], &["subvolume", "snapshot"])?;
            Ok("Btrfs snapshot created.".into())
        }
        LinkKind::Junction | LinkKind::Symlink | LinkKind::Bind => {
            if kind != LinkKind::Symlink && !is_dir {
                return Err("This link type works for folders only.".into());
            }
            match mode {
                Mode::Link => {
                    rep("linking", Some(src), 0, 0);
                    create_link(kind, target, src, is_dir)?;
                    Ok(format!("Link created in destination{}", bind_hint(kind, src, target)))
                }
                Mode::Move => {
                    if md.file_type().is_symlink() {
                        return Err("The source is already a link — nothing to move.".into());
                    }
                    move_and_link(src, target, kind, is_dir, rep)
                }
            }
        }
    }
}

fn move_and_link(src: &Path, target: &Path, kind: LinkKind, is_dir: bool, rep: &mut Report) -> Result<String, String> {
    let same = match (crate::fsinfo::vol_info(src), crate::fsinfo::vol_info(target.parent().unwrap_or(target))) {
        (Some(a), Some(b)) => a.same_volume(&b),
        _ => false,
    };

    if same {
        rep("moving", Some(src), 0, 0);
        fs::rename(src, target).map_err(|e| in_use_msg("Could not move", &e))?;
        rep("linking", Some(src), 0, 0);
        if let Err(e) = create_link(kind, src, target, is_dir) {
            return Err(match fs::rename(target, src) {
                Ok(_) => format!("{e} The original was restored."),
                Err(_) => format!("{e} WARNING: the data is now at {}.", display_path(target)),
            });
        }
        return Ok(format!("Moved and linked{}", bind_hint(kind, target, src)));
    }

    // Different volumes: copy, verify, swap.
    rep("scanning", Some(src), 0, 0);
    let before = size::measure(src);
    let total = before.bytes;
    let mut done = 0u64;
    if let Err(e) = copy_tree(src, target, &mut done, &mut |f, d| rep("copying", Some(f), d, total)) {
        let _ = remove_tree(target);
        return Err(format!("Copy failed: {e}. Nothing was changed."));
    }
    rep("verifying", None, total, total);
    let after = size::measure(target);
    if after.bytes != before.bytes || after.files != before.files {
        let _ = remove_tree(target);
        return Err(format!(
            "Verification failed ({} files / {} expected, got {} files / {}). Copy removed, nothing changed.",
            before.files,
            fmt_bytes(before.bytes),
            after.files,
            fmt_bytes(after.bytes)
        ));
    }

    let aside = aside_path(src);
    if let Err(e) = fs::rename(src, &aside) {
        let _ = remove_tree(target);
        return Err(in_use_msg("The original is in use and can't be replaced", &e) + " Copy removed, nothing changed.");
    }
    rep("linking", Some(src), total, total);
    if let Err(e) = create_link(kind, src, target, is_dir) {
        let restored = fs::rename(&aside, src).is_ok();
        let _ = remove_tree(target);
        return Err(if restored {
            format!("{e} The original was restored.")
        } else {
            format!("{e} WARNING: original is at {}.", display_path(&aside))
        });
    }
    rep("cleaning", Some(&aside), total, total);
    match remove_tree(&aside) {
        Ok(_) => Ok(format!("Moved {} and linked{}", fmt_bytes(total), bind_hint(kind, target, src))),
        Err(e) => Ok(format!(
            "Moved and linked, but the old copy at {} could not be fully deleted ({e}). You can delete it manually.",
            display_path(&aside)
        )),
    }
}

fn in_use_msg(prefix: &str, e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied => format!("{prefix}: access denied or a program is using it. Close programs using it and try again."),
        _ => format!("{prefix}: {e}."),
    }
}

fn bind_hint(kind: LinkKind, data: &Path, mountpoint: &Path) -> String {
    if kind == LinkKind::Bind {
        format!(
            ". To keep it after reboot add to /etc/fstab:  {}  {}  none  bind  0  0",
            display_path(data),
            display_path(mountpoint)
        )
    } else {
        ".".into()
    }
}

fn aside_path(src: &Path) -> PathBuf {
    let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut i = 0;
    loop {
        let candidate = if i == 0 { format!("{name}.dirlinker-old") } else { format!("{name}.dirlinker-old{i}") };
        let p = src.with_file_name(candidate);
        if fs::symlink_metadata(&p).is_err() {
            return p;
        }
        i += 1;
    }
}

/// Create `link` pointing to `target`.
pub fn create_link(kind: LinkKind, link: &Path, target: &Path, target_is_dir: bool) -> Result<(), String> {
    match kind {
        LinkKind::Junction => create_junction(link, target),
        LinkKind::Symlink => create_symlink(link, target, target_is_dir),
        LinkKind::Hardlink => fs::hard_link(target, link).map_err(|e| format!("Hard link failed: {e}.")),
        LinkKind::Bind => {
            fs::create_dir(link).map_err(|e| format!("Can't create mount point: {e}."))?;
            run_cmd("mount", &[&display_path(target), &display_path(link)], &["--bind"]).inspect_err(|_| {
                let _ = fs::remove_dir(link);
            })
        }
        LinkKind::Clone | LinkKind::Snapshot => Err("Not a link type.".into()),
    }
}

#[cfg(windows)]
fn create_junction(link: &Path, target: &Path) -> Result<(), String> {
    junction::create(clean_path(target), link).map_err(|e| format!("Junction failed: {e}."))
}
#[cfg(not(windows))]
fn create_junction(_: &Path, _: &Path) -> Result<(), String> {
    Err("Junctions exist only on Windows.".into())
}

#[cfg(windows)]
fn create_symlink(link: &Path, target: &Path, is_dir: bool) -> Result<(), String> {
    let r = if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    r.map_err(|e| {
        if e.raw_os_error() == Some(1314) {
            "Windows refused to create a symbolic link. Enable Developer Mode (Settings › System › For developers) or run as administrator.".into()
        } else {
            format!("Symbolic link failed: {e}.")
        }
    })
}
#[cfg(unix)]
fn create_symlink(link: &Path, target: &Path, _is_dir: bool) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| format!("Symbolic link failed: {e}."))
}

fn run_cmd(program: &str, paths: &[&str], args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(program)
        .args(args)
        .args(paths)
        .output()
        .map_err(|e| format!("Could not run {program}: {e}."))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Recreate a symlink/junction found inside a tree that is being copied.
fn copy_link(src: &Path, dst: &Path) -> io::Result<()> {
    let t = clean_path(&fs::read_link(src)?);
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if crate::listing::is_junction(src) {
            return junction::create(&t, dst);
        }
        if fs::symlink_metadata(src)?.file_attributes() & 0x10 != 0 {
            std::os::windows::fs::symlink_dir(&t, dst)
        } else {
            std::os::windows::fs::symlink_file(&t, dst)
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&t, dst)
    }
}

fn copy_tree(src: &Path, dst: &Path, done: &mut u64, on: &mut dyn FnMut(&Path, u64)) -> io::Result<()> {
    let md = fs::symlink_metadata(src)?;
    if md.file_type().is_symlink() {
        return copy_link(src, dst);
    }
    if md.is_dir() {
        fs::create_dir(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()), done, on)?;
        }
        if let Ok(m) = md.modified() {
            let _ = fs::File::open(dst).and_then(|f| f.set_modified(m));
        }
        Ok(())
    } else {
        *done += fs::copy(src, dst).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", display_path(src))))?;
        on(src, *done);
        Ok(())
    }
}

fn clone_tree(src: &Path, dst: &Path, done: &mut u64, on: &mut dyn FnMut(&Path, u64)) -> io::Result<()> {
    let md = fs::symlink_metadata(src)?;
    if md.file_type().is_symlink() {
        return copy_link(src, dst);
    }
    if md.is_dir() {
        fs::create_dir(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            clone_tree(&e.path(), &dst.join(e.file_name()), done, on)?;
        }
        Ok(())
    } else {
        reflink_copy::reflink(src, dst)?;
        *done += md.len();
        on(src, *done);
        Ok(())
    }
}

/// Delete a tree WITHOUT ever following links or junctions inside it.
pub fn remove_tree(p: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(p)?;
    if md.file_type().is_symlink() {
        return remove_link(p);
    }
    if md.is_dir() {
        for e in fs::read_dir(p)? {
            remove_tree(&e?.path())?;
        }
        fs::remove_dir(p).or_else(|e| if clear_readonly(p) { fs::remove_dir(p) } else { Err(e) })
    } else {
        fs::remove_file(p).or_else(|e| if clear_readonly(p) { fs::remove_file(p) } else { Err(e) })
    }
}

/// Remove only the link itself (never its target's contents).
pub fn remove_link(p: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if fs::symlink_metadata(p)?.file_attributes() & 0x10 != 0 {
            return fs::remove_dir(p);
        }
        fs::remove_file(p)
    }
    #[cfg(unix)]
    {
        fs::remove_file(p)
    }
}

fn clear_readonly(p: &Path) -> bool {
    match fs::symlink_metadata(p) {
        Ok(md) if md.permissions().readonly() => {
            let mut perm = md.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            fs::set_permissions(p, perm).is_ok()
        }
        _ => false,
    }
}
