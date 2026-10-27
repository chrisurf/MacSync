use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use filetime::{set_file_mtime, FileTime};
use macsync_dupes::{apply, compare, Done, Options, Outcome, Plan, State, Summary, Trash};

const DAY: i64 = 86_400;
const T0: i64 = 1_700_000_000;

fn put(root: &Path, rel: &str, body: &str, day: i64) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
    set_file_mtime(&p, FileTime::from_unix_time(T0 + day * DAY, 0)).unwrap();
}

fn mtime(p: &Path) -> i64 {
    fs::metadata(p).unwrap().mtime()
}

struct Dirs {
    old: tempfile::TempDir,
    new: tempfile::TempDir,
    trash: tempfile::TempDir,
}

fn dirs() -> Dirs {
    Dirs { old: tempfile::tempdir().unwrap(), new: tempfile::tempdir().unwrap(), trash: tempfile::tempdir().unwrap() }
}

fn run(d: &Dirs, plan: &str) -> (Vec<Outcome>, Summary) {
    let plan: Plan = serde_json::from_str(plan).unwrap();
    let mut out = Vec::new();
    let s =
        apply(d.old.path(), d.new.path(), &plan, &Trash::Dir(d.trash.path().to_path_buf()), |o| out.push(o.clone()))
            .unwrap();
    (out, s)
}

#[test]
fn copy_file_keeps_content_date_and_mode() {
    let d = dirs();
    put(d.old.path(), "a/b.txt", "hello world\n", 3);
    fs::set_permissions(d.old.path().join("a/b.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    let tag = vec![("com.apple.metadata:_kMDItemUserTags".to_string(), b"rot".to_vec())];
    macsync_fs::meta::write_xattrs(&d.old.path().join("a/b.txt"), &tag, false).unwrap();
    let (out, s) = run(&d, r#"{"actions":[{"op":"copy","kind":"file","path":"a/b.txt","to":"x/y/b.txt"}]}"#);
    assert_eq!(out[0].result, Done::Done, "{:?}", out[0].error);
    assert_eq!((s.done, s.failed, s.bytes), (1, 0, 12));
    let t = d.new.path().join("x/y/b.txt");
    assert_eq!(fs::read_to_string(&t).unwrap(), "hello world\n");
    assert_eq!(mtime(&t), T0 + 3 * DAY);
    assert_eq!(fs::metadata(&t).unwrap().permissions().mode() & 0o777, 0o640);
    assert_eq!(macsync_fs::meta::read_xattrs(&t), tag);
    assert!(d.old.path().join("a/b.txt").exists(), "copy leaves ALT alone");
    let left: Vec<_> = fs::read_dir(d.new.path().join("x/y")).unwrap().collect();
    assert_eq!(left.len(), 1, "no temporary file left behind");
}

#[test]
fn copy_dir_copies_the_whole_tree() {
    let d = dirs();
    put(d.old.path(), "exp/a.txt", "a", 1);
    put(d.old.path(), "exp/sub/b.txt", "bb", 2);
    set_file_mtime(d.old.path().join("exp"), FileTime::from_unix_time(T0, 0)).unwrap();
    let (out, s) = run(&d, r#"{"actions":[{"op":"copy","kind":"dir","path":"exp"}]}"#);
    assert_eq!(out[0].result, Done::Done, "{:?}", out[0].error);
    assert_eq!(s.bytes, 3);
    assert_eq!(fs::read_to_string(d.new.path().join("exp/sub/b.txt")).unwrap(), "bb");
    assert_eq!(mtime(&d.new.path().join("exp/sub/b.txt")), T0 + 2 * DAY);
    assert_eq!(mtime(&d.new.path().join("exp")), T0);
}

#[test]
fn move_puts_the_source_into_the_trash() {
    let d = dirs();
    put(d.old.path(), "a.txt", "a", 1);
    put(d.old.path(), "exp/b.txt", "b", 1);
    let (out, _) = run(
        &d,
        r#"{"actions":[{"op":"move","kind":"file","path":"a.txt","to":"neu/a.txt"},
                       {"op":"move","kind":"dir","path":"exp"}]}"#,
    );
    for o in &out {
        assert_eq!(o.result, Done::Done, "{:?}", o.error);
        let t = Path::new(o.trashed.as_deref().unwrap());
        assert!(t.starts_with(d.trash.path()) && t.exists(), "{t:?}");
    }
    assert!(!d.old.path().join("a.txt").exists());
    assert!(!d.old.path().join("exp").exists());
    assert!(d.new.path().join("neu/a.txt").exists());
    assert!(d.new.path().join("exp/b.txt").exists());
    assert!(d.trash.path().join("exp/b.txt").exists());
}

#[test]
fn replace_trashes_the_neu_file() {
    let d = dirs();
    put(d.old.path(), "x.py", "neuer\n", 5);
    put(d.new.path(), "x.py", "alt\n", 1);
    let plan = format!(
        r#"{{"actions":[{{"op":"replace","path":"x.py",
            "expect_old":{{"size":6,"mtime_ms":{}}},"expect_new":{{"size":4,"mtime_ms":{}}}}}]}}"#,
        (T0 + 5 * DAY) * 1000,
        (T0 + DAY) * 1000
    );
    let (out, _) = run(&d, &plan);
    assert_eq!(out[0].result, Done::Done, "{:?}", out[0].error);
    assert_eq!(fs::read_to_string(d.new.path().join("x.py")).unwrap(), "neuer\n");
    assert_eq!(mtime(&d.new.path().join("x.py")), T0 + 5 * DAY);
    let t = out[0].trashed.as_deref().unwrap();
    assert_eq!(fs::read_to_string(t).unwrap(), "alt\n");
}

