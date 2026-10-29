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
