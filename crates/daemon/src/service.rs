//! Service definitions: launchd LaunchAgent (macOS) and systemd user unit.

use std::path::{Path, PathBuf};

pub const LABEL: &str = "com.reposync.daemon";

pub fn launchd_plist(binary: &Path, config: &Path, log_dir: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>--config</string>
    <string>{cfg}</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
  <key>StandardOutPath</key><string>{log}/daemon.log</string>
  <key>StandardErrorPath</key><string>{log}/daemon.log</string>
</dict>
</plist>
"#,
        bin = binary.display(),
        cfg = config.display(),
        log = log_dir.display()
    )
}

pub fn systemd_unit(binary: &Path, config: &Path) -> String {
    format!(
        "[Unit]\nDescription=RepoSync daemon\nAfter=network-online.target\n\n[Service]\nExecStart={} --config {} daemon\nRestart=always\nRestartSec=5\nNice=10\nIOSchedulingClass=idle\n\n[Install]\nWantedBy=default.target\n",
        binary.display(),
        config.display()
    )
}

/// Where the service definition is installed for the current user.
pub fn install_path() -> PathBuf {
    let home = crate::config::home();
    if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
    } else {
        home.join(".config/systemd/user/reposync.service")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders() {
        let p = launchd_plist(Path::new("/usr/local/bin/reposync"), Path::new("/c.toml"), Path::new("/logs"));
        assert!(p.contains("<string>/usr/local/bin/reposync</string>"));
        assert!(p.contains("KeepAlive"));
        let u = systemd_unit(Path::new("/bin/reposync"), Path::new("/c.toml"));
        assert!(u.contains("ExecStart=/bin/reposync --config /c.toml daemon"));
    }
}
