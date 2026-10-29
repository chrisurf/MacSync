//! "This Mac's state wins" for a folder or a whole unit.

mod common;

use common::*;
use macsync_engine::PullOptions;

fn diverged() -> Pair {
    let p = Pair::new();
    for f in ["a.txt", "b.txt", "c.txt", "sub/d.txt", "other/e.txt"] {
        write(&p.ra(), &format!("w/{f}"), "base");
    }
    p.sync();
    write(&p.ra(), "w/c.txt", "A version");
    write(&p.rb(), "w/c.txt", "B version!");
    p.sync();
    assert_eq!(p.a.store().conflicts(false).unwrap().len(), 1, "c.txt in conflict");
    // Not synchronised yet: changes on both sides.
    write(&p.ra(), "w/a.txt", "A edited");
    write(&p.rb(), "w/b.txt", "B edited");
    write(&p.rb(), "w/only-b.txt", "new on B");
    write(&p.rb(), "w/sub/d.txt", "B in sub");
    write(&p.rb(), "w/other/e.txt", "B in other");
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    p
}

#[test]
fn the_whole_unit_of_this_mac_wins() {
    let p = diverged();
    let r = p.a.win(&p.b, "w", "").unwrap();
    assert_eq!(r.conflicts_closed, 1);
    assert!(p.a.store().conflicts(false).unwrap().is_empty());
    p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    p.sync();
    for (f, want) in [("a.txt", "A edited"), ("b.txt", "base"), ("c.txt", "A version"), ("sub/d.txt", "base")] {
        assert_eq!(read(&p.rb(), &format!("w/{f}")), want, "{f}");
    }
    assert!(!p.rb().join("w/only-b.txt").exists(), "only on B: removed");
    assert!(p.b.store().conflicts(false).unwrap().is_empty());
    p.assert_identical();
    // Kept in B's MacSync trash.
    let trash = p.b.trash_root();
    let kept: Vec<String> =
        walk(&trash).into_iter().filter(|f| f.ends_with("only-b.txt") || f.ends_with("b.txt")).collect();
    assert!(!kept.is_empty(), "previous versions in the trash");
}

#[test]
fn only_the_chosen_folder_is_overwritten() {
    let p = diverged();
    // B's state of `sub` wins; everything else stays as synchronised.
    p.b.win(&p.a, "w", "sub").unwrap();
    p.a.sync_from(&p.b, None, PullOptions::default()).unwrap();
    assert_eq!(read(&p.ra(), "w/sub/d.txt"), "B in sub");
    assert_eq!(read(&p.ra(), "w/other/e.txt"), "B in other", "normal sync");
    assert_eq!(read(&p.ra(), "w/c.txt"), "A version", "conflict outside sub untouched");
    assert_eq!(p.a.store().conflicts(false).unwrap().len(), 1);
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out
}
