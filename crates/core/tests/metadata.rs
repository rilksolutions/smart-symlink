//! Moving data between drives must not change ANY metadata. These tests build a tree with
//! unusual metadata, move it (forcing the copy path even on single-drive machines), compare
//! every item against snapshots taken beforehand, then unlink (move back) and compare again.

use smart_symlink_core::links::{LinkKind, Mode};
use smart_symlink_core::meta;
use smart_symlink_core::ops::{self, Request, UnlinkMode};
use smart_symlink_core::util::display_path;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let cfg = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("smart-symlink-meta-config-{}", std::process::id()));
        // SAFETY: set once before any test reads them, always to the same values.
        unsafe {
            std::env::set_var("SMART_SYMLINK_CONFIG_DIR", cfg);
            std::env::set_var("SMART_SYMLINK_FORCE_COPY", "1");
            // on a failure, show the exact security text that differs
            std::env::set_var("SMART_SYMLINK_VERBOSE", "1");
        }
    });
}

fn fresh(base: &Path, name: &str) -> PathBuf {
    setup();
    let p = base.join(format!("smart-symlink-meta-{name}-{}", std::process::id()));
    let _ = ops::remove_tree(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

/// Snapshot the given items (relative to `root`). Deliberately does NOT list folders:
/// listing a folder bumps its "accessed" date on systems with access-time updates enabled.
fn snapshot(root: &Path, items: &[&str]) -> BTreeMap<PathBuf, meta::Snapshot> {
    items.iter().map(|rel| (PathBuf::from(rel), meta::capture(&root.join(rel)).unwrap())).collect()
}

fn assert_same(root: &Path, before: &BTreeMap<PathBuf, meta::Snapshot>) {
    let mut problems = Vec::new();
    for (rel, snap) in before {
        let p = root.join(rel);
        if let Err(what) = meta::verify(&p, snap) {
            problems.push(format!("{}: {what}", display_path(&p)));
        }
    }
    assert!(problems.is_empty(), "metadata changed:\n{}", problems.join("\n"));
}

fn run_move(src: &Path, dest: &Path, kind: LinkKind) {
    let r = ops::execute(
        &Request { sources: vec![display_path(src)], dest: display_path(dest), kind, mode: Mode::Move, verify: true },
        &|_| {},
    );
    assert!(r[0].ok, "move failed: {}", r[0].message);
}

fn run_unlink(link: &Path) {
    let r = ops::unlink(&[display_path(link)], UnlinkMode::Restore, true, &|_| {});
    assert!(r[0].ok, "unlink failed: {}", r[0].message);
}

#[cfg(windows)]
#[test]
fn move_and_unlink_keep_every_detail_windows() {
    use std::fs::FileTimes;
    use std::os::windows::fs::{FileTimesExt, OpenOptionsExt};
    use std::process::Command;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let a = fresh(&std::env::temp_dir(), "win-a");
    let b = fresh(Path::new(env!("CARGO_TARGET_TMPDIR")), "win-b");
    let src = a.join("home");
    let sh = |args: &[&str]| assert!(Command::new(args[0]).args(&args[1..]).output().unwrap().status.success(), "{args:?}");

    // destination parent hands out an extra inheritable permission: it must NOT show up on the copies
    sh(&["icacls", &display_path(&b), "/grant", "*S-1-1-0:(OI)(CI)(RX)"]);

    fs::create_dir_all(src.join("sub/deeper")).unwrap();
    fs::write(src.join("f1.txt"), "one").unwrap();
    fs::write(src.join("sub/f2.bin"), vec![9u8; 70_000]).unwrap();
    fs::write(src.join("sub/f2.bin:extra"), "alternate data stream").unwrap();
    fs::write(src.join("f3.txt"), "hard linked").unwrap();
    fs::hard_link(src.join("f3.txt"), src.join("sub/f3-link.txt")).unwrap();
    fs::write(src.join("f4.txt"), "own permissions").unwrap();
    fs::write(src.join("sub/deeper/f5.txt"), "deep").unwrap();
    // f4: inheritance off, explicit entries only
    let me = std::env::var("USERNAME").unwrap();
    sh(&["icacls", &display_path(&src.join("f4.txt")), "/inheritance:r", "/grant:r", &format!("{me}:(F)"), "*S-1-5-18:(R)"]);
    sh(&["attrib", "+h", "+r", &display_path(&src.join("f1.txt"))]);
    sh(&["attrib", "+h", &display_path(&src.join("sub"))]);

    // distinct created / accessed / modified dates; folders last (children change folder dates)
    let t = |y: u64| UNIX_EPOCH + Duration::from_secs((y - 1970) * 365 * 86_400 + 12_345);
    let set = |p: &Path, c: SystemTime, a: SystemTime, m: SystemTime| {
        let f = fs::OpenOptions::new().access_mode(0x100).custom_flags(0x0200_0000).open(p).unwrap();
        f.set_times(FileTimes::new().set_created(c).set_accessed(a).set_modified(m)).unwrap();
    };
    for f in ["f1.txt", "sub/f2.bin", "f3.txt", "f4.txt", "sub/deeper/f5.txt"] {
        set(&src.join(f), t(2001), t(2002), t(2003));
    }
    for d in ["sub/deeper", "sub", ""] {
        set(&src.join(d), t(2005), t(2006), t(2007));
    }

    let before = snapshot(&src, &["", "f1.txt", "f3.txt", "f4.txt", "sub", "sub/f2.bin", "sub/f3-link.txt", "sub/deeper", "sub/deeper/f5.txt"]);
    run_move(&src, &b, LinkKind::Junction);
    let moved = b.join("home");
    assert_same(&moved, &before);
    // (the alternate data stream is checked at the end: reading it here would bump its access date)
    assert_eq!(
        smart_symlink_core::fsinfo::file_id(&moved.join("f3.txt")),
        smart_symlink_core::fsinfo::file_id(&moved.join("sub/f3-link.txt")),
        "hard link inside the tree must stay a hard link"
    );

    // and back again
    run_unlink(&src);
    assert_same(&src, &before);
    assert_eq!(fs::read_to_string(src.join("sub/f2.bin:extra")).unwrap(), "alternate data stream");

    ops::remove_tree(&a).unwrap();
    ops::remove_tree(&b).unwrap();
}

#[cfg(unix)]
#[test]
fn move_and_unlink_keep_every_detail_unix() {
    use filetime::FileTime;
    use std::os::unix::fs::PermissionsExt;

    let a = fresh(&std::env::temp_dir(), "unix-a");
    let b = fresh(Path::new(env!("CARGO_TARGET_TMPDIR")), "unix-b");
    let src = a.join("home");
    fs::create_dir_all(src.join("sub/deeper")).unwrap();
    fs::write(src.join("f1.txt"), "one").unwrap();
    fs::write(src.join("sub/f2.bin"), vec![9u8; 70_000]).unwrap();
    fs::write(src.join("f3.txt"), "hard linked").unwrap();
    fs::hard_link(src.join("f3.txt"), src.join("sub/f3-link.txt")).unwrap();
    std::os::unix::fs::symlink("f1.txt", src.join("rel-link")).unwrap();
    let _ = xattr::set(src.join("f1.txt"), "user.smart-symlink", b"kept"); // ignored if the filesystem can't
    fs::set_permissions(src.join("f1.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::set_permissions(src.join("sub/f2.bin"), fs::Permissions::from_mode(0o751)).unwrap();
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o750)).unwrap();
    let t = |s: i64| FileTime::from_unix_time(s, 123_456_789);
    for f in ["f1.txt", "sub/f2.bin", "f3.txt"] {
        filetime::set_file_times(src.join(f), t(1_000_000_000), t(1_100_000_000)).unwrap();
    }
    filetime::set_symlink_file_times(src.join("rel-link"), t(900_000_000), t(950_000_000)).unwrap();
    for d in ["sub/deeper", "sub", ""] {
        filetime::set_file_times(src.join(d), t(1_200_000_000), t(1_300_000_000)).unwrap();
    }

    let before = snapshot(&src, &["", "f1.txt", "f3.txt", "rel-link", "sub", "sub/f2.bin", "sub/f3-link.txt", "sub/deeper"]);
    run_move(&src, &b, LinkKind::Symlink);
    let moved = b.join("home");
    assert_same(&moved, &before);
    // read through the app's own reader: a plain readlink would change the link's "accessed" date on Linux
    assert_eq!(smart_symlink_core::fsx::read_link(&moved.join("rel-link")).unwrap(), Path::new("f1.txt"));
    assert_same(&moved, &before);
    assert_eq!(
        smart_symlink_core::fsinfo::file_id(&moved.join("f3.txt")),
        smart_symlink_core::fsinfo::file_id(&moved.join("sub/f3-link.txt"))
    );

    run_unlink(&src);
    assert_same(&src, &before);

    ops::remove_tree(&a).unwrap();
    ops::remove_tree(&b).unwrap();
}

/// Browsing a folder and measuring its size must not change any "accessed" date.
#[cfg(any(windows, target_os = "linux"))]
#[test]
fn browsing_and_sizes_do_not_touch_access_dates() {
    use std::time::{Duration, UNIX_EPOCH};
    let base = fresh(Path::new(env!("CARGO_TARGET_TMPDIR")), "quiet");
    fs::create_dir_all(base.join("a/b")).unwrap();
    fs::write(base.join("a/b/f.txt"), "x").unwrap();
    let old = UNIX_EPOCH + Duration::from_secs(30 * 365 * 86_400);
    let set_old = |p: &Path| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let f = fs::OpenOptions::new().access_mode(0x100).custom_flags(0x0200_0000).open(p).unwrap();
            f.set_times(fs::FileTimes::new().set_accessed(old)).unwrap();
        }
        #[cfg(not(windows))]
        filetime::set_file_atime(p, filetime::FileTime::from_system_time(old)).unwrap();
    };
    for d in ["a/b/f.txt", "a/b", "a"] {
        set_old(&base.join(d));
    }
    let before = snapshot(&base, &["a", "a/b", "a/b/f.txt"]);
    smart_symlink_core::listing::list_dir(&display_path(&base.join("a"))).unwrap();
    smart_symlink_core::size::dir_size(&base.join("a"), None).unwrap();
    assert_same(&base, &before);
    ops::remove_tree(&base).unwrap();
}
