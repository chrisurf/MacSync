//! Two-peer integration tests with real Git repositories.

mod common;

use std::fs;

use common::*;
use macsync_engine::{Choice, Outcome, PullOptions};

macro_rules! require_git {
    () => {
        if !git_available() {
            eprintln!("git not available; skipping");
            return;
        }
    };
}

#[test]
fn repository_is_mirrored_with_uncommitted_work_and_no_history_changes() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    // Uncommitted, staged, untracked and ignored work must all arrive.
    fs::write(a.join("src/main.rs"), "fn main() { println!(\"wip\"); }\n").unwrap();
    fs::write(a.join("staged.txt"), "staged").unwrap();
    git(&a, &["add", "staged.txt"]);
    fs::write(a.join(".gitignore"), "build/\n").unwrap();
    write(&a, "build/out.bin", "ignored artefact");
    write(&a, "untracked.txt", "untracked");
    git(&a, &["stash", "list"]);
    let refs_before = refs(&a);

    p.sync();
    let b = p.rb().join("app");
    assert_fsck(&b);
    assert_eq!(git(&a, &["status", "--porcelain"]), git(&b, &["status", "--porcelain"]));
    assert_eq!(git(&a, &["log", "--format=%H %s"]), git(&b, &["log", "--format=%H %s"]));
    assert_eq!(read(&b, "build/out.bin"), "ignored artefact");
    assert_eq!(git(&b, &["diff", "--cached", "--name-only"]).trim(), "staged.txt");
    // H2: MacSync never creates commits, refs, branches, stashes.
    assert_eq!(refs(&a), refs_before);
    assert_eq!(refs(&b), refs_before);
    p.assert_identical();
}

#[test]
fn commits_branches_and_checkouts_follow() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    p.sync();
    let b = p.rb().join("app");
    fs::write(a.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-qm", "second"]);
    git(&a, &["checkout", "-qb", "feature"]);
    fs::write(a.join("feature.txt"), "f").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-qm", "feature work"]);
    p.sync();
    assert_fsck(&b);
    assert_eq!(git(&b, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "feature");
    assert_eq!(git(&a, &["rev-parse", "HEAD"]), git(&b, &["rev-parse", "HEAD"]));
    assert_eq!(git(&b, &["status", "--porcelain"]), "");

    // Work continues on B and flows back to A.
    git(&b, &["checkout", "-q", "main"]);
    fs::write(b.join("from-b.txt"), "b").unwrap();
    git(&b, &["add", "."]);
    git(&b, &["commit", "-qm", "from B"]);
    p.sync();
    assert_fsck(&a);
    assert_eq!(git(&a, &["log", "-1", "--format=%s"]).trim(), "from B");
    assert_eq!(git(&a, &["status", "--porcelain"]), "");
    p.assert_identical();
}

#[test]
fn git_status_on_the_receiver_causes_no_traffic() {
    require_git!();
    let p = Pair::new();
    init_repo(&p.ra().join("app"));
    p.sync();
    let b = p.rb().join("app");
    // Touch the stat cache of B's index: bytes change, semantics do not.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    for f in ["README.md", "src/main.rs"] {
        let c = fs::read(b.join(f)).unwrap();
        fs::write(b.join(f), c).unwrap();
    }
    git(&b, &["status", "--porcelain"]);
    git(&b, &["update-index", "--refresh", "-q"]);
    let reports = p.b.scan_all(false).unwrap();
    let app = reports.iter().find(|r| r.unit == "app").unwrap();
    // Touched worktree files are re-hashed but unchanged; the index bytes
    // changed but its semantic hash did not.
    assert_eq!(app.changed, 0, "{app:?}");
    assert!(p.roots_equal());
    let r = p.sync();
    assert!(r.iter().all(|r| r.outcome == Outcome::InSync), "{r:?}");
}

#[test]
fn quiescence_gate_blocks_capture_and_apply() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    p.sync();
    // A git command "runs" on A: its lock blocks capture.
    fs::write(a.join("new.txt"), "n").unwrap();
    fs::write(a.join(".git/index.lock"), "").unwrap();
    let r = p.a.scan_unit("app", &Default::default()).unwrap();
    assert!(r.deferred.as_deref().unwrap_or("").contains("index.lock"), "{r:?}");
    fs::remove_file(a.join(".git/index.lock")).unwrap();
    p.sync();
    assert!(p.rb().join("app/new.txt").exists());

    // A rebase in progress on B blocks applying there.
    let b = p.rb().join("app");
    fs::create_dir_all(b.join(".git/rebase-merge")).unwrap();
    fs::write(a.join("new2.txt"), "n").unwrap();
    p.a.scan_all(false).unwrap();
    let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    let app = r.iter().find(|r| r.unit == "app").unwrap();
    assert!(matches!(&app.outcome, Outcome::Deferred(m) if m.contains("rebase")), "{:?}", app.outcome);
    assert!(!b.join("new2.txt").exists());
    fs::remove_dir_all(b.join(".git/rebase-merge")).unwrap();
    p.sync();
    assert!(b.join("new2.txt").exists());
    assert_fsck(&b);
}

