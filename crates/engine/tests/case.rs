//! Renames that only change case (or Unicode normalisation) on
//! case-insensitive volumes: `agent` → `Agent` is the same name to APFS, so
//! it must arrive on the peer as a rename, never as a case collision, a
//! second unit or a deletion of the renamed files.

mod common;

use std::fs;

use common::*;
use macsync_engine::{ActionKind, Outcome, PullReport};

fn case_insensitive(p: &Pair) -> bool {
    p.a.is_case_insensitive() && p.b.is_case_insensitive()
}

fn open_conflicts(e: &macsync_engine::Engine) -> usize {
    e.store().conflicts(false).unwrap().len()
}

fn unit_names(e: &macsync_engine::Engine) -> Vec<String> {
    e.store().units().unwrap().into_iter().map(|u| u.name).collect()
}

/// A spelling change must arrive as a rename: no conflict, no deletion, no
/// re-transfer and nothing in the trash.
fn assert_clean_rename(reports: &[PullReport], p: &Pair) {
    for r in reports {
        for a in &r.actions {
            assert!(
                !matches!(a.kind, ActionKind::Conflict | ActionKind::Delete | ActionKind::Write),
                "{} {:?} {} ({})",
                r.unit,
                a.kind,
                a.path,
                a.reason
            );
        }
        assert!(!matches!(r.outcome, Outcome::Skipped(_) | Outcome::Deferred(_)), "{}: {:?}", r.unit, r.outcome);
    }
    for root in [p.ra(), p.rb()] {
        let trash = root.join(".macsync/trash");
        let n = fs::read_dir(&trash).map(|d| d.count()).unwrap_or(0);
        assert_eq!(n, 0, "trash not empty in {}", trash.display());
    }
}

