//! One-time pairing with a 6-digit code (ADR-0006).
//!
//! Both sides run symmetric SPAKE2 with the code as password. From the shared
//! secret each side derives one key per role and sends its static public key
//! and device information encrypted with ChaCha20-Poly1305: a passive
//! listener sees neither names nor paths. A wrong code yields different
//! secrets, so decryption fails. The listener stays open for the right Mac:
//! at most three attempts per address and ten in total.
//!
//! Afterwards both sides show the same [`fingerprint`] of the two keys.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload as Aad};
use chacha20poly1305::ChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};
use zeroize::{Zeroize, Zeroizing};

use crate::client::HelloInfo;
use crate::identity::Identity;

const CONTEXT: &[u8] = b"macsync-pair-v2";
const MAX_ATTEMPTS: u32 = 10;
const MAX_PER_ADDRESS: u32 = 3;
/// The SPAKE2 exchange is automatic; an idle connection must not block the
/// window for long.
const STEP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairResult {
    pub pubkey: Vec<u8>,
    pub info: HelloInfo,
    /// Address the pairing connection came from / went to.
    pub addr: SocketAddr,
    /// Protocol port the peer listens on.
    pub port: u16,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    pubkey: Vec<u8>,
    info: HelloInfo,
    protocol: u32,
    port: u16,
}

/// Random 6-digit code.
pub fn generate_code() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("OS random number generator");
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

/// Four words derived from both public keys, the same on both Macs. If they
/// differ, the code was used with a different Mac.
pub fn fingerprint(a: &[u8], b: &[u8]) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut h = blake3::Hasher::new_derive_key("macsync-pair-v2 fingerprint");
    h.update(lo);
    h.update(hi);
    let d = h.finalize();
    d.as_bytes()[..4].iter().map(|b| crate::words::WORDS[*b as usize]).collect::<Vec<_>>().join(" ")
}

fn send(s: &mut TcpStream, data: &[u8]) -> Result<()> {
    if data.len() > 64 * 1024 {
        bail!("pairing message too large");
    }
    s.write_all(&(data.len() as u32).to_be_bytes())?;
    s.write_all(data)?;
    Ok(())
}

fn recv(s: &mut TcpStream) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > 64 * 1024 {
        bail!("pairing message too large");
    }
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

/// One key per role, so neither side can be fed its own message back.
struct Keys {
    connector: Zeroizing<[u8; 32]>,
    listener: Zeroizing<[u8; 32]>,
}

impl Keys {
    fn get(&self, role: &[u8]) -> &[u8; 32] {
        if role == b"C" {
            &self.connector
        } else {
            &self.listener
        }
    }
}

fn spake(s: &mut TcpStream, code: &str, first: bool) -> Result<Keys> {
    let (state, msg) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.as_bytes()), &SpakeId::new(CONTEXT));
    let other = if first {
        send(s, &msg)?;
        recv(s)?
    } else {
        let o = recv(s)?;
        send(s, &msg)?;
        o
    };
    let mut secret = state.finish(&other).map_err(|e| anyhow::anyhow!("pairing failed: {e:?}"))?;
    let keys = Keys {
        connector: Zeroizing::new(blake3::derive_key("macsync-pair-v2 connector", &secret)),
        listener: Zeroizing::new(blake3::derive_key("macsync-pair-v2 listener", &secret)),
    };
    secret.zeroize();
    Ok(keys)
}

fn encode(identity: &Identity, info: &HelloInfo, port: u16) -> Result<Vec<u8>> {
    Ok(macsync_core::codec::encode(&Payload {
        pubkey: identity.public.clone(),
        info: info.clone(),
        protocol: macsync_core::PROTOCOL_VERSION,
        port,
    })?)
}

/// Each key encrypts exactly one message, so a fixed nonce is safe.
const NONCE: [u8; 12] = [0; 12];

fn seal(keys: &Keys, role: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
    let aad = [CONTEXT, role].concat();
    ChaCha20Poly1305::new(keys.get(role).into())
        .encrypt(&NONCE.into(), Aad { msg: payload, aad: &aad })
        .map_err(|_| anyhow::anyhow!("encrypt pairing message"))
}

