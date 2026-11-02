//! Build and package folders switched off in the settings: no longer
//! synchronised, never deleted, not fetched back from the other Mac.

mod common;

use std::path::Path;

use common::*;
use macsync_engine::{Engine, EngineConfig, PeerApi, PullOptions};

fn reopen(base: &Path, name: &str, skip: &[&str]) -> Engine {
    let root = base.join(format!("{name}-root"));
    let mut cfg = EngineConfig::new(root.clone(), base.join(format!("{name}-state")), name);
    let skip: Vec<String> = skip.iter().map(|s| s.to_string()).collect();
    cfg.excludes = macsync_fs::Excludes::new(&[], true).unwrap().with_ecosystems(&root, &skip, true);
    Engine::open(cfg).unwrap()
}

fn setup() -> Pair {
    let p = Pair::new();
    write(&p.ra(), "web/package.json", "{}");
    write(&p.ra(), "web/node_modules/left-pad/index.js", "module.exports = 1");
    write(&p.ra(), "rs/Cargo.toml", "[package]");
    write(&p.ra(), "rs/src/main.rs", "fn main() {}");
    write(&p.ra(), "rs/target/debug/app", "binary");
    write(&p.ra(), "photos/target/a.jpg", "jpeg");
    p.sync();
    assert!(p.rb().join("web/node_modules/left-pad/index.js").exists());
    p
}

#[test]
fn switching_off_keeps_the_files_and_stops_syncing() {
    let p = setup();
    let base = p.dir.path().to_path_buf();
    let (a, b) = (reopen(&base, "devA", &["node", "rust"]), reopen(&base, "devB", &["node", "rust"]));
    drop(p.a);
    drop(p.b);
    a.scan_all(false).unwrap();
    b.scan_all(false).unwrap();
    // Forgotten, not deleted: no tombstones, files stay on both Macs.
    for e in [&a, &b] {
        assert!(e.store().entry("web", "node_modules/left-pad/index.js").unwrap().is_none());
        assert!(e.store().entry("rs", "target/debug/app").unwrap().is_none());
        assert!(e.store().entry("photos", "target/a.jpg").unwrap().is_some(), "not a Cargo target");
    }
    b.sync_from(&a, None, PullOptions::default()).unwrap();
    a.sync_from(&b, None, PullOptions::default()).unwrap();
    for r in [a.root(), b.root()] {
        assert!(r.join("web/node_modules/left-pad/index.js").exists());
        assert!(r.join("rs/target/debug/app").exists());
    }
    // Changes inside are not transferred any more, the rest is.
    write(a.root(), "rs/target/debug/app", "rebuilt on A");
    write(a.root(), "rs/src/main.rs", "fn main() { println!(); }");
    a.scan_all(false).unwrap();
    b.sync_from(&a, None, PullOptions::default()).unwrap();
    assert_eq!(read(b.root(), "rs/target/debug/app"), "binary");
    assert_eq!(read(b.root(), "rs/src/main.rs"), "fn main() { println!(); }");
    assert_eq!(a.local_roots().unwrap(), b.local_roots().unwrap(), "in sync without the build folders");
}

/// The other Mac still synchronises its build folders: this Mac does not
/// fetch them (not even for a new project whose `Cargo.toml` arrives in the
/// same pull) and does not list them.
#[test]
fn not_fetched_from_a_mac_that_still_syncs_them() {
    let p = Pair::new();
    write(&p.ra(), "rs/Cargo.toml", "[package]");
    write(&p.ra(), "rs/target/debug/app", "binary");
    write(&p.ra(), "web/node_modules/x/index.js", "x");
    write(&p.ra(), "web/index.js", "app");
    p.a.scan_all(false).unwrap();
    let base = p.dir.path().to_path_buf();
    drop(p.b);
    let b = reopen(&base, "devB", &["node", "rust"]);
    b.scan_all(false).unwrap();
    b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    assert_eq!(read(b.root(), "web/index.js"), "app");
    assert_eq!(read(b.root(), "rs/Cargo.toml"), "[package]");
    assert!(!b.root().join("rs/target").exists());
    assert!(!b.root().join("web/node_modules").exists());
    let items = b.browse(Some(&p.a as &dyn PeerApi), Some("web"), "").unwrap();
    assert!(items.iter().all(|i| i.name != "node_modules"), "{items:#?}");
}

