//! Bonjour/mDNS discovery. Best effort: when multicast is unavailable (guest
//! Wi-Fi with client isolation) a manually configured address is used instead.
//!
//! * `_macsync._tcp.local.` – a running daemon. It does not announce its
//!   device id but a *blinded* id that changes every hour and that only the
//!   paired Mac can compute (see [`Blinding`]), so others on the network
//!   cannot recognise the Mac over time (ADR-0012).
//! * `_macsync-pair._tcp.local.` – a Mac waiting with a pairing code, so the
//!   other Mac can pair without knowing its name. Only during the pairing
//!   window, with a short name.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use curve25519_dalek::montgomery::MontgomeryPoint;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use zeroize::Zeroizing;

use crate::identity::Identity;

pub const SERVICE: &str = "_macsync._tcp.local.";
pub const PAIR_SERVICE: &str = "_macsync-pair._tcp.local.";

const BLIND_CONTEXT: &[u8] = b"macsync-mdns-v1";

/// Hours since the Unix epoch: the rotation period of blinded ids.
pub fn current_hour() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 3600).unwrap_or(0)
}

/// Blinded announcement ids for one pair of Macs. The key is an X25519
/// Diffie-Hellman of our private and the peer's public key, so both Macs
/// derive the same key and nobody else can.
pub struct Blinding {
    key: Zeroizing<[u8; 32]>,
    own: Vec<u8>,
    peer: Vec<u8>,
}

impl Blinding {
    pub fn new(identity: &Identity, peer_public: &[u8]) -> Result<Blinding> {
        let (Ok(private), Ok(public)) =
            (<[u8; 32]>::try_from(identity.private.as_slice()), <[u8; 32]>::try_from(peer_public))
        else {
            bail!("invalid key length");
        };
        let private = Zeroizing::new(private);
        let shared = Zeroizing::new(MontgomeryPoint(public).mul_clamped(*private).to_bytes());
        if shared.iter().all(|b| *b == 0) {
            bail!("invalid peer key");
        }
        let key = Zeroizing::new(blake3::derive_key("macsync-mdns-v1 key", &shared[..]));
        Ok(Blinding { key, own: identity.public.clone(), peer: peer_public.to_vec() })
    }

    fn id(&self, announcer: &[u8], hour: u64) -> String {
        let mut h = blake3::Hasher::new_keyed(&self.key);
        h.update(BLIND_CONTEXT);
        h.update(announcer);
        h.update(&hour.to_be_bytes());
        hex::encode(&h.finalize().as_bytes()[..8])
    }

    /// The id this Mac announces in `hour`.
    pub fn own_id(&self, hour: u64) -> String {
        self.id(&self.own, hour)
    }

    /// Ids the peer may announce around `hour`: the previous and the current
    /// hour, and the next one for clocks that are a little ahead.
    pub fn peer_ids(&self, hour: u64) -> [String; 3] {
        [self.id(&self.peer, hour.saturating_sub(1)), self.id(&self.peer, hour), self.id(&self.peer, hour + 1)]
    }
}

pub struct Advertiser {
    daemon: ServiceDaemon,
    stop: Arc<AtomicBool>,
}

impl Advertiser {
    /// Announce the protocol port under the blinded id; re-announces when the
    /// hour changes.
    pub fn start(blinding: Blinding, port: u16) -> Result<Advertiser> {
        let daemon = ServiceDaemon::new()?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut hour = current_hour();
        let mut fullname = register(&daemon, SERVICE, &blinding.own_id(hour), port, &[("v", "2")])?;
        let (d2, s2) = (daemon.clone(), stop.clone());
        std::thread::Builder::new().name("macsync-mdns".into()).spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(1));
                let now = current_hour();
                if now == hour {
                    continue;
                }
                hour = now;
                let _ = d2.unregister(&fullname);
                match register(&d2, SERVICE, &blinding.own_id(hour), port, &[("v", "2")]) {
                    Ok(f) => fullname = f,
                    Err(e) => tracing::warn!(error = %e, "mDNS re-announcement failed"),
                }
            }
        })?;
        Ok(Advertiser { daemon, stop })
    }

    /// Announce that this Mac waits for pairing on `port` (the pairing port).
    /// `name` is shown on the other Mac; keep it short (see [`short_name`]).
    pub fn start_pairing(device: &str, name: &str, port: u16) -> Result<Advertiser> {
        let daemon = ServiceDaemon::new()?;
        register(&daemon, PAIR_SERVICE, device, port, &[("id", device), ("name", name), ("v", "2")])?;
        Ok(Advertiser { daemon, stop: Arc::new(AtomicBool::new(true)) })
    }
}

