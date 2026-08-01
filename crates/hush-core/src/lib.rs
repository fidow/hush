//! Hush core: protocol, crypto, and client logic shared across platforms.

pub use libsignal_protocol;

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