#[test]
fn keep_both_names_the_copy_after_the_alt_date() {
    let d = dirs();
    put(d.old.path(), "doc/x.py", "alt-version\n", 0);
    put(d.new.path(), "doc/x.py", "neu\n", 2);
    let plan = r#"{"actions":[{"op":"keep_both","path":"doc/x.py"},{"op":"keep_both","path":"doc/x.py"}]}"#;
    let (out, _) = run(&d, plan);
    let date = &macsync_fs::time::local(T0 * 1000)[..10];
    assert_eq!(out[0].to, format!("doc/x (ALT {date}).py"));
    assert_eq!(out[1].to, format!("doc/x (ALT {date} 2).py"));
    assert_eq!(fs::read_to_string(d.new.path().join(&out[0].to)).unwrap(), "alt-version\n");
    assert_eq!(fs::read_to_string(d.new.path().join("doc/x.py")).unwrap(), "neu\n");
    assert!(out.iter().all(|o| o.result == Done::Done && o.trashed.is_none()));
}

#[test]
fn changed_files_are_skipped() {
    let d = dirs();
    put(d.old.path(), "a.txt", "a", 1);
    let plan = r#"{"actions":[{"op":"copy","path":"a.txt","expect_old":{"size":1,"mtime_ms":1}},
                             {"op":"copy","path":"weg.txt"}]}"#;
    let (out, s) = run(&d, plan);
    assert_eq!(out[0].result, Done::Skipped);
    assert_eq!(out[0].error.as_deref(), Some("changed since the comparison"));
    assert_eq!(out[1].result, Done::Skipped);
    assert_eq!((s.done, s.skipped, s.failed), (0, 2, 0));
    assert!(!d.new.path().join("a.txt").exists());
}

