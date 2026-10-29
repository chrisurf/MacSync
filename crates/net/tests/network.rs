//! Full synchronisation between two engines over Noise/TCP on loopback.

use std::sync::Arc;

use macsync_engine::{Engine, EngineConfig, PeerApi, PullOptions};
use macsync_net::client::HelloInfo;
use macsync_net::{Identity, NetPeer, Server};

fn start(dir: &std::path::Path, name: &str) -> (Arc<Engine>, Identity) {
    let id = Identity::load_or_create(&dir.join(format!("{name}-state"))).unwrap();
    let cfg = EngineConfig::new(dir.join(format!("{name}-root")), dir.join(format!("{name}-state")), id.device_id());
    (Arc::new(Engine::open(cfg).unwrap()), id)
}

fn serve(engine: Arc<Engine>, id: &Identity, allowed: Vec<u8>) -> Server {
    serve_on("127.0.0.1:0", engine, id, allowed)
}

fn serve_on(listen: &str, engine: Arc<Engine>, id: &Identity, allowed: Vec<u8>) -> Server {
    let hello = HelloInfo::local(id, "srv", "/");
    Server::start(listen.parse().unwrap(), id.clone(), hello, allowed, Arc::new(move |_peer, req| engine.handle(req)))
        .unwrap()
}

#[test]
fn sync_over_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let (a, ida) = start(dir.path(), "a");
    let (b, idb) = start(dir.path(), "b");
    std::fs::create_dir_all(a.root().join("docs/sub")).unwrap();
    std::fs::write(a.root().join("docs/sub/x.txt"), "over the wire").unwrap();
    let big: Vec<u8> = (0..9_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(a.root().join("docs/big.bin"), &big).unwrap();
    for i in 0..500 {
        std::fs::write(a.root().join(format!("docs/sub/s{i}.js")), format!("export const v = {i};")).unwrap();
    }
    a.scan_all(false).unwrap();
    b.scan_all(false).unwrap();

    let server = serve(a.clone(), &ida, idb.public.clone());
    let peer = NetPeer::new(server.local_addr(), idb.clone(), ida.public.clone(), HelloInfo::local(&idb, "b", "/"));
    peer.ping().unwrap();
    let reports = b.sync_from(&peer, None, PullOptions::default()).unwrap();
    let docs = reports.iter().find(|r| r.unit == "docs").unwrap();
    assert!(docs.converged, "{docs:?}");
    assert_eq!(std::fs::read(b.root().join("docs/big.bin")).unwrap(), big);
    assert_eq!(std::fs::read_to_string(b.root().join("docs/sub/x.txt")).unwrap(), "over the wire");
    assert_eq!(peer.remote_hello().unwrap().device, ida.device_id());
    // Roots are equal on both sides.
    let ra = a.local_roots().unwrap();
    let rb = peer.roots().unwrap();
    assert_eq!(ra, rb);
    server.stop();
}

#[test]
fn unpaired_and_impostor_keys_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (a, ida) = start(dir.path(), "a");
    let (_b, idb) = start(dir.path(), "b");
    let stranger = Identity::generate().unwrap();
    let server = serve(a, &ida, idb.public.clone());

    // A stranger is not paired with A.
    let p =
        NetPeer::new(server.local_addr(), stranger.clone(), ida.public.clone(), HelloInfo::local(&stranger, "x", "/"));
    assert!(p.ping().is_err());

    // B expects A's key but the server presents another one (impostor).
    let impostor = Identity::generate().unwrap();
    let (c, _) = start(dir.path(), "c");
    let fake = serve(c, &impostor, idb.public.clone());
    let p = NetPeer::new(fake.local_addr(), idb.clone(), ida.public.clone(), HelloInfo::local(&idb, "b", "/"));
    let err = format!("{:#}", p.ping().unwrap_err());
    assert!(err.contains("did not complete the handshake"), "{err}");

    // The genuine pairing works.
    let p = NetPeer::new(server.local_addr(), idb.clone(), ida.public.clone(), HelloInfo::local(&idb, "b", "/"));
    p.ping().unwrap();
}

#[test]
fn public_addresses_are_refused() {
    let id = Identity::generate().unwrap();
    let p = NetPeer::new("8.8.8.8:47123".parse().unwrap(), id.clone(), id.public.clone(), HelloInfo::default());
    let err = p.ping().unwrap_err().to_string();
    assert!(err.contains("non-local"), "{err}");
}

/// This machine's LAN address (no packet is sent), so a test can connect
/// from an address other than the loopback one.
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.168.0.1:9").ok()?;
    Some(s.local_addr().ok()?.ip()).filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
}

