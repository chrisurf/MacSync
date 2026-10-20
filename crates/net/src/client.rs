//! Protocol client: a remote engine reachable over the network.

use std::net::{SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reposync_core::proto::{DirListing, Request, Response, UnitRoot};
use reposync_core::{Entry, Hash, PROTOCOL_VERSION};
use reposync_engine::PeerApi;
use serde::{Deserialize, Serialize};

use crate::identity::Identity;
use crate::secure::SecureStream;

/// What a device tells its peer about itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloInfo {
    pub device: String,
    pub name: String,
    pub arch: String,
    pub root: String,
}

impl HelloInfo {
    pub fn local(identity: &Identity, name: &str, root: &str) -> HelloInfo {
        HelloInfo {
            device: identity.device_id(),
            name: name.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            root: root.to_string(),
        }
    }

    pub fn request(&self) -> Request {
        Request::Hello {
            device: self.device.clone(),
            name: self.name.clone(),
            protocol: PROTOCOL_VERSION,
            arch: self.arch.clone(),
            root: self.root.clone(),
        }
    }

    pub fn response(&self) -> Response {
        Response::Hello {
            device: self.device.clone(),
            name: self.name.clone(),
            protocol: PROTOCOL_VERSION,
            arch: self.arch.clone(),
            root: self.root.clone(),
        }
    }
}

pub struct NetPeer {
    addr: SocketAddr,
    identity: Identity,
    expected: Vec<u8>,
    hello: HelloInfo,
    conn: Mutex<Option<SecureStream>>,
    remote: Mutex<Option<HelloInfo>>,
    timeout: Duration,
}

impl NetPeer {
    /// `expected` is the pinned static public key of the peer.
    pub fn new(addr: SocketAddr, identity: Identity, expected: Vec<u8>, hello: HelloInfo) -> NetPeer {
        NetPeer {
            addr,
            identity,
            expected,
            hello,
            conn: Mutex::new(None),
            remote: Mutex::new(None),
            timeout: Duration::from_secs(120),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Information the peer sent in its Hello (after the first connection).
    pub fn remote_hello(&self) -> Option<HelloInfo> {
        self.remote.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn connect(&self) -> Result<SecureStream> {
        if !crate::lan::is_local(&self.addr.ip()) {
            bail!("refusing to connect to non-local address {}", self.addr);
        }
        let tcp = TcpStream::connect_timeout(&self.addr, Duration::from_secs(5))
            .with_context(|| format!("connect {}", self.addr))?;
        let mut s = SecureStream::connect(tcp, &self.identity, self.timeout)?;
        if s.remote_static() != self.expected.as_slice() {
            bail!("peer at {} presented an unknown key (device {})", self.addr, crate::device_id(s.remote_static()));
        }
        s.send(&bincode::serialize(&self.hello.request())?)?;
        match bincode::deserialize::<Response>(&s.recv()?)? {
            Response::Hello { device, name, protocol, arch, root } => {
                if protocol != PROTOCOL_VERSION {
                    bail!("peer speaks protocol {protocol}, expected {PROTOCOL_VERSION}");
                }
                *self.remote.lock().unwrap_or_else(|e| e.into_inner()) = Some(HelloInfo { device, name, arch, root });
            }
            Response::Error(e) => bail!("peer refused: {e}"),
            other => bail!("unexpected handshake response {other:?}"),
        }
        Ok(s)
    }

    /// Send one request; reconnects once on transport errors (all requests are
    /// idempotent).
    pub fn call(&self, req: &Request) -> Result<Response> {
        let payload = bincode::serialize(req)?;
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(self.connect()?);
            }
            let s = guard.as_mut().expect("connected");
            let r = s.send(&payload).and_then(|_| s.recv());
            match r {
                Ok(bytes) => {
                    let resp: Response = bincode::deserialize(&bytes)?;
                    if let Response::Error(e) = resp {
                        return Err(anyhow!("peer error: {e}"));
                    }
                    return Ok(resp);
                }
                Err(e) if attempt == 0 => {
                    tracing::debug!(error = %e, "reconnecting");
                    *guard = None;
                }
                Err(e) => {
                    *guard = None;
                    return Err(e);
                }
            }
        }
        unreachable!()
    }

    pub fn ping(&self) -> Result<()> {
        match self.call(&Request::Ping)? {
            Response::Pong => Ok(()),
            r => bail!("unexpected {r:?}"),
        }
    }

    pub fn notify_changed(&self, units: Vec<String>) -> Result<()> {
        self.call(&Request::Changed { units })?;
        Ok(())
    }

    pub fn disconnect(&self) {
        *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn unexpected<T>(r: Response) -> Result<T> {
    Err(anyhow!("unexpected response {r:?}"))
}

impl PeerApi for NetPeer {
    fn roots(&self) -> Result<Vec<UnitRoot>> {
        match self.call(&Request::Roots)? {
            Response::Roots(r) => Ok(r),
            r => unexpected(r),
        }
    }

    fn tree(&self, unit: &str, dirs: &[String]) -> Result<Vec<DirListing>> {
        match self.call(&Request::Tree { unit: unit.into(), dirs: dirs.to_vec() })? {
            Response::Tree(t) => Ok(t),
            r => unexpected(r),
        }
    }

    fn entries(&self, unit: &str, paths: &[String], prefixes: &[String]) -> Result<Vec<Entry>> {
        match self.call(&Request::Entries { unit: unit.into(), paths: paths.to_vec(), prefixes: prefixes.to_vec() })? {
            Response::Entries(e) => Ok(e),
            r => unexpected(r),
        }
    }

    fn blobs(&self, hashes: &[Hash]) -> Result<Vec<Option<Vec<u8>>>> {
        match self.call(&Request::Blobs { hashes: hashes.to_vec() })? {
            Response::Blobs(b) => Ok(b),
            r => unexpected(r),
        }
    }

    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        match self.call(&Request::BlobRange { hash: *hash, offset, len })? {
            Response::BlobRange(b) => Ok(b),
            r => unexpected(r),
        }
    }
}
