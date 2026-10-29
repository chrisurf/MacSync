//! Network layer: identities, the Noise-encrypted transport (ADR-0002),
//! SPAKE2 pairing (ADR-0006), LAN-only enforcement, mDNS discovery, and the
//! protocol client/server.

pub mod client;
pub mod discovery;
pub mod guard;
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

/// Noise protocol pattern. KK: both sides know each other's static key from
/// pairing, so a stranger cannot build a valid first message and gets no
/// answer (ADR-0012).
pub const NOISE_PARAMS: &str = "Noise_KK_25519_ChaChaPoly_BLAKE2s";

/// Noise prologue: binds the handshake to MacSync and the protocol version.
pub fn prologue() -> Vec<u8> {
    format!("macsync/{}", macsync_core::PROTOCOL_VERSION).into_bytes()
}

static BUILD_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set the build version announced to the peer (e.g. `0.1.0 (a1b2c3d4e5)`).
pub fn set_build_version(v: &str) {
    let _ = BUILD_VERSION.set(v.to_string());
}

/// Build version announced in the handshake.
pub fn build_version() -> String {
    BUILD_VERSION.get().cloned().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// Short device id derived from a static public key.
pub fn device_id(pubkey: &[u8]) -> String {
    hex::encode(&blake3::hash(pubkey).as_bytes()[..8])
}