#[test]
fn concurrent_commits_are_one_git_state_conflict() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    p.sync();
    let b = p.rb().join("app");
    fs::write(a.join("a.txt"), "a").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-qm", "on A"]);
    fs::write(b.join("b.txt"), "b").unwrap();
    git(&b, &["add", "."]);
    git(&b, &["commit", "-qm", "on B"]);
    let head_a = git(&a, &["rev-parse", "HEAD"]).trim().to_string();
    let head_b = git(&b, &["rev-parse", "HEAD"]).trim().to_string();

    p.sync();
    // Each side keeps its own HEAD, index and working tree…
    assert_eq!(git(&a, &["rev-parse", "HEAD"]).trim(), head_a);
    assert_eq!(git(&b, &["rev-parse", "HEAD"]).trim(), head_b);
    assert_fsck(&a);
    assert_fsck(&b);
    assert_eq!(git(&a, &["status", "--porcelain", "--untracked-files=no"]), "");
    // …but objects are merged, so the other commit can be inspected/merged.
    assert!(git_ok(&b, &["cat-file", "-e", &format!("{head_a}^{{commit}}")]));
    assert!(git_ok(&a, &["cat-file", "-e", &format!("{head_b}^{{commit}}")]));
    // Exactly one Git state conflict per side.
    let ca: Vec<_> = p.a.store().conflicts(false).unwrap().into_iter().filter(|c| c.class == "git-state").collect();
    assert_eq!(ca.len(), 1, "{ca:?}");
    assert!(ca[0].remotes.iter().any(|r| r.path == ".git/refs/heads/main" || r.path == ".git/index"));

    // Resolve on A: take B's Git state (HEAD, refs, index); the worktree files
    // of both sides are already present.
    p.a.resolve(ca[0].id, Choice::Remote).unwrap();
    assert_eq!(git(&a, &["rev-parse", "HEAD"]).trim(), head_b);
    assert_fsck(&a);
    p.sync();
    assert_eq!(git(&b, &["rev-parse", "HEAD"]).trim(), head_b);
    assert!(p.b.store().conflicts(false).unwrap().iter().all(|c| c.class != "git-state"));
}

/// Like a `git pull` on both Macs before they synchronised, then a commit on
/// one of them: both wrote the refs, but one Mac is only behind. That is a
/// fast-forward, not a conflict.
#[test]
fn pull_on_both_and_commit_on_one_is_a_fast_forward() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("notes");
    init_repo(&a);
    p.sync();
    let b = p.rb().join("notes");
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path().join("origin");
    git(origin_dir.path(), &["clone", "-q", &a.display().to_string(), "origin"]);
    fs::write(origin.join("server.md"), "from the server").unwrap();
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-qm", "on the server"]);
    let o = origin.display().to_string();
    git(&a, &["pull", "-q", "--ff-only", &o, "main"]);
    git(&b, &["pull", "-q", "--ff-only", &o, "main"]);
    fs::write(a.join("note.md"), "written on A").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-qm", "on A"]);
    let head_a = git(&a, &["rev-parse", "HEAD"]).trim().to_string();

    p.sync();
    for (e, w) in [(&p.a, &a), (&p.b, &b)] {
        let open: Vec<_> = e.store().conflicts(false).unwrap();
        assert!(open.is_empty(), "{open:?}");
        assert_eq!(git(w, &["rev-parse", "HEAD"]).trim(), head_a);
        assert_eq!(git(w, &["status", "--porcelain"]), "");
        assert_fsck(w);
    }
    assert_eq!(read(&p.rb(), "notes/note.md"), "written on A");
    p.assert_identical();
}

fn two_commits(dir: &std::path::Path) -> (String, String) {
    init_repo(dir);
    let first = git(dir, &["rev-parse", "HEAD"]).trim().to_string();
    fs::write(dir.join("README.md"), "# demo, second version\n").unwrap();
    git(dir, &["commit", "-qam", "second"]);
    (first, git(dir, &["rev-parse", "HEAD"]).trim().to_string())
}

