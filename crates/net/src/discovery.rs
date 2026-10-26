//! Bonjour/mDNS discovery. Best effort: when multicast is unavailable (guest
//! Wi-Fi with client isolation) a manually configured address is used instead.
//!
//! * `_reposync._tcp.local.` – a running daemon, found by device id.
//! * `_reposync-pair._tcp.local.` – a Mac waiting with a pairing code, so the
//!   other Mac can pair without knowing its name.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE: &str = "_reposync._tcp.local.";
pub const PAIR_SERVICE: &str = "_reposync-pair._tcp.local.";

pub struct Advertiser {
    daemon: ServiceDaemon,
}

impl Advertiser {
    pub fn start(device: &str, port: u16) -> Result<Advertiser> {
        Self::register(SERVICE, device, port, &[("id", device), ("v", "1")])
    }

    /// Announce that this Mac waits for pairing on `port` (the pairing port).
    pub fn start_pairing(device: &str, name: &str, port: u16) -> Result<Advertiser> {
        Self::register(PAIR_SERVICE, device, port, &[("id", device), ("name", name), ("v", "1")])
    }

    fn register(ty: &str, device: &str, port: u16, props: &[(&str, &str)]) -> Result<Advertiser> {
        let daemon = ServiceDaemon::new()?;
        let host = format!("reposync-{device}.local.");
        let info = ServiceInfo::new(ty, device, &host, "", port, props)?.enable_addr_auto();
        daemon.register(info)?;
        Ok(Advertiser { daemon })
    }
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
        let _ = self.daemon.shutdown();
    }
}

/// Look for the peer with `device` id; returns its first local address.
pub fn find(device: &str, timeout: Duration) -> Result<Option<SocketAddr>> {
    let daemon = ServiceDaemon::new()?;
    let rx = daemon.browse(SERVICE)?;
    let deadline = Instant::now() + timeout;
    let mut found = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(ev) = rx.recv_timeout(left) else { break };
        if let ServiceEvent::ServiceResolved(info) = ev {
            if info.txt_properties.get_property_val_str("id") != Some(device) {
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
