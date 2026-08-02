//! Encryption of the local database at rest.
//!
//! Everything sensitive on disk — message history, the identity private key,
//! the archive recovery key, the session token — is sealed with
//! XChaCha20-Poly1305 under a key generated on this device and never shared.
//!
//! The device key itself is kept next to the database, wrapped by the
//! operating system so that the file alone is useless: on Windows through
//! DPAPI bound to the user account, which means copying the files to another
//! machine, or reading them as another user, yields nothing.
//!
//! What this does *not* defend against is code already running as that user:
//! it can ask the OS to unwrap the key exactly as the app does. Protecting
//! against that needs a passphrase the user types, which is a separate
//! decision about friction.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use std::path::Path;

/// The per-device key protecting local storage.
#[derive(Clone)]
pub struct DeviceKey([u8; 32]);

impl DeviceKey {
    /// Loads the key stored beside `db_path`, creating one on first run.
    pub fn load_or_create(db_path: &Path) -> Result<Self> {
        let key_path = db_path.with_extension("key");
        if key_path.exists() {
            let wrapped = std::fs::read(&key_path).context("cannot read the device key")?;
            let raw = unwrap_key(&wrapped).context(
                "the device key cannot be unwrapped: it belongs to another user or machine",
            )?;
            let key: [u8; 32] = raw
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("the device key file is corrupt"))?;
            return Ok(Self(key));
        }

        let mut key = [0u8; 32];
        crate::engine::os_rng().fill_bytes(&mut key);
        if let Some(dir) = key_path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(&key_path, wrap_key(&key)?).context("cannot store the device key")?;
        tracing::info!("created a device key for local storage");
        Ok(Self(key))
    }

    /// A fixed key, for tests.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self([7u8; 32])
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }

    /// Seals a value for storage. Returns base64 of `nonce ‖ ciphertext`.
    pub fn seal(&self, plaintext: &[u8]) -> Result<String> {
        let mut nonce = [0u8; 24];
        crate::engine::os_rng().fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher()
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| anyhow!("cannot encrypt local data"))?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ciphertext);
        Ok(B64.encode(blob))
    }

    pub fn open(&self, sealed: &str) -> Result<Vec<u8>> {
        let raw = B64.decode(sealed).context("corrupt local data")?;
        if raw.len() < 24 {
            bail!("corrupt local data");
        }
        let (nonce, ciphertext) = raw.split_at(24);
        self.cipher()
            .decrypt(XNonce::from_slice(nonce), ciphertext)
            .map_err(|_| anyhow!("cannot decrypt local data with this device key"))
    }

    /// Convenience for the many text columns.
    pub fn seal_str(&self, text: &str) -> Result<String> {
        self.seal(text.as_bytes())
    }

    pub fn open_str(&self, sealed: &str) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.open(sealed)?).into_owned())
    }
}

/// Wraps the device key with the OS so the file cannot be reused elsewhere.
#[cfg(windows)]
fn wrap_key(key: &[u8]) -> Result<Vec<u8>> {
    dpapi(key, true)
}

#[cfg(windows)]
fn unwrap_key(wrapped: &[u8]) -> Result<Vec<u8>> {
    dpapi(wrapped, false)
}

/// DPAPI, scoped to the current Windows user.
#[cfg(windows)]
fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    let mut in_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // SAFETY: both blobs are valid for the duration of the call, and the
    // buffer the API allocates is copied out and freed before returning.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &mut in_blob,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut out_blob,
            )
        } else {
            CryptUnprotectData(
                &mut in_blob,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut out_blob,
            )
        }
    };
    if ok == 0 {
        bail!("the operating system refused to handle the device key");
    }

    let out = unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) }
        .to_vec();
    unsafe { LocalFree(out_blob.pbData as _) };
    Ok(out)
}

/// Elsewhere the key is stored as-is; the platform hook goes here when the
/// app grows beyond Windows.
#[cfg(not(windows))]
fn wrap_key(key: &[u8]) -> Result<Vec<u8>> {
    tracing::warn!("no OS key protection on this platform; the device key is stored unwrapped");
    Ok(key.to_vec())
}

#[cfg(not(windows))]
fn unwrap_key(wrapped: &[u8]) -> Result<Vec<u8>> {
    Ok(wrapped.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_need_the_right_key() {
        let key = DeviceKey::for_test();
        let sealed = key.seal_str("hola bob").unwrap();
        assert!(!sealed.contains("hola"));
        assert_eq!(key.open_str(&sealed).unwrap(), "hola bob");

        let other = DeviceKey([9u8; 32]);
        assert!(other.open(&sealed).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn the_os_wraps_and_unwraps_the_key() {
        let key = [3u8; 32];
        let wrapped = wrap_key(&key).unwrap();
        assert_ne!(wrapped, key.to_vec(), "the key must not be stored as-is");
        assert_eq!(unwrap_key(&wrapped).unwrap(), key.to_vec());
    }
}
