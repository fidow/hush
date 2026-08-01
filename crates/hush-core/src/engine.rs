//! Cryptographic engine: identity, prekey bundles, PQXDH session establishment
//! and Double Ratchet messaging, all via libsignal.
//!
//! Wire conventions (everything the server sees is opaque):
//! - Keys travel base64-encoded inside small JSON documents.
//! - A message envelope is `{"t": "prekey"|"signal", "c": <base64 ciphertext>}`.

use std::time::SystemTime;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use libsignal_protocol::{
    kem, message_decrypt, message_encrypt, process_prekey_bundle, CiphertextMessage, DeviceId,
    GenericSignedPreKey, IdentityKey, IdentityKeyPair, IdentityKeyStore, InMemSignalProtocolStore, KeyPair,
    KyberPreKeyRecord, KyberPreKeyStore, PreKeyBundle, PreKeyRecord, PreKeySignalMessage,
    PreKeyStore, ProtocolAddress, PublicKey, SessionStore, SignalMessage, SignedPreKeyRecord,
    SignedPreKeyStore, Timestamp,
};
use rand::{CryptoRng, Rng, RngCore, TryRngCore};

/// OS-backed CSPRNG. Unlike `os_rng()` (ThreadRng) this is `Send`, which
/// keeps the async methods usable from tauri commands and spawned tasks.
fn os_rng() -> impl CryptoRng + RngCore + Send {
    rand::rngs::OsRng.unwrap_err()
}
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const SIGNED_PREKEY_ID: u32 = 1;
const KYBER_LAST_RESORT_ID: u32 = 1;
const ONE_TIME_ID_BASE: u32 = 100;

fn device_one() -> DeviceId {
    DeviceId::new(1).expect("1 is a valid device id")
}

fn now_timestamp() -> Timestamp {
    Timestamp::from_epoch_millis(
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    )
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    t: String,
    c: String,
}

pub struct Engine {
    store: InMemSignalProtocolStore,
    address: ProtocolAddress,
}

impl Engine {
    /// Generates a fresh identity for `username`.
    pub fn new(username: &str) -> Result<Self> {
        let mut rng = os_rng();
        let identity = IdentityKeyPair::generate(&mut rng);
        let registration_id: u32 = rng.random_range(1..16380);
        let store = InMemSignalProtocolStore::new(identity, registration_id)
            .map_err(|e| anyhow!("create store: {e}"))?;
        Ok(Self {
            store,
            address: ProtocolAddress::new(username.to_string(), device_one()),
        })
    }

    pub fn username(&self) -> &str {
        self.address.name()
    }

    pub async fn registration_id(&self) -> Result<u32> {
        Ok(self.store.get_local_registration_id().await?)
    }

    /// Base64 public identity key, as sent to the server at registration.
    pub async fn identity_key_b64(&self) -> Result<String> {
        let pair = self.store.get_identity_key_pair().await?;
        Ok(B64.encode(pair.public_key().serialize()))
    }

    /// Generates the signed prekey, last-resort kyber prekey and `n` one-time
    /// prekeys of each kind; stores the private halves locally and returns the
    /// JSON body for `PUT /v1/keys`.
    pub async fn generate_prekeys(&mut self, n: u32) -> Result<Value> {
        let mut rng = os_rng();
        let identity = self.store.get_identity_key_pair().await?;

        let spk_pair = KeyPair::generate(&mut rng);
        let spk_sig = identity
            .private_key()
            .calculate_signature(&spk_pair.public_key.serialize(), &mut rng)?;
        let spk = SignedPreKeyRecord::new(
            SIGNED_PREKEY_ID.into(),
            now_timestamp(),
            &spk_pair,
            &spk_sig,
        );
        self.store
            .save_signed_pre_key(SIGNED_PREKEY_ID.into(), &spk)
            .await?;

        let kyber_last_resort = KyberPreKeyRecord::generate(
            kem::KeyType::Kyber1024,
            KYBER_LAST_RESORT_ID.into(),
            identity.private_key(),
        )?;
        self.store
            .save_kyber_pre_key(KYBER_LAST_RESORT_ID.into(), &kyber_last_resort)
            .await?;

        let mut one_time = Vec::new();
        for i in 0..n {
            let id = ONE_TIME_ID_BASE + i;
            let record = PreKeyRecord::new(id.into(), &KeyPair::generate(&mut rng));
            self.store.save_pre_key(id.into(), &record).await?;
            one_time.push(json!({
                "kind": "ec",
                "data": json!({
                    "id": id,
                    "public": B64.encode(record.public_key()?.serialize()),
                }).to_string(),
            }));

            let record = KyberPreKeyRecord::generate(
                kem::KeyType::Kyber1024,
                id.into(),
                identity.private_key(),
            )?;
            self.store.save_kyber_pre_key(id.into(), &record).await?;
            one_time.push(json!({
                "kind": "kyber",
                "data": json!({
                    "id": id,
                    "public": B64.encode(record.public_key()?.serialize()),
                    "signature": B64.encode(record.signature()?),
                }).to_string(),
            }));
        }

        Ok(json!({
            "bundle_static": {
                "device_id": 1,
                "signed_prekey": {
                    "id": SIGNED_PREKEY_ID,
                    "public": B64.encode(spk_pair.public_key.serialize()),
                    "signature": B64.encode(&spk_sig),
                },
                "kyber_last_resort": {
                    "id": KYBER_LAST_RESORT_ID,
                    "public": B64.encode(kyber_last_resort.public_key()?.serialize()),
                    "signature": B64.encode(kyber_last_resort.signature()?),
                },
            },
            "one_time_prekeys": one_time,
        }))
    }

