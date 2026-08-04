//! Cryptographic engine: identity, prekey bundles, PQXDH session establishment
//! and Double Ratchet messaging, all via libsignal, persisted in the local
//! SQLite database so state survives restarts.
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
    GenericSignedPreKey, IdentityKey, IdentityKeyStore, KeyPair, KyberPreKeyRecord,
    KyberPreKeyStore, PreKeyBundle, PreKeyRecord, PreKeySignalMessage, PreKeyStore,
    ProtocolAddress, PublicKey, SessionStore, SignalMessage, SignedPreKeyRecord,
    SignedPreKeyStore, Timestamp,
};
use rand::{CryptoRng, RngCore, TryRngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::LocalDb;
use crate::store::{
    SqliteIdentityStore, SqliteKyberPreKeyStore, SqlitePreKeyStore, SqliteSessionStore,
    SqliteSignedPreKeyStore,
};

/// OS-backed CSPRNG. Unlike `rand::rng()` (ThreadRng) this is `Send`, which
/// keeps the async methods usable from spawned tasks.
pub(crate) fn os_rng() -> impl CryptoRng + RngCore + Send {
    rand::rngs::OsRng.unwrap_err()
}

/// Every device of an account gets its own Signal address, so a message is
/// encrypted separately for each and no ratchet is ever shared.
fn device_id(device: u32) -> DeviceId {
    // Signal device ids are a byte, which is why the server hands out the
    // lowest free number rather than counting upwards forever.
    let device = u8::try_from(device).unwrap_or(1).max(1);
    DeviceId::new(device).expect("device ids start at 1")
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
    db: LocalDb,
    address: ProtocolAddress,
    sessions: SqliteSessionStore,
    prekeys: SqlitePreKeyStore,
    signed_prekeys: SqliteSignedPreKeyStore,
    kyber_prekeys: SqliteKyberPreKeyStore,
    identity: SqliteIdentityStore,
}

impl Engine {
    /// Opens the engine over the local database, loading the persisted
    /// identity or generating a fresh one on first use.
    pub fn open(db: LocalDb, username: &str, device: u32) -> Result<Self> {
        let identity = SqliteIdentityStore { db: db.clone() };
        identity.ensure_identity()?;
        Ok(Self {
            address: ProtocolAddress::new(username.to_string(), device_id(device)),
            sessions: SqliteSessionStore { db: db.clone() },
            prekeys: SqlitePreKeyStore { db: db.clone() },
            signed_prekeys: SqliteSignedPreKeyStore { db: db.clone() },
            kyber_prekeys: SqliteKyberPreKeyStore { db: db.clone() },
            identity,
            db,
        })
    }

    pub fn username(&self) -> &str {
        self.address.name()
    }

    pub async fn registration_id(&self) -> Result<u32> {
        Ok(self.identity.get_local_registration_id().await?)
    }

    /// Base64 public identity key, as sent to the server at registration.
    pub async fn identity_key_b64(&self) -> Result<String> {
        let pair = self.identity.get_identity_key_pair().await?;
        Ok(B64.encode(pair.public_key().serialize()))
    }


    /// Generates a fresh signed prekey, last-resort kyber prekey and `n`
    /// one-time prekeys of each kind; stores the private halves locally and
    /// returns the JSON body for `PUT /v1/keys`.
    pub async fn generate_prekeys(&mut self, n: u32) -> Result<Value> {
        let mut rng = os_rng();
        let identity = self.identity.get_identity_key_pair().await?;

        let spk_id = self.db.next_id("signed_prekeys")?;
        let spk_pair = KeyPair::generate(&mut rng);
        let spk_sig = identity
            .private_key()
            .calculate_signature(&spk_pair.public_key.serialize(), &mut rng)?;
        let spk = SignedPreKeyRecord::new(spk_id.into(), now_timestamp(), &spk_pair, &spk_sig);
        self.signed_prekeys
            .save_signed_pre_key(spk_id.into(), &spk)
            .await?;

        let kyber_last_resort_id = self.db.next_id("kyber_prekeys")?;
        let kyber_last_resort = KyberPreKeyRecord::generate(
            kem::KeyType::Kyber1024,
            kyber_last_resort_id.into(),
            identity.private_key(),
        )?;
        self.kyber_prekeys
            .save_kyber_pre_key(kyber_last_resort_id.into(), &kyber_last_resort)
            .await?;

        let mut one_time = Vec::new();
        let first_ec = self.db.next_id("prekeys")?;
        for i in 0..n {
            let id = first_ec + i;
            let record = PreKeyRecord::new(id.into(), &KeyPair::generate(&mut rng));
            self.prekeys.save_pre_key(id.into(), &record).await?;
            one_time.push(json!({
                "kind": "ec",
                "data": json!({
                    "id": id,
                    "public": B64.encode(record.public_key()?.serialize()),
                }).to_string(),
            }));

            let id = kyber_last_resort_id + 1 + i;
            let record = KyberPreKeyRecord::generate(
                kem::KeyType::Kyber1024,
                id.into(),
                identity.private_key(),
            )?;
            self.kyber_prekeys.save_kyber_pre_key(id.into(), &record).await?;
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
            "identity_key": B64.encode(identity.public_key().serialize()),
            "registration_id": self.registration_id().await?,
            "bundle_static": {
                "device_id": 1,
                "signed_prekey": {
                    "id": spk_id,
                    "public": B64.encode(spk_pair.public_key.serialize()),
                    "signature": B64.encode(&spk_sig),
                },
                "kyber_last_resort": {
                    "id": kyber_last_resort_id,
                    "public": B64.encode(kyber_last_resort.public_key()?.serialize()),
                    "signature": B64.encode(kyber_last_resort.signature()?),
                },
            },
            "one_time_prekeys": one_time,
        }))
    }

    pub async fn has_session(&self, remote: &str, device: u32) -> Result<bool> {
        let addr = ProtocolAddress::new(remote.to_string(), device_id(device));
        Ok(self.sessions.load_session(&addr).await?.is_some())
    }

    /// A short, readable fingerprint of an identity key.
    ///
    /// Shown when a contact's key changes, so the two of you can read it to
    /// each other over some channel this app does not control and settle
    /// whether the change was really them. Without that, accepting a new key
    /// is trusting whatever the server said.
    pub fn fingerprint(identity_b64: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(identity_b64.as_bytes());
        digest
            .iter()
            .take(10)
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .chunks(2)
            .map(|pair| pair.concat())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The identity key we have on record for `remote`, if any (base64).
    pub async fn known_identity_b64(&self, remote: &str, device: u32) -> Result<Option<String>> {
        let addr = ProtocolAddress::new(remote.to_string(), device_id(device));
        Ok(self
            .identity
            .get_identity(&addr)
            .await?
            .map(|k| B64.encode(k.serialize())))
    }

    /// Drops the session and recorded identity for `remote` (e.g. after the
    /// contact re-provisioned their keys on a new device).
    /// Throws away the ratchet with `remote`, so the next message rebuilds it.
    ///
    /// The pinned identity deliberately stays. Losing a ratchet is ordinary —
    /// a reinstall, a message that arrived for a session we no longer have —
    /// and rebuilding it needs nothing more than this. Forgetting *who* the
    /// contact is at the same time would mean the next bundle the server
    /// hands over is trusted on sight, whoever it belongs to, and the server
    /// gets to decide when that happens by sending one unreadable message.
    /// That is [`forget_identity`](Self::forget_identity), and only the user
    /// may ask for it.
    pub fn reset_session(&mut self, remote: &str, device: u32) -> Result<()> {
        let addr = ProtocolAddress::new(remote.to_string(), device_id(device));
        let key = format!("{}:{}", addr.name(), u32::from(addr.device_id()));
        self.db
            .with(|c| c.execute("DELETE FROM sessions WHERE address = ?1", [&key]).map(|_| ()))
    }

    /// Un-pins the identity of `remote`, accepting whatever they publish next.
    ///
    /// Only ever called after the person using the app has been shown that the
    /// contact's key changed and has said to go ahead: from here on, messages
    /// go to whoever holds the new key.
    pub fn forget_identity(&mut self, remote: &str, device: u32) -> Result<()> {
        let addr = ProtocolAddress::new(remote.to_string(), device_id(device));
        let key = format!("{}:{}", addr.name(), u32::from(addr.device_id()));
        self.db.with(|c| {
            c.execute("DELETE FROM sessions WHERE address = ?1", [&key])?;
            c.execute("DELETE FROM identities WHERE address = ?1", [&key])?;
            Ok(())
        })
    }

    /// Establishes a PQXDH session with `remote` from a bundle returned by
    /// `GET /v1/keys/{remote}`. No-op if a session already exists.
    pub async fn ensure_session(&mut self, remote: &str, device: u32, bundle: &Value) -> Result<()> {
        let addr = ProtocolAddress::new(remote.to_string(), device_id(device));
        if self.sessions.load_session(&addr).await?.is_some() {
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
            device_id(device),
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
            &mut self.sessions,
            &mut self.identity,
            &pqxdh_bundle,
            SystemTime::now(),
            &mut os_rng(),
        )
        .await?;
        Ok(())
    }

    /// Encrypts `plaintext` for `remote` (session must exist) and returns the
    /// envelope string to send as the message body.
    pub async fn encrypt(&mut self, remote: &str, device: u32, plaintext: &[u8]) -> Result<String> {
        let addr = ProtocolAddress::new(remote.to_string(), device_id(device));
        let msg = message_encrypt(
            plaintext,
            &addr,
            &self.address,
            &mut self.sessions,
            &mut self.identity,
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
    pub async fn decrypt(&mut self, remote: &str, device: u32, envelope: &str) -> Result<Vec<u8>> {
        let addr = ProtocolAddress::new(remote.to_string(), device_id(device));
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
            &mut self.sessions,
            &mut self.identity,
            &mut self.prekeys,
            &self.signed_prekeys,
            &mut self.kyber_prekeys,
            &mut os_rng(),
        )
        .await?;
        Ok(plaintext)
    }
}
