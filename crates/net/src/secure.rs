//! Noise-encrypted, mutually authenticated message stream over TCP.
//!
//! Wire format: every Noise message is framed as `u16 BE length ‖ ciphertext`
//! (≤ 65 535 bytes). A logical message is `u32 BE length ‖ flag ‖ body`
//! split across as many Noise messages as needed; `flag = 1` marks a zstd
//! compressed body.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use macsync_core::proto::limits::MAX_MESSAGE;

use crate::identity::Identity;

const NOISE_MAX: usize = 65_535;
const TAG: usize = 16;
const PLAIN_MAX: usize = NOISE_MAX - TAG;
const COMPRESS_MIN: usize = 4096;

pub struct SecureStream {
    stream: TcpStream,
    noise: snow::TransportState,
    remote_static: Vec<u8>,
}

fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    let mut buf = Vec::with_capacity(data.len() + 2);
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
    stream.write_all(&buf)?;
    Ok(())
}

fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn configure(stream: &TcpStream, timeout: Duration) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(())
}

fn builder(identity: &Identity) -> Result<snow::Builder<'_>> {
    Ok(snow::Builder::new(crate::NOISE_PARAMS.parse()?).local_private_key(&identity.private)?)
}

impl SecureStream {
    /// Run the XX handshake as initiator.
    pub fn connect(mut stream: TcpStream, identity: &Identity, timeout: Duration) -> Result<SecureStream> {
        configure(&stream, timeout)?;
        let prologue = crate::prologue();
        let mut hs = builder(identity)?.prologue(&prologue)?.build_initiator()?;
        let mut buf = vec![0u8; NOISE_MAX];
        let mut payload = vec![0u8; NOISE_MAX];
        let n = hs.write_message(&[], &mut buf)?; // -> e
        write_frame(&mut stream, &buf[..n])?;
        let msg = read_frame(&mut stream).context("handshake: no response")?; // <- e, ee, s, es
        hs.read_message(&msg, &mut payload).context("handshake failed")?;
        let n = hs.write_message(&[], &mut buf)?; // -> s, se
        write_frame(&mut stream, &buf[..n])?;
        let remote_static = hs.get_remote_static().context("no remote static key")?.to_vec();
        Ok(SecureStream { stream, noise: hs.into_transport_mode()?, remote_static })
    }

    /// Run the XX handshake as responder.
    pub fn accept(mut stream: TcpStream, identity: &Identity, timeout: Duration) -> Result<SecureStream> {
        configure(&stream, timeout)?;
        let prologue = crate::prologue();
        let mut hs = builder(identity)?.prologue(&prologue)?.build_responder()?;
        let mut buf = vec![0u8; NOISE_MAX];
        let mut payload = vec![0u8; NOISE_MAX];
        let msg = read_frame(&mut stream)?;
        hs.read_message(&msg, &mut payload).context("handshake failed")?;
        let n = hs.write_message(&[], &mut buf)?;
        write_frame(&mut stream, &buf[..n])?;
        let msg = read_frame(&mut stream)?;
        hs.read_message(&msg, &mut payload).context("handshake failed")?;
        let remote_static = hs.get_remote_static().context("no remote static key")?.to_vec();
        Ok(SecureStream { stream, noise: hs.into_transport_mode()?, remote_static })
    }

    pub fn remote_static(&self) -> &[u8] {
        &self.remote_static
    }

    pub fn peer_addr(&self) -> Option<std::net::SocketAddr> {
        self.stream.peer_addr().ok()
    }

    pub fn set_timeout(&self, t: Duration) -> Result<()> {
        configure(&self.stream, t)
    }

    pub fn send(&mut self, payload: &[u8]) -> Result<()> {
        let mut body = Vec::with_capacity(payload.len() + 1);
        let compressed = if payload.len() >= COMPRESS_MIN { zstd::encode_all(payload, 1).ok() } else { None };
        match compressed {
            Some(c) if c.len() < payload.len() - payload.len() / 20 => {
                body.push(1);
                body.extend_from_slice(&c);
            }
            _ => {
                body.push(0);
                body.extend_from_slice(payload);
            }
        }
        if body.len() > MAX_MESSAGE {
            bail!("message too large ({} bytes)", body.len());
        }
        let mut plain = Vec::with_capacity(body.len() + 4);
        plain.extend_from_slice(&(body.len() as u32).to_be_bytes());
        plain.extend_from_slice(&body);
        let mut out = vec![0u8; NOISE_MAX];
        let mut wire = Vec::with_capacity(plain.len() + plain.len() / PLAIN_MAX * 18 + 18);
        for chunk in plain.chunks(PLAIN_MAX) {
            let n = self.noise.write_message(chunk, &mut out)?;
            wire.extend_from_slice(&(n as u16).to_be_bytes());
            wire.extend_from_slice(&out[..n]);
        }
        self.stream.write_all(&wire)?;
        Ok(())
    }

    pub fn recv(&mut self) -> Result<Vec<u8>> {
        let mut plain = vec![0u8; NOISE_MAX];
        let frame = read_frame(&mut self.stream)?;
        let n = self.noise.read_message(&frame, &mut plain).context("decrypt")?;
        if n < 4 {
            bail!("short message");
        }
        let total = u32::from_be_bytes(plain[..4].try_into().expect("4 bytes")) as usize;
        if total > MAX_MESSAGE || total == 0 {
            bail!("invalid message length {total}");
        }
        let mut body = Vec::with_capacity(total);
        body.extend_from_slice(&plain[4..n]);
        while body.len() < total {
            let frame = read_frame(&mut self.stream)?;
            let n = self.noise.read_message(&frame, &mut plain).context("decrypt")?;
            body.extend_from_slice(&plain[..n]);
        }
        if body.len() != total {
            bail!("message length mismatch");
        }
        match body[0] {
            0 => Ok(body.split_off(1)),
            1 => {
                let mut out = Vec::new();
                zstd::Decoder::new(&body[1..])?.take(MAX_MESSAGE as u64 + 1).read_to_end(&mut out)?;
                if out.len() > MAX_MESSAGE {
                    bail!("decompressed message too large");
                }
                Ok(out)
            }
            f => bail!("unknown message flag {f}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn roundtrip_small_large_compressible() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server_id = Identity::generate().unwrap();
        let client_id = Identity::generate().unwrap();
        let sid = server_id.clone();
        let cpub = client_id.public.clone();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut ss = SecureStream::accept(s, &sid, Duration::from_secs(5)).unwrap();
            assert_eq!(ss.remote_static(), &cpub[..]);
            for _ in 0..3 {
                let m = ss.recv().unwrap();
                ss.send(&m).unwrap();
            }
        });
        let mut c =
            SecureStream::connect(TcpStream::connect(addr).unwrap(), &client_id, Duration::from_secs(5)).unwrap();
        assert_eq!(c.remote_static(), &server_id.public[..]);
        let random: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for m in [b"hi".to_vec(), vec![7u8; 1_000_000], random] {
            c.send(&m).unwrap();
            assert_eq!(c.recv().unwrap(), m);
        }
        t.join().unwrap();
    }
}