/// 200 idle or junk connections at once (from the LAN address) neither stop
/// the server nor keep the paired Mac (on loopback) out, and none of them
/// gets a single byte back.
#[test]
fn connection_flood_does_not_lock_out_the_peer() {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let (a, ida) = start(dir.path(), "a");
    let (_b, idb) = start(dir.path(), "b");
    let server = serve_on("0.0.0.0:0", a, &ida, idb.public.clone());
    let port = server.local_addr().port();
    let Some(attacker) = lan_ip() else {
        eprintln!("no LAN address; skipping");
        return;
    };
    let addr = SocketAddr::new(attacker, port);
    let threads: Vec<_> = (0..200)
        .map(|i| {
            std::thread::spawn(move || {
                let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else { return 0 };
                if i % 2 == 0 {
                    // A junk handshake frame.
                    let _ = s.write_all(&[0, 48]).and_then(|_| s.write_all(&[7u8; 48]));
                }
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut answer = Vec::new();
                let _ = s.read_to_end(&mut answer);
                answer.len()
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let p = NetPeer::new(addr, idb.clone(), ida.public.clone(), HelloInfo::local(&idb, "b", "/"));
    p.ping().unwrap();
    for t in threads {
        assert_eq!(t.join().unwrap(), 0, "a stranger got an answer");
    }
    p.disconnect();
    p.ping().unwrap();
    server.stop();
}

/// A slow transfer on one connection (as over Wi-Fi) does not hold up
/// requests on a sibling connection; on the same connection they wait.
#[test]
fn sibling_connection_does_not_wait_behind_a_transfer() {
    use macsync_core::proto::{Request, Response};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let (a, ida) = start(dir.path(), "a");
    let (_b, idb) = start(dir.path(), "b");
    let hello = HelloInfo::local(&ida, "srv", "/");
    let slow = Arc::new(move |_peer: &_, req: Request| {
        if matches!(req, Request::Blobs { .. }) {
            std::thread::sleep(Duration::from_millis(1500));
            return Response::Blobs(Vec::new());
        }
        a.handle(req)
    });
    let server = Server::start("127.0.0.1:0".parse().unwrap(), ida.clone(), hello, idb.public.clone(), slow).unwrap();
    let main = Arc::new(NetPeer::new(
        server.local_addr(),
        idb.clone(),
        ida.public.clone(),
        HelloInfo::local(&idb, "b", "/"),
    ));
    main.ping().unwrap();
    let side = main.sibling();
    side.ping().unwrap();

    let timed = |p: &NetPeer| {
        let m = main.clone();
        let t = std::thread::spawn(move || m.blobs(&[macsync_core::Hash::ZERO]).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        let start = Instant::now();
        p.roots().unwrap();
        let d = start.elapsed();
        t.join().unwrap();
        d
    };
    let on_side = timed(&side);
    assert!(on_side < Duration::from_millis(500), "sibling waited {on_side:?}");
    let on_main = timed(&main);
    assert!(on_main > Duration::from_millis(1000), "same connection did not wait ({on_main:?})");
    server.stop();
}

/// A large transfer uses several connections at once and still arrives
/// bit for bit.
#[test]
fn large_transfer_runs_over_several_connections() {
    use macsync_core::proto::Request;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let (a, ida) = start(dir.path(), "a");
    let (b, idb) = start(dir.path(), "b");
    std::fs::create_dir_all(a.root().join("big/small")).unwrap();
    let large: Vec<u8> = (0..120_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    std::fs::write(a.root().join("big/large.bin"), &large).unwrap();
    for i in 0..400 {
        let data: Vec<u8> = (0..200_000u32).map(|j| (j ^ i) as u8).collect();
        std::fs::write(a.root().join(format!("big/small/f{i}.bin")), data).unwrap();
    }
    a.scan_all(false).unwrap();
    b.scan_all(false).unwrap();

    let busy = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (busy2, most2, a2) = (busy.clone(), most.clone(), a.clone());
    let handler = Arc::new(move |_peer: &_, req: Request| {
        let content = matches!(req, Request::Blobs { .. } | Request::BlobRange { .. });
        if content {
            let n = busy2.fetch_add(1, Ordering::SeqCst) + 1;
            most2.fetch_max(n, Ordering::SeqCst);
            // Like Wi-Fi: each answer takes a moment.
            std::thread::sleep(Duration::from_millis(20));
        }
        let r = a2.handle(req);
        if content {
            busy2.fetch_sub(1, Ordering::SeqCst);
        }
        r
    });
    let hello = HelloInfo::local(&ida, "srv", "/");
    let server = Server::start("127.0.0.1:0".parse().unwrap(), ida.clone(), hello, idb.public.clone(), handler).unwrap();
    let peer = NetPeer::new(server.local_addr(), idb.clone(), ida.public.clone(), HelloInfo::local(&idb, "b", "/"));
    let reports = b.sync_from(&peer, None, PullOptions::default()).unwrap();
    let r = reports.iter().find(|r| r.unit == "big").unwrap();
    assert!(r.converged, "{r:?}");
    assert_eq!(std::fs::read(b.root().join("big/large.bin")).unwrap(), large);
    assert_eq!(a.local_roots().unwrap(), b.local_roots().unwrap());
    let most = most.load(Ordering::SeqCst);
    assert!(most >= 3, "at most {most} content requests at once");
    server.stop();
}
