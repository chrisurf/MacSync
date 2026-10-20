//! Bonjour/mDNS discovery (`_reposync._tcp.local.`). Best effort: when
//! multicast is unavailable (guest Wi-Fi with client isolation) a manually
//! configured address is used instead.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE: &str = "_reposync._tcp.local.";

pub struct Advertiser {
    daemon: ServiceDaemon,
}

impl Advertiser {
    pub fn start(device: &str, port: u16) -> Result<Advertiser> {
        let daemon = ServiceDaemon::new()?;
        let host = format!("reposync-{device}.local.");
        let props = [("id", device), ("v", "1")];
        let info = ServiceInfo::new(SERVICE, device, &host, "", port, &props[..])?.enable_addr_auto();
        daemon.register(info)?;
        Ok(Advertiser { daemon })
    }
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
            let mut addrs: Vec<_> =
                info.addresses.iter().map(|a| a.to_ip_addr()).filter(crate::lan::is_local).collect();
            // Prefer IPv4 (simpler routing on typical home networks).
            addrs.sort_by_key(|a| !a.is_ipv4());
            if let Some(ip) = addrs.first() {
                found = Some(SocketAddr::new(*ip, info.port));
                break;
            }
        }
    }
    let _ = daemon.shutdown();
    Ok(found)
}
