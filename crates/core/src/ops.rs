//! The actual operations. Moving between drives is done as:
//!
//! 1. copy source -> target
//! 2. verify (file count + bytes)
//! 3. rename source -> `<name>.smart-symlink-old` (fails if something holds files open -> abort, nothing changed)
//! 4. create the link at the original path
//! 5. delete the renamed original
//!
//! Every step before 5 rolls back on failure. Unlinking is the same dance in reverse,
//! and re-creates the link if moving the data back fails.

use crate::links::{LinkKind, Mode};
use crate::listing::{is_dir_like, is_junction};
use crate::registry::{self, SavedLink};
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
    /// Compare every copied file byte by byte (slower)
    #[serde(default)]
    pub verify: bool,
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
    /// scanning | copying | verifying | moving | linking | unlinking | cloning | cleaning | deleting
    pub phase: String,
    pub file: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

type Report<'a> = dyn FnMut(&str, Option<&Path>, u64, u64) + 'a;

/// Runs `f` for each item with a progress reporter bound to that item.
fn for_each_item(
    items: &[String],
    cb: &dyn Fn(&Progress),
    mut f: impl FnMut(&Path, &mut Report) -> (String, Result<String, String>),
) -> Vec<ItemResult> {
    let n = items.len();
    items
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let src = crate::util::normalize_input(s);
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
            let (target, r) = f(&src, &mut report);
            size::invalidate(&src);
            if !target.is_empty() {
                size::invalidate(Path::new(&target));
            }
            ItemResult { source: display_path(&src), target, ok: r.is_ok(), message: r.unwrap_or_else(|e| e) }
        })
        .collect()
}

// ------------------------------------------------------------------ link

pub fn execute(req: &Request, cb: &dyn Fn(&Progress)) -> Vec<ItemResult> {
    let dest = crate::util::normalize_input(&req.dest);
    with_verify(req.verify, || for_each_item(&req.sources, cb, |src, rep| {
        let Some(name) = src.file_name() else {
            return (String::new(), Err("Drive roots can't be linked.".into()));
        };
        let target = dest.join(name);
        let r = run_item(src, &target, req.kind, req.mode, rep);
        if r.is_ok() && !matches!(req.kind, LinkKind::Clone | LinkKind::Snapshot) {
            let moved = req.mode == Mode::Move && !crate::links::keeps_source(req.kind);
            // moved: the link sits where the data used to be; otherwise the new link is in the destination
            let (link, data) = if moved { (src, target.as_path()) } else { (target.as_path(), src) };
            registry::add_link(SavedLink {
                link: display_path(link),
                target: display_path(data),
                kind: req.kind,
                created: registry::now_ms(),
                moved,
            });
        }
        (display_path(&target), r)
    }))
}

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

fn same_volume(a: &Path, b: &Path) -> bool {
    // test hook: force the copy path even when everything is on one drive
    if std::env::var_os("SMART_SYMLINK_FORCE_COPY").is_some() {
        return false;
    }
    match (crate::fsinfo::vol_info(a), crate::fsinfo::vol_info(b)) {
        (Some(a), Some(b)) => a.same_volume(&b),
        _ => false,
    }
}

/// Cross-volume move, phase 1: copy `src` to `target`, verify, then rename `src` aside.
/// On success returns the aside path (delete it to finish) and the byte count.
/// On failure nothing has changed.
type Applied = Vec<(PathBuf, crate::meta::Snapshot)>;

fn copy_verify_aside(src: &Path, target: &Path, rep: &mut Report) -> Result<(PathBuf, u64, Applied), String> {
    rep("scanning", Some(src), 0, 0);
    // record every item's metadata before anything is read (reading can change "accessed" dates)
    let mut snaps = std::collections::HashMap::new();
    let mut before = size::SizeInfo::default();
    if let Err(e) = snapshot_tree(src, &mut snaps, &mut before) {
        return Err(format!("{e}. Nothing was changed."));
    }
    let total = before.bytes;
    let (mut done, mut files) = (0u64, 0u64);
    let mut applied = Applied::new();
    if let Err(e) = copy_tree(src, target, snaps, &mut done, &mut files, &mut applied, &mut |f, d| rep("copying", Some(f), d, total)) {
        let _ = remove_tree(target);
        return Err(format!("Copy failed: {e}. Nothing was changed."));
    }
    // every item was already compared one by one; this catches files that changed size while copying.
    // (No re-listing of the copy: listing a folder can change its "accessed" date.)
    rep("verifying", None, total, total);
    let after = size::SizeInfo { bytes: done, files, ..Default::default() };
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
    // other programs (typically an antivirus scanning the new files) may have touched the copies
    // after their metadata was set: re-check everything and re-apply what changed
    if let Err(e) = settle(&applied) {
        let _ = remove_tree(target);
        return Err(format!("{e}. Copy removed, nothing changed."));
    }
    let aside = sibling(src, "smart-symlink-old");
    if let Err(e) = fs::rename(src, &aside) {
        let _ = remove_tree(target);
        return Err(in_use_msg(&format!("{} is in use and can't be moved", display_path(src)), &e) + " Copy removed, nothing changed.");
    }
    Ok((aside, total, applied))
}

