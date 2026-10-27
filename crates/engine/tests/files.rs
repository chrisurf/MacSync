//! Two-peer integration tests for ordinary folders.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

use common::*;
use macsync_engine::{Choice, Mode, Outcome, PullOptions};

#[test]
fn initial_sync_copies_everything() {
    let p = Pair::new();
    let a = p.ra();
    write(&a, "notes/todo.md", "- [ ] sync\n");
    write(&a, "notes/deep/er/file.txt", "deep");
    fs::create_dir_all(a.join("notes/empty")).unwrap();
    write(&a, "notes/run.sh", "#!/bin/sh\necho hi\n");
    fs::set_permissions(a.join("notes/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("todo.md", a.join("notes/link")).unwrap();
    symlink("/nowhere/at/all", a.join("notes/dead-link")).unwrap();
    write(&a, "loose.txt", "in the root");
    write(&a, "proj/node_modules/.bin/tool", "x");
    write(&a, "notes/ünïcödé.txt", "u");
    fs::write(a.join("notes/empty-file"), "").unwrap();
    write(&a, "notes/.DS_Store", "mac junk");

    p.sync();
    p.assert_identical_except_ds_store();
    let b = p.rb();
    assert_eq!(read(&b, "notes/deep/er/file.txt"), "deep");
    assert_eq!(fs::metadata(b.join("notes/run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::read_link(b.join("notes/link")).unwrap().to_str(), Some("todo.md"));
    assert!(fs::symlink_metadata(b.join("notes/dead-link")).unwrap().file_type().is_symlink());
    assert!(b.join("notes/empty").is_dir());
    assert!(!b.join("notes/.DS_Store").exists(), "default exclusion");
    assert_eq!(read(&b, "loose.txt"), "in the root");
    assert_eq!(
        fs::metadata(a.join("notes/todo.md")).unwrap().mtime(),
        fs::metadata(b.join("notes/todo.md")).unwrap().mtime(),
        "mtime preserved"
    );
}

impl Pair {
    fn assert_identical_except_ds_store(&self) {
        let _ = fs::remove_file(self.ra().join("notes/.DS_Store"));
        self.assert_identical();
    }
}

#[test]
fn changes_and_deletions_propagate_both_ways() {
    let p = Pair::new();
    write(&p.ra(), "docs/a.txt", "1");
    write(&p.ra(), "docs/b.txt", "1");
    write(&p.ra(), "docs/sub/c.txt", "1");
    p.sync();
    // Change on B, delete on A.
    write(&p.rb(), "docs/a.txt", "2 from B");
    fs::remove_file(p.ra().join("docs/b.txt")).unwrap();
    fs::remove_dir_all(p.ra().join("docs/sub")).unwrap();
    p.sync();
    p.assert_identical();
    assert_eq!(read(&p.ra(), "docs/a.txt"), "2 from B");
    assert!(!p.rb().join("docs/b.txt").exists());
    assert!(!p.rb().join("docs/sub").exists());
    // Old versions are in the trash, not lost.
    let trash = p.rb().join(".macsync/trash");
    let n = walkdir_count(&trash, "b.txt");
    assert!(n >= 1, "deleted file kept in trash");
    // A tombstone prevents resurrection: B still had b.txt in its index; after
    // the sync it must not come back on A.
    p.sync();
    assert!(!p.ra().join("docs/b.txt").exists());
}

fn walkdir_count(dir: &std::path::Path, name: &str) -> usize {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name() == name {
                n += 1;
            }
            if e.file_type().unwrap().is_dir() {
                n += walkdir_count(&e.path(), name);
            }
        }
    }
    n
}

#[test]
fn concurrent_edit_is_a_conflict_and_never_silently_lost() {
    let p = Pair::new();
    write(&p.ra(), "w/f.txt", "base");
    p.sync();
    write(&p.ra(), "w/f.txt", "edit on A");
    write(&p.rb(), "w/f.txt", "edit on B!");
    let reports = p.sync();
    assert!(reports.iter().any(|r| r.conflicts > 0));
    // Each side keeps its own version.
    assert_eq!(read(&p.ra(), "w/f.txt"), "edit on A");
    assert_eq!(read(&p.rb(), "w/f.txt"), "edit on B!");
    let ca = p.a.store().conflicts(false).unwrap();
    let cb = p.b.store().conflicts(false).unwrap();
    assert_eq!(ca.len(), 1);
    assert_eq!(cb.len(), 1);
    // The remote version is in the conflict store outside the repository.
    let stored = std::path::Path::new(&ca[0].stored).join("f.txt");
    assert_eq!(fs::read_to_string(stored).unwrap(), "edit on B!");
    // Repeated syncs do not duplicate the conflict.
    p.sync();
    assert_eq!(p.a.store().conflicts(false).unwrap().len(), 1);

    // Resolve on A by taking B's version; B's conflict then disappears too.
    p.a.resolve(ca[0].id, Choice::Remote).unwrap();
    p.sync();
    assert_eq!(read(&p.ra(), "w/f.txt"), "edit on B!");
    assert_eq!(read(&p.rb(), "w/f.txt"), "edit on B!");
    // B's conflict became moot (A now holds B's content with a newer vector)
    // and was closed automatically.
    assert!(p.b.store().conflicts(false).unwrap().is_empty());
    assert!(p.b.resolve(cb[0].id, Choice::Local).is_err(), "already resolved");
    p.assert_identical();
}

#[test]
fn resolve_both_keeps_two_files() {
    let p = Pair::new();
    write(&p.ra(), "w/f.txt", "base");
    p.sync();
    write(&p.ra(), "w/f.txt", "A");
    write(&p.rb(), "w/f.txt", "B");
    p.sync();
    let c = p.a.store().conflicts(false).unwrap().remove(0);
    p.a.resolve(c.id, Choice::Both).unwrap();
    p.sync();
    // A's resolution dominates; B's conflict is superseded automatically.
    assert!(p.b.store().conflicts(false).unwrap().is_empty());
    p.assert_identical();
    let names: Vec<String> = fs::read_dir(p.rb().join("w"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().any(|n| n.starts_with("f.conflict-")), "{names:?}");
    assert_eq!(read(&p.rb(), "w/f.txt"), "A");
}

#[test]
fn modification_beats_deletion() {
    let p = Pair::new();
    write(&p.ra(), "w/f.txt", "base");
    p.sync();
    write(&p.ra(), "w/f.txt", "still needed");
    fs::remove_file(p.rb().join("w/f.txt")).unwrap();
    p.sync();
    p.assert_identical();
    assert_eq!(read(&p.rb(), "w/f.txt"), "still needed");
}

#[test]
fn identical_concurrent_change_is_not_a_conflict() {
    let p = Pair::new();
    write(&p.ra(), "w/f.txt", "base");
    p.sync();
    write(&p.ra(), "w/f.txt", "same");
    write(&p.rb(), "w/f.txt", "same");
    let r = p.sync();
    assert!(r.iter().all(|r| r.conflicts == 0));
    p.assert_identical();
    assert!(p.a.store().conflicts(false).unwrap().is_empty());
}

#[test]
fn prefer_local_resolves_automatically() {
    let p = Pair::new();
    write(&p.ra(), "w/f.txt", "base");
    p.sync();
    p.a.store().set_prefer("w", macsync_core::Prefer::Local).unwrap();
    write(&p.ra(), "w/f.txt", "A wins");
    write(&p.rb(), "w/f.txt", "B loses");
    p.sync();
    assert_eq!(read(&p.ra(), "w/f.txt"), "A wins");
    // B recorded a conflict when it pulled first, but A's dominating version
    // arrives and supersedes it automatically.
    assert_eq!(read(&p.rb(), "w/f.txt"), "A wins");
    assert!(p.b.store().conflicts(false).unwrap().is_empty());
    p.assert_identical();
}

#[test]
fn mirror_receive_overwrites_local_changes() {
    let p = Pair::with_modes(Mode::MirrorSend, Mode::MirrorReceive);
    write(&p.ra(), "m/f.txt", "source");
    p.sync();
    write(&p.rb(), "m/f.txt", "local edit on mirror");
    write(&p.rb(), "m/extra.txt", "only on mirror");
    p.sync();
    assert_eq!(read(&p.rb(), "m/f.txt"), "source");
    assert!(!p.rb().join("m/extra.txt").exists());
    assert_eq!(read(&p.ra(), "m/f.txt"), "source", "the sender never applies remote state");
    p.assert_identical();
}

#[test]
fn whole_unit_deletion_and_recreation() {
    let p = Pair::new();
    write(&p.ra(), "gone/x.txt", "x");
    p.sync();
    assert!(p.rb().join("gone/x.txt").exists());
    fs::remove_dir_all(p.ra().join("gone")).unwrap();
    p.sync();
    assert!(!p.rb().join("gone").exists());
    write(&p.ra(), "gone/y.txt", "back");
    p.sync();
    assert_eq!(read(&p.rb(), "gone/y.txt"), "back");
    p.assert_identical();
}

#[test]
fn large_files_and_dedup() {
    let p = Pair::new();
    let big: Vec<u8> = (0..6_000_000u32).map(|i| (i % 251) as u8).collect();
    fs::create_dir_all(p.ra().join("media")).unwrap();
    fs::write(p.ra().join("media/big.bin"), &big).unwrap();
    let r = p.sync();
    assert_eq!(fs::read(p.rb().join("media/big.bin")).unwrap(), big);
    assert!(r.iter().map(|r| r.bytes_fetched).sum::<u64>() >= big.len() as u64);
    // Same content in another unit is materialised locally (no transfer).
    fs::create_dir_all(p.ra().join("copy")).unwrap();
    fs::write(p.ra().join("copy/big-again.bin"), &big).unwrap();
    let r = p.sync();
    let fetched: u64 = r.iter().map(|r| r.bytes_fetched).sum();
    let deduped: u64 = r.iter().map(|r| r.bytes_deduped).sum();
    assert!(deduped >= big.len() as u64, "deduped {deduped}");
    assert!(fetched < 1024, "fetched {fetched}");
    p.assert_identical();
}

#[test]
fn many_small_files_are_bundled() {
    let p = Pair::new();
    for i in 0..2000 {
        write(&p.ra(), &format!("app/node_modules/pkg{}/index{}.js", i % 50, i), &format!("module.exports = {i};\n"));
    }
    let r = p.sync();
    p.assert_identical();
    let writes: usize = r.iter().map(|r| r.writes).sum();
    assert!(writes >= 2000);
}

#[test]
fn xattrs_are_synced_when_supported() {
    let p = Pair::new();
    write(&p.ra(), "x/tagged.txt", "t");
    let f = p.ra().join("x/tagged.txt");
    if xattr::set(&f, "user.macsync.test", b"red").is_err() {
        eprintln!("xattrs unsupported; skipping");
        return;
    }
    p.sync();
    assert_eq!(xattr::get(p.rb().join("x/tagged.txt"), "user.macsync.test").unwrap().as_deref(), Some(&b"red"[..]));
    // Metadata-only change propagates without content transfer.
    xattr::set(&f, "user.macsync.test", b"blue").unwrap();
    let r = p.sync();
    assert_eq!(xattr::get(p.rb().join("x/tagged.txt"), "user.macsync.test").unwrap().as_deref(), Some(&b"blue"[..]));
    assert_eq!(r.iter().map(|r| r.bytes_fetched).sum::<u64>(), 0);
}

#[test]
fn hard_links_are_recreated() {
    let p = Pair::new();
    write(&p.ra(), "h/store/file", "shared");
    fs::create_dir_all(p.ra().join("h/proj")).unwrap();
    fs::hard_link(p.ra().join("h/store/file"), p.ra().join("h/proj/file")).unwrap();
    p.sync();
    let m1 = fs::metadata(p.rb().join("h/store/file")).unwrap();
    let m2 = fs::metadata(p.rb().join("h/proj/file")).unwrap();
    assert_eq!(m1.ino(), m2.ino(), "hard link group recreated");
    p.assert_identical();
}

#[test]
fn dry_run_changes_nothing() {
    let p = Pair::new();
    write(&p.ra(), "d/new.txt", "n");
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    let r = p.b.sync_from(&p.a, None, PullOptions { dry_run: true }).unwrap();
    let d = r.iter().find(|r| r.unit == "d").unwrap();
    assert_eq!(d.outcome, Outcome::DryRun);
    assert!(d.actions.iter().any(|a| a.path == "new.txt"));
    assert!(!p.rb().join("d").exists());
}

#[test]
fn unscanned_local_change_is_never_overwritten() {
    let p = Pair::new();
    write(&p.ra(), "u/f.txt", "1");
    p.sync();
    write(&p.ra(), "u/f.txt", "2 from A");
    p.a.scan_all(false).unwrap();
    // B edits but does not capture before pulling.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&p.rb(), "u/f.txt", "unsaved B edit");
    let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    let u = r.iter().find(|r| r.unit == "u").unwrap();
    assert!(matches!(u.outcome, Outcome::Deferred(_)), "{:?}", u.outcome);
    assert_eq!(read(&p.rb(), "u/f.txt"), "unsaved B edit");
    // After capture it becomes a regular conflict.
    p.sync();
    assert_eq!(read(&p.rb(), "u/f.txt"), "unsaved B edit");
    assert_eq!(p.b.store().conflicts(false).unwrap().len(), 1);
}

#[test]
fn paused_unit_is_not_synced() {
    let p = Pair::new();
    write(&p.ra(), "p/f.txt", "1");
    p.a.scan_all(false).unwrap();
    p.a.store().set_paused("p", true).unwrap();
    p.sync();
    assert!(!p.rb().join("p").exists());
    p.a.store().set_paused("p", false).unwrap();
    p.sync();
    assert!(p.rb().join("p/f.txt").exists());
}

#[test]
fn excluded_paths_are_not_synced_and_not_deleted_remotely() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = macsync_engine::EngineConfig::new(dir.path().join("a"), dir.path().join("as"), "A");
    cfg.excludes = macsync_fs::Excludes::new(&["**/target".to_string()], true).unwrap();
    let a = macsync_engine::Engine::open(cfg).unwrap();
    let b = engine(dir.path(), "B", Mode::TwoWay);
    write(a.root(), "r/src/lib.rs", "x");
    write(a.root(), "r/target/debug/out", "big");
    a.scan_all(false).unwrap();
    b.scan_all(false).unwrap();
    b.sync_from(&a, None, PullOptions::default()).unwrap();
    assert!(b.root().join("r/src/lib.rs").exists());
    assert!(!b.root().join("r/target").exists());
}

#[test]
fn conflict_view_and_bulk_resolve() {
    let p = Pair::new();
    for f in ["a.txt", "b.txt", "c.txt"] {
        write(&p.ra(), &format!("w/{f}"), "base");
    }
    p.sync();
    for f in ["a.txt", "b.txt", "c.txt"] {
        write(&p.ra(), &format!("w/{f}"), "A");
        write(&p.rb(), &format!("w/{f}"), "B!");
    }
    p.sync();

    let views = p.a.conflict_views(false).unwrap();
    assert_eq!(views.len(), 3);
    let v = views.iter().find(|v| v.path == "a.txt").unwrap();
    assert_eq!((v.class.as_str(), v.local.state, v.remote.state), ("concurrent-modify", "present", "present"));
    assert_eq!((v.local.size, v.remote.size), (Some(1), Some(2)));
    assert!(v.can_keep_both);
    // Both versions can be opened on this Mac: the file and the stored copy.
    assert_eq!(fs::read_to_string(v.local.file.as_ref().unwrap()).unwrap(), "A");
    assert_eq!(fs::read_to_string(v.remote.file.as_ref().unwrap()).unwrap(), "B!");

    // All at once; an unknown id fails on its own without stopping the rest.
    let mut ids: Vec<i64> = views.iter().map(|v| v.id).collect();
    ids.push(9999);
    let r = p.a.resolve_many(&ids, Choice::Remote).unwrap();
    assert_eq!(r.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ids, "results in request order");
    assert_eq!(r.iter().filter(|(_, r)| r.is_ok()).count(), 3);
    assert!(r[3].1.is_err());
    assert!(p.a.conflict_views(false).unwrap().is_empty());
    assert_eq!(
        p.a.conflict_views(true).unwrap().iter().filter(|v| v.resolution.as_deref() == Some("remote")).count(),
        3
    );
    // Resolving again reports it instead of doing anything.
    assert!(p.a.resolve_many(&ids[..1], Choice::Local).unwrap()[0].1.is_err());

    p.sync();
    for f in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(read(&p.ra(), &format!("w/{f}")), "B!");
    }
    p.assert_identical();
}
