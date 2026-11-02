//! Folders moved or deleted on one Mac: the other Mac does the same, without
//! fetching content it already has and without bringing anything back.

mod common;

use common::*;
use macsync_engine::PullOptions;

fn pull(p: &Pair, unit: &str) -> macsync_engine::PullReport {
    let r = p.b.sync_from(&p.a, Some(&[unit.to_string()]), PullOptions::default()).unwrap();
    r.into_iter().next().unwrap()
}

/// The receiving Mac applies the deletion of the old place first (its files
/// go to the trash), then fetches the new place: the content comes from the
/// trash, nothing from the network.
#[test]
fn moved_folder_is_not_transferred_again() {
    let p = Pair::new();
    let big = "x".repeat(300_000);
    write(&p.ra(), "old/src/main.rs", "fn main() {}");
    write(&p.ra(), "old/assets/big.bin", &big);
    write(&p.ra(), "old/README.md", "readme");
    p.sync();
    assert_eq!(read(&p.rb(), "old/assets/big.bin"), big);

    std::fs::rename(p.ra().join("old"), p.ra().join("new")).unwrap();
    p.a.scan_all(false).unwrap();
    let gone = pull(&p, "old");
    assert_eq!(gone.deletes, 3 + 2 + 1, "files, folders and the unit");
    assert!(!p.rb().join("old").exists());
    let moved = pull(&p, "new");
    assert_eq!(moved.bytes_fetched, 0, "moved, not downloaded: {moved:?}");
    assert!(moved.bytes_deduped >= big.len() as u64);
    assert_eq!(read(&p.rb(), "new/assets/big.bin"), big);
    p.sync();
    p.assert_identical();
}

/// The same files created on both Macs independently are in sync without
/// ever having been exchanged, so each Mac has its own version. Deleting the
/// folder on one Mac must delete it on the other, not bring the files back.
#[test]
fn deleted_folder_goes_even_where_its_files_were_created_independently() {
    let p = Pair::new();
    for r in [p.ra(), p.rb()] {
        write(&r, "site/LICENSE", "MIT");
        write(&r, "site/css/prism.css", "pre {}");
    }
    write(&p.ra(), "site/index.html", "<html>");
    p.sync();
    p.assert_identical();
    let ours = p.a.store().entry("site", "LICENSE").unwrap().unwrap().entry.vv;
    let theirs = p.b.store().entry("site", "LICENSE").unwrap().unwrap().entry.vv;
    assert_eq!(ours.compare(&theirs), macsync_core::VvOrdering::Concurrent, "test needs independent versions");

    std::fs::remove_dir_all(p.ra().join("site")).unwrap();
    p.sync();
    assert!(!p.ra().join("site").exists(), "came back");
    assert!(!p.rb().join("site").exists(), "kept on the other Mac");
    assert!(p.a.store().conflicts(false).unwrap().is_empty());
}

/// A different file at the same place still wins against the deletion.
#[test]
fn deleted_folder_keeps_a_different_independent_file() {
    let p = Pair::new();
    write(&p.ra(), "site/notes.md", "mine");
    write(&p.ra(), "site/index.html", "<html>");
    p.sync();
    std::fs::remove_dir_all(p.ra().join("site")).unwrap();
    write(&p.rb(), "site/notes.md", "changed on B");
    p.sync();
    assert_eq!(read(&p.ra(), "site/notes.md"), "changed on B");
    assert_eq!(read(&p.rb(), "site/notes.md"), "changed on B");
    assert!(!p.rb().join("site/index.html").exists());
}

/// A pull that changes nothing is not reported as a change, so the daemon
/// does not wake the other Mac for it.
#[test]
fn pull_without_changes_is_not_a_change() {
    let p = Pair::new();
    write(&p.ra(), "w/a.txt", "a");
    let first = p.sync();
    assert!(first.iter().any(|r| r.changed));
    write(&p.ra(), "w/b.txt", "b");
    p.a.scan_all(false).unwrap();
    let r = pull(&p, "w");
    assert!(r.changed);
    let again = p.b.sync_from(&p.a, Some(&["w".to_string()]), PullOptions { ..Default::default() }).unwrap();
    assert!(again.iter().all(|r| !r.changed), "{again:?}");
}

/// Like the move of `chrisurf/<repo>` to `github/chrisurf/<repo>`: a
/// repository below a folder, moved into another folder. Neither the work
/// tree nor `.git` is downloaded again.
#[test]
fn moved_repository_is_not_transferred_again() {
    if !git_available() {
        return;
    }
    let p = Pair::new();
    init_repo(&p.ra().join("chrisurf/app"));
    write(&p.ra(), "chrisurf/app/data.bin", &"d".repeat(200_000));
    write(&p.ra(), "chrisurf/notes.md", "stays");
    p.sync();
    assert_fsck(&p.rb().join("chrisurf/app"));

    std::fs::create_dir_all(p.ra().join("github/chrisurf")).unwrap();
    std::fs::rename(p.ra().join("chrisurf/app"), p.ra().join("github/chrisurf/app")).unwrap();
    p.a.scan_all(false).unwrap();
    pull(&p, "chrisurf/app");
    assert!(!p.rb().join("chrisurf/app").exists());
    let all = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    // Only `.git/index`: each Mac writes its own bytes for the same index.
    let index = p.a.store().entry("github/chrisurf/app", ".git/index").unwrap().unwrap().entry.size;
    let fetched: u64 = all.iter().map(|r| r.bytes_fetched).sum();
    assert!(fetched <= index, "{:?}", all.iter().map(|r| (&r.unit, r.bytes_fetched)).collect::<Vec<_>>());
    p.sync();
    p.assert_identical();
    assert_fsck(&p.rb().join("github/chrisurf/app"));
    assert_eq!(read(&p.rb(), "chrisurf/notes.md"), "stays");
}

/// Moving a folder reports only the folder itself (FSEvents). Repositories
/// inside it are units of their own and must be captured as gone as well –
/// like the daemon does it: scan the folder, then what it reports as nested.
#[test]
fn repository_inside_a_moved_folder_is_captured_as_gone() {
    if !git_available() {
        return;
    }
    let p = Pair::new();
    write(&p.ra(), "reviews/README.md", "reviews");
    init_repo(&p.ra().join("reviews/targets/grid"));
    p.sync();
    assert!(p.rb().join("reviews/targets/grid/.git").exists());

    std::fs::create_dir_all(p.ra().join("github")).unwrap();
    std::fs::rename(p.ra().join("reviews"), p.ra().join("github/reviews")).unwrap();
    let mut queue = vec!["reviews".to_string()];
    while let Some(u) = queue.pop() {
        let r = p.a.scan_unit(&u, &Default::default()).unwrap();
        queue.extend(r.nested);
    }
    let live = p.a.store().entries("reviews/targets/grid").unwrap().into_iter().filter(|s| !s.entry.deleted).count();
    assert_eq!(live, 0, "repository inside the moved folder not captured as gone");
    p.sync();
    assert!(!p.rb().join("reviews").exists(), "old place kept on the other Mac");
    assert!(p.rb().join("github/reviews/targets/grid/.git").exists());
    p.assert_identical();
}