/// Register `instance` (also used as neutral host name); returns the full name.
fn register(daemon: &ServiceDaemon, ty: &str, instance: &str, port: u16, props: &[(&str, &str)]) -> Result<String> {
    let host = format!("macsync-{instance}.local.");
    let info = ServiceInfo::new(ty, instance, &host, "", port, props)?.enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    daemon.register(info)?;
    Ok(fullname)
}

/// The part of a computer name that does not name its owner:
/// "Chris’ MacBook Pro" and "MacBook Pro von Chris" → "MacBook Pro".
pub fn short_name(device_name: &str) -> String {
    let n = device_name.trim();
    for sep in [" von ", " de ", " di "] {
        if let Some((model, _)) = n.split_once(sep) {
            return model.trim().to_string();
        }
    }
    for sep in ["’s ", "'s ", "’ ", "' "] {
        if let Some((_, model)) = n.split_once(sep) {
            return model.trim().to_string();
        }
    }
    n.to_string()
}

/// A Mac that waits for pairing.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PairCandidate {
    pub device: String,
    pub name: String,
    /// Pairing address (IPv4 preferred).
    pub addr: SocketAddr,
}

/// Macs currently waiting for pairing, except `own_device`.
pub fn browse_pairing(own_device: Option<&str>, timeout: Duration) -> Result<Vec<PairCandidate>> {
    let daemon = ServiceDaemon::new()?;
    let rx = daemon.browse(PAIR_SERVICE)?;
    let deadline = Instant::now() + timeout;
    let mut found: Vec<PairCandidate> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(ev) = rx.recv_timeout(left) else { break };
        let ServiceEvent::ServiceResolved(info) = ev else { continue };
        let Some(device) = info.txt_properties.get_property_val_str("id").map(str::to_string) else { continue };
        if Some(device.as_str()) == own_device || found.iter().any(|c| c.device == device) {
            continue;
        }
        let Some(ip) = best_ip(info.addresses.iter().map(|a| a.to_ip_addr())) else { continue };
        let name = info.txt_properties.get_property_val_str("name").unwrap_or(&device).to_string();
        found.push(PairCandidate { device, name, addr: SocketAddr::new(ip, info.port) });
    }
    let _ = daemon.shutdown();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// First local address: LAN before loopback (an announcement can carry
/// 127.0.0.1, which would point to ourselves), IPv4 before IPv6 (simpler
/// routing on home networks, and the listeners bind `0.0.0.0`).
fn best_ip(addrs: impl Iterator<Item = std::net::IpAddr>) -> Option<std::net::IpAddr> {
    let mut v: Vec<_> = addrs.filter(crate::lan::is_local).collect();
    v.sort_by_key(|a| (a.is_loopback(), !a.is_ipv4()));
    v.into_iter().next()
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.daemon.shutdown();
    }
}