#[test]
fn directory_case_rename_inside_a_unit() {
    let p = Pair::new();
    if !case_insensitive(&p) {
        return;
    }
    write(&p.ra(), "vault/agent/jobs/a.md", "a");
    write(&p.ra(), "vault/agent/b.md", "b");
    write(&p.ra(), "vault/learning/c.md", "c");
    p.sync();
    p.assert_identical();

    fs::rename(p.ra().join("vault/agent"), p.ra().join("vault/Agent")).unwrap();
    fs::rename(p.ra().join("vault/learning/c.md"), p.ra().join("vault/learning/C.md")).unwrap();
    assert_clean_rename(&p.sync(), &p);

    assert_eq!(open_conflicts(&p.a), 0);
    assert_eq!(open_conflicts(&p.b), 0);
    let names: Vec<_> = fs::read_dir(p.rb().join("vault")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(names.iter().any(|n| n == "Agent"), "{names:?}");
    assert_eq!(read(&p.rb(), "vault/Agent/jobs/a.md"), "a");
    assert_eq!(read(&p.rb(), "vault/Agent/b.md"), "b");
    assert_eq!(read(&p.rb(), "vault/learning/C.md"), "c");
    p.assert_identical();
}

#[test]
fn case_rename_and_back_again() {
    let p = Pair::new();
    if !case_insensitive(&p) {
        return;
    }
    write(&p.ra(), "docs/readme.md", "r");
    p.sync();
    fs::rename(p.ra().join("docs/readme.md"), p.ra().join("docs/README.md")).unwrap();
    assert_clean_rename(&p.sync(), &p);
    fs::rename(p.rb().join("docs/README.md"), p.rb().join("docs/Readme.md")).unwrap();
    assert_clean_rename(&p.sync(), &p);
    assert_eq!(open_conflicts(&p.a) + open_conflicts(&p.b), 0);
    assert_eq!(read(&p.ra(), "docs/Readme.md"), "r");
    p.assert_identical();
}

#[test]
fn case_rename_with_content_change() {
    let p = Pair::new();
    if !case_insensitive(&p) {
        return;
    }
    write(&p.ra(), "docs/notes/x.md", "old");
    p.sync();
    fs::rename(p.ra().join("docs/notes"), p.ra().join("docs/Notes")).unwrap();
    write(&p.ra(), "docs/Notes/x.md", "new");
    p.sync();
    assert_eq!(open_conflicts(&p.a) + open_conflicts(&p.b), 0);
    assert_eq!(read(&p.rb(), "docs/Notes/x.md"), "new");
    p.assert_identical();
}

#[test]
fn top_level_unit_case_rename_with_nested_repo() {
    let p = Pair::new();
    if !case_insensitive(&p) || !git_available() {
        return;
    }
    init_repo(&p.ra().join("obsidian/verification"));
    write(&p.ra(), "obsidian/verification/agent/a.md", "a");
    write(&p.ra(), "obsidian/loose.md", "l");
    p.sync();
    p.assert_identical();

    fs::rename(p.ra().join("obsidian"), p.ra().join("Obsidian")).unwrap();
    assert_clean_rename(&p.sync(), &p);

    for e in [&p.a, &p.b] {
        let names = unit_names(e);
        assert!(!names.iter().any(|n| n.starts_with("obsidian")), "stale case variant: {names:?}");
        assert!(names.contains(&"Obsidian/verification".to_string()), "{names:?}");
        assert_eq!(open_conflicts(e), 0);
    }
    let top: Vec<_> = fs::read_dir(p.rb()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(top.iter().any(|n| n == "Obsidian"), "{top:?}");
    assert_eq!(read(&p.rb(), "Obsidian/verification/agent/a.md"), "a");
    assert_eq!(read(&p.rb(), "Obsidian/loose.md"), "l");
    assert_fsck(&p.rb().join("Obsidian/verification"));
    p.assert_identical();

    // Afterwards edits flow normally through the renamed unit.
    write(&p.rb(), "Obsidian/verification/agent/a.md", "a2");
    p.sync();
    assert_eq!(read(&p.ra(), "Obsidian/verification/agent/a.md"), "a2");
    assert_eq!(open_conflicts(&p.a) + open_conflicts(&p.b), 0);
}

#[test]
fn stale_case_variant_unit_is_retired_without_touching_files() {
    let p = Pair::new();
    if !case_insensitive(&p) {
        return;
    }
    write(&p.ra(), "Notes/a.md", "a");
    p.sync();
    // Simulate the state left behind by earlier versions: a second unit row
    // whose name differs only in case from the real directory.
    for e in [&p.a, &p.b] {
        e.store().upsert_unit("notes", macsync_core::UnitKind::Folder).unwrap();
    }
    p.sync();
    for e in [&p.a, &p.b] {
        assert_eq!(unit_names(e).iter().filter(|n| n.to_lowercase() == "notes").count(), 1, "{:?}", unit_names(e));
    }
    assert_eq!(read(&p.ra(), "Notes/a.md"), "a");
    assert_eq!(read(&p.rb(), "Notes/a.md"), "a");
    p.assert_identical();
}

#[test]
fn staged_git_case_renames_and_top_level_rename_together() {
    let p = Pair::new();
    if !case_insensitive(&p) || !git_available() {
        return;
    }
    let repo = p.ra().join("obsidian/verification");
    init_repo(&repo);
    write(&repo, "agent/jobs/a.md", "a");
    write(&repo, "learning/l.md", "l");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "notes"]);
    p.sync();
    p.assert_identical();

    // What we did by hand: `git mv` of folders (two steps on APFS), one edit,
    // then the vault folder itself got a capital letter.
    git(&repo, &["mv", "agent", "agent.tmp"]);
    git(&repo, &["mv", "agent.tmp", "Agent"]);
    git(&repo, &["mv", "learning", "learning.tmp"]);
    git(&repo, &["mv", "learning.tmp", "Learning"]);
    write(&repo, "CLAUDE.md", "edited\n");
    fs::rename(p.ra().join("obsidian"), p.ra().join("Obsidian")).unwrap();
    let reports = p.sync();

    for r in &reports {
        for a in &r.actions {
            assert!(!matches!(a.kind, ActionKind::Conflict | ActionKind::Delete), "{} {:?} {}", r.unit, a.kind, a.path);
        }
    }
    for e in [&p.a, &p.b] {
        assert_eq!(open_conflicts(e), 0);
        assert!(!unit_names(e).iter().any(|n| n.starts_with("obsidian")), "{:?}", unit_names(e));
    }
    let rb = p.rb().join("Obsidian/verification");
    assert_eq!(read(&rb, "Agent/jobs/a.md"), "a");
    assert_eq!(read(&rb, "CLAUDE.md"), "edited\n");
    assert_eq!(
        git(&p.ra().join("Obsidian/verification"), &["status", "--porcelain"]),
        git(&rb, &["status", "--porcelain"])
    );
    assert_fsck(&rb);
    p.assert_identical();
}

