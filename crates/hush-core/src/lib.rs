//! Hush core: protocol, crypto, and client logic shared across platforms.
//!
//! - [`engine::Engine`]: identity, PQXDH (X25519 + Kyber-1024) session
//!   establishment and Double Ratchet messaging via libsignal.
//! - [`api::ApiClient`]: HTTP client for the Hush relay server.

pub mod api;
pub mod client;
pub mod db;
pub mod engine;
pub mod keystore;
pub mod store;
pub mod transfer;

pub use api::{ApiClient, ContactEntry, IncomingMessage, RemoteProfile, ServerEvent};
pub use client::{ClientEvent, DecryptedMessage, HushClient, ProfileInfo};
pub use db::{LocalDb, Profile, StoredMessage};
pub use engine::Engine;

#[cfg(test)]
mod tests {
    use libsignal_protocol::IdentityKeyPair;

    #[test]
    fn generate_identity_keypair() {
        let mut rng = rand::rng();
        let pair = IdentityKeyPair::generate(&mut rng);
        assert_eq!(pair.public_key().serialize().len(), 33);
    }
}