/// Looking at the past (`git checkout <commit>`) is not distributed: the
/// other Mac keeps the current state, and its changes wait until a branch is
/// checked out again here. Then everything continues without a conflict.
#[test]
fn checking_out_an_old_commit_is_not_distributed() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("notes");
    let (first, second) = two_commits(&a);
    p.sync();
    let b = p.rb().join("notes");

    git(&a, &["checkout", "-q", &first]);
    assert_eq!(p.a.old_commit_checked_out("notes"), Some(first.clone()));
    fs::write(b.join("from-b.md"), "written on B meanwhile").unwrap();
    p.sync();
    assert_eq!(git(&b, &["rev-parse", "HEAD"]).trim(), second, "the old commit reached the other Mac");
    assert_eq!(read(&p.rb(), "notes/README.md"), "# demo, second version\n");
    assert!(!a.join("from-b.md").exists(), "written into the old checkout");

    git(&a, &["checkout", "-q", "main"]);
    assert_eq!(p.a.old_commit_checked_out("notes"), None);
    p.sync();
    for (e, w) in [(&p.a, &a), (&p.b, &b)] {
        assert!(e.store().conflicts(false).unwrap().is_empty());
        assert_eq!(git(w, &["rev-parse", "HEAD"]).trim(), second);
        assert_fsck(w);
    }
    assert_eq!(read(&p.ra(), "notes/from-b.md"), "written on B meanwhile");
    p.assert_identical();
}

/// Going back on a branch (`git reset --hard`) is a real change and is
/// distributed like a commit.
#[test]
fn reset_on_a_branch_is_distributed() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("notes");
    let (first, _) = two_commits(&a);
    p.sync();
    git(&a, &["reset", "-q", "--hard", &first]);
    assert_eq!(p.a.old_commit_checked_out("notes"), None);
    p.sync();
    let b = p.rb().join("notes");
    assert_eq!(git(&b, &["rev-parse", "HEAD"]).trim(), first);
    assert_eq!(read(&p.rb(), "notes/README.md"), "# demo\n");
    p.assert_identical();
}

/// A repository that always sits on a commit (like a submodule) is
/// synchronised as before, also when it moves to another commit.
#[test]
fn repository_that_always_sits_on_a_commit_is_synchronised() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("pinned");
    let (first, second) = two_commits(&a);
    git(&a, &["checkout", "-q", &second]);
    p.sync();
    let b = p.rb().join("pinned");
    assert_eq!(git(&b, &["rev-parse", "HEAD"]).trim(), second);
    git(&a, &["checkout", "-q", &first]);
    assert_eq!(p.a.old_commit_checked_out("pinned"), None);
    p.sync();
    assert_eq!(git(&b, &["rev-parse", "HEAD"]).trim(), first);
    p.assert_identical();
}

#[test]
fn gc_and_repack_converge_and_stay_valid() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    for i in 0..5 {
        fs::write(a.join(format!("f{i}.txt")), format!("{i}")).unwrap();
        git(&a, &["add", "."]);
        git(&a, &["commit", "-qm", &format!("c{i}")]);
    }
    p.sync();
    git(&a, &["gc", "-q", "--prune=now"]);
    p.sync();
    let b = p.rb().join("app");
    assert_fsck(&b);
    assert_eq!(git(&a, &["rev-parse", "HEAD"]), git(&b, &["rev-parse", "HEAD"]));
    assert!(p.roots_equal());
    // Loose objects were removed on B as well (after the new pack arrived).
    let loose = fs::read_dir(b.join(".git/objects")).unwrap().flatten().filter(|e| e.file_name().len() == 2).count();
    assert_eq!(loose, 0);
}

#[test]
fn crash_during_apply_is_completed_on_restart() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    p.sync();
    for i in 0..3 {
        fs::write(a.join(format!("crash{i}.txt")), "x").unwrap();
        git(&a, &["add", "."]);
        git(&a, &["commit", "-qm", &format!("c{i}")]);
    }
    p.a.scan_all(false).unwrap();
    p.b.scan_all(false).unwrap();
    // Abort B's apply after a few file operations.
    p.b.inject_crash_after(4);
    let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    let app = r.iter().find(|r| r.unit == "app").unwrap();
    assert!(matches!(&app.outcome, Outcome::Failed(m) if m.contains("injected crash")), "{:?}", app.outcome);
    assert_eq!(p.b.store().tx_pending().unwrap().len(), 1, "transaction journaled");
    let b = p.rb().join("app");
    // Git sees our lock and refuses to run concurrently.
    assert!(b.join(".git/index.lock").exists());

    // "Restart": a new engine on the same state recovers the transaction.
    let cfg = p.b.config().clone();
    let b2 = macsync_engine::Engine::open(cfg).unwrap();
    assert!(b2.store().tx_pending().unwrap().is_empty());
    assert!(!b.join(".git/index.lock").exists());
    assert_fsck(&b);
    assert_eq!(git(&a, &["rev-parse", "HEAD"]), git(&b, &["rev-parse", "HEAD"]));
    assert_eq!(git(&b, &["status", "--porcelain"]), "");
}

