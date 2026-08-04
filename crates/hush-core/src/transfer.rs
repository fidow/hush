//! Exporting and importing conversations.
//!
//! The server keeps no history: what a device holds is what it received. To
//! move conversations to another device the user exports them to a file and
//! imports it there.
//!
//! The file is the whole of the protection, so it is built to be attacked. It
//! leaves the app already encrypted and travels by whatever means the user
//! picks — a cable, a memory stick, a cloud drive that keeps copies forever —
//! and nothing about it is secret except the password. So:
//!
//! - The password is turned into a key with **Argon2id**, which is memory-hard:
//!   guessing is slow to parallelise on the graphics cards that make short work
//!   of ordinary hashes. The cost is written into the file, so it can be raised
//!   later without making old exports unreadable.
//! - The contents are sealed with **XChaCha20-Poly1305**, which is symmetric
//!   and therefore already quantum-resistant, under a 24-byte random nonce.
//! - The header — including the Argon2 cost — is authenticated along with the
//!   contents, so it cannot be edited down to something cheap to crack.
//!
//! What it cannot fix is a weak password: a memorable one is guessable however
//! expensive each guess is, which is why [`export`] refuses short ones and the
//! interface says so plainly.

use anyhow::{anyhow, bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::db::StoredMessage;

/// Marks the file as ours and pins the layout. A different version number
/// means a different layout, not a corrupt file.
const MAGIC: &[u8; 16] = b"HUSH-CHAT-EXPORT";
const VERSION: u8 = 1;

/// Argon2id cost. Memory is what makes guessing expensive, so it carries the
/// weight; 64 MiB is far above the usual recommendation while still fitting
/// comfortably in a phone's share of memory, which is where an import has to
/// run too. Eight passes over it puts a single guess in the region of a
/// second on a desktop.
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 8;
const LANES: u32 = 1;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
/// Everything before the ciphertext: magic, version, the three costs, salt and
/// nonce. Authenticated as associated data.
const HEADER_LEN: usize = 16 + 1 + 4 + 4 + 4 + SALT_LEN + NONCE_LEN;

/// Shortest password [`export`] will accept. Not a serious defence on its own
/// — it is the Argon2 cost that buys time — but it rules out the passwords
/// that would fall in seconds regardless.
pub const MIN_PASSWORD_LEN: usize = 10;

/// One message, as it travels inside the encrypted file.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedMessage {
    pub id: String,
    pub contact: String,
    pub mine: bool,
    pub kind: String,
    pub text: String,
    #[serde(default = "default_state")]
    pub state: String,
    #[serde(default)]
    pub delivered_at: Option<i64>,
    #[serde(default)]
    pub read_at: Option<i64>,
    pub created_at: i64,
}

fn default_state() -> String {
    "sent".to_string()
}

impl From<&StoredMessage> for ExportedMessage {
    fn from(m: &StoredMessage) -> Self {
        Self {
            id: m.id.clone(),
            contact: m.contact.clone(),
            mine: m.mine,
            kind: m.kind.clone(),
            text: m.text.clone(),
            state: m.state.clone(),
            delivered_at: m.delivered_at,
            read_at: m.read_at,
            created_at: m.created_at,
        }
    }
}

impl From<ExportedMessage> for StoredMessage {
    fn from(e: ExportedMessage) -> Self {
        Self {
            id: e.id,
            contact: e.contact,
            mine: e.mine,
            kind: e.kind,
            text: e.text,
            state: e.state,
            delivered_at: e.delivered_at,
            read_at: e.read_at,
            created_at: e.created_at,
        }
    }
}

/// A contact as it travels, so an import on a device that has never spoken to
/// them still shows a name and a picture rather than a bare username.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedContact {
    pub username: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

/// What the file contains once opened.
#[derive(Debug, Serialize, Deserialize)]
pub struct Export {
    /// Which account the conversations came from. Only shown to the user, so
    /// importing somebody else's file is at least noticeable.
    #[serde(default)]
    pub username: String,
    pub exported_at: i64,
    pub messages: Vec<ExportedMessage>,
    #[serde(default)]
    pub contacts: Vec<ExportedContact>,
}

