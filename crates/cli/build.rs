//! Embeds the Git commit into the binary so that `macsync --version` and
//! the peer handshake show exactly which build is running.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let describe = git(&["describe", "--always", "--dirty", "--abbrev=10"]).unwrap_or_else(|| "unknown".into());
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_default();
    println!("cargo:rustc-env=MACSYNC_GIT={describe}");
    println!("cargo:rustc-env=MACSYNC_GIT_DATE={date}");
    // Rebuild when HEAD moves or the index changes (new commit, checkout, dirty state).
    for p in ["../../.git/HEAD", "../../.git/index"] {
        println!("cargo:rerun-if-changed={p}");
    }
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
        println!("cargo:rerun-if-changed=../../.git/{r}");
    }
}
