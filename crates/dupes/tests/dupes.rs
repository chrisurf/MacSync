use std::fs;
use std::path::Path;

use filetime::{set_file_mtime, FileTime};
use macsync_dupes::{scan, GroupKind, Options, Report, Verdict};

const DAY: i64 = 86_400;
const T0: i64 = 1_700_000_000;

fn put(root: &Path, rel: &str, body: &str, day: i64) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
    set_file_mtime(&p, FileTime::from_unix_time(T0 + day * DAY, 0)).unwrap();
}

/// The same small project, as it was on day 0.
fn project(root: &Path, dir: &str) {
    put(root, &format!("{dir}/README.md"), "# Projekt\n", 0);
    put(root, &format!("{dir}/src/main.rs"), "fn main() {}\n", 0);
    put(root, &format!("{dir}/docs/plan.md"), "Plan v1\n", 0);
    put(root, &format!("{dir}/docs/logo.png"), "PNG…", 0);
}

fn run(roots: &[&Path]) -> Report {
    let roots: Vec<_> = roots.iter().map(|p| p.to_path_buf()).collect();
    scan(&roots, &Options::default()).unwrap()
}

#[test]
fn identical_copy_is_one_pair() {
    let d = tempfile::tempdir().unwrap();
    project(d.path(), "Arbeit/Projekt");
    project(d.path(), "Backup 2021/Projekt");
    let r = run(&[d.path()]);
    assert_eq!(r.pairs.len(), 1, "sub-folders are covered by the outer pair: {:#?}", r.pairs);
    let p = &r.pairs[0];
    assert_eq!(p.verdict, Verdict::Identical);
    assert_eq!(p.identical, 4);
    assert!(p.reclaimable_bytes > 0);
    assert!(r.groups.is_empty(), "files inside the pair are not repeated: {:#?}", r.groups);
}

#[test]
fn copy_worked_on_later_is_newer() {
    let d = tempfile::tempdir().unwrap();
    project(d.path(), "Backup 2021/Projekt");
    project(d.path(), "Arbeit/Projekt");
    put(d.path(), "Arbeit/Projekt/docs/plan.md", "Plan v2, viel länger\n", 30);
    put(d.path(), "Arbeit/Projekt/docs/angebot.pdf", "%PDF", 31);
    let r = run(&[d.path()]);
    assert_eq!(r.pairs.len(), 1);
    let p = &r.pairs[0];
    assert_eq!(p.verdict, Verdict::Newer);
    assert_eq!(p.a.display, "Arbeit/Projekt");
    assert_eq!(p.b.display, "Backup 2021/Projekt");
    assert_eq!(p.newer_a.len(), 1);
    assert_eq!(p.newer_a[0].path, "docs/plan.md");
    assert_eq!(p.only_a.len(), 1);
    assert!(p.only_b.is_empty() && p.newer_b.is_empty());
    assert_eq!(p.reclaimable_bytes, p.b.bytes);
}

#[test]
fn both_changed_is_diverged() {
    let d = tempfile::tempdir().unwrap();
    project(d.path(), "Alt/Projekt");
    project(d.path(), "Neu/Projekt");
    put(d.path(), "Neu/Projekt/docs/plan.md", "Plan v2\n", 30);
    put(d.path(), "Alt/Projekt/notizen.md", "nur hier\n", 10);
    let r = run(&[d.path()]);
    let p = &r.pairs[0];
    assert_eq!(p.verdict, Verdict::Diverged);
    // The more recently changed copy comes first.
    assert_eq!(p.a.display, "Neu/Projekt");
    assert_eq!(p.only_b[0].path, "notizen.md");
    assert_eq!(p.reclaimable_bytes, 0);
}

#[test]
fn same_date_different_content_is_unclear() {
    let d = tempfile::tempdir().unwrap();
    project(d.path(), "A/Projekt");
    project(d.path(), "B/Projekt");
    put(d.path(), "B/Projekt/docs/plan.md", "Plan v1!\n", 0);
    let p = &run(&[d.path()]).pairs[0];
    assert_eq!(p.verdict, Verdict::Diverged);
    assert_eq!(p.unclear.len(), 1);
}

#[test]
fn unrelated_same_named_folders_are_ignored() {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "eins/src/main.rs", "fn main() { eins() }\n", 0);
    put(d.path(), "zwei/src/main.rs", "fn main() { zwei() }\n", 5);
    let r = run(&[d.path()]);
    assert!(r.pairs.is_empty(), "{:#?}", r.pairs);
}

#[test]
fn loose_files_duplicates_and_versions() {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "Dokumente/Vertrag.pdf", "Vertrag", 1);
    put(d.path(), "Downloads/Vertrag.pdf", "Vertrag", 1);
    put(d.path(), "Kunde/Angebot.docx", "Angebot v1", 2);
    put(d.path(), "Mail/Angebot.docx", "Angebot v2 final", 9);
    put(d.path(), "a/leer.txt", "", 0);
    put(d.path(), "b/leer.txt", "", 0);
    let r = run(&[d.path()]);
    assert_eq!(r.groups.len(), 2, "{:#?}", r.groups);
    let g = &r.groups[0];
    assert_eq!((g.name.as_str(), g.kind, g.reclaimable_bytes), ("Vertrag.pdf", GroupKind::Duplicates, 7));
    let v = &r.groups[1];
    assert_eq!(v.kind, GroupKind::Versions);
    assert!(v.members[0].newest);
    assert_eq!(v.members[0].display, "Mail/Angebot.docx");
}

#[test]
fn hard_links_free_nothing() {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "a/foto.jpg", "JPEG", 0);
    fs::create_dir(d.path().join("b")).unwrap();
    fs::hard_link(d.path().join("a/foto.jpg"), d.path().join("b/foto.jpg")).unwrap();
    let r = run(&[d.path()]);
    assert_eq!(r.groups[0].kind, GroupKind::Duplicates);
    assert_eq!(r.groups[0].reclaimable_bytes, 0);
}

#[test]
fn two_folders_are_compared_directly() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    project(a.path(), "Projekt");
    project(b.path(), "Projekt");
    put(b.path(), "Projekt/neu.txt", "neu", 3);
    put(a.path(), ".git/HEAD", "ref", 0);
    let r = run(&[a.path(), b.path()]);
    assert_eq!(r.pairs.len(), 1, "{:#?}", r.pairs);
    let p = &r.pairs[0];
    assert_eq!(p.verdict, Verdict::Newer);
    assert_eq!(p.a.path, fs::canonicalize(b.path()).unwrap());
    assert_eq!(p.only_a[0].path, "Projekt/neu.txt");
    assert!(r.files == 9, ".git is not scanned: {}", r.files);
}

#[test]
fn nested_roots_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("x")).unwrap();
    let roots = vec![d.path().to_path_buf(), d.path().join("x")];
    assert!(scan(&roots, &Options::default()).is_err());
}
