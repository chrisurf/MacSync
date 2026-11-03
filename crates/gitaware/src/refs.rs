//! Which side of a Git state conflict is simply ahead.
//!
//! Both Macs wrote HEAD or refs before they saw each other's version. If every
//! differing ref of one side is part of the other side's history, that side
//! is only behind: taking the other side's state is a fast-forward and loses
//! nothing. Only real divergence (both sides have commits the other lacks, a
//! different branch checked out) stays a conflict.

use std::collections::BTreeMap;
use std::path::Path;

use crate::cmd;

/// Values of the refs one file holds: a loose ref or `HEAD` holds itself
/// (`name → sha` or `name → "ref: refs/heads/x"`), `packed-refs` holds many.
pub fn ref_values(path: &str, bytes: &[u8]) -> Option<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut out = BTreeMap::new();
    if path == ".git/packed-refs" {
        for line in text.lines() {
            if line.starts_with('#') || line.starts_with('^') || line.trim().is_empty() {
                continue;
            }
            let (sha, name) = line.split_once(' ')?;
            out.insert(name.trim().to_string(), sha.trim().to_string());
        }
    } else {
        out.insert(path.trim_start_matches(".git/").to_string(), text.trim().to_string());
    }
    Some(out)
}

/// The side whose Git state is the newer one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ahead {
    Local,
    Remote,
    /// Both sides have something the other lacks: a real conflict.
    Diverged,
    /// Git cannot tell yet, typically because the other side's commits have
    /// not arrived here.
    Unknown,
}

/// Compare the refs of both sides (merged over all differing ref files) in
/// the working tree `work`, whose objects are this Mac's.
pub fn ahead(work: &Path, local: &BTreeMap<String, String>, remote: &BTreeMap<String, String>) -> Ahead {
    let mut votes = Vec::new();
    let names: std::collections::BTreeSet<&String> = local.keys().chain(remote.keys()).collect();
    for name in names {
        let vote = match (local.get(name), remote.get(name)) {
            (Some(l), Some(r)) if l == r => continue,
            // A ref that only one side has: that side has more.
            (Some(_), None) => Ahead::Local,
            (None, Some(_)) => Ahead::Remote,
            (Some(l), Some(r)) if is_sha(l) && is_sha(r) => {
                match (cmd::is_ancestor(work, l, r).ok().flatten(), cmd::is_ancestor(work, r, l).ok().flatten()) {
                    (Some(true), _) => Ahead::Remote,
                    (_, Some(true)) => Ahead::Local,
                    (Some(false), Some(false)) => Ahead::Diverged,
                    _ => Ahead::Unknown,
                }
            }
            // Another branch checked out, or a symbolic ref against a commit.
            _ => Ahead::Diverged,
        };
        votes.push(vote);
    }
    combine(&votes)
}

fn combine(votes: &[Ahead]) -> Ahead {
    if votes.is_empty() || votes.contains(&Ahead::Diverged) {
        return Ahead::Diverged;
    }
    let local = votes.contains(&Ahead::Local);
    let remote = votes.contains(&Ahead::Remote);
    match (local, remote) {
        (true, true) => Ahead::Diverged,
        _ if votes.contains(&Ahead::Unknown) => Ahead::Unknown,
        (true, false) => Ahead::Local,
        (false, true) => Ahead::Remote,
        (false, false) => Ahead::Diverged,
    }
}

fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit `HEAD` points at when it is detached (a single commit checked
/// out instead of a branch); `None` on a branch or without a repository.
pub fn detached_head(work: &Path) -> Option<String> {
    let dir = crate::resolve_git_dir(work)?;
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let head = head.trim();
    is_sha(head).then(|| head.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::tests::{git, git_available};

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn packed_refs_are_parsed_without_peeled_lines() {
        let text = b"# pack-refs with: peeled fully-peeled sorted\n\
            1111111111111111111111111111111111111111 refs/heads/main\n\
            2222222222222222222222222222222222222222 refs/tags/v1\n\
            ^3333333333333333333333333333333333333333\n";
        let m = ref_values(".git/packed-refs", text).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m["refs/tags/v1"], "2222222222222222222222222222222222222222");
        let h = ref_values(".git/HEAD", b"ref: refs/heads/main\n").unwrap();
        assert_eq!(h["HEAD"], "ref: refs/heads/main");
    }

    #[test]
    fn older_commit_is_behind_and_branches_diverge() {
        if !git_available() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let w = d.path();
        git(w, &["init", "-q"]);
        std::fs::write(w.join("a"), "1").unwrap();
        git(w, &["add", "a"]);
        git(w, &["commit", "-qm", "one"]);
        let one = git(w, &["rev-parse", "HEAD"]).trim().to_string();
        std::fs::write(w.join("a"), "2").unwrap();
        git(w, &["commit", "-qam", "two"]);
        let two = git(w, &["rev-parse", "HEAD"]).trim().to_string();
        git(w, &["checkout", "-q", "-b", "side", &one]);
        std::fs::write(w.join("b"), "x").unwrap();
        git(w, &["add", "b"]);
        git(w, &["commit", "-qm", "side"]);
        let side = git(w, &["rev-parse", "HEAD"]).trim().to_string();

        let r = |v: &str| map(&[("refs/heads/main", v)]);
        assert_eq!(ahead(w, &r(&one), &r(&two)), Ahead::Remote);
        assert_eq!(ahead(w, &r(&two), &r(&one)), Ahead::Local);
        assert_eq!(ahead(w, &r(&two), &r(&side)), Ahead::Diverged);
        let missing = "4444444444444444444444444444444444444444";
        assert_eq!(ahead(w, &r(&two), &r(missing)), Ahead::Unknown);
        // Several refs must agree on the direction.
        let both = |m: &str, o: &str| map(&[("refs/heads/main", m), ("refs/remotes/origin/main", o)]);
        assert_eq!(ahead(w, &both(&one, &one), &both(&two, &two)), Ahead::Remote);
        assert_eq!(ahead(w, &both(&one, &two), &both(&two, &one)), Ahead::Diverged);
        // Another branch checked out is no fast-forward.
        let head = |v: &str| map(&[("HEAD", v)]);
        assert_eq!(ahead(w, &head("ref: refs/heads/main"), &head("ref: refs/heads/side")), Ahead::Diverged);

        assert_eq!(detached_head(w), None);
        git(w, &["checkout", "-q", &one]);
        assert_eq!(detached_head(w), Some(one));
    }
}