fn open(keys: &Keys, role: &[u8], msg: &[u8]) -> Result<Payload> {
    let aad = [CONTEXT, role].concat();
    let plain = Zeroizing::new(
        ChaCha20Poly1305::new(keys.get(role).into())
            .decrypt(&NONCE.into(), Aad { msg, aad: &aad })
            .map_err(|_| anyhow::anyhow!("wrong pairing code"))?,
    );
    let p: Payload = macsync_core::codec::decode(&plain)?;
    if p.pubkey.len() != 32 || p.info.device != crate::device_id(&p.pubkey) {
        bail!("inconsistent pairing payload");
    }
    if p.protocol != macsync_core::PROTOCOL_VERSION {
        bail!("peer speaks protocol {}, expected {}", p.protocol, macsync_core::PROTOCOL_VERSION);
    }
    Ok(p)
}

/// Attempts in one pairing window: per address and in total.
#[derive(Default)]
struct Attempts {
    total: u32,
    per_address: HashMap<IpAddr, u32>,
}

impl Attempts {
    fn exhausted(&self) -> bool {
        self.total >= MAX_ATTEMPTS
    }

    fn admit(&mut self, ip: IpAddr) -> bool {
        let n = self.per_address.entry(ip).or_default();
        if *n >= MAX_PER_ADDRESS || self.total >= MAX_ATTEMPTS {
            return false;
        }
        *n += 1;
        self.total += 1;
        true
    }
}

/// Wait for a peer to pair using `code`. `port` is our protocol port.
/// Returns the peer's identity.
pub fn listen(
    listener: &TcpListener,
    code: &str,
    identity: &Identity,
    info: &HelloInfo,
    port: u16,
    timeout: Duration,
) -> Result<PairResult> {
    let deadline = Instant::now() + timeout;
    listener.set_nonblocking(true)?;
    let mut attempts = Attempts::default();
    while !attempts.exhausted() {
        if Instant::now() > deadline {
            bail!("pairing timed out");
        }
        let (mut s, addr) = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if !crate::lan::is_local(&addr.ip()) {
            tracing::warn!(%addr, "pairing attempt from non-local address rejected");
            continue;
        }
        if !attempts.admit(addr.ip()) {
            continue; // closed without a word
        }
        s.set_nonblocking(false)?;
        s.set_read_timeout(Some(STEP_TIMEOUT))?;
        s.set_write_timeout(Some(STEP_TIMEOUT))?;
        let r: Result<PairResult> = (|| {
            let keys = spake(&mut s, code, false)?;
            match open(&keys, b"C", &recv(&mut s)?) {
                Ok(p) => {
                    send(&mut s, &seal(&keys, b"L", &encode(identity, info, port)?)?)?;
                    Ok(PairResult { pubkey: p.pubkey, info: p.info, addr, port: p.port })
                }
                Err(e) => {
                    let _ = send(&mut s, b"");
                    Err(e)
                }
            }
        })();
        match r {
            Ok(p) => return Ok(p),
            Err(e) => tracing::warn!(%addr, attempt = attempts.total, error = %e, "pairing attempt failed"),
        }
    }
    bail!("pairing failed after {MAX_ATTEMPTS} attempts")
}

