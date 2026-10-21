//! Protocol server: accepts Noise connections from paired peers only.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reposync_core::proto::{Request, Response};
use reposync_core::PROTOCOL_VERSION;

use crate::client::HelloInfo;
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
/// Returns true if the given static public key belongs to a paired peer.
pub type Authorizer = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

pub struct Server {
    local: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

impl Server {
    pub fn start(
        listen: SocketAddr,
        identity: Identity,
        hello: HelloInfo,
        authorize: Authorizer,
        handler: Handler,
    ) -> Result<Server> {
        let listener = TcpListener::bind(listen).with_context(|| format!("bind {listen}"))?;
        let local = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new().name("reposync-server".into()).spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = conn else { continue };
                let (identity, hello, authorize, handler) =
                    (identity.clone(), hello.clone(), authorize.clone(), handler.clone());
                let _ = std::thread::Builder::new().name("reposync-conn".into()).spawn(move || {
                    if let Err(e) = serve_conn(stream, &identity, &hello, &authorize, &handler) {
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

fn serve_conn(
    stream: TcpStream,
    identity: &Identity,
    hello: &HelloInfo,
    authorize: &Authorizer,
    handler: &Handler,
) -> Result<()> {
    let addr = stream.peer_addr()?;
    if !crate::lan::is_local(&addr.ip()) {
        bail!("refusing non-local peer {addr}");
    }
    let mut s = SecureStream::accept(stream, identity, Duration::from_secs(10))?;
    let pubkey = s.remote_static().to_vec();
    if !authorize(&pubkey) {
        tracing::warn!(%addr, device = %crate::device_id(&pubkey), "rejected unpaired peer");
        bail!("unpaired peer");
    }
    s.set_timeout(IDLE_TIMEOUT)?;
    // The first message must be Hello.
    let first: Request = reposync_core::codec::decode(&s.recv()?)?;
    let Request::Hello { device, name, protocol, arch, root, version } = first else { bail!("expected Hello") };
    if protocol != PROTOCOL_VERSION {
        let r = Response::Error(format!("protocol {protocol} not supported (expected {PROTOCOL_VERSION})"));
        s.send(&reposync_core::codec::encode(&r)?)?;
        bail!("protocol mismatch");
    }
    if device != crate::device_id(&pubkey) {
        bail!("device id does not match key");
    }
    s.send(&reposync_core::codec::encode(&hello.response())?)?;
    let info =
        PeerInfo { device: device.clone(), pubkey, addr, hello: HelloInfo { device, name, arch, root, version } };
    loop {
        let msg = match s.recv() {
            Ok(m) => m,
            Err(_) => return Ok(()), // closed or idle
        };
        let req: Request = reposync_core::codec::decode(&msg).context("decode request")?;
        let resp = handler(&info, req);
        s.send(&reposync_core::codec::encode(&resp)?)?;
    }
}