/// Git switched off: the working tree is synchronised, `.git` stays where it
/// is (nothing deleted, nothing transferred).
#[test]
fn git_switched_off_syncs_only_the_working_tree() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let open = |name: &str| {
        let root = dir.path().join(format!("{name}-root"));
        let mut cfg = EngineConfig::new(root.clone(), dir.path().join(format!("{name}-state")), name);
        cfg.excludes = macsync_fs::Excludes::new(&[], true).unwrap().with_ecosystems(&root, &[], false);
        Engine::open(cfg).unwrap()
    };
    let (a, b) = (open("devA"), open("devB"));
    init_repo(&a.root().join("app"));
    write(a.root(), "app/README.md", "hello");
    a.scan_all(false).unwrap();
    b.scan_all(false).unwrap();
    b.sync_from(&a, None, PullOptions::default()).unwrap();
    assert_eq!(read(b.root(), "app/README.md"), "hello");
    assert!(!b.root().join("app/.git").exists(), "Git's folder not transferred");
    assert!(a.root().join("app/.git/HEAD").exists());
    write(a.root(), "app/README.md", "hello again");
    a.scan_all(false).unwrap();
    b.sync_from(&a, None, PullOptions::default()).unwrap();
    assert_eq!(read(b.root(), "app/README.md"), "hello again");
    assert!(a.root().join("app/.git/HEAD").exists(), "never deleted");
}

/// Both Macs skip build folders and pull in both directions until quiet.
fn sync_skipping(a: &Engine, b: &Engine) {
    for _ in 0..4 {
        a.scan_all(false).unwrap();
        b.scan_all(false).unwrap();
        b.sync_from(a, None, PullOptions::default()).unwrap();
        a.sync_from(b, None, PullOptions::default()).unwrap();
    }
}

fn pair_skipping() -> (tempfile::TempDir, Engine, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let a = reopen(dir.path(), "devA", &["node", "rust"]);
    let b = reopen(dir.path(), "devB", &["node", "rust"]);
    (dir, a, b)
}

/// A folder deleted on one Mac is deleted on the other as a whole, with the
/// build folders, `.DS_Store` and everything else that is never synchronised;
/// none of it comes back.
#[test]
fn deleted_folder_goes_with_its_build_folders() {
    let (_dir, a, b) = pair_skipping();
    write(a.root(), "rs/Cargo.toml", "[package]");
    write(a.root(), "rs/src/main.rs", "fn main() {}");
    write(a.root(), "web/package.json", "{}");
    write(a.root(), "web/src/index.js", "app");
    sync_skipping(&a, &b);
    // Local builds on B only, as Cargo and npm leave them.
    write(b.root(), "rs/target/CACHEDIR.TAG", "Signature: 8a477f597d28d172789f06886806bc55");
    write(b.root(), "rs/target/release/app", "binary B");
    write(b.root(), "web/node_modules/x/index.js", "x");
    write(b.root(), "web/src/.DS_Store", "finder");
    sync_skipping(&a, &b);

    std::fs::remove_dir_all(a.root().join("rs")).unwrap();
    std::fs::remove_dir_all(a.root().join("web")).unwrap();
    sync_skipping(&a, &b);
    for r in [a.root(), b.root()] {
        assert!(!r.join("rs").exists(), "rs left in {r:?}");
        assert!(!r.join("web").exists(), "web left in {r:?}");
    }
    // In MacSync's trash on B, not gone.
    let in_trash =
        std::fs::read_dir(b.trash_root()).unwrap().flatten().any(|tx| tx.path().join("rs/target/release/app").exists());
    assert!(in_trash, "build folder not in the trash");
}