#[test]
fn concurrent_edit_in_renamed_folder_is_never_lost() {
    let p = Pair::new();
    if !case_insensitive(&p) {
        return;
    }
    write(&p.ra(), "vault/agent/x.md", "base");
    p.sync();
    fs::rename(p.ra().join("vault/agent"), p.ra().join("vault/Agent")).unwrap();
    write(&p.rb(), "vault/agent/x.md", "edited on B");
    p.sync();
    // Both versions must still exist somewhere: on disk or in the conflict store.
    let mut seen = Vec::new();
    for (root, e) in [(p.ra(), &p.a), (p.rb(), &p.b)] {
        seen.push(read(&root, "vault/agent/x.md"));
        for c in e.store().conflicts(false).unwrap() {
            if let Ok(s) = fs::read_to_string(std::path::Path::new(&c.stored).join(&c.path)) {
                seen.push(s);
            }
        }
    }
    assert!(seen.iter().any(|s| s == "edited on B"), "{seen:?}");
}

#[test]
fn diverging_spellings_never_rename_back_and_forth() {
    let p = Pair::new();
    if !case_insensitive(&p) {
        return;
    }
    write(&p.ra(), "notes/a.md", "a");
    p.sync();
    // Left behind by older versions: both devices know both spellings, but
    // the directory is spelled differently on each disk.
    fs::rename(p.ra().join("notes"), p.ra().join("Notes")).unwrap();
    for e in [&p.a, &p.b] {
        e.store().upsert_unit("Notes", macsync_core::UnitKind::Folder).unwrap();
    }
    for _ in 0..3 {
        p.sync();
        let top_a: Vec<_> = fs::read_dir(p.ra()).unwrap().map(|e| e.unwrap().file_name()).collect();
        let top_b: Vec<_> = fs::read_dir(p.rb()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(top_a.iter().any(|n| n == "Notes"), "{top_a:?}");
        assert!(top_b.iter().any(|n| n == "notes"), "{top_b:?}");
    }
    assert_eq!(unit_names(&p.a), vec![".", "Notes"]);
    assert_eq!(unit_names(&p.b), vec![".", "notes"]);
    assert_eq!(read(&p.rb(), "notes/a.md"), "a");

    // Aligning the spelling by hand on one Mac resolves it.
    fs::rename(p.rb().join("notes"), p.rb().join("Notes")).unwrap();
    p.sync();
    assert_eq!(unit_names(&p.b), vec![".", "Notes"]);
    write(&p.rb(), "Notes/a.md", "a2");
    p.sync();
    assert_eq!(read(&p.ra(), "Notes/a.md"), "a2");
    p.assert_identical();
}

/// Independently created `todo.md` / `TODO.md` is a real case collision.
/// Taking the other Mac's version takes its spelling as well, and both Macs
/// end up with one file.
#[test]
fn resolving_a_case_collision_takes_the_remote_spelling() {
    let p = Pair::new();
    if !case_insensitive(&p) {
        return;
    }
    write(&p.ra(), "w/base.md", "x");
    p.sync();
    write(&p.ra(), "w/todo.md", "from A");
    write(&p.rb(), "w/TODO.md", "from B");
    p.sync();
    let c = p.b.store().conflicts(false).unwrap();
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0].class, "case-collision");

    p.b.resolve(c[0].id, macsync_engine::Choice::Remote).unwrap();
    p.sync();
    p.sync();
    for root in [p.ra(), p.rb()] {
        let names: Vec<String> = fs::read_dir(root.join("w"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_lowercase() == "todo.md")
            .collect();
        assert_eq!(names, vec!["todo.md".to_string()], "in {}", root.display());
        assert_eq!(read(&root, "w/todo.md"), "from A");
    }
    assert_eq!(open_conflicts(&p.a), 0);
    assert_eq!(open_conflicts(&p.b), 0);
}