#[test]
fn nested_repository_inside_folder_is_its_own_unit() {
    require_git!();
    let p = Pair::new();
    write(&p.ra(), "clients/notes.md", "n");
    init_repo(&p.ra().join("clients/acme-site"));
    p.sync();
    let units: Vec<String> = p.b.store().units().unwrap().into_iter().map(|u| u.name).collect();
    assert!(units.contains(&"clients".to_string()));
    assert!(units.contains(&"clients/acme-site".to_string()), "{units:?}");
    let b = p.rb().join("clients/acme-site");
    assert_fsck(&b);
    assert_eq!(git(&b, &["status", "--porcelain"]), "");
    // The parent unit does not contain the nested repository's paths.
    assert!(p.b.store().entries("clients").unwrap().iter().all(|e| !e.entry.path.starts_with("acme-site")));
    p.assert_identical();
}

#[test]
fn folder_becomes_repository_keeping_versions() {
    require_git!();
    let p = Pair::new();
    write(&p.ra(), "lib/x.txt", "x");
    p.sync();
    let vv_before = p.a.store().entry("lib", "x.txt").unwrap().unwrap().entry.vv;
    let a = p.ra().join("lib");
    git(&a, &["init", "-q"]);
    git(&a, &["add", "."]);
    git(&a, &["commit", "-qm", "now a repo"]);
    p.sync();
    assert_eq!(p.b.store().unit("lib").unwrap().unwrap().kind, macsync_core::UnitKind::Git);
    assert_eq!(p.a.store().entry("lib", "x.txt").unwrap().unwrap().entry.vv, vv_before);
    assert_fsck(&p.rb().join("lib"));
    p.assert_identical();
}

#[test]
fn hooks_are_synced_but_reported() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    write(&a, ".git/hooks/pre-commit", "#!/bin/sh\nexit 0\n");
    p.sync();
    assert!(p.rb().join("app/.git/hooks/pre-commit").exists());
    let log = p.b.store().journal(0, Some("app"), 1000).unwrap();
    assert!(log.iter().any(|l| l.action == "warning" && l.path == ".git/hooks/pre-commit"));
    assert!(!log.iter().any(|l| l.action == "warning" && l.path.ends_with(".sample")), "sample hooks are inert");
}

#[test]
fn git_config_changes_are_reported() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    p.sync();
    // A newly transferred repository (its own config, sample hooks) is no news.
    let items = p.b.browse(None, None, "").unwrap();
    assert_eq!(items.iter().find(|i| i.unit == "app").unwrap().git_notice, None);
    let config = fs::read_to_string(a.join(".git/config")).unwrap();
    write(&a, ".git/config", &format!("{config}[core]\n\tfsmonitor = /bin/true\n"));
    p.sync();
    assert!(fs::read_to_string(p.rb().join("app/.git/config")).unwrap().contains("fsmonitor"));
    let log = p.b.store().journal(0, Some("app"), 1000).unwrap();
    assert!(log.iter().any(|l| l.action == "warning" && l.path == ".git/config"));
    // The manager shows it on the repository.
    let items = p.b.browse(None, None, "").unwrap();
    let app = items.iter().find(|i| i.unit == "app" && i.is_unit).unwrap();
    assert_eq!(app.git_notice.as_ref().map(|n| n.path.as_str()), Some(".git/config"));
}

#[test]
fn deleted_repository_stays_deleted() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    // Loose and packed objects, as in a real repository.
    git(&a, &["gc", "-q"]);
    write(&a, "loose.txt", "loose");
    git(&a, &["add", "loose.txt"]);
    git(&a, &["commit", "-qm", "loose"]);
    p.sync();
    let b = p.rb().join("app");
    assert_fsck(&b);

    // Moving the repository elsewhere removes it here as a whole.
    fs::create_dir_all(p.ra().join("moved")).unwrap();
    fs::rename(&a, p.ra().join("moved/app")).unwrap();
    p.sync();
    p.sync();
    assert!(!a.exists());
    assert!(!b.exists(), "left over on B: {:?}", snapshot(&b).keys().take(5).collect::<Vec<_>>());
    assert_fsck(&p.rb().join("moved/app"));
    p.assert_identical();
}

#[test]
fn deleted_repository_is_not_restored_around_local_git_files() {
    require_git!();
    let p = Pair::new();
    let a = p.ra().join("app");
    init_repo(&a);
    p.sync();
    let b = p.rb().join("app");
    // Never synchronised, so `.git` on B cannot be removed with the rest.
    write(&b, ".git/fsmonitor--daemon/cookies/1", "");

    fs::remove_dir_all(&a).unwrap();
    p.sync();
    p.sync();
    // HEAD is gone: the repository was removed, not damaged. Its objects must
    // not be restored from the trash (they would come back forever).
    assert!(
        !b.join(".git/objects").exists(),
        "objects restored: {:?}",
        snapshot(&b).keys().take(5).collect::<Vec<_>>()
    );
    assert!(!a.join(".git/objects").exists());
}
