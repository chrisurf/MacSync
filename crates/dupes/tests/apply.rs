use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use filetime::{set_file_mtime, FileTime};
use std::sync::atomic::{AtomicBool, Ordering};

use macsync_dupes::{apply, apply_with, compare, Done, Event, Options, Outcome, Plan, State, Summary, Trash};

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

#[test]
fn sizes_come_first_and_large_files_report_progress() {
    let d = dirs();
    let big = "x".repeat(3 << 20);
    put(d.old.path(), "exp/big.bin", &big, 1);
    put(d.old.path(), "exp/sub/a.txt", "abc", 1);
    put(d.old.path(), "c.txt", "hello", 1);
    let plan: Plan = serde_json::from_str(
        r#"{"actions":[{"op":"copy","kind":"dir","path":"exp"},{"op":"copy","kind":"file","path":"c.txt"}]}"#,
    )
    .unwrap();
    let mut events = Vec::new();
    let s = apply_with(
        d.old.path(),
        d.new.path(),
        &plan,
        &Trash::Dir(d.trash.path().to_path_buf()),
        &AtomicBool::new(false),
        |e| {
            events.push(
                serde_json::to_value(
                    match e {
                        Event::Start(s) => serde_json::to_value(s),
                        Event::Progress(p) => serde_json::to_value(p),
                        Event::Outcome(o) => serde_json::to_value(o),
                    }
                    .unwrap(),
                )
                .unwrap(),
            )
        },
    )
    .unwrap();
    assert_eq!(s.done, 2);
    assert_eq!(events[0]["start"], true);
    assert_eq!(events[0]["bytes_total"], (3 << 20) + 3 + 5);
    assert_eq!(events[0]["actions"][0]["files"], 2);
    assert_eq!(events[0]["actions"][1]["bytes"], 5);
    let first = events.iter().find(|e| e["progress"] == true).expect("progress reported");
    assert_eq!(first["index"], 0);
    assert_eq!(first["bytes"], (3 << 20) + 3);
    assert!(first["file"].as_str().unwrap().starts_with("exp/"), "{first}");
    let last = events.iter().rposition(|e| e["progress"] == true).unwrap();
    assert!(last < events.len() - 1, "the outcome follows the progress");
}

#[test]
fn cancel_stops_cleanly_and_leaves_new_unchanged() {
    let d = dirs();
    put(d.old.path(), "done.txt", "first", 1);
    put(d.old.path(), "exp/big.bin", &"x".repeat(4 << 20), 1);
    put(d.old.path(), "later.txt", "never", 1);
    let plan: Plan = serde_json::from_str(
        r#"{"actions":[{"op":"copy","path":"done.txt"},{"op":"move","kind":"dir","path":"exp"},{"op":"copy","path":"later.txt"}]}"#,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let mut out = Vec::new();
    let s = apply_with(d.old.path(), d.new.path(), &plan, &Trash::Dir(d.trash.path().to_path_buf()), &cancel, |e| {
        match e {
            // Cancel as the large file starts (inside the folder copy).
            Event::Progress(p) if p.index == 1 => cancel.store(true, Ordering::Relaxed),
            Event::Outcome(o) => out.push(o.clone()),
            _ => {}
        }
    })
    .unwrap();
    let results: Vec<_> = out.iter().map(|o| o.result).collect();
    assert_eq!(results, [Done::Done, Done::Cancelled, Done::Cancelled]);
    assert!(s.cancelled);
    assert_eq!((s.done, s.failed), (1, 0));
    let mut left: Vec<_> =
        fs::read_dir(d.new.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    left.sort();
    assert_eq!(left, ["done.txt"], "no half copy and nothing after the cancel");
    assert!(d.old.path().join("exp/big.bin").exists(), "the cancelled move left ALT alone");
    assert_eq!(fs::read_dir(d.trash.path()).unwrap().count(), 0);
}

#[test]
fn trash_old_only_when_new_has_identical_copies() {
    let d = dirs();
    for root in [d.old.path(), d.new.path()] {
        put(root, "repo/src/main.rs", "fn main() {}", 1);
        put(root, "repo/README.md", "# repo", 1);
    }
    put(d.new.path(), "repo/extra.txt", "only in NEU", 2);
    put(d.old.path(), "other/a.txt", "ALT", 1);
    put(d.new.path(), "other/a.txt", "NEU", 1);
    put(d.old.path(), "gone/b.txt", "b", 1);
    put(d.old.path(), "f.txt", "same", 1);
    put(d.new.path(), "f.txt", "same", 5);
    let (out, s) = run(
        &d,
        r#"{"actions":[
            {"op":"trash_old","kind":"dir","path":"repo"},
            {"op":"trash_old","kind":"dir","path":"other"},
            {"op":"trash_old","kind":"dir","path":"gone"},
            {"op":"trash_old","kind":"file","path":"f.txt"}]}"#,
    );
    let results: Vec<_> = out.iter().map(|o| o.result).collect();
    assert_eq!(results, [Done::Done, Done::Skipped, Done::Skipped, Done::Done], "{out:?}");
    assert_eq!(out[1].error.as_deref(), Some("other/a.txt differs in NEU"));
    assert_eq!(out[2].error.as_deref(), Some("not in NEU"));
    assert_eq!((s.done, s.skipped, s.bytes), (2, 2, 0));
    assert!(!d.old.path().join("repo").exists() && !d.old.path().join("f.txt").exists());
    assert!(d.trash.path().join("repo/src/main.rs").exists(), "in the Trash, not deleted");
    assert!(d.old.path().join("other/a.txt").exists() && d.old.path().join("gone/b.txt").exists());
    assert!(d.new.path().join("repo/extra.txt").exists(), "NEU untouched");
}

