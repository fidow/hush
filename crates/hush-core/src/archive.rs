//! Encrypted history archive.
//!
//! Message history cannot be handed to a new device by the server without
//! breaking end-to-end encryption, so instead each device re-encrypts every
//! message it sends or receives under a key only the user holds, and uploads
//! the result. Signing in on a new device and supplying the history
//! passphrase restores the full conversation; the server only ever sees
//! opaque blobs.
//!
//! - Key: Argon2id(passphrase, per-account salt) → 32 bytes.
//! - Entry: XChaCha20-Poly1305, blob = base64(nonce ‖ ciphertext).
//!
//! Both primitives are symmetric, so this layer is already quantum-resistant
//! (Grover only halves the effective key size, leaving 128-bit security).

use anyhow::{anyhow, bail, Context, Result};
use argon2::Argon2;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::db::StoredMessage;

/// One archived message, as stored inside the encrypted blob.
#[derive(Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub id: String,
    pub contact: String,
    pub mine: bool,
    pub kind: String,
    pub text: String,
    pub created_at: i64,
}

impl From<&StoredMessage> for ArchiveEntry {
    fn from(m: &StoredMessage) -> Self {
        Self {
            id: m.id.clone(),
            contact: m.contact.clone(),
            mine: m.mine,
            kind: m.kind.clone(),
            text: m.text.clone(),
            created_at: m.created_at,
        }
    }
}

impl From<ArchiveEntry> for StoredMessage {
    fn from(e: ArchiveEntry) -> Self {
        Self {
            id: e.id,
            contact: e.contact,
            mine: e.mine,
            kind: e.kind,
            text: e.text,
            created_at: e.created_at,
        }
    }
}

/// The symmetric key protecting the history archive.
#[derive(Clone)]
pub struct ArchiveKey([u8; 32]);

impl ArchiveKey {
    /// Derives the key from the user's history passphrase and the salt the
    /// server keeps for the account.
    pub fn derive(passphrase: &str, salt_b64: &str) -> Result<Self> {
        let salt = B64
            .decode(salt_b64)
            .context("la sal del historial no es válida")?;
        if salt.len() < 8 {
            bail!("la sal del historial no es válida");
        }
        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
            .map_err(|e| anyhow!("no se pudo derivar la clave de historial: {e}"))?;
        Ok(Self(key))
    }

    /// Restores a key previously saved on this device.
    pub fn from_b64(raw: &str) -> Result<Self> {
        let bytes = B64.decode(raw).context("clave de historial corrupta")?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("clave de historial corrupta"))?;
        Ok(Self(key))
    }

    pub fn to_b64(&self) -> String {
        B64.encode(self.0)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }

    pub fn encrypt_entry(&self, entry: &ArchiveEntry) -> Result<String> {
        let plaintext = serde_json::to_vec(entry)?;
        let mut nonce_bytes = [0u8; 24];
        crate::engine::os_rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher()
            .encrypt(nonce, plaintext.as_slice())
            .map_err(|_| anyhow!("no se pudo cifrar la entrada de historial"))?;
        let mut blob = nonce_bytes.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(B64.encode(blob))
    }

    pub fn decrypt_entry(&self, blob: &str) -> Result<ArchiveEntry> {
        let raw = B64.decode(blob).context("entrada de historial corrupta")?;
        if raw.len() < 24 {
            bail!("entrada de historial corrupta");
        }
        let (nonce_bytes, ciphertext) = raw.split_at(24);
        let plaintext = self
            .cipher()
            .decrypt(XNonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| anyhow!("frase de historial incorrecta"))?;
        Ok(serde_json::from_slice(&plaintext)?)
    }
}

/// A fresh random salt for a new account, base64-encoded.
pub fn new_salt() -> String {
    let mut salt = [0u8; 16];
    crate::engine::os_rng().fill_bytes(&mut salt);
    B64.encode(salt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> ArchiveEntry {
        ArchiveEntry {
            id: "1".into(),
            contact: "bob".into(),
            mine: true,
            kind: "text".into(),
            text: "hola bob".into(),
            created_at: 1234,
        }
    }

    #[test]
    fn roundtrip_with_right_passphrase() {
        let salt = new_salt();
        let key = ArchiveKey::derive("frase de historial", &salt).unwrap();
        let blob = key.encrypt_entry(&entry()).unwrap();
        assert!(!blob.contains("hola"), "el blob no debe filtrar el mensaje");

        // A different device deriving the same key can read it back.
        let same = ArchiveKey::derive("frase de historial", &salt).unwrap();
        assert_eq!(same.decrypt_entry(&blob).unwrap().text, "hola bob");
    }

    #[test]
    fn wrong_passphrase_fails() {
        let salt = new_salt();
        let blob = ArchiveKey::derive("correcta", &salt)
            .unwrap()
            .encrypt_entry(&entry())
            .unwrap();
        let wrong = ArchiveKey::derive("incorrecta", &salt).unwrap();
        assert!(wrong.decrypt_entry(&blob).is_err());
    }
}