/// The same for a repository (a unit of its own, like `chrisurf/RepoSync`):
/// its `.git` objects are deleted after the folders.
#[test]
fn deleted_repository_goes_with_its_build_folder() {
    if !git_available() {
        return;
    }
    let (_dir, a, b) = pair_skipping();
    init_repo(&a.root().join("app"));
    write(a.root(), "app/Cargo.toml", "[package]");
    sync_skipping(&a, &b);
    write(b.root(), "app/target/CACHEDIR.TAG", "Signature: 8a477f597d28d172789f06886806bc55");
    write(b.root(), "app/target/debug/app", "binary B");
    sync_skipping(&a, &b);

    std::fs::remove_dir_all(a.root().join("app")).unwrap();
    sync_skipping(&a, &b);
    assert!(!b.root().join("app").exists(), "left on B");
    assert!(!a.root().join("app").exists(), "came back on A");
}

/// Without `Cargo.toml` the folder `target` stays a build folder (Cargo's
/// `CACHEDIR.TAG` inside): it is not transferred as new files.
#[test]
fn build_folder_stays_excluded_without_its_project_file() {
    let (_dir, a, b) = pair_skipping();
    write(a.root(), "rs/Cargo.toml", "[package]");
    write(a.root(), "rs/notes.md", "keep");
    sync_skipping(&a, &b);
    write(b.root(), "rs/target/CACHEDIR.TAG", "Signature: 8a477f597d28d172789f06886806bc55");
    write(b.root(), "rs/target/release/app", "binary B");
    sync_skipping(&a, &b);

    std::fs::remove_file(a.root().join("rs/Cargo.toml")).unwrap();
    sync_skipping(&a, &b);
    assert!(!b.root().join("rs/Cargo.toml").exists());
    assert!(!a.root().join("rs/target").exists(), "B's build folder transferred");
    assert!(b.root().join("rs/target/release/app").exists(), "never deleted");
}

/// New work on B inside a folder deleted on A is not thrown away.
#[test]
fn deleted_folder_keeps_new_work_of_the_other_mac() {
    let (_dir, a, b) = pair_skipping();
    write(a.root(), "docs/a.md", "a");
    sync_skipping(&a, &b);
    std::fs::remove_dir_all(a.root().join("docs")).unwrap();
    write(b.root(), "docs/node_modules/x/index.js", "x");
    write(b.root(), "docs/new.md", "written on B, not yet synchronised");
    sync_skipping(&a, &b);
    let kept = [a.root(), b.root()].iter().any(|r| r.join("docs/new.md").exists());
    assert!(kept, "new work lost");
}

/// A repository inside a build folder (`.build/checkouts/…`, a crate in
/// `target/`) that was a unit before the folder was switched off: forgotten
/// on both Macs, files kept, never listed as "not in sync" again.
#[test]
fn repository_inside_a_switched_off_build_folder_is_forgotten() {
    if !git_available() {
        return;
    }
    let p = Pair::new();
    write(&p.ra(), "rs/Cargo.toml", "[package]");
    init_repo(&p.ra().join("rs/target/checkouts/dep"));
    p.sync();
    assert!(p.b.store().unit("rs/target/checkouts/dep").unwrap().is_some());
    let base = p.dir.path().to_path_buf();
    drop(p.a);
    drop(p.b);
    let (a, b) = (reopen(&base, "devA", &["rust"]), reopen(&base, "devB", &["rust"]));
    a.scan_all(false).unwrap();
    b.scan_all(false).unwrap();
    for e in [&a, &b] {
        assert!(e.store().unit("rs/target/checkouts/dep").unwrap().is_none());
        assert!(e.root().join("rs/target/checkouts/dep/.git").exists(), "files stay");
        assert!(e.local_roots().unwrap().iter().all(|r| !r.name.contains("target")));
    }
    let r = b.sync_from(&a, None, PullOptions::default()).unwrap();
    assert!(r.iter().all(|r| !r.unit.contains("target")), "{r:?}");
    assert_eq!(a.local_roots().unwrap(), b.local_roots().unwrap());
}
