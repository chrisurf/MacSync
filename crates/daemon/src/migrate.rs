//! Takes over an installation from the time the product was called RepoSync
//! (up to 0.2.x): configuration, state (identity, pairing, database) and the
//! LaunchAgent. Idempotent; runs before every command (ADR-0011).

use std::path::{Path, PathBuf};

use crate::config::{default_config_path, default_state_dir};

/// Legacy names, kept only for this migration.
const LEGACY_DIR: &str = "reposync";
const LEGACY_STATE_DIR_MAC: &str = "Library/Application Support/RepoSync";
const LEGACY_LAUNCH_AGENT: &str = "Library/LaunchAgents/com.reposync.daemon.plist";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Moves `from` to `to` if only `from` exists. Returns a message if it did.
fn take_over(from: &Path, to: &Path) -> Option<String> {
    if to.exists() || !from.exists() {
        return None;
    }
    if let Some(p) = to.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    match std::fs::rename(from, to) {
        Ok(()) => Some(format!("moved {} → {}", from.display(), to.display())),
        Err(e) => Some(format!("could not move {} to {}: {e}", from.display(), to.display())),
    }
}

/// Migrates the default locations. Does nothing when `MACSYNC_CONFIG` points
/// elsewhere (tests, custom setups); callers with an explicit configuration
/// path must not call it either. Returns what was done.
pub fn legacy() -> Vec<String> {
    if std::env::var_os("MACSYNC_CONFIG").is_some() {
        return Vec::new();
    }
    let mut done = Vec::new();
    let config = default_config_path();
    if let Some(new_dir) = config.parent() {
        if let Some(base) = new_dir.parent() {
            done.extend(take_over(&base.join(LEGACY_DIR), new_dir));
        }
    }
    let state = default_state_dir();
    let old_state =
        if cfg!(target_os = "macos") { home().join(LEGACY_STATE_DIR_MAC) } else { state.with_file_name(LEGACY_DIR) };
    done.extend(take_over(&old_state, &state));
    if let Err(e) = macsync_store::db_path(&state) {
        done.push(format!("database: {e:#}"));
    }
    let agent = home().join(LEGACY_LAUNCH_AGENT);
    if agent.exists() {
        done.push(format!(
            "old LaunchAgent {} still installed – remove it: launchctl bootout gui/$(id -u)/com.reposync.daemon; rm {}",
            agent.display(),
            agent.display()
        ));
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_over_moves_only_when_target_is_free() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("old"), d.path().join("new/inner"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("f"), "x").unwrap();
        assert!(take_over(&a, &b).is_some());
        assert!(b.join("f").exists() && !a.exists());
        std::fs::create_dir_all(&a).unwrap();
        assert!(take_over(&a, &b).is_none(), "existing target is never overwritten");
        assert!(a.exists());
    }
}