/// Derives the file key from the password and the salt in its header.
fn derive_key(password: &str, salt: &[u8], memory_kib: u32, passes: u32, lanes: u32) -> Result<[u8; 32]> {
    let params = Params::new(memory_kib, passes, lanes, Some(32))
        .map_err(|e| anyhow!("bad key derivation parameters: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("cannot derive the key: {e}"))?;
    Ok(key)
}

/// Packs and encrypts `export` under `password`.
///
/// Slow on purpose: the derivation is the same work an attacker has to repeat
/// for every password they try.
pub fn export(export: &Export, password: &str) -> Result<Vec<u8>> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        bail!("export_password_too_short");
    }

    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    crate::engine::os_rng().fill_bytes(&mut salt);
    crate::engine::os_rng().fill_bytes(&mut nonce);

    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.push(VERSION);
    header.extend_from_slice(&MEMORY_KIB.to_le_bytes());
    header.extend_from_slice(&PASSES.to_le_bytes());
    header.extend_from_slice(&LANES.to_le_bytes());
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce);

    let plaintext = serde_json::to_vec(export).context("cannot pack the conversations")?;
    let key = derive_key(password, &salt, MEMORY_KIB, PASSES, LANES)?;
    let ciphertext = XChaCha20Poly1305::new((&key).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                // The cost lives in the header, so the header has to be
                // covered by the tag: otherwise it could be rewritten to a
                // cost of nothing and the guessing would be cheap.
                aad: &header,
            },
        )
        .map_err(|_| anyhow!("cannot encrypt the export"))?;

    let mut out = header;
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Opens a file produced by [`export`].
///
/// A wrong password is indistinguishable from a damaged file, which is the
/// point: there is nothing to tell an attacker they are getting warmer.
pub fn import(bytes: &[u8], password: &str) -> Result<Export> {
    if bytes.len() < HEADER_LEN || &bytes[..16] != MAGIC {
        bail!("import_not_an_export");
    }
    let version = bytes[16];
    if version != VERSION {
        bail!("import_unsupported_version");
    }

    let number = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (memory_kib, passes, lanes) = (number(17), number(21), number(25));
    // A file asking for a gigabyte would take the app down with it before the
    // password was ever wrong.
    if memory_kib > 1024 * 1024 || passes == 0 || passes > 64 || lanes == 0 || lanes > 16 {
        bail!("import_not_an_export");
    }
    let salt = &bytes[29..29 + SALT_LEN];
    let nonce = &bytes[29 + SALT_LEN..HEADER_LEN];
    let (header, ciphertext) = bytes.split_at(HEADER_LEN);

    let key = derive_key(password, salt, memory_kib, passes, lanes)?;
    let plaintext = XChaCha20Poly1305::new((&key).into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: header,
            },
        )
        .map_err(|_| anyhow!("import_wrong_password"))?;

    serde_json::from_slice(&plaintext).map_err(|_| anyhow!("import_wrong_password"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Export {
        Export {
            username: "alice".into(),
            exported_at: 1_700_000_000_000,
            messages: vec![ExportedMessage {
                id: "m1".into(),
                contact: "bob".into(),
                mine: true,
                kind: "text".into(),
                text: "hola bob".into(),
                state: "read".into(),
                delivered_at: Some(2),
                read_at: Some(3),
                created_at: 1,
            }],
            contacts: vec![ExportedContact {
                username: "bob".into(),
                alias: "Bob".into(),
                avatar: None,
            }],
        }
    }

    #[test]
    fn a_file_opens_with_its_password_and_with_nothing_else() {
        let sealed = export(&sample(), "una-contraseña-larga").unwrap();
        // The point of the exercise: none of it is readable as it stands.
        assert!(!String::from_utf8_lossy(&sealed).contains("hola bob"));

        let opened = import(&sealed, "una-contraseña-larga").unwrap();
        assert_eq!(opened.messages.len(), 1);
        assert_eq!(opened.messages[0].text, "hola bob");
        assert_eq!(opened.contacts[0].alias, "Bob");

        assert_eq!(
            import(&sealed, "una-contraseña-largo").unwrap_err().to_string(),
            "import_wrong_password"
        );
    }

    #[test]
    fn the_cost_cannot_be_edited_down() {
        let mut sealed = export(&sample(), "una-contraseña-larga").unwrap();
        // Rewrite the memory cost to the smallest Argon2 accepts. The tag
        // covers the header, so this must fail rather than crack cheaply.
        sealed[17..21].copy_from_slice(&8u32.to_le_bytes());
        assert!(import(&sealed, "una-contraseña-larga").is_err());
    }

    #[test]
    fn rubbish_is_refused_before_any_work_is_done() {
        assert_eq!(
            import(b"no soy un export", "x").unwrap_err().to_string(),
            "import_not_an_export"
        );
        assert_eq!(
            export(&sample(), "corta").unwrap_err().to_string(),
            "export_password_too_short"
        );
    }
}
