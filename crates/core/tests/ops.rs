//! Integration tests for the real file operations.
//! Cross-volume tests use the system temp dir and Cargo's target tmp dir; they only
//! exercise the cross-drive path when those are on different volumes.

use smart_symlink_core::links::{self, LinkKind, Mode};
use smart_symlink_core::listing;
use smart_symlink_core::ops::{self, Request};
use smart_symlink_core::util::display_path;
use std::fs;
use std::path::{Path, PathBuf};

/// Keep the saved-links store of tests away from the user's real one.
fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("smart-symlink-test-config-{}", std::process::id()));
        // SAFETY: set once, before any test reads it, always to the same value.
        unsafe { std::env::set_var("SMART_SYMLINK_CONFIG_DIR", dir) };
    });
}

fn fresh(base: &Path, name: &str) -> PathBuf {
    isolate();
    let p =base.join(format!("smart-symlink-test-{name}-{}", std::process::id()));
    let _ = ops::remove_tree(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn make_tree(root: &Path) {
    fs::create_dir_all(root.join("sub/deeper")).unwrap();
    fs::write(root.join("a.txt"), "hello").unwrap();
    fs::write(root.join("sub/b.bin"), vec![7u8; 100_000]).unwrap();
    fs::write(root.join("sub/deeper/c.txt"), "deep").unwrap();
}

fn assert_tree(root: &Path) {
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "hello");
    assert_eq!(fs::read(root.join("sub/b.bin")).unwrap().len(), 100_000);
    assert_eq!(fs::read_to_string(root.join("sub/deeper/c.txt")).unwrap(), "deep");
}

fn link_kind_of(p: &Path) -> Option<LinkKind> {
    let md = fs::symlink_metadata(p).ok()?;
    listing::detect_link(p, &md, true).map(|l| l.kind)
}

fn run(src: &Path, dest: &Path, kind: LinkKind, mode: Mode) -> ops::ItemResult {
    let r = ops::execute(
        &Request { sources: vec![display_path(src)], dest: display_path(dest), kind, mode, verify: true },
        &|_| {},
    );
    r.into_iter().next().unwrap()
}

fn move_kind() -> LinkKind {
    if cfg!(windows) { LinkKind::Junction } else { LinkKind::Symlink }
}

#[test]
fn move_and_link_same_volume() {
    let base = fresh(&std::env::temp_dir(), "same");
    let src = base.join("home");
    let dest = base.join("D");
    make_tree(&src);
    fs::create_dir(&dest).unwrap();

    let r = run(&src, &dest, move_kind(), Mode::Move);
    assert!(r.ok, "{}", r.message);
    assert_eq!(link_kind_of(&src), Some(move_kind()));
    assert_tree(&dest.join("home"));
    assert_tree(&src); // through the link
    ops::remove_tree(&base).unwrap();
}

#[test]
fn move_and_link_cross_volume() {
    let a = fresh(&std::env::temp_dir(), "xa");
    let b = fresh(Path::new(env!("CARGO_TARGET_TMPDIR")), "xb");
    let src = a.join("home");
    make_tree(&src);

    let r = run(&src, &b, move_kind(), Mode::Move);
    assert!(r.ok, "{}", r.message);
    assert_eq!(link_kind_of(&src), Some(move_kind()));
    assert_tree(&b.join("home"));
    assert_tree(&src);
    // the temporary ".smart-symlink-old" copy must be gone
    let leftovers: Vec<_> = fs::read_dir(&a).unwrap().filter_map(Result::ok).map(|e| e.file_name()).collect();
    assert_eq!(leftovers.len(), 1, "unexpected leftovers: {leftovers:?}");
    ops::remove_tree(&a).unwrap();
    ops::remove_tree(&b).unwrap();
}

#[test]
fn link_only_keeps_data_in_place() {
    let base = fresh(&std::env::temp_dir(), "linkonly");
    let src = base.join("data");
    let dest = base.join("elsewhere");
    make_tree(&src);
    fs::create_dir(&dest).unwrap();

    let r = run(&src, &dest, move_kind(), Mode::Link);
    assert!(r.ok, "{}", r.message);
    assert!(link_kind_of(&src).is_none(), "source must stay a real folder");
    assert_eq!(link_kind_of(&dest.join("data")), Some(move_kind()));
    assert_tree(&dest.join("data"));
    ops::remove_tree(&base).unwrap();
}

#[test]
fn hardlink_files() {
    let base = fresh(&std::env::temp_dir(), "hard");
    let f = base.join("file.txt");
    fs::write(&f, "same data").unwrap();
    let dest = base.join("other");
    fs::create_dir(&dest).unwrap();

    let a = links::analyze(&[display_path(&f)], &display_path(&dest), Mode::Move);
    let opt = a.options.iter().find(|o| o.kind == LinkKind::Hardlink).unwrap();
    assert!(opt.available, "{:?}", opt.reason);

    let r = run(&f, &dest, LinkKind::Hardlink, Mode::Move);
    assert!(r.ok, "{}", r.message);
    assert_eq!(link_kind_of(&f), Some(LinkKind::Hardlink));
    fs::write(dest.join("file.txt"), "changed").unwrap();
    assert_eq!(fs::read_to_string(&f).unwrap(), "changed");
    ops::remove_tree(&base).unwrap();
}

#[test]
fn analysis_rules() {
    let a = fresh(&std::env::temp_dir(), "rules-a");
    let b = fresh(Path::new(env!("CARGO_TARGET_TMPDIR")), "rules-b");
    let dir = a.join("folder");
    fs::create_dir(&dir).unwrap();
    let file = a.join("f.txt");
    fs::write(&file, "x").unwrap();

    // folders: hard link never possible
    let an = links::analyze(&[display_path(&dir)], &display_path(&b), Mode::Move);
    assert!(!an.options.iter().find(|o| o.kind == LinkKind::Hardlink).unwrap().available);
    assert!(an.blocking.is_empty(), "{:?}", an.blocking);
    #[cfg(windows)]
    assert_eq!(an.recommended, Some(LinkKind::Junction));

    // a file on another volume: hard link unavailable with a reason
    let an = links::analyze(&[display_path(&file)], &display_path(&b), Mode::Move);
    if !an.same_volume {
        let h = an.options.iter().find(|o| o.kind == LinkKind::Hardlink).unwrap();
        assert!(!h.available && h.reason.as_deref().unwrap().contains("same drive"));
    }

    // moving a folder into itself is blocked
    let an = links::analyze(&[display_path(&a)], &display_path(&dir), Mode::Move);
    assert!(!an.blocking.is_empty());

    ops::remove_tree(&a).unwrap();
    ops::remove_tree(&b).unwrap();
}

#[test]
fn remove_tree_never_follows_links() {
    let base = fresh(&std::env::temp_dir(), "nofollow");
    let precious = base.join("precious");
    fs::create_dir(&precious).unwrap();
    fs::write(precious.join("keep.txt"), "keep me").unwrap();
    let doomed = base.join("doomed");
    fs::create_dir(&doomed).unwrap();
    ops::create_link(move_kind(), &doomed.join("link"), &precious, true).unwrap();

    ops::remove_tree(&doomed).unwrap();
    assert!(!doomed.exists());
    assert_eq!(fs::read_to_string(precious.join("keep.txt")).unwrap(), "keep me");
    ops::remove_tree(&base).unwrap();
}

#[test]
fn existing_target_is_refused() {
    let base = fresh(&std::env::temp_dir(), "exists");
    let src = base.join("x");
    let dest = base.join("d");
    make_tree(&src);
    fs::create_dir_all(dest.join("x")).unwrap();
    let r = run(&src, &dest, move_kind(), Mode::Move);
    assert!(!r.ok);
    assert!(link_kind_of(&src).is_none());
    assert_tree(&src);
    ops::remove_tree(&base).unwrap();
}

fn unlink(link: &Path, mode: ops::UnlinkMode) -> ops::ItemResult {
    ops::unlink(&[display_path(link)], mode, false, &|_| {}).into_iter().next().unwrap()
}

fn saved_links() -> Vec<String> {
    smart_symlink_core::registry::load().links.into_iter().map(|l| l.link).collect()
}

#[test]
fn created_links_are_saved_and_unlink_restores_same_volume() {
    let base = fresh(&std::env::temp_dir(), "unlink-same");
    let src = base.join("home");
    let dest = base.join("D");
    make_tree(&src);
    fs::create_dir(&dest).unwrap();
    assert!(run(&src, &dest, move_kind(), Mode::Move).ok);
    assert!(saved_links().iter().any(|l| l == &display_path(&src)), "link not saved");

    let r = unlink(&src, ops::UnlinkMode::Restore);
    assert!(r.ok, "{}", r.message);
    assert!(link_kind_of(&src).is_none(), "should be a real folder again");
    assert_tree(&src);
    assert!(!dest.join("home").exists());
    assert!(!saved_links().iter().any(|l| l == &display_path(&src)), "link not forgotten");
    ops::remove_tree(&base).unwrap();
}

#[test]
fn unlink_restores_cross_volume() {
    let a = fresh(&std::env::temp_dir(), "unlink-xa");
    let b = fresh(Path::new(env!("CARGO_TARGET_TMPDIR")), "unlink-xb");
    let src = a.join("home");
    make_tree(&src);
    assert!(run(&src, &b, move_kind(), Mode::Move).ok);

    let r = unlink(&src, ops::UnlinkMode::Restore);
    assert!(r.ok, "{}", r.message);
    assert!(link_kind_of(&src).is_none());
    assert_tree(&src);
    assert!(!b.join("home").exists());
    assert_eq!(fs::read_dir(&a).unwrap().count(), 1, "leftover temp folders");
    ops::remove_tree(&a).unwrap();
    ops::remove_tree(&b).unwrap();
}

#[test]
fn unlink_remove_only_keeps_data_at_target() {
    let base = fresh(&std::env::temp_dir(), "unlink-remove");
    let src = base.join("home");
    let dest = base.join("D");
    make_tree(&src);
    fs::create_dir(&dest).unwrap();
    assert!(run(&src, &dest, move_kind(), Mode::Move).ok);
    let r = unlink(&src, ops::UnlinkMode::Remove);
    assert!(r.ok, "{}", r.message);
    assert!(!src.exists());
    assert_tree(&dest.join("home"));
    ops::remove_tree(&base).unwrap();
}

/// If the data can't be moved back (a file is open), the removed link must be re-created.
#[cfg(windows)]
#[test]
fn failed_unlink_restores_the_link() {
    let base = fresh(&std::env::temp_dir(), "unlink-fail");
    let src = base.join("home");
    let dest = base.join("D");
    make_tree(&src);
    fs::create_dir(&dest).unwrap();
    assert!(run(&src, &dest, LinkKind::Junction, Mode::Move).ok);

    // hold a file open without delete sharing -> renaming the folder fails
    use std::os::windows::fs::OpenOptionsExt;
    let lock = fs::OpenOptions::new().read(true).share_mode(1).open(dest.join("home/a.txt")).unwrap();
    let r = unlink(&src, ops::UnlinkMode::Restore);
    drop(lock);
    assert!(!r.ok, "unlink should fail while a file is open");
    assert!(r.message.contains("link was restored"), "{}", r.message);
    assert_eq!(link_kind_of(&src), Some(LinkKind::Junction), "link must be back");
    assert_tree(&src);
    ops::remove_tree(&base).unwrap();
}

#[test]
fn hardlink_unlink_and_file_ops() {
    let base = fresh(&std::env::temp_dir(), "fileops");
    let f = base.join("file.txt");
    fs::write(&f, "data").unwrap();
    let dest = base.join("other");
    fs::create_dir(&dest).unwrap();
    assert!(run(&f, &dest, LinkKind::Hardlink, Mode::Link).ok);
    let r = unlink(&dest.join("file.txt"), ops::UnlinkMode::Remove);
    assert!(r.ok, "{}", r.message);
    assert_eq!(fs::read_to_string(&f).unwrap(), "data");

    let made = ops::create_dir(&display_path(&base), "New folder").unwrap();
    assert!(Path::new(&made).is_dir());
    assert!(ops::create_dir(&display_path(&base), "bad/name").is_err());
    let renamed = ops::rename(&made, "Renamed").unwrap();
    assert!(Path::new(&renamed).is_dir());
    let moved = ops::move_items(&[renamed.clone()], &display_path(&dest), false, &|_| {});
    assert!(moved[0].ok, "{}", moved[0].message);
    assert!(dest.join("Renamed").is_dir());
    let del = ops::delete(&[display_path(&dest.join("Renamed"))], true, &|_| {});
    assert!(del[0].ok, "{}", del[0].message);
    assert!(!dest.join("Renamed").exists());
    ops::remove_tree(&base).unwrap();
}
