//! Quiescence gate: a Git unit is only captured or written while Git is not
//! working on it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reposync_core::gitpath::{LOCK_FILES, OPERATION_MARKERS};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateState {
    Open,
    /// A lock file exists (relative path inside the Git directory).
    Locked(String),
    /// A multi-step operation (merge, rebase, …) is in progress.
    Operation(String),
    /// A `git` process works inside the unit.
    GitProcess(u32),
}

impl GateState {
    pub fn is_open(&self) -> bool {
        *self == GateState::Open
    }

    pub fn describe(&self) -> String {
        match self {
            GateState::Open => "open".into(),
            GateState::Locked(l) => format!("lock file {l}"),
            GateState::Operation(o) => format!("git operation in progress ({o})"),
            GateState::GitProcess(p) => format!("git process {p} running"),
        }
    }
}

/// Process-probing gate with a 500 ms cache of running `git` processes.
pub struct Gate {
    procs: Mutex<ProcCache>,
}

struct ProcCache {
    sys: System,
    at: Option<Instant>,
    git_cwds: Vec<(u32, PathBuf)>,
}

const PROC_CACHE: Duration = Duration::from_millis(500);

impl Default for Gate {
    fn default() -> Self {
        Gate::new()
    }
}

impl Gate {
    pub fn new() -> Gate {
        Gate {
            procs: Mutex::new(ProcCache { sys: System::new(), at: None, git_cwds: Vec::new() }),
        }
    }

    /// Check a Git working tree `work` (the unit directory).
    pub fn check(&self, work: &Path) -> GateState {
        if let Some(git_dir) = crate::resolve_git_dir(work) {
            if let Some(s) = check_git_dir(&git_dir) {
                return s;
            }
        }
        if let Some(pid) = self.git_process_in(work) {
            return GateState::GitProcess(pid);
        }
        GateState::Open
    }

    /// Like [`Gate::check`] but only the file based checks (no process scan).
    pub fn check_files(work: &Path) -> GateState {
        match crate::resolve_git_dir(work).and_then(|g| check_git_dir(&g)) {
            Some(s) => s,
            None => GateState::Open,
        }
    }

    fn git_process_in(&self, work: &Path) -> Option<u32> {
        let mut c = self.procs.lock().unwrap_or_else(|e| e.into_inner());
        if c.at.is_none_or(|t| t.elapsed() > PROC_CACHE) {
            c.sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
            );
            let me = std::process::id();
            let list = c
                .sys
                .processes()
                .iter()
                .filter(|(pid, p)| {
                    pid.as_u32() != me && {
                        let n = p.name().to_string_lossy();
                        n == "git" || n.starts_with("git-")
                    }
                })
                .filter_map(|(pid, p)| Some((pid.as_u32(), p.cwd()?.to_path_buf())))
                .collect();
            c.git_cwds = list;
            c.at = Some(Instant::now());
        }
        let work = std::fs::canonicalize(work).unwrap_or_else(|_| work.to_path_buf());
        c.git_cwds.iter().find(|(_, cwd)| cwd.starts_with(&work)).map(|(p, _)| *p)
    }
}

fn check_git_dir(git_dir: &Path) -> Option<GateState> {
    for l in LOCK_FILES {
        if git_dir.join(l).exists() {
            return Some(GateState::Locked(l.to_string()));
        }
    }
    for m in OPERATION_MARKERS {
        if git_dir.join(m).exists() {
            return Some(GateState::Operation(m.to_string()));
        }
    }
    find_ref_lock(&git_dir.join("refs"), "refs", 0).map(GateState::Locked)
}

fn find_ref_lock(dir: &Path, rel: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let r = format!("{rel}/{name}");
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if let Some(f) = find_ref_lock(&e.path(), &r, depth + 1) {
                return Some(f);
            }
        } else if name.ends_with(".lock") {
            return Some(r);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn locks_and_operations_close_the_gate() {
        let d = tempfile::tempdir().unwrap();
        let w = d.path();
        fs::create_dir_all(w.join(".git/refs/heads/feature")).unwrap();
        let g = Gate::new();
        assert_eq!(g.check(w), GateState::Open);
        fs::write(w.join(".git/index.lock"), "").unwrap();
        assert_eq!(g.check(w), GateState::Locked("index.lock".into()));
        fs::remove_file(w.join(".git/index.lock")).unwrap();
        fs::write(w.join(".git/refs/heads/feature/x.lock"), "").unwrap();
        assert_eq!(Gate::check_files(w), GateState::Locked("refs/heads/feature/x.lock".into()));
        fs::remove_file(w.join(".git/refs/heads/feature/x.lock")).unwrap();
        fs::create_dir_all(w.join(".git/rebase-merge")).unwrap();
        assert_eq!(Gate::check_files(w), GateState::Operation("rebase-merge".into()));
    }

    #[test]
    fn gitdir_file_is_followed() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("store/wt")).unwrap();
        fs::create_dir_all(d.path().join("work")).unwrap();
        fs::write(d.path().join("work/.git"), "gitdir: ../store/wt\n").unwrap();
        fs::write(d.path().join("store/wt/MERGE_HEAD"), "x").unwrap();
        assert_eq!(Gate::check_files(&d.path().join("work")), GateState::Operation("MERGE_HEAD".into()));
    }
}
