//! LAN-only enforcement: RepoSync never talks to public addresses.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use anyhow::{Context, Result};

/// True for loopback, RFC 1918, link-local, CGNAT-free private ranges and
/// IPv6 ULA/link-local addresses.
pub fn is_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local(&IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

/// Resolves `host:port`, preferring IPv4: the daemon and the pairing
/// listener bind `0.0.0.0`, while `name.local` often resolves to an IPv6
/// link-local address first.
pub fn resolve(addr: &str) -> Result<SocketAddr> {
    let mut all: Vec<SocketAddr> = addr.to_socket_addrs().with_context(|| format!("cannot resolve {addr}"))?.collect();
    all.sort_by_key(|a| !a.is_ipv4());
    all.into_iter().next().with_context(|| format!("cannot resolve {addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        for ok in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.20",
            "169.254.3.4",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.0.1",
        ] {
            assert!(is_local(&ok.parse().unwrap()), "{ok}");
        }
        for bad in ["8.8.8.8", "172.32.0.1", "1.1.1.1", "2001:4860::8888", "100.64.0.1", "::ffff:8.8.8.8"] {
            assert!(!is_local(&bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn resolve_prefers_ipv4() {
        assert!(resolve("localhost:47123").unwrap().is_ipv4());
        assert_eq!(resolve("[::1]:1").unwrap(), "[::1]:1".parse().unwrap());
    }
}
