//! Which network this Mac is in, recognised by the hardware address of the
//! default gateway (no location permission needed, unlike the Wi-Fi name).
//! With `[peer] hide_in_unknown_networks` the daemon neither listens nor
//! announces itself in networks where it never reached the peer.

use std::process::{Command, Stdio};

use anyhow::Result;
use macsync_store::Store;

const KNOWN_KEY: &str = "known_networks";
const MAX_KNOWN: usize = 64;

/// Id of the current network: a hash of the gateway's hardware address, so
/// the database does not hold the address itself. `None` without a gateway.
/// `MACSYNC_NETWORK_ID` overrides it (tests).
pub fn current() -> Option<String> {
    if let Ok(id) = std::env::var("MACSYNC_NETWORK_ID") {
        return Some(id).filter(|s| !s.is_empty());
    }
    let gw = parse_gateway(&run("route", &["-n", "get", "default"])?)?;
    let mac = parse_arp(&run("arp", &["-n", &gw])?)?;
    Some(id_of(&mac))
}

pub fn id_of(mac: &str) -> String {
    hex::encode(&blake3::derive_key("macsync-network-v1", mac.to_ascii_lowercase().as_bytes())[..8])
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `gateway: 192.168.1.1` from `route -n get default` (an IP, not `link#…`).
fn parse_gateway(route: &str) -> Option<String> {
    let gw = route.lines().find_map(|l| l.trim().strip_prefix("gateway:"))?.trim();
    gw.parse::<std::net::IpAddr>().ok().map(|_| gw.to_string())
}

/// `? (192.168.1.1) at a4:2b:b0:12:34:56 on en0 …` from `arp -n`.
fn parse_arp(arp: &str) -> Option<String> {
    let mac = arp.split_whitespace().skip_while(|w| *w != "at").nth(1)?;
    (mac.split(':').count() == 6).then(|| mac.to_string())
}

pub fn known(store: &Store) -> Result<Vec<String>> {
    Ok(store
        .kv(KNOWN_KEY)?
        .map(|s| s.split(',').filter(|x| !x.is_empty()).map(String::from).collect())
        .unwrap_or_default())
}

pub fn is_known(store: &Store, id: &str) -> Result<bool> {
    Ok(known(store)?.iter().any(|k| k == id))
}

/// Remember `id` as a network where the peer is met. Returns true if new.
pub fn remember(store: &Store, id: &str) -> Result<bool> {
    let mut k = known(store)?;
    if k.iter().any(|x| x == id) {
        return Ok(false);
    }
    k.push(id.to_string());
    if k.len() > MAX_KNOWN {
        k.remove(0);
    }
    store.set_kv(KNOWN_KEY, &k.join(","))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_route_and_arp() {
        let route = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(parse_gateway(route).as_deref(), Some("192.168.1.1"));
        assert_eq!(parse_gateway("    gateway: link#14\n"), None);
        let arp = "? (192.168.1.1) at a4:2b:b0:12:34:56 on en0 ifscope [ethernet]\n";
        assert_eq!(parse_arp(arp).as_deref(), Some("a4:2b:b0:12:34:56"));
        assert_eq!(parse_arp("? (192.168.1.1) at (incomplete) on en0\n"), None);
        assert_eq!(id_of("A4:2B:B0:12:34:56"), id_of("a4:2b:b0:12:34:56"));
        assert_eq!(id_of("a4:2b:b0:12:34:56").len(), 16);
    }

    #[test]
    fn remembers_networks() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(&d.path().join("db")).unwrap();
        assert!(!is_known(&store, "n1").unwrap());
        assert!(remember(&store, "n1").unwrap());
        assert!(!remember(&store, "n1").unwrap());
        assert!(remember(&store, "n2").unwrap());
        assert!(is_known(&store, "n1").unwrap() && is_known(&store, "n2").unwrap());
        for i in 0..100 {
            remember(&store, &format!("x{i}")).unwrap();
        }
        assert_eq!(known(&store).unwrap().len(), MAX_KNOWN);
    }
}
