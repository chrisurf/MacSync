//! Network layer: identities, the Noise-encrypted transport (ADR-0002),
//! SPAKE2 pairing (ADR-0006), LAN-only enforcement, mDNS discovery, and the
//! protocol client/server.

pub mod client;
pub mod discovery;
pub mod identity;
pub mod lan;
pub mod pairing;
pub mod secure;
pub mod server;

pub use client::NetPeer;
pub use identity::Identity;
pub use server::{PeerInfo, Server};

/// Default TCP port.
pub const DEFAULT_PORT: u16 = 47123;

/// Noise protocol pattern.
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Noise prologue: binds the handshake to RepoSync and the protocol version.
pub fn prologue() -> Vec<u8> {
    format!("reposync/{}", reposync_core::PROTOCOL_VERSION).into_bytes()
}

/// Short device id derived from a static public key.
pub fn device_id(pubkey: &[u8]) -> String {
    hex::encode(&blake3::hash(pubkey).as_bytes()[..8])
}