/// Re-check every copied item and re-apply metadata that another program changed after it was set.
///
/// Everything must end up identical, with one exception: the "last accessed" date. Any program that
/// reads a file or lists a folder (indexers, sync/backup tools, antivirus) updates it, and some keep
/// doing so whenever the item changes. Smart Symlink restores it; if another program keeps changing it,
/// the result carries a note instead of failing. Retries for ~3 s.
///
/// Ok(None): all identical. Ok(Some(note)): only "accessed" dates were changed by another program.
fn settle(applied: &[(PathBuf, crate::meta::Snapshot)]) -> Result<Option<String>, String> {
    const ROUNDS: usize = 10;
    for round in 0..ROUNDS {
        if round > 0 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let bad: Vec<_> = applied.iter().filter_map(|(p, s)| crate::meta::verify(p, s).err().map(|e| (p, s, e))).collect();
        if bad.is_empty() {
            return Ok(None);
        }
        let only_accessed = bad.iter().all(|(_, _, e)| e == "accessed date");
        if round == ROUNDS - 1 {
            if only_accessed {
                return Ok(Some(format!(
                    " Note: another program on this computer (e.g. a search indexer, sync, backup or antivirus tool) keeps reading \
                     {} of the copied items, which updates their “last accessed” date. Smart Symlink restored the original dates, but \
                     that program changed them again. Everything else is exactly as before.",
                    bad.len()
                )));
            }
            let (p, _, e) = &bad[0];
            return Err(format!("{}: {e} keeps being changed by another program after copying", display_path(p)));
        }
        for (p, s, e) in &bad {
            let r = if e == "accessed date" { crate::meta::reapply_dates(p, s) } else { crate::meta::apply_and_verify(p, s) };
            if let Err(e) = r {
                return Err(e);
            }
        }
    }
    unreachable!()
}

/// Final re-check after the swap; returns a note for the result message if something is off.
fn late_check(applied: &[(PathBuf, crate::meta::Snapshot)]) -> String {
    match settle(applied) {
        Ok(None) => String::new(),
        Ok(Some(note)) => note,
        Err(e) => format!(" Warning: {e}."),
    }
}

