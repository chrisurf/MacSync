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
    let hello = HelloInfo::local(id, "srv", "/");
    Server::start(
        "127.0.0.1:0".parse().unwrap(),
        id.clone(),
        hello,
        Arc::new(move |k| k == allowed.as_slice()),
        Arc::new(move |_peer, req| engine.handle(req)),
    )
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
    let err = p.ping().unwrap_err().to_string();
    assert!(err.contains("unknown key"), "{err}");

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
