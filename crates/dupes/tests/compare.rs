use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use filetime::{set_file_mtime, FileTime};
use macsync_dupes::{compare, Comparison, FolderStatus, Options, OrderStatus, State};

const DAY: i64 = 86_400;
const T0: i64 = 1_700_000_000;

fn put(root: &Path, rel: &str, body: &str, day: i64) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
    set_file_mtime(&p, FileTime::from_unix_time(T0 + day * DAY, 0)).unwrap();
}

/// Two repositories, as they were on day 0.
fn repos(root: &Path) {
    for r in ["alpha", "beta"] {
        put(root, &format!("{r}/README.md"), &format!("# {r}\n"), 0);
        put(root, &format!("{r}/src/main.rs"), &format!("fn main() {{ {r}() }}\n"), 0);
        put(root, &format!("{r}/docs/plan.md"), &format!("Plan {r}\n"), 0);
    }
}

fn run(old: &Path, new: &Path) -> Comparison {
    compare(old, new, &Options::default()).unwrap()
}

fn folder<'a>(c: &'a Comparison, path: &str) -> &'a macsync_dupes::Folder {
    c.folders.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no folder {path}: {:#?}", c.folders))
}

#[test]
fn identical_copy_can_go_completely() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    let c = run(old.path(), new.path());
    assert_eq!(c.folders[0].status, FolderStatus::Identical);
    assert_eq!(c.folders[0].total.same, 6);
    assert_eq!(c.deletable_bytes, c.old_bytes);
    assert!(c.entries.is_empty(), "identical files are only counted: {:#?}", c.entries);
    assert_eq!(c.order.status, OrderStatus::Unknown);
}

#[test]
fn newer_and_added_files_in_new_still_let_old_go() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    put(new.path(), "alpha/docs/plan.md", "Plan alpha, viel länger\n", 30);
    put(new.path(), "alpha/docs/neu.md", "neu\n", 31);
    put(new.path(), "gamma/README.md", "# gamma\n", 31);
    let c = run(old.path(), new.path());
    assert_eq!(c.folders[0].status, FolderStatus::Deletable);
    assert_eq!(folder(&c, "alpha").status, FolderStatus::Deletable);
    assert_eq!(folder(&c, "alpha").total.updated, 1);
    assert_eq!(folder(&c, "alpha").total.only_new, 1);
    assert_eq!(folder(&c, "beta").status, FolderStatus::Identical);
    assert_eq!(folder(&c, "gamma").status, FolderStatus::OnlyNew);
    assert_eq!(c.order.status, OrderStatus::Confirmed);
}

#[test]
fn work_only_in_old_is_pointed_out_down_to_its_folder() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    put(old.path(), "alpha/src/main.rs", "fn main() { alpha(); später() }\n", 20);
    put(old.path(), "alpha/docs/notizen.md", "nur hier\n", 21);
    put(old.path(), "altprojekt/a.txt", "alt\n", 3);
    let c = run(old.path(), new.path());
    assert_eq!(c.folders[0].status, FolderStatus::Check);
    assert_eq!(folder(&c, "alpha").status, FolderStatus::Check);
    assert_eq!(folder(&c, "alpha/src").status, FolderStatus::Check);
    assert_eq!(folder(&c, "alpha/src").own.old_newer, 1);
    assert_eq!(folder(&c, "alpha/docs").own.only_old, 1);
    assert_eq!(folder(&c, "beta").status, FolderStatus::Identical);
    assert_eq!(folder(&c, "altprojekt").status, FolderStatus::OnlyOld);
    // Only beta can go.
    assert_eq!(c.deletable_bytes, folder(&c, "beta").old_bytes);
    let e = c.entries.iter().find(|e| e.path == "alpha/src/main.rs").unwrap();
    assert_eq!(e.state, State::OldNewer);
    // The only changed file is newer in ALT: the order looks wrong.
    assert_eq!(c.order.status, OrderStatus::Contradicted);
}

#[test]
fn moved_file_is_no_loss() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    fs::create_dir(new.path().join("alpha/archiv")).unwrap();
    fs::rename(new.path().join("alpha/docs/plan.md"), new.path().join("alpha/archiv/plan.md")).unwrap();
    let c = run(old.path(), new.path());
    let e = c.entries.iter().find(|e| e.path == "alpha/docs/plan.md").unwrap();
    assert_eq!(e.state, State::Moved);
    assert_eq!(e.moved_to.as_deref(), Some("alpha/archiv/plan.md"));
    assert_eq!(folder(&c, "alpha").status, FolderStatus::Deletable);
    assert!(!c.entries.iter().any(|e| e.state == State::OnlyNew), "the target is explained by the move");
}

#[test]
fn same_date_different_content_is_unclear() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    put(new.path(), "beta/docs/plan.md", "Plan beta!\n", 0);
    let c = run(old.path(), new.path());
    assert_eq!(folder(&c, "beta/docs").own.unclear, 1);
    assert_eq!(folder(&c, "beta").status, FolderStatus::Check);
}

#[test]
fn unreadable_root_is_an_error_not_an_empty_folder() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    let locked = old.path().join("alpha");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let res = compare(&locked, &new.path().join("alpha"), &Options::default());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let err = res.expect_err("an unreadable root must not look empty");
    assert!(err.to_string().contains("cannot read the folder"), "{err}");
}

#[test]
fn unreadable_sub_folder_is_never_deletable() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    let locked = old.path().join("alpha/docs");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let c = run(old.path(), new.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(folder(&c, "alpha/docs").status, FolderStatus::Unreadable);
    assert_eq!(folder(&c, "alpha").status, FolderStatus::Unreadable);
    assert_eq!(folder(&c, "beta").status, FolderStatus::Identical);
    assert!(!c.folders[0].status.deletable());
}

#[test]
fn unreadable_place_in_new_does_not_count_as_only_old() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    repos(old.path());
    repos(new.path());
    let locked = new.path().join("beta/src");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let c = run(old.path(), new.path());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let e = c.entries.iter().find(|e| e.path == "beta/src/main.rs").unwrap();
    assert_eq!(e.state, State::Unreadable);
    assert_eq!(folder(&c, "beta").status, FolderStatus::Unreadable);
}

#[test]
fn names_differing_in_case_are_the_same_path() {
    let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    put(old.path(), "Projekt/Readme.md", "x\n", 0);
    put(new.path(), "projekt/README.md", "x\n", 0);
    let c = run(old.path(), new.path());
    assert_eq!(c.folders[0].status, FolderStatus::Identical, "{:#?}", c.entries);
    assert_eq!(folder(&c, "Projekt").name, "Projekt", "ALT's spelling is shown");
}