fn move_and_link(src: &Path, target: &Path, kind: LinkKind, is_dir: bool, rep: &mut Report) -> Result<String, String> {
    if same_volume(src, target.parent().unwrap_or(target)) {
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

    let (aside, total, applied) = copy_verify_aside(src, target, rep)?;
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
    let note = late_check(&applied);
    match remove_tree(&aside) {
        Ok(_) => Ok(format!("Moved {} and linked{}{note}", fmt_bytes(total), bind_hint(kind, target, src))),
        Err(e) => Ok(format!(
            "Moved and linked, but the old copy at {} could not be fully deleted ({e}). You can delete it manually.",
            display_path(&aside)
        )),
    }
}

// ------------------------------------------------------------------ unlink

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum UnlinkMode {
    /// Move the data back to the link's place and remove the link
    Restore,
    /// Only remove the link; the data stays where it is
    Remove,
}

/// Where a symlink / junction points (absolute, without verbatim prefixes).
pub fn link_target(link: &Path) -> Option<PathBuf> {
    let t = clean_path(&crate::fsx::read_link(link).ok()?);
    Some(if t.is_relative() { link.parent().map(|p| p.join(&t)).unwrap_or(t) } else { t })
}

pub fn unlink(links: &[String], mode: UnlinkMode, verify: bool, cb: &dyn Fn(&Progress)) -> Vec<ItemResult> {
    with_verify(verify, || for_each_item(links, cb, |link, rep| {
        let r = unlink_one(link, mode, rep);
        match r {
            Ok((target, msg)) => {
                let _ = registry::forget_link(&display_path(link));
                (target, Ok(msg))
            }
            Err(e) => (String::new(), Err(e)),
        }
    }))
}

fn unlink_one(link: &Path, mode: UnlinkMode, rep: &mut Report) -> Result<(String, String), String> {
    let md = fs::symlink_metadata(link).map_err(|e| format!("Can't read {}: {e}", display_path(link)))?;

    if md.file_type().is_symlink() {
        let kind = if is_junction(link) { LinkKind::Junction } else { LinkKind::Symlink };
        let is_dir = is_dir_like(link, &md);
        let target = link_target(link).ok_or("Can't read where the link points.")?;
        let tshow = display_path(&target);
        let tmd = fs::symlink_metadata(&target)
            .map_err(|_| format!("The link points to {tshow}, which no longer exists — nothing to restore. Use Delete to remove the broken link."))?;
        if tmd.file_type().is_symlink() {
            return Err(format!("{tshow} is itself a link — unlink that one first."));
        }
        let msg = match mode {
            UnlinkMode::Remove => {
                rep("unlinking", Some(link), 0, 0);
                remove_link(link).map_err(|e| format!("Could not remove the link: {e}"))?;
                format!("Link removed. The data stays at {tshow}.")
            }
            UnlinkMode::Restore => restore(link, &target, kind, is_dir, rep)?,
        };
        return Ok((tshow, msg));
    }

    if md.is_file() {
        let saved = registry::load()
            .links
            .into_iter()
            .find(|l| l.kind == LinkKind::Hardlink && registry::same_path(&l.link, &display_path(link)));
        let Some(s) = saved else {
            return Err("This is a regular file, not a link.".into());
        };
        let partner = Path::new(&s.target);
        if crate::fsinfo::file_id(link).is_none() || crate::fsinfo::file_id(link) != crate::fsinfo::file_id(partner) {
            return Err(format!("{} no longer shares its data with {} — refusing to remove it.", display_path(link), s.target));
        }
        rep("unlinking", Some(link), 0, 0);
        fs::remove_file(link).map_err(|e| format!("Could not remove the hard link: {e}"))?;
        return Ok((s.target.clone(), format!("Hard link removed. The data stays at {}.", s.target)));
    }

    Err("This is a regular folder, not a link.".into())
}

/// Move the data from `target` back to `link`'s place. If anything fails after the link
/// was removed, the link is re-created so nothing is lost or left dangling.
fn restore(link: &Path, target: &Path, kind: LinkKind, is_dir: bool, rep: &mut Report) -> Result<String, String> {
    if same_volume(target, link.parent().unwrap_or(link)) {
        rep("unlinking", Some(link), 0, 0);
        remove_link(link).map_err(|e| format!("Could not remove the link: {e}"))?;
        rep("moving", Some(target), 0, 0);
        if let Err(e) = fs::rename(target, link) {
            return Err(match create_link(kind, link, target, is_dir) {
                Ok(_) => in_use_msg("Could not move the data back", &e) + " The link was restored.",
                Err(e2) => format!(
                    "Could not move the data back ({e}) and the link could not be re-created ({e2}). The data is safe at {}.",
                    display_path(target)
                ),
            });
        }
        return Ok("Data moved back and link removed.".into());
    }

    let tmp = sibling(link, "smart-symlink-restore");
    let (aside, total, applied) = copy_verify_aside(target, &tmp, rep)?;
    rep("unlinking", Some(link), total, total);
    if let Err(e) = remove_link(link) {
        let _ = fs::rename(&aside, target);
        let _ = remove_tree(&tmp);
        return Err(format!("Could not remove the link: {e}. Nothing was changed."));
    }
    if let Err(e) = fs::rename(&tmp, link) {
        let back = fs::rename(&aside, target).is_ok();
        let relinked = back && create_link(kind, link, target, is_dir).is_ok();
        let _ = remove_tree(&tmp);
        return Err(if relinked {
            format!("Could not put the data back in place ({e}). The link was restored, nothing changed.")
        } else {
            format!("Could not put the data back in place ({e}). WARNING: data is at {}.", display_path(if back { target } else { &aside }))
        });
    }
    rep("cleaning", Some(&aside), total, total);
    // the copy was made under a temporary name and then renamed into place
    let applied: Applied = applied
        .into_iter()
        .map(|(p, s)| (p.strip_prefix(&tmp).map(|rest| link.join(rest)).unwrap_or(p), s))
        .collect();
    let note = late_check(&applied);
    match remove_tree(&aside) {
        Ok(_) => Ok(format!("Moved {} back and removed the link.{note}", fmt_bytes(total))),
        Err(e) => Ok(format!(
            "Data moved back and link removed, but the old copy at {} could not be fully deleted ({e}).",
            display_path(&aside)
        )),
    }
}

// ------------------------------------------------------------------ re-link

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RelinkRequest {
    pub links: Vec<String>,
    /// Folder the data should live in (the data keeps its name). If it is already there, only the link is re-created.
    pub dest: String,
    pub kind: LinkKind,
    #[serde(default)]
    pub verify: bool,
}

/// Re-create existing links: move the data they point to into `dest` and/or change the link type.
/// The link path itself stays the same. If anything fails, the data and the old link are put back.
pub fn relink(req: &RelinkRequest, cb: &dyn Fn(&Progress)) -> Vec<ItemResult> {
    let dest = crate::util::normalize_input(&req.dest);
    with_verify(req.verify, || for_each_item(&req.links, cb, |link, rep| match relink_one(link, &dest, req.kind, rep) {
        Ok((target, msg)) => {
            let shown = display_path(link);
            let moved = registry::load().links.iter().find(|l| registry::same_path(&l.link, &shown)).is_none_or(|l| l.moved);
            registry::add_link(SavedLink { link: shown, target: target.clone(), kind: req.kind, created: registry::now_ms(), moved });
            (target, Ok(msg))
        }
        Err(e) => (String::new(), Err(e)),
    }))
}

fn relink_one(link: &Path, dest: &Path, kind: LinkKind, rep: &mut Report) -> Result<(String, String), String> {
    if !matches!(kind, LinkKind::Junction | LinkKind::Symlink) {
        return Err("Re-linking works with junctions and symbolic links.".into());
    }
    let md = fs::symlink_metadata(link).map_err(|e| format!("Can't read {}: {e}", display_path(link)))?;
    if !md.file_type().is_symlink() {
        return Err("This is not a junction or symbolic link.".into());
    }
    let old_kind = if is_junction(link) { LinkKind::Junction } else { LinkKind::Symlink };
    let old = link_target(link).ok_or("Can't read where the link points.")?;
    let oshow = display_path(&old);
    let tmd = fs::symlink_metadata(&old).map_err(|_| format!("The link points to {oshow}, which no longer exists."))?;
    if tmd.file_type().is_symlink() {
        return Err(format!("{oshow} is itself a link — unlink that one first."));
    }
    let is_dir = tmd.is_dir();
    if kind == LinkKind::Junction && !is_dir {
        return Err("Junctions work for folders only.".into());
    }
    let new = crate::links::relink_target(&old, dest);
    let nshow = display_path(&new);
    let moving = new != old;
    if !moving && kind == old_kind {
        return Err("Nothing to change: same place and same link type.".into());
    }
    if moving {
        if !dest.is_dir() {
            return Err(format!("{} is not an accessible folder.", display_path(dest)));
        }
        if dest.starts_with(&old) {
            return Err("Can't move a folder into itself.".into());
        }
        if new == link {
            return Err("That is where the link itself is — use Unlink to move the data back.".into());
        }
        if fs::symlink_metadata(&new).is_ok() {
            return Err(format!("{nshow} already exists."));
        }
    }

    // 1. move the data (the old copy is kept aside until the new link exists)
    let mut aside: Option<(PathBuf, u64, Applied)> = None;
    if moving {
        if same_volume(&old, dest) {
            rep("moving", Some(&old), 0, 0);
            fs::rename(&old, &new).map_err(|e| in_use_msg("Could not move the data", &e) + " Nothing was changed.")?;
        } else {
            aside = Some(copy_verify_aside(&old, &new, rep)?);
        }
    }
    let total = aside.as_ref().map(|a| a.1).unwrap_or(0);
    // puts the data back where it was
    let undo_move = |aside: &Option<(PathBuf, u64, Applied)>| -> bool {
        if !moving {
            return true;
        }
        match aside {
            Some((a, _, _)) => {
                let back = fs::rename(a, &old).is_ok();
                if back {
                    let _ = remove_tree(&new);
                }
                back
            }
            None => fs::rename(&new, &old).is_ok(),
        }
    };

    // 2. replace the link
    rep("linking", Some(link), total, total);
    if let Err(e) = remove_link(link) {
        return Err(if undo_move(&aside) {
            format!("Could not remove the old link: {e}. Nothing was changed.")
        } else {
            format!("Could not remove the old link: {e}. WARNING: the data is now at {nshow}.")
        });
    }
    if let Err(e) = create_link(kind, link, &new, is_dir) {
        let back = undo_move(&aside);
        let data = if back { &old } else { &new };
        return Err(match create_link(old_kind, link, data, is_dir) {
            Ok(_) if back => format!("{e} The old link was restored, nothing changed."),
            Ok(_) => format!("{e} The data could not be moved back, so the old link now points to {nshow}."),
            Err(e2) => format!("{e} The old link could not be re-created either ({e2}). The data is safe at {}.", display_path(data)),
        });
    }

    // 3. clean up
    let what = if kind == old_kind { String::new() } else { format!(" The link is now a {}.", crate::links::text(kind).title.to_lowercase()) };
    let Some((a, total, applied)) = aside else {
        return Ok((nshow.clone(), if moving { format!("Data moved to {nshow} and re-linked.{what}") } else { format!("Link re-created.{what}") }));
    };
    rep("cleaning", Some(&a), total, total);
    let note = late_check(&applied);
    Ok((nshow.clone(), match remove_tree(&a) {
        Ok(_) => format!("Moved {} to {nshow} and re-linked.{what}{note}", fmt_bytes(total)),
        Err(e) => format!("Moved and re-linked, but the old copy at {} could not be fully deleted ({e}). You can delete it manually.", display_path(&a)),
    }))
}

// ------------------------------------------------------------------ plain file operations

/// Move items into `dest` (no links). Uses the same safe copy/verify path between drives.
pub fn move_items(sources: &[String], dest: &str, verify: bool, cb: &dyn Fn(&Progress)) -> Vec<ItemResult> {
    let dest = crate::util::normalize_input(dest);
    with_verify(verify, || for_each_item(sources, cb, |src, rep| {
        let Some(name) = src.file_name() else {
            return (String::new(), Err("Drive roots can't be moved.".into()));
        };
        let target = dest.join(name);
        let tshow = display_path(&target);
        if fs::symlink_metadata(&target).is_ok() {
            return (tshow.clone(), Err(format!("{tshow} already exists.")));
        }
        if dest.starts_with(src) {
            return (tshow, Err("Can't move a folder into itself.".into()));
        }
        let r = if same_volume(src, &dest) {
            rep("moving", Some(src), 0, 0);
            fs::rename(src, &target).map(|_| "Moved.".to_string()).map_err(|e| in_use_msg("Could not move", &e))
        } else {
            copy_verify_aside(src, &target, rep).map(|(aside, total, applied)| {
                rep("cleaning", Some(&aside), total, total);
                let note = late_check(&applied);
                match remove_tree(&aside) {
                    Ok(_) => format!("Moved {}.{note}", fmt_bytes(total)),
                    Err(e) => format!("Moved, but the old copy at {} could not be fully deleted ({e}).", display_path(&aside)),
                }
            })
        };
        if r.is_ok() {
            registry::path_moved(&display_path(src), &tshow);
        }
        (tshow, r)
    }))
}

fn check_name(name: &str) -> Result<(), String> {
    let bad: &[char] = if cfg!(windows) { &['\\', '/', ':', '*', '?', '"', '<', '>', '|'] } else { &['/'] };
    if name.trim().is_empty() || name == "." || name == ".." {
        return Err("Please enter a name.".into());
    }
    if let Some(c) = name.chars().find(|c| bad.contains(c) || c.is_control()) {
        return Err(format!("A name can't contain '{c}'."));
    }
    if cfg!(windows) && (name.ends_with(' ') || name.ends_with('.')) {
        return Err("On Windows a name can't end with a space or a dot.".into());
    }
    Ok(())
}

pub fn create_dir(parent: &str, name: &str) -> Result<String, String> {
    check_name(name)?;
    let p = crate::util::normalize_input(parent).join(name);
    fs::create_dir(&p).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => format!("'{name}' already exists."),
        _ => format!("Could not create folder: {e}"),
    })?;
    Ok(display_path(&p))
}

