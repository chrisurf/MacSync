//! Protocol server: accepts Noise connections from the paired peer only.
//!
//! Anyone on the network can open a TCP connection, so the server bounds what
//! that costs: at most [`MAX_HANDSHAKES`] connections in their handshake
//! (the oldest gives way), [`HANDSHAKE_TIMEOUT`], a short ban for addresses
//! whose handshakes keep failing, and at most [`MAX_PAIRED`] authenticated
//! connections.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use macsync_core::proto::{Request, Response};
use macsync_core::PROTOCOL_VERSION;

use crate::client::HelloInfo;
use crate::guard::{Bans, Handshakes};
use crate::identity::Identity;
use crate::secure::SecureStream;

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub device: String,
    pub pubkey: Vec<u8>,
    pub addr: SocketAddr,
    pub hello: HelloInfo,
}

pub type Handler = Arc<dyn Fn(&PeerInfo, Request) -> Response + Send + Sync>;

/// Connections in their handshake at the same time.
pub const MAX_HANDSHAKES: usize = 4;
/// Authenticated connections at the same time.
pub const MAX_PAIRED: usize = 4;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Server {
    local: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct Limits {
    handshakes: Handshakes,
    bans: Bans,
    paired: AtomicUsize,
}

/// Decrements the paired-connection count on drop.
struct PairedSlot<'a>(&'a AtomicUsize);

impl Drop for PairedSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Server {
    /// Listen for the peer with the static key `peer` (from pairing).
    pub fn start(
        listen: SocketAddr,
        identity: Identity,
        hello: HelloInfo,
        peer: Vec<u8>,
        handler: Handler,
    ) -> Result<Server> {
        let listener = TcpListener::bind(listen).with_context(|| format!("bind {listen}"))?;
        let local = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let limits =
            Arc::new(Limits { handshakes: Handshakes::new(MAX_HANDSHAKES), bans: Bans::default(), paired: 0.into() });
        let peer: Arc<[u8]> = peer.into();
        let thread = std::thread::Builder::new().name("macsync-server".into()).spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = conn else { continue };
                let Ok(addr) = stream.peer_addr() else { continue };
                if !crate::lan::is_local(&addr.ip()) || limits.bans.banned(addr.ip()) {
                    continue; // dropped: closed without a word
                }
                let Ok(slot) = limits.handshakes.admit(&stream) else { continue };
                let (identity, hello, peer, handler, limits) =
                    (identity.clone(), hello.clone(), peer.clone(), handler.clone(), limits.clone());
                let _ = std::thread::Builder::new().name("macsync-conn".into()).spawn(move || {
                    let s = SecureStream::accept(stream, &identity, &peer, HANDSHAKE_TIMEOUT);
                    drop(slot);
                    let s = match s {
                        Ok(s) => s,
                        Err(e) => {
                            if limits.bans.fail(addr.ip()) {
                                tracing::warn!(%addr, "too many failed handshakes; ignoring this address for a minute");
                            }
                            tracing::debug!(%addr, error = %format!("{e:#}"), "handshake failed");
                            return;
                        }
                    };
                    limits.bans.succeed(addr.ip());
                    if limits.paired.fetch_add(1, Ordering::SeqCst) >= MAX_PAIRED {
                        limits.paired.fetch_sub(1, Ordering::SeqCst);
                        tracing::warn!(%addr, "too many connections from the peer");
                        return;
                    }
                    let _paired = PairedSlot(&limits.paired);
                    if let Err(e) = serve_conn(s, addr, &hello, &handler) {
                        tracing::debug!(error = %format!("{e:#}"), "connection closed");
                    }
                });
            }
        })?;
        Ok(Server { local, stop, thread: Some(thread) })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept.
        let wake = if self.local.ip().is_unspecified() {
            SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), self.local.port())
        } else {
            self.local
        };
        let _ = TcpStream::connect_timeout(&wake, Duration::from_millis(200));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown();
        }
    }
}

fn serve_conn(mut s: SecureStream, addr: SocketAddr, hello: &HelloInfo, handler: &Handler) -> Result<()> {
    let pubkey = s.remote_static().to_vec();
    s.set_timeout(IDLE_TIMEOUT)?;
    // The first message must be Hello.
    let first: Request = macsync_core::codec::decode(&s.recv()?)?;
    let Request::Hello { device, name, protocol, arch, root, version } = first else { bail!("expected Hello") };
    if protocol != PROTOCOL_VERSION {
        let r = Response::Error(format!("protocol {protocol} not supported (expected {PROTOCOL_VERSION})"));
        s.send(&macsync_core::codec::encode(&r)?)?;
        bail!("protocol mismatch");
    }
    if device != crate::device_id(&pubkey) {
        bail!("device id does not match key");
    }
    s.send(&macsync_core::codec::encode(&hello.response())?)?;
    let info =
        PeerInfo { device: device.clone(), pubkey, addr, hello: HelloInfo { device, name, arch, root, version } };
    loop {
        let msg = match s.recv() {
            Ok(m) => m,
            Err(_) => return Ok(()), // closed or idle
        };
        let req: Request = macsync_core::codec::decode(&msg).context("decode request")?;
        let resp = handler(&info, req);
        s.send(&macsync_core::codec::encode(&resp)?)?;
    }
}
