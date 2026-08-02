//! Encrypted history archive.
//!
//! Message history cannot be handed to a new device by the server without
//! breaking end-to-end encryption, so instead each device re-encrypts every
//! message it sends or receives under a key only the user holds, and uploads
//! the result. Restoring elsewhere needs nothing but the recovery key.
//!
//! The key is 32 random bytes generated on the device at registration — not
//! derived from a passphrase, so it cannot be weak or guessed — shown to the
//! user on request as a grouped base32 code. Entries are sealed with
//! XChaCha20-Poly1305, which is symmetric and therefore already
//! quantum-resistant (Grover only halves the effective key size).

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use data_encoding::BASE32_NOPAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::db::StoredMessage;

/// Characters per group in the printed recovery code.
const GROUP: usize = 4;

/// One archived message, as stored inside the encrypted blob.
#[derive(Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub id: String,
    pub contact: String,
    pub mine: bool,
    pub kind: String,
    pub text: String,
    #[serde(default = "default_state")]
    pub state: String,
    pub created_at: i64,
}

/// Entries archived before delivery states existed.
fn default_state() -> String {
    "sent".to_string()
}

impl From<&StoredMessage> for ArchiveEntry {
    fn from(m: &StoredMessage) -> Self {
        Self {
            id: m.id.clone(),
            contact: m.contact.clone(),
            mine: m.mine,
            kind: m.kind.clone(),
            text: m.text.clone(),
            state: m.state.clone(),
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
            state: e.state,
            created_at: e.created_at,
        }
    }
}

/// The symmetric key protecting the history archive.
#[derive(Clone)]
pub struct ArchiveKey([u8; 32]);

impl ArchiveKey {
    /// A fresh key for a new account.
    pub fn generate() -> Self {
        let mut key = [0u8; 32];
        crate::engine::os_rng().fill_bytes(&mut key);
        Self(key)
    }

    /// The key as the user sees it: uppercase base32 in dash-separated
    /// groups, an alphabet without lowercase so it survives being written
    /// down and retyped.
    pub fn to_recovery_code(&self) -> String {
        let encoded = BASE32_NOPAD.encode(&self.0);
        encoded
            .as_bytes()
            .chunks(GROUP)
            .map(|c| std::str::from_utf8(c).expect("base32 is ascii"))
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parses a recovery code, ignoring dashes, spaces and letter case.
    pub fn from_recovery_code(code: &str) -> Result<Self> {
        let cleaned: String = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let bytes = BASE32_NOPAD
            .decode(cleaned.as_bytes())
            .context("invalid_recovery_key")?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("invalid_recovery_key"))?;
        Ok(Self(key))
    }

    /// Restores a key previously saved on this device.
    pub fn from_b64(raw: &str) -> Result<Self> {
        let bytes = B64.decode(raw).context("invalid_recovery_key")?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("invalid_recovery_key"))?;
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
            .map_err(|_| anyhow!("cannot encrypt history entry"))?;
        let mut blob = nonce_bytes.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(B64.encode(blob))
    }

    pub fn decrypt_entry(&self, blob: &str) -> Result<ArchiveEntry> {
        let raw = B64.decode(blob).context("corrupt history entry")?;
        if raw.len() < 24 {
            bail!("corrupt history entry");
        }
        let (nonce_bytes, ciphertext) = raw.split_at(24);
        let plaintext = self
            .cipher()
            .decrypt(XNonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| anyhow!("wrong_recovery_key"))?;
        Ok(serde_json::from_slice(&plaintext)?)
    }
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
            state: "sent".into(),
            created_at: 1234,
        }
    }

    #[test]
    fn roundtrip_through_the_recovery_code() {
        let key = ArchiveKey::generate();
        let blob = key.encrypt_entry(&entry()).unwrap();
        assert!(!blob.contains("hola"), "el blob no debe filtrar el mensaje");

        // Another device restores from the printed code alone.
        let code = key.to_recovery_code();
        let restored = ArchiveKey::from_recovery_code(&code).unwrap();
        assert_eq!(restored.decrypt_entry(&blob).unwrap().text, "hola bob");
    }

    #[test]
    fn codes_survive_retyping() {
        let code = ArchiveKey::generate().to_recovery_code();
        let messy = format!(" {} ", code.to_lowercase().replace('-', " "));
        assert_eq!(
            ArchiveKey::from_recovery_code(&messy).unwrap().to_b64(),
            ArchiveKey::from_recovery_code(&code).unwrap().to_b64()
        );
    }

    #[test]
    fn wrong_key_cannot_read_the_archive() {
        let blob = ArchiveKey::generate().encrypt_entry(&entry()).unwrap();
        assert!(ArchiveKey::generate().decrypt_entry(&blob).is_err());
        assert!(ArchiveKey::from_recovery_code("not-a-key").is_err());
    }
}