pub fn rename(path: &str, new_name: &str) -> Result<String, String> {
    check_name(new_name)?;
    let old = crate::util::normalize_input(path);
    let new = old.parent().ok_or("Drives can't be renamed here.")?.join(new_name);
    let case_only = registry::same_path(&display_path(&old), &display_path(&new));
    if !case_only && fs::symlink_metadata(&new).is_ok() {
        return Err(format!("'{new_name}' already exists."));
    }
    fs::rename(&old, &new).map_err(|e| in_use_msg("Could not rename", &e))?;
    registry::path_moved(&display_path(&old), &display_path(&new));
    size::invalidate(&old);
    Ok(display_path(&new))
}

/// Delete items. Links are always removed directly (never their target); everything else goes
/// to the recycle bin / trash unless `permanent`.
pub fn delete(paths: &[String], permanent: bool, cb: &dyn Fn(&Progress)) -> Vec<ItemResult> {
    for_each_item(paths, cb, |p, rep| {
        rep("deleting", Some(p), 0, 0);
        let r = match fs::symlink_metadata(p) {
            Err(e) => Err(format!("Can't read: {e}")),
            Ok(md) if md.file_type().is_symlink() => remove_link(p)
                .map(|_| {
                    let _ = registry::forget_link(&display_path(p));
                    "Link removed (its target was not touched).".to_string()
                })
                .map_err(|e| e.to_string()),
            Ok(_) if permanent => remove_tree(p).map(|_| "Deleted permanently.".to_string()).map_err(|e| in_use_msg("Could not delete", &e)),
            Ok(_) => trash::delete(p).map(|_| "Moved to the recycle bin.".to_string()).map_err(|e| {
                format!("Could not move to the recycle bin ({e}). Try again with “Delete permanently”.")
            }),
        };
        (String::new(), r)
    })
}

