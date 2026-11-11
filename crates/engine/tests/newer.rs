//! Files every Mac rewrites on its own (Obsidian's window layout, plugin
//! settings): changed on both, the newer version wins on both Macs instead
//! of a conflict, and the other one is kept in the trash. Other files still
//! become conflicts.

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::*;

fn set_mtime(path: &Path, t: SystemTime) {
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

/// Contents of all files below `dir`.
fn contents(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(contents(&p));
        } else if let Ok(s) = std::fs::read_to_string(&p) {
            out.push(s);
        }
    }
    out
}

const LAYOUT: &str = "vault/.obsidian/workspace.json";

fn both_changed(a_is_newer: bool) {
    let p = Pair::new();
    write(&p.ra(), LAYOUT, "{\"start\":1}");
    p.sync();

    let now = SystemTime::now();
    let (ta, tb) = if a_is_newer { (now, now - Duration::from_secs(60)) } else { (now - Duration::from_secs(60), now) };
    write(&p.ra(), LAYOUT, "{\"layout\":\"A\"}");
    set_mtime(&p.ra().join(LAYOUT), ta);
    write(&p.rb(), LAYOUT, "{\"layout\":\"B\"}");
    set_mtime(&p.rb().join(LAYOUT), tb);

    p.sync();
    let (winner, loser, loser_engine) = if a_is_newer {
        ("{\"layout\":\"A\"}", "{\"layout\":\"B\"}", &p.b)
    } else {
        ("{\"layout\":\"B\"}", "{\"layout\":\"A\"}", &p.a)
    };
    for e in [&p.a, &p.b] {
        let open = e.store().conflicts(false).unwrap();
        assert!(open.is_empty(), "{open:?}");
    }
    assert_eq!(read(&p.ra(), LAYOUT), winner);
    assert_eq!(read(&p.rb(), LAYOUT), winner);
    assert!(contents(&loser_engine.trash_root()).iter().any(|c| c == loser), "the older version is in the trash");
    p.assert_identical();
}

#[test]
fn newer_layout_wins_on_both_macs() {
    both_changed(true);
    both_changed(false);
}

#[test]
fn plugin_settings_follow_the_newer_version_but_notes_still_conflict() {
    let p = Pair::new();
    let settings = "vault/.obsidian/plugins/couchdb-sync/data.json";
    write(&p.ra(), settings, "{}");
    write(&p.ra(), "vault/note.md", "start");
    p.sync();

    let now = SystemTime::now();
    write(&p.ra(), settings, "{\"a\":1}");
    set_mtime(&p.ra().join(settings), now - Duration::from_secs(60));
    write(&p.rb(), settings, "{\"b\":1}");
    set_mtime(&p.rb().join(settings), now);
    write(&p.ra(), "vault/note.md", "written on A");
    write(&p.rb(), "vault/note.md", "written on B");

    p.sync();
    assert_eq!(read(&p.ra(), settings), "{\"b\":1}");
    let open: Vec<_> = p.a.store().conflicts(false).unwrap().into_iter().map(|c| c.path).collect();
    assert_eq!(open, vec!["note.md".to_string()]);
}