#[test]
fn existing_targets_are_never_overwritten() {
    let d = dirs();
    put(d.old.path(), "a.txt", "aus alt", 1);
    put(d.new.path(), "a.txt", "aus neu", 1);
    put(d.old.path(), "exp/b.txt", "b", 1);
    fs::create_dir(d.new.path().join("exp")).unwrap();
    let (out, _) = run(&d, r#"{"actions":[{"op":"move","path":"a.txt"},{"op":"copy","kind":"dir","path":"exp"}]}"#);
    assert!(out.iter().all(|o| o.result == Done::Skipped), "{out:?}");
    assert_eq!(fs::read_to_string(d.new.path().join("a.txt")).unwrap(), "aus neu");
    assert!(d.old.path().join("a.txt").exists());
    assert!(!d.new.path().join("exp/b.txt").exists());
    assert_eq!(fs::read_dir(d.trash.path()).unwrap().count(), 0);
}

#[test]
fn paths_outside_the_folders_are_rejected() {
    let d = dirs();
    put(d.old.path(), "a.txt", "a", 1);
    fs::create_dir(d.new.path().join("sub")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), d.new.path().join("link")).unwrap();
    let plan = r#"{"actions":[{"op":"copy","path":"a.txt","to":"../a.txt"},
                             {"op":"copy","path":"../a.txt","to":"a.txt"},
                             {"op":"copy","path":"a.txt","to":"/tmp/a.txt"},
                             {"op":"copy","path":"a.txt","to":"sub/../../a.txt"},
                             {"op":"copy","path":"a.txt","to":"link/a.txt"},
                             {"op":"replace","kind":"dir","path":"a.txt"}]}"#;
    let (out, s) = run(&d, plan);
    assert!(out.iter().all(|o| o.result == Done::Failed), "{out:?}");
    assert_eq!(s.failed, 6);
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn files_option_lists_identical_files_too() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    put(old.path(), "a/same.txt", "same", 1);
    put(new.path(), "a/same.txt", "same", 1);
    put(old.path(), "a/Gross.txt", "x", 1);
    put(new.path(), "a/gross.txt", "x", 1);
    put(old.path(), "a/alt.txt", "nur alt", 1);
    let plain = compare(old.path(), new.path(), &Options::default()).unwrap();
    assert_eq!(plain.entries.len(), 1);
    let c = compare(old.path(), new.path(), &Options { files: true, ..Default::default() }).unwrap();
    assert_eq!(c.entries.len(), 3);
    let same: Vec<_> = c.entries.iter().filter(|e| e.state == State::Same).collect();
    assert_eq!(same.len(), 2);
    let g = c.entries.iter().find(|e| e.path == "a/Gross.txt").unwrap();
    assert_eq!(g.path_new.as_deref(), Some("a/gross.txt"));
    assert_eq!(g.size_new, Some(1));
    assert_eq!(c.entries.iter().find(|e| e.path == "a/alt.txt").unwrap().size_new, None);
    assert!(c.entries.iter().find(|e| e.path == "a/same.txt").unwrap().path_new.is_none());
    // Every entry is listed by exactly one folder.
    let mut listed: Vec<usize> = c.folders.iter().flat_map(|f| f.entries.iter().copied()).collect();
    listed.sort_unstable();
    assert_eq!(listed, (0..c.entries.len()).collect::<Vec<_>>());
    let a = c.folders.iter().find(|f| f.path == "a").unwrap();
    assert!(a.entries.iter().all(|&i| c.entries[i].path.starts_with("a/")));
}

#[test]
fn only_old_files_follow_their_moved_folder() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for f in ["a", "b", "c"] {
        put(old.path(), &format!("projekt/src/{f}.rs"), &format!("fn {f}() {{}}\n"), 1);
        put(new.path(), &format!("archiv/Projekt/src/{f}.rs"), &format!("fn {f}() {{}}\n"), 1);
    }
    put(old.path(), "projekt/src/neu.rs", "fn nur_alt() {}\n", 2);
    put(old.path(), "projekt/docs/notiz.md", "notiz\n", 2);
    put(old.path(), "lose.txt", "lose\n", 2);
    let c = compare(old.path(), new.path(), &Options::default()).unwrap();
    let e = |p: &str| c.entries.iter().find(|e| e.path == p).unwrap();
    assert_eq!(e("projekt/src/a.rs").state, State::Moved);
    assert_eq!(e("projekt/src/neu.rs").state, State::OnlyOld);
    assert_eq!(e("projekt/src/neu.rs").suggested.as_deref(), Some("archiv/Projekt/src/neu.rs"));
    assert_eq!(e("projekt/docs/notiz.md").suggested.as_deref(), Some("archiv/Projekt/docs/notiz.md"));
    assert_eq!(e("lose.txt").suggested, None);
}

#[test]
fn long_names_fit_the_temporary_file() {
    let d = dirs();
    let name = format!("{}.txt", "ä".repeat(124));
    put(d.old.path(), &name, "alt", 5);
    put(d.new.path(), &name, "neu", 1);
    put(d.old.path(), "x.txt", "x", 1);
    let to = format!("sub/{name}");
    let plan = serde_json::json!({"actions": [
        {"op": "replace", "path": name},
        {"op": "copy", "path": "x.txt", "to": to},
    ]});
    let (out, _) = run(&d, &plan.to_string());
    assert!(out.iter().all(|o| o.result == Done::Done), "{out:?}");
    assert_eq!(fs::read_to_string(d.new.path().join(&name)).unwrap(), "alt");
    assert_eq!(fs::read_to_string(d.new.path().join(&to)).unwrap(), "x");
}
