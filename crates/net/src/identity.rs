//! The device identity: a static X25519 key pair used for Noise.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{bail, Context, Result};

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

impl Identity {
    pub fn generate() -> Result<Identity> {
        let kp = snow::Builder::new(crate::NOISE_PARAMS.parse()?).generate_keypair()?;
        Ok(Identity { private: kp.private.to_vec(), public: kp.public.to_vec() })
    }

    /// Load `<state_dir>/identity.key` or create it with mode 0600.
    pub fn load_or_create(state_dir: &Path) -> Result<Identity> {
        let path = state_dir.join("identity.key");
        if path.exists() {
            let s = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let mut lines = s.lines();
            let private = hex::decode(lines.next().unwrap_or_default().trim())?;
            let public = hex::decode(lines.next().unwrap_or_default().trim())?;
            if private.len() != 32 || public.len() != 32 {
                bail!("corrupt identity file {}", path.display());
            }
            return Ok(Identity { private, public });
        }
        fs::create_dir_all(state_dir)?;
        let id = Identity::generate()?;
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        writeln!(f, "{}\n{}", hex::encode(&id.private), hex::encode(&id.public))?;
        f.sync_all()?;
        Ok(id)
    }

    pub fn device_id(&self) -> String {
        crate::device_id(&self.public)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn persisted_and_private() {
        let d = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(d.path()).unwrap();
        let b = Identity::load_or_create(d.path()).unwrap();
        assert_eq!(a.public, b.public);
        assert_eq!(a.device_id().len(), 16);
        let mode = fs::metadata(d.path().join("identity.key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