/// Open the operating system's own properties window. Returns false if the OS has none
/// (then the UI shows its own).
pub fn native_properties(path: &str) -> Result<bool, String> {
    let p = crate::util::normalize_input(path);
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::{SHOP_FILEPATH, SHObjectProperties};
        let w: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: null-terminated wide string; must be called on a thread with a message loop (the UI thread).
        let ok = unsafe { SHObjectProperties(std::ptr::null_mut(), SHOP_FILEPATH as u32, w.as_ptr(), std::ptr::null()) };
        Ok(ok != 0)
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Finder\"\nactivate\nopen information window of (POSIX file \"{}\" as alias)\nend tell",
            display_path(&p).replace('\\', "\\\\").replace('"', "\\\"")
        );
        std::process::Command::new("osascript").arg("-e").arg(script).spawn().map(|_| true).map_err(|e| e.to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = p;
        Ok(false)
    }
}

// ------------------------------------------------------------------ helpers

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

/// `<path>.<suffix>`, numbered if that already exists.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut i = 0;
    loop {
        let candidate = if i == 0 { format!("{name}.{suffix}") } else { format!("{name}.{suffix}{i}") };
        let p = path.with_file_name(candidate);
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
    let t = clean_path(&crate::fsx::read_link(src)?);
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if is_junction(src) {
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

thread_local! {
    /// "Verify file contents" option of the operation running on this thread.
    static VERIFY_CONTENTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with the content-verification option set (operations run on one worker thread).
fn with_verify<T>(verify: bool, f: impl FnOnce() -> T) -> T {
    VERIFY_CONTENTS.with(|v| v.set(verify));
    let r = f();
    VERIFY_CONTENTS.with(|v| v.set(false));
    r
}

/// Copies a tree keeping every piece of metadata (see [`crate::meta`]) and hard links
/// between files inside the tree. Every item is read back and compared after copying.
struct Copier<'a> {
    clone: bool,
    verify: bool,
    done: u64,
    files: u64,
    /// file identity -> first copy, to re-create hard links instead of duplicating data
    seen: std::collections::HashMap<(u64, u64), PathBuf>,
    /// every copied item with the metadata applied to it (for the final re-check, see `settle`)
    applied: Vec<(PathBuf, crate::meta::Snapshot)>,
    /// metadata recorded before copying started (see `snapshot_tree`)
    snaps: std::collections::HashMap<PathBuf, crate::meta::Snapshot>,
    on: &'a mut dyn FnMut(&Path, u64),
}

impl Copier<'_> {
    fn copy(&mut self, src: &Path, dst: &Path) -> Result<(), String> {
        let ctx = |what: &str, e: io::Error| format!("{}: {what} failed ({e})", display_path(src));
        // capture BEFORE reading the source, so the "accessed" date is the original one
        let snap = match self.snaps.remove(src) {
            Some(s) => s,
            None => crate::meta::capture(src).map_err(|e| ctx("reading metadata", e))?,
        };
        let md = fs::symlink_metadata(src).map_err(|e| ctx("reading", e))?;

        if md.file_type().is_symlink() {
            copy_link(src, dst).map_err(|e| ctx("re-creating link", e))?;
            crate::meta::apply_and_verify(dst, &snap)?;
            self.applied.push((dst.to_path_buf(), snap));
            return Ok(());
        }
        if md.is_dir() {
            crate::meta::create_dir_like(src, dst).map_err(|e| ctx("creating folder", e))?;
            for e in crate::fsx::list(src).map_err(|e| ctx("listing", e))? {
                self.copy(&e.path, &dst.join(&e.name))?;
            }
            // folder metadata last: adding children changes its dates
            crate::meta::apply_and_verify(dst, &snap)?;
            self.applied.push((dst.to_path_buf(), snap));
            return Ok(());
        }

        // hard links inside the tree stay hard links
        if crate::fsinfo::link_count(src).unwrap_or(1) > 1 {
            if let Some(id) = crate::fsinfo::file_id(src) {
                if let Some(first) = self.seen.get(&id) {
                    fs::hard_link(first, dst).map_err(|e| ctx("re-creating hard link", e))?;
                    self.done += md.len();
                    self.files += 1;
                    (self.on)(src, self.done);
                    return Ok(());
                }
                self.seen.insert(id, dst.to_path_buf());
            }
        }

        if self.clone {
            reflink_copy::reflink(src, dst).map_err(|e| ctx("cloning", e))?;
        } else {
            fs::copy(src, dst).map_err(|e| ctx("copying", e))?;
        }
        if self.verify && !crate::meta::same_contents(src, dst).map_err(|e| ctx("verifying contents", e))? {
            return Err(format!("{}: the copy's contents differ from the original — refusing to continue.", display_path(src)));
        }
        crate::meta::apply_and_verify(dst, &snap)?;
        self.applied.push((dst.to_path_buf(), snap));
        self.done += md.len();
        self.files += 1;
        (self.on)(src, self.done);
        Ok(())
    }
}

fn copy_tree(
    src: &Path,
    dst: &Path,
    snaps: std::collections::HashMap<PathBuf, crate::meta::Snapshot>,
    done: &mut u64,
    files: &mut u64,
    applied: &mut Vec<(PathBuf, crate::meta::Snapshot)>,
    on: &mut dyn FnMut(&Path, u64),
) -> Result<(), String> {
    let verify = VERIFY_CONTENTS.with(|v| v.get());
    let mut c = Copier { clone: false, verify, done: *done, files: 0, seen: Default::default(), applied: Vec::new(), snaps, on };
    let r = c.copy(src, dst);
    *done = c.done;
    *files = c.files;
    *applied = std::mem::take(&mut c.applied);
    r
}

fn clone_tree(src: &Path, dst: &Path, done: &mut u64, on: &mut dyn FnMut(&Path, u64)) -> Result<(), String> {
    let mut c = Copier { clone: true, verify: false, done: *done, files: 0, seen: Default::default(), applied: Vec::new(), snaps: Default::default(), on };
    let r = c.copy(src, dst);
    *done = c.done;
    r
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

/// Record the metadata of every item (each folder before it is listed) and count files/bytes.
fn snapshot_tree(
    p: &Path,
    snaps: &mut std::collections::HashMap<PathBuf, crate::meta::Snapshot>,
    total: &mut size::SizeInfo,
) -> Result<(), String> {
    let ctx = |e: io::Error| format!("{}: reading metadata failed ({e})", display_path(p));
    snaps.insert(p.to_path_buf(), crate::meta::capture(p).map_err(ctx)?);
    let md = fs::symlink_metadata(p).map_err(ctx)?;
    if md.file_type().is_symlink() {
        return Ok(());
    }
    if md.is_dir() {
        for e in crate::fsx::list(p).map_err(ctx)? {
            snapshot_tree(&e.path, snaps, total)?;
        }
    } else {
        total.bytes += md.len();
        total.files += 1;
    }
    Ok(())
}