    pub async fn has_session(&self, remote: &str) -> Result<bool> {
        let addr = ProtocolAddress::new(remote.to_string(), device_one());
        Ok(self.store.load_session(&addr).await?.is_some())
    }

    /// Establishes a PQXDH session with `remote` from a bundle returned by
    /// `GET /v1/keys/{remote}`. No-op if a session already exists.
    pub async fn ensure_session(&mut self, remote: &str, bundle: &Value) -> Result<()> {
        let addr = ProtocolAddress::new(remote.to_string(), device_one());
        if self.store.load_session(&addr).await?.is_some() {
            return Ok(());
        }

        let b64_field = |v: &Value, what: &str| -> Result<Vec<u8>> {
            let s = v.as_str().with_context(|| format!("missing {what}"))?;
            B64.decode(s).with_context(|| format!("bad base64 in {what}"))
        };

        let registration_id = bundle["registration_id"]
            .as_u64()
            .context("missing registration_id")? as u32;
        let identity_key = IdentityKey::decode(&b64_field(&bundle["identity_key"], "identity_key")?)
            .map_err(|e| anyhow!("bad identity key: {e}"))?;
        let stat = &bundle["bundle_static"];

        // One-time EC prekey is optional; the signed prekey always exists.
        let pre_key = match bundle["one_time_prekey"].as_str() {
            Some(data) => {
                let v: Value = serde_json::from_str(data)?;
                Some((
                    (v["id"].as_u64().context("otk id")? as u32).into(),
                    PublicKey::deserialize(&b64_field(&v["public"], "otk public")?)?,
                ))
            }
            None => None,
        };

        // Prefer a one-time kyber prekey; fall back to the last-resort one.
        let kyber: Value = match bundle["kyber_prekey"].as_str() {
            Some(data) => serde_json::from_str(data)?,
            None => stat["kyber_last_resort"].clone(),
        };

        let spk = &stat["signed_prekey"];
        let pqxdh_bundle = PreKeyBundle::new(
            registration_id,
            device_one(),
            pre_key,
            (spk["id"].as_u64().context("spk id")? as u32).into(),
            PublicKey::deserialize(&b64_field(&spk["public"], "spk public")?)?,
            b64_field(&spk["signature"], "spk signature")?,
            (kyber["id"].as_u64().context("kyber id")? as u32).into(),
            kem::PublicKey::deserialize(&b64_field(&kyber["public"], "kyber public")?)?,
            b64_field(&kyber["signature"], "kyber signature")?,
            identity_key,
        )?;

        process_prekey_bundle(
            &addr,
            &self.address,
            &mut self.store.session_store,
            &mut self.store.identity_store,
            &pqxdh_bundle,
            SystemTime::now(),
            &mut os_rng(),
        )
        .await?;
        Ok(())
    }

    /// Encrypts `plaintext` for `remote` (session must exist) and returns the
    /// envelope string to send as the message body.
    pub async fn encrypt(&mut self, remote: &str, plaintext: &[u8]) -> Result<String> {
        let addr = ProtocolAddress::new(remote.to_string(), device_one());
        let msg = message_encrypt(
            plaintext,
            &addr,
            &self.address,
            &mut self.store.session_store,
            &mut self.store.identity_store,
            SystemTime::now(),
            &mut os_rng(),
        )
        .await?;
        let t = match &msg {
            CiphertextMessage::PreKeySignalMessage(_) => "prekey",
            CiphertextMessage::SignalMessage(_) => "signal",
            other => bail!("unexpected ciphertext type {:?}", other.message_type()),
        };
        Ok(serde_json::to_string(&Envelope {
            t: t.into(),
            c: B64.encode(msg.serialize()),
        })?)
    }

    /// Decrypts an envelope received from `remote`.
    pub async fn decrypt(&mut self, remote: &str, envelope: &str) -> Result<Vec<u8>> {
        let addr = ProtocolAddress::new(remote.to_string(), device_one());
        let env: Envelope = serde_json::from_str(envelope).context("bad envelope")?;
        let raw = B64.decode(&env.c).context("bad envelope base64")?;
        let msg = match env.t.as_str() {
            "prekey" => CiphertextMessage::PreKeySignalMessage(PreKeySignalMessage::try_from(
                raw.as_slice(),
            )?),
            "signal" => CiphertextMessage::SignalMessage(SignalMessage::try_from(raw.as_slice())?),
            other => bail!("unknown envelope type {other:?}"),
        };
        let plaintext = message_decrypt(
            &msg,
            &addr,
            &self.address,
            &mut self.store.session_store,
            &mut self.store.identity_store,
            &mut self.store.pre_key_store,
            &self.store.signed_pre_key_store,
            &mut self.store.kyber_pre_key_store,
            &mut os_rng(),
        )
        .await?;
        Ok(plaintext)
    }
}
