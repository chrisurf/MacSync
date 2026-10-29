//! The device identity: a static X25519 key pair used for Noise.
//!
//! Stored in `<state_dir>/identity.key` (mode 0600, directory 0700, excluded
//! from Time Machine). Loading refuses a file others can read and a key pair
//! that does not belong together. The private key is wiped from memory when
//! the identity is dropped.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{bail, Context, Result};
use curve25519_dalek::montgomery::MontgomeryPoint;
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone)]
pub struct Identity {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.device_id())
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}

/// The X25519 public key of `private`.
fn public_of(private: &[u8]) -> Option<[u8; 32]> {
    let k = Zeroizing::new(<[u8; 32]>::try_from(private).ok()?);
    Some(MontgomeryPoint::mul_base_clamped(*k).to_bytes())
}

impl Identity {
    pub fn generate() -> Result<Identity> {
        let kp = snow::Builder::new(crate::NOISE_PARAMS.parse()?).generate_keypair()?;
        Ok(Identity { private: kp.private.to_vec(), public: kp.public.to_vec() })
    }

    /// Load `<state_dir>/identity.key` or create it with mode 0600.
    pub fn load_or_create(state_dir: &Path) -> Result<Identity> {
        fs::create_dir_all(state_dir)?;
        // Database, socket and key: nobody else needs to look inside.
        fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict {}", state_dir.display()))?;
        let path = state_dir.join("identity.key");
        if path.exists() {
            let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                bail!(
                    "{} is readable by others (mode {mode:o}); run `chmod 600 {}` if this is your file",
                    path.display(),
                    path.display()
                );
            }
            let s = Zeroizing::new(fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?);
            let mut lines = s.lines();
            let private = hex::decode(lines.next().unwrap_or_default().trim())?;
            let public = hex::decode(lines.next().unwrap_or_default().trim())?;
            let id = Identity { private, public };
            if id.private.len() != 32 || id.public.len() != 32 {
                bail!("corrupt identity file {}", path.display());
            }
            if public_of(&id.private).as_ref().map(|p| &p[..]) != Some(&id.public[..]) {
                bail!("identity file {}: public key does not belong to the private key", path.display());
            }
            exclude_from_backup(&path);
            return Ok(id);
        }
        let id = Identity::generate()?;
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        let text = Zeroizing::new(format!("{}\n{}\n", hex::encode(&id.private), hex::encode(&id.public)));
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        exclude_from_backup(&path);
        Ok(id)
    }

    pub fn device_id(&self) -> String {
        crate::device_id(&self.public)
    }
}

/// Keep the private key out of Time Machine (a restored Mac pairs anew).
fn exclude_from_backup(path: &Path) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("tmutil")
            .arg("addexclusion")
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_and_private() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let a = Identity::load_or_create(&state).unwrap();
        let b = Identity::load_or_create(&state).unwrap();
        assert_eq!(a.public, b.public);
        assert_eq!(a.device_id().len(), 16);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state.join("identity.key")), 0o600);
        assert_eq!(mode(&state), 0o700);
        assert_eq!(public_of(&a.private).unwrap()[..], a.public[..], "generated pair is consistent");
    }

    #[test]
    fn readable_file_is_refused() {
        let d = tempfile::tempdir().unwrap();
        Identity::load_or_create(d.path()).unwrap();
        let path = d.path().join("identity.key");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let e = Identity::load_or_create(d.path()).unwrap_err().to_string();
        assert!(e.contains("readable by others"), "{e}");
    }

    #[test]
    fn tampered_file_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(d.path()).unwrap();
        let path = d.path().join("identity.key");
        // Someone swaps the public key for another one.
        let other = Identity::generate().unwrap();
        fs::write(&path, format!("{}\n{}\n", hex::encode(&a.private), hex::encode(&other.public))).unwrap();
        let e = Identity::load_or_create(d.path()).unwrap_err().to_string();
        assert!(e.contains("does not belong"), "{e}");
        fs::write(&path, "zz\n").unwrap();
        assert!(Identity::load_or_create(d.path()).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn key_is_excluded_from_time_machine() {
        let d = tempfile::tempdir().unwrap();
        Identity::load_or_create(d.path()).unwrap();
        let out =
            std::process::Command::new("tmutil").arg("isexcluded").arg(d.path().join("identity.key")).output().unwrap();
        let out = String::from_utf8_lossy(&out.stdout);
        assert!(out.contains("[Excluded]"), "{out}");
    }
}