#[test]
fn probe_reports_sizes_and_no_cloud_for_local_files() {
    let d = dirs();
    put(d.old.path(), "exp/a.txt", "abc", 1);
    put(d.old.path(), "b.txt", "hello", 1);
    let plan: Plan =
        serde_json::from_str(r#"{"actions":[{"op":"copy","kind":"dir","path":"exp"},{"op":"copy","path":"b.txt"}]}"#)
            .unwrap();
    let p = macsync_dupes::probe(d.old.path(), &plan).unwrap();
    assert_eq!((p.files, p.bytes_total, p.cloud_files), (2, 8, 0));
    assert_eq!(p.cloud_ok, None, "nothing to fetch from the cloud");
    assert_eq!(p.actions.iter().map(|a| a.files).collect::<Vec<_>>(), [1, 1]);
    assert_eq!(fs::read_dir(d.new.path()).unwrap().count(), 0, "a probe changes nothing");
}

#[test]
fn folder_copies_leave_out_rebuildable_folders_but_keep_git() {
    let d = dirs();
    put(d.old.path(), "repo/src/main.rs", "fn main() {}", 1);
    put(d.old.path(), "repo/.git/HEAD", "ref: refs/heads/main", 1);
    put(d.old.path(), "repo/node_modules/x/index.js", "module.exports = 1", 1);
    put(d.old.path(), "repo/.venv/lib/site.py", "x", 1);
    put(d.old.path(), "repo/target/CACHEDIR.TAG", "Signature: 8a477f597d28d172789f06886806bc55", 1);
    put(d.old.path(), "repo/target/debug/bin", "x", 1);
    let (out, s) = run(&d, r#"{"actions":[{"op":"copy","kind":"dir","path":"repo"}]}"#);
    assert_eq!(out[0].result, Done::Done, "{:?}", out[0].error);
    let n = d.new.path().join("repo");
    assert!(n.join("src/main.rs").exists() && n.join(".git/HEAD").exists(), ".git is history: copied");
    assert!(!n.join("node_modules").exists() && !n.join(".venv").exists() && !n.join("target").exists());
    assert_eq!(s.bytes, 12 + 20);
    // ALT can go although NEU lacks the rebuildable folders.
    let (out, _) = run(&d, r#"{"actions":[{"op":"trash_old","kind":"dir","path":"repo"}]}"#);
    assert_eq!(out[0].result, Done::Done, "{:?}", out[0].error);
}

#[test]
fn trash_old_names_a_differing_git_history() {
    let d = dirs();
    for (root, head) in [(d.old.path(), "ref: refs/heads/feature"), (d.new.path(), "ref: refs/heads/main")] {
        put(root, "repo/README.md", "# repo", 1);
        put(root, "repo/.git/HEAD", head, 1);
    }
    let (out, _) = run(&d, r#"{"actions":[{"op":"trash_old","kind":"dir","path":"repo"}]}"#);
    assert_eq!(out[0].result, Done::Skipped);
    assert!(out[0].error.as_deref().unwrap().starts_with("repo/.git differs in NEU: ALT may have commits"), "{out:?}");
    assert!(d.old.path().join("repo/.git/HEAD").exists());
}