/// Pair with a listening peer at `addr` using `code`. `port` is our protocol port.
pub fn connect(addr: SocketAddr, code: &str, identity: &Identity, info: &HelloInfo, port: u16) -> Result<PairResult> {
    if !crate::lan::is_local(&addr.ip()) {
        bail!("refusing to pair with non-local address {addr}");
    }
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).with_context(|| format!("connect {addr}"))?;
    s.set_read_timeout(Some(Duration::from_secs(20)))?;
    let keys = spake(&mut s, code, true)?;
    send(&mut s, &seal(&keys, b"C", &encode(identity, info, port)?)?)?;
    let reply = recv(&mut s).context("no answer (the other Mac allows a few attempts only)")?;
    if reply.is_empty() {
        bail!("wrong pairing code");
    }
    let p = open(&keys, b"L", &reply)?;
    Ok(PairResult { pubkey: p.pubkey, info: p.info, addr, port: p.port })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(listen_code: &'static str, connect_codes: &[&str]) -> (Result<PairResult>, Vec<Result<PairResult>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let ia = HelloInfo::local(&a, "Mac A", "/r");
        let ib = HelloInfo::local(&b, "Mac B", "/r");
        let t = std::thread::spawn(move || listen(&l, listen_code, &a, &ia, 1111, Duration::from_secs(10)));
        let mut results = Vec::new();
        for c in connect_codes {
            results.push(connect(addr, c, &b, &ib, 2222));
            if results.last().unwrap().is_ok() {
                break;
            }
        }
        (t.join().unwrap(), results)
    }

    #[test]
    fn correct_code_pairs_with_the_same_fingerprint() {
        let (l, c) = run("123456", &["123456"]);
        let l = l.unwrap();
        let c = c[0].as_ref().unwrap();
        assert_eq!(l.info.name, "Mac B");
        assert_eq!(c.info.name, "Mac A");
        assert_eq!(c.info.device, crate::device_id(&c.pubkey));
        assert_eq!((l.port, c.port), (2222, 1111));
        // Each side computes it from its own key and the one it received.
        assert_eq!(fingerprint(&l.pubkey, &c.pubkey), fingerprint(&c.pubkey, &l.pubkey));
        assert_eq!(fingerprint(&l.pubkey, &c.pubkey).split(' ').count(), 4);
    }

    #[test]
    fn second_try_with_right_code_succeeds() {
        let (l, c) = run("654321", &["000000", "654321"]);
        assert!(c[0].is_err());
        assert!(c[1].is_ok());
        assert!(l.is_ok());
    }

    /// Nothing readable on the wire: no name, no path, no key.
    #[test]
    fn payload_is_encrypted() {
        let keys = Keys { connector: Zeroizing::new([1; 32]), listener: Zeroizing::new([2; 32]) };
        let id = Identity::generate().unwrap();
        let info = HelloInfo::local(&id, "Chris MacBook", "/Users/chris/Code");
        let plain = encode(&id, &info, 47123).unwrap();
        let wire = seal(&keys, b"C", &plain).unwrap();
        let has = |needle: &[u8]| wire.windows(needle.len()).any(|w| w == needle);
        assert!(!has(b"Chris") && !has(b"/Users/chris") && !has(&id.public[..8]));
        assert_eq!(open(&keys, b"C", &wire).unwrap().info, info);
        // The other role's key (a reflected message) and a modified byte fail.
        assert!(open(&keys, b"L", &wire).is_err());
        let mut bad = wire.clone();
        bad[3] ^= 1;
        assert!(open(&keys, b"C", &bad).is_err());
    }

    #[test]
    fn attempts_per_address_and_in_total() {
        let mut a = Attempts::default();
        let ip = |i: u8| IpAddr::from([192, 168, 1, i]);
        assert!(a.admit(ip(1)) && a.admit(ip(1)) && a.admit(ip(1)));
        assert!(!a.admit(ip(1)), "fourth attempt from one address");
        for i in 2..=8 {
            assert!(a.admit(ip(i)));
        }
        assert!(a.exhausted());
        assert!(!a.admit(ip(9)), "eleventh attempt in total");
    }

    /// Three wrong codes from one address leave the window open for the
    /// right Mac at another address.
    #[test]
    fn a_stranger_does_not_end_the_window() {
        let Some(lan) = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.connect("192.168.0.1:9").map(|_| s))
            .and_then(|s| s.local_addr())
            .ok()
            .map(|a| a.ip())
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified() && crate::lan::is_local(ip))
        else {
            eprintln!("no LAN address; skipping");
            return;
        };
        let l = TcpListener::bind("0.0.0.0:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let a = Identity::generate().unwrap();
        let ia = HelloInfo::local(&a, "Mac A", "/r");
        let t = std::thread::spawn(move || listen(&l, "123456", &a, &ia, 1111, Duration::from_secs(20)));
        let stranger = Identity::generate().unwrap();
        let is = HelloInfo::local(&stranger, "X", "/");
        for _ in 0..4 {
            assert!(connect(SocketAddr::new(lan, port), "000000", &stranger, &is, 1).is_err());
        }
        // Even the right code is refused from the address that used up its tries.
        assert!(connect(SocketAddr::new(lan, port), "123456", &stranger, &is, 1).is_err());
        let b = Identity::generate().unwrap();
        let ib = HelloInfo::local(&b, "Mac B", "/r");
        let ok = connect(SocketAddr::new([127, 0, 0, 1].into(), port), "123456", &b, &ib, 2222).unwrap();
        assert_eq!(ok.info.name, "Mac A");
        assert_eq!(t.join().unwrap().unwrap().info.name, "Mac B");
    }

    #[test]
    fn codes_have_six_digits() {
        for _ in 0..20 {
            let c = generate_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|ch| ch.is_ascii_digit()));
        }
    }

    #[test]
    fn word_list_is_unique() {
        let set: std::collections::HashSet<_> = crate::words::WORDS.iter().collect();
        assert_eq!(set.len(), 256);
    }
}
