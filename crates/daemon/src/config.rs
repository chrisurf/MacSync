//! TOML configuration (`~/.config/reposync/config.toml`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use reposync_engine::{EngineConfig, Mode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Human readable name of this Mac.
    pub device_name: String,
    /// The root directory whose children are synchronised.
    pub root: PathBuf,
    /// Database, identity and conflict store. Defaults to
    /// `~/Library/Application Support/RepoSync` (macOS) or
    /// `$XDG_DATA_HOME/reposync`.
    pub state_dir: Option<PathBuf>,
    /// Address of the protocol server.
    pub listen: SocketAddr,
    /// Port used while pairing.
    pub pair_port: u16,
    /// `two-way`, `mirror-send` or `mirror-receive`.
    pub mode: String,
    pub peer: PeerConfig,
    pub sync: SyncConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PeerConfig {
    /// Fixed address of the peer (`host:port`); otherwise the paired address
    /// or Bonjour discovery is used.
    pub address: Option<String>,
    /// Use Bonjour/mDNS to advertise and find the peer.
    pub discovery: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    /// Minimum quiet time after the last event before a unit is captured.
    pub quiet_ms: u64,
    /// Quiet time used during bursts (e.g. `npm install`).
    pub max_quiet_ms: u64,
    /// Capture a unit after this long even if it never becomes quiet.
    pub max_wait_s: u64,
    /// Interval of the root-hash comparison with the peer.
    pub anti_entropy_s: u64,
    /// Interval of a stat-only rescan of all units (catches missed file
    /// system events within minutes instead of at the daily verification).
    pub rescan_interval_s: u64,
    /// Interval of the full re-hash against the file system.
    pub verify_interval_h: u64,
    pub fsck_after_apply: bool,
    pub strip_quarantine: bool,
    pub default_excludes: bool,
    /// Globs relative to the root, e.g. `"**/target"`.
    pub excludes: Vec<String>,
    pub tombstone_ttl_days: u64,
    pub trash_retention_days: u64,
    pub journal_retention_days: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            quiet_ms: 400,
            max_quiet_ms: 5000,
            max_wait_s: 30,
            anti_entropy_s: 60,
            rescan_interval_s: 600,
            verify_interval_h: 24,
            fsck_after_apply: true,
            strip_quarantine: false,
            default_excludes: true,
            excludes: Vec::new(),
            tombstone_ttl_days: 30,
            trash_retention_days: 14,
            journal_retention_days: 90,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            device_name: hostname(),
            root: home().join("Code"),
            state_dir: None,
            listen: SocketAddr::from(([0, 0, 0, 0], reposync_net::DEFAULT_PORT)),
            pair_port: reposync_net::DEFAULT_PORT + 1,
            mode: "two-way".into(),
            peer: PeerConfig { address: None, discovery: true },
            sync: SyncConfig::default(),
        }
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "mac".into())
}

/// Expand a leading `~/`.
pub fn expand(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => p.to_path_buf(),
    }
}

/// Default location of the configuration file.
pub fn default_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("REPOSYNC_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
    base.join("reposync").join("config.toml")
}

/// Default state directory for this platform.
pub fn default_state_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/RepoSync")
    } else {
        let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"));
        base.join("reposync")
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("read config {} (run `reposync init` first)", path.display()))?;
        let cfg: Config = toml::from_str(&s).with_context(|| format!("parse config {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, format!("# RepoSync configuration\n# Docs: docs/operations.md\n\n{body}"))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        Mode::parse(&self.mode).with_context(|| format!("unknown mode {:?}", self.mode))?;
        reposync_fs::Excludes::new(&self.sync.excludes, self.sync.default_excludes).context("invalid exclude glob")?;
        if !reposync_net::lan::is_local(&self.listen.ip()) && !self.listen.ip().is_unspecified() {
            anyhow::bail!("listen address {} is not a local address", self.listen);
        }
        if let Some(a) = &self.peer.address {
            let (host, port) = a.rsplit_once(':').unwrap_or((a, ""));
            if host.trim_matches(['[', ']']).is_empty() || port.parse::<u16>().is_err() {
                anyhow::bail!("peer address {a:?} must be host:port, e.g. \"mac-b.local:47123\"");
            }
        }
        Ok(())
    }

    pub fn root(&self) -> PathBuf {
        expand(&self.root)
    }

    pub fn state_dir(&self) -> PathBuf {
        self.state_dir.as_deref().map(expand).unwrap_or_else(default_state_dir)
    }

    pub fn mode(&self) -> Mode {
        Mode::parse(&self.mode).unwrap_or_default()
    }

    pub fn engine_config(&self, device: &str) -> Result<EngineConfig> {
        let mut e = EngineConfig::new(self.root(), self.state_dir(), device);
        e.mode = self.mode();
        e.excludes = reposync_fs::Excludes::new(&self.sync.excludes, self.sync.default_excludes)?;
        e.strip_quarantine = self.sync.strip_quarantine;
        e.fsck_after_apply = self.sync.fsck_after_apply;
        e.tombstone_ttl_days = self.sync.tombstone_ttl_days;
        e.trash_retention_days = self.sync.trash_retention_days;
        e.journal_retention_days = self.sync.journal_retention_days;
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_address_needs_host_and_port() {
        let mut c = Config::default();
        for bad in [":47123", "mac-b.local", "mac-b.local:", "[]:47123"] {
            c.peer.address = Some(bad.into());
            assert!(c.validate().is_err(), "{bad}");
        }
        for ok in ["mac-b.local:47123", "192.168.1.20:47123", "[fe80::1]:47123"] {
            c.peer.address = Some(ok.into());
            assert!(c.validate().is_ok(), "{ok}");
        }
    }

    #[test]
    fn roundtrip_and_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("c.toml");
        let mut c = Config { root: "~/Projects".into(), ..Config::default() };
        c.sync.excludes = vec!["**/target".into()];
        c.save(&p).unwrap();
        let l = Config::load(&p).unwrap();
        assert_eq!(l, c);
        assert!(l.root().is_absolute());
        // Partial files use defaults.
        std::fs::write(&p, "root = \"/tmp/x\"\nmode = \"mirror-send\"\n[sync]\nquiet_ms = 100\n").unwrap();
        let l = Config::load(&p).unwrap();
        assert_eq!(l.sync.quiet_ms, 100);
        assert_eq!(l.sync.anti_entropy_s, 60);
        assert_eq!(l.mode(), Mode::MirrorSend);
        std::fs::write(&p, "mode = \"sideways\"\n").unwrap();
        assert!(Config::load(&p).is_err());
        std::fs::write(&p, "listen = \"8.8.8.8:1\"\n").unwrap();
        assert!(Config::load(&p).is_err());
    }
}
