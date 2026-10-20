//! One-time pairing with a 6-digit code (ADR-0006).
//!
//! Both sides run symmetric SPAKE2 with the code as password. From the shared
//! secret each side derives a MAC key and authenticates its static public key
//! and device information. A wrong code yields different secrets, so the MACs
//! fail; the listener allows at most three attempts.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};

use crate::client::HelloInfo;
use crate::identity::Identity;

const CONTEXT: &[u8] = b"reposync-pair-v1";
const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairResult {
    pub pubkey: Vec<u8>,
    pub info: HelloInfo,
    pub addr: SocketAddr,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    pubkey: Vec<u8>,
    info: HelloInfo,
    protocol: u32,
}

/// Random 6-digit code.
pub fn generate_code() -> String {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("OS random number generator");
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
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

fn mac(key: &[u8; 32], role: &[u8], payload: &[u8]) -> blake3::Hash {
    let mut h = blake3::Hasher::new_keyed(key);
    h.update(CONTEXT);
    h.update(role);
    h.update(payload);
    h.finalize()
}

fn spake(s: &mut TcpStream, code: &str, first: bool) -> Result<[u8; 32]> {
    let (state, msg) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.as_bytes()), &SpakeId::new(CONTEXT));
    let other = if first {
        send(s, &msg)?;
        recv(s)?
    } else {
        let o = recv(s)?;
        send(s, &msg)?;
        o
    };
    let secret = state.finish(&other).map_err(|e| anyhow::anyhow!("pairing failed: {e:?}"))?;
    Ok(blake3::derive_key("reposync pairing mac v1", &secret))
}

fn encode(identity: &Identity, info: &HelloInfo) -> Result<Vec<u8>> {
    Ok(bincode::serialize(&Payload {
        pubkey: identity.public.clone(),
        info: info.clone(),
        protocol: reposync_core::PROTOCOL_VERSION,
    })?)
}

fn decode_verified(key: &[u8; 32], role: &[u8], msg: &[u8]) -> Result<Payload> {
    if msg.len() < 32 {
        bail!("short pairing message");
    }
    let (payload, tag) = msg.split_at(msg.len() - 32);
    let tag: [u8; 32] = tag.try_into().expect("32 bytes");
    if mac(key, role, payload) != blake3::Hash::from(tag) {
        bail!("wrong pairing code");
    }
    let p: Payload = bincode::deserialize(payload)?;
    if p.pubkey.len() != 32 || p.info.device != crate::device_id(&p.pubkey) {
        bail!("inconsistent pairing payload");
    }
    Ok(p)
}

fn signed(key: &[u8; 32], role: &[u8], payload: Vec<u8>) -> Vec<u8> {
    let tag = mac(key, role, &payload);
    let mut out = payload;
    out.extend_from_slice(tag.as_bytes());
    out
}

/// Wait for a peer to pair using `code`. Returns the peer's identity.
pub fn listen(
    listener: &TcpListener,
    code: &str,
    identity: &Identity,
    info: &HelloInfo,
    timeout: Duration,
) -> Result<PairResult> {
    let deadline = Instant::now() + timeout;
    listener.set_nonblocking(true)?;
    let mut attempts = 0;
    while attempts < MAX_ATTEMPTS {
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
        s.set_nonblocking(false)?;
        s.set_read_timeout(Some(Duration::from_secs(20)))?;
        attempts += 1;
        let r: Result<PairResult> = (|| {
            let key = spake(&mut s, code, false)?;
            let theirs = decode_verified(&key, b"C", &recv(&mut s)?);
            match theirs {
                Ok(p) => {
                    send(&mut s, &signed(&key, b"L", encode(identity, info)?))?;
                    Ok(PairResult { pubkey: p.pubkey, info: p.info, addr })
                }
                Err(e) => {
                    let _ = send(&mut s, b"");
                    Err(e)
                }
            }
        })();
        match r {
            Ok(p) => return Ok(p),
            Err(e) => tracing::warn!(%addr, attempt = attempts, error = %e, "pairing attempt failed"),
        }
    }
    bail!("pairing failed after {MAX_ATTEMPTS} attempts")
}

/// Pair with a listening peer at `addr` using `code`.
pub fn connect(addr: SocketAddr, code: &str, identity: &Identity, info: &HelloInfo) -> Result<PairResult> {
    if !crate::lan::is_local(&addr.ip()) {
        bail!("refusing to pair with non-local address {addr}");
    }
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).with_context(|| format!("connect {addr}"))?;
    s.set_read_timeout(Some(Duration::from_secs(20)))?;
    let key = spake(&mut s, code, true)?;
    send(&mut s, &signed(&key, b"C", encode(identity, info)?))?;
    let reply = recv(&mut s)?;
    if reply.is_empty() {
        bail!("wrong pairing code");
    }
    let p = decode_verified(&key, b"L", &reply)?;
    Ok(PairResult { pubkey: p.pubkey, info: p.info, addr })
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
        let t = std::thread::spawn(move || listen(&l, listen_code, &a, &ia, Duration::from_secs(10)));
        let mut results = Vec::new();
        for c in connect_codes {
            results.push(connect(addr, c, &b, &ib));
            if results.last().unwrap().is_ok() {
                break;
            }
        }
        (t.join().unwrap(), results)
    }

    #[test]
    fn correct_code_pairs() {
        let (l, c) = run("123456", &["123456"]);
        let l = l.unwrap();
        let c = c[0].as_ref().unwrap();
        assert_eq!(l.info.name, "Mac B");
        assert_eq!(c.info.name, "Mac A");
        assert_eq!(c.info.device, crate::device_id(&c.pubkey));
    }

    #[test]
    fn wrong_code_fails_and_attempts_are_limited() {
        let (l, c) = run("123456", &["000000", "111111", "222222"]);
        assert!(c.iter().all(|r| r.is_err()));
        assert!(l.is_err());
    }

    #[test]
    fn second_try_with_right_code_succeeds() {
        let (l, c) = run("654321", &["000000", "654321"]);
        assert!(c[0].is_err());
        assert!(c[1].is_ok());
        assert!(l.is_ok());
    }

    #[test]
    fn codes_have_six_digits() {
        for _ in 0..20 {
            let c = generate_code();
            assert_eq!(c.len(), 6);
            assert!(c.chars().all(|ch| ch.is_ascii_digit()));
        }
    }
}