/// Look for the paired peer by its blinded id; returns its first local address.
pub fn find(blinding: &Blinding, timeout: Duration) -> Result<Option<SocketAddr>> {
    let ids = blinding.peer_ids(current_hour());
    let daemon = ServiceDaemon::new()?;
    let rx = daemon.browse(SERVICE)?;
    let deadline = Instant::now() + timeout;
    let mut found = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(ev) = rx.recv_timeout(left) else { break };
        if let ServiceEvent::ServiceResolved(info) = ev {
            let instance = info.fullname.strip_suffix(&format!(".{SERVICE}")).unwrap_or("");
            if !ids.iter().any(|id| id == instance) {
                continue;
            }
            if let Some(ip) = best_ip(info.addresses.iter().map(|a| a.to_ip_addr())) {
                found = Some(SocketAddr::new(ip, info.port));
                break;
            }
        }
    }
    let _ = daemon.shutdown();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_local_ipv4() {
        let ips = ["8.8.8.8", "fe80::1", "192.168.1.20"].map(|s| s.parse().unwrap());
        assert_eq!(best_ip(ips.into_iter()), Some("192.168.1.20".parse().unwrap()));
        assert_eq!(best_ip(["8.8.8.8".parse().unwrap()].into_iter()), None);
        let ips = ["127.0.0.1", "fe80::1", "192.168.1.20"].map(|s| s.parse().unwrap());
        assert_eq!(best_ip(ips.into_iter()), Some("192.168.1.20".parse().unwrap()));
    }

    #[test]
    fn blinded_ids_match_only_for_the_pair_and_rotate() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let stranger = Identity::generate().unwrap();
        let ab = Blinding::new(&a, &b.public).unwrap();
        let ba = Blinding::new(&b, &a.public).unwrap();
        let h = current_hour();
        // B finds what A announces, in this and the neighbouring hours.
        assert!(ba.peer_ids(h).contains(&ab.own_id(h)));
        assert!(ba.peer_ids(h).contains(&ab.own_id(h - 1)));
        assert!(ba.peer_ids(h).contains(&ab.own_id(h + 1)));
        assert!(!ba.peer_ids(h).contains(&ab.own_id(h + 2)));
        // Not its own announcement, and not stable over time.
        assert!(!ba.peer_ids(h).contains(&ba.own_id(h)));
        assert_ne!(ab.own_id(h), ab.own_id(h + 1));
        // Unrelated to the device id, and a stranger computes something else.
        assert_ne!(ab.own_id(h), a.device_id());
        // A stranger who knows both public keys still cannot predict A's id.
        let guess = Blinding::new(&stranger, &a.public).unwrap();
        assert!(!guess.peer_ids(h).contains(&ab.own_id(h)));
        assert_eq!(ab.own_id(h).len(), 16);
        assert!(Blinding::new(&a, &[0u8; 32]).is_err());
    }

    #[test]
    fn short_names_drop_the_owner() {
        assert_eq!(short_name("Chris’ MacBook Pro"), "MacBook Pro");
        assert_eq!(short_name("Anna's iMac"), "iMac");
        assert_eq!(short_name("MacBook Air von Chris"), "MacBook Air");
        assert_eq!(short_name("Studio"), "Studio");
    }

    /// Announce blinded and find it from the peer's side. Needs multicast;
    /// ignored in CI.
    #[test]
    #[ignore]
    fn blinded_roundtrip_on_lan() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let _adv = Advertiser::start(Blinding::new(&a, &b.public).unwrap(), 47998).unwrap();
        let found = find(&Blinding::new(&b, &a.public).unwrap(), Duration::from_secs(8)).unwrap();
        assert_eq!(found.map(|a| a.port()), Some(47998));
        // A third Mac paired with someone else does not take it for its peer.
        let c = Identity::generate().unwrap();
        assert_eq!(find(&Blinding::new(&c, &b.public).unwrap(), Duration::from_secs(3)).unwrap(), None);
    }

    /// Advertise and browse on this machine. Needs multicast; ignored in CI.
    #[test]
    #[ignore]
    fn pairing_roundtrip_on_lan() {
        let _a = Advertiser::start_pairing("testdevice00001", "Test Mac", 47999).unwrap();
        let found = browse_pairing(Some("other"), Duration::from_secs(5)).unwrap();
        let c = found.iter().find(|c| c.device == "testdevice00001").expect("found");
        assert_eq!((c.name.as_str(), c.addr.port()), ("Test Mac", 47999));
        assert!(browse_pairing(Some("testdevice00001"), Duration::from_secs(3))
            .unwrap()
            .iter()
            .all(|c| c.device != "testdevice00001"));
    }
}
