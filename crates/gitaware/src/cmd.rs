//! The only Git commands RepoSync runs. None of them creates commits, refs,
//! branches, tags or stashes.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

fn git(work: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(work)
        .env("GIT_TERMINAL_PROMPT", "0")
        // Never run hooks or pagers from the daemon.
        .env("GIT_PAGER", "cat")
        .stdin(Stdio::null());
    c
}

/// True if a `git` executable is available.
pub fn available() -> bool {
    Command::new("git").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// `git update-index --refresh -q`: refresh the stat cache of the index after
/// files were written by RepoSync. Exit code 1 ("needs update") is expected
/// when the working tree differs from the index and is not an error.
pub fn refresh_index(work: &Path) -> io::Result<()> {
    let out =
        git(work).args(["update-index", "--refresh", "-q"]).stdout(Stdio::null()).stderr(Stdio::piped()).output()?;
    match out.status.code() {
        Some(0) | Some(1) => Ok(()),
        _ => Err(io::Error::other(format!(
            "git update-index --refresh failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// `git fsck --connectivity-only`: verifies that all refs point to complete
/// object graphs. Returns the error output on failure.
pub fn fsck_connectivity(work: &Path) -> io::Result<Result<(), String>> {
    let out = git(work)
        .args(["fsck", "--connectivity-only", "--no-progress", "--no-dangling"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if out.status.success() {
        Ok(Ok(()))
    } else {
        let mut msg = String::from_utf8_lossy(&out.stderr).into_owned();
        msg.push_str(&String::from_utf8_lossy(&out.stdout));
        Ok(Err(msg.trim().to_string()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn git_available() -> bool {
        let ok = available();
        if !ok {
            eprintln!("git not available; skipping");
        }
        ok
    }

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn refresh_and_fsck_on_fresh_repo() {
        if !git_available() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        std::fs::write(d.path().join("a"), "a").unwrap();
        git(d.path(), &["add", "a"]);
        git(d.path(), &["commit", "-qm", "init"]);
        refresh_index(d.path()).unwrap();
        assert_eq!(fsck_connectivity(d.path()).unwrap(), Ok(()));
        // Corrupt: point main at a missing commit.
        std::fs::write(d.path().join(".git/refs/heads/main"), "1111111111111111111111111111111111111111\n").unwrap();
        assert!(fsck_connectivity(d.path()).unwrap().is_err());
    }
}
