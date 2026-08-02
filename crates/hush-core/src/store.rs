//! libsignal storage traits backed by the local SQLite database, so identity,
//! sessions and ratchet state survive app restarts.

use async_trait::async_trait;
use libsignal_protocol::{
    CiphertextMessageType, Direction, GenericSignedPreKey, IdentityChange, IdentityKey,
    IdentityKeyPair, IdentityKeyStore, KyberPreKeyId, KyberPreKeyRecord, KyberPreKeyStore,
    PreKeyId, PreKeyRecord, PreKeyStore, ProtocolAddress, PublicKey, SessionRecord, SessionStore,
    SignalProtocolError, SignedPreKeyId, SignedPreKeyRecord, SignedPreKeyStore,
};
use rusqlite::{params, OptionalExtension};

use crate::db::LocalDb;

type Result<T> = std::result::Result<T, SignalProtocolError>;

fn db_err(e: impl std::fmt::Display) -> SignalProtocolError {
    SignalProtocolError::InvalidState("sqlite", e.to_string())
}

fn addr_key(address: &ProtocolAddress) -> String {
    format!("{}:{}", address.name(), u32::from(address.device_id()))
}

#[derive(Clone)]
pub struct SqliteSessionStore {
    pub(crate) db: LocalDb,
}

#[async_trait(?Send)]
impl SessionStore for SqliteSessionStore {
    async fn load_session(&self, address: &ProtocolAddress) -> Result<Option<SessionRecord>> {
        let blob: Option<Vec<u8>> = self
            .db
            .with(|c| {
                c.query_row(
                    "SELECT record FROM sessions WHERE address = ?1",
                    [addr_key(address)],
                    |r| r.get(0),
                )
                .optional()
            })
            .map_err(db_err)?;
        blob.map(|b| SessionRecord::deserialize(&b)).transpose()
    }

    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: &SessionRecord,
    ) -> Result<()> {
        let blob = record.serialize()?;
        self.db
            .with(|c| {
                c.execute(
                    "INSERT INTO sessions (address, record) VALUES (?1, ?2)
                     ON CONFLICT(address) DO UPDATE SET record = excluded.record",
                    params![addr_key(address), blob],
                )
                .map(|_| ())
            })
            .map_err(db_err)
    }
}

#[derive(Clone)]
pub struct SqlitePreKeyStore {
    pub(crate) db: LocalDb,
}

#[async_trait(?Send)]
impl PreKeyStore for SqlitePreKeyStore {
    async fn get_pre_key(&self, prekey_id: PreKeyId) -> Result<PreKeyRecord> {
        let blob: Option<Vec<u8>> = self
            .db
            .with(|c| {
                c.query_row(
                    "SELECT record FROM prekeys WHERE id = ?1",
                    [u32::from(prekey_id)],
                    |r| r.get(0),
                )
                .optional()
            })
            .map_err(db_err)?;
        PreKeyRecord::deserialize(&blob.ok_or(SignalProtocolError::InvalidPreKeyId)?)
    }

    async fn save_pre_key(&mut self, prekey_id: PreKeyId, record: &PreKeyRecord) -> Result<()> {
        let blob = record.serialize()?;
        self.db
            .with(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO prekeys (id, record) VALUES (?1, ?2)",
                    params![u32::from(prekey_id), blob],
                )
                .map(|_| ())
            })
            .map_err(db_err)
    }

    async fn remove_pre_key(&mut self, prekey_id: PreKeyId) -> Result<()> {
        self.db
            .with(|c| {
                c.execute("DELETE FROM prekeys WHERE id = ?1", [u32::from(prekey_id)])
                    .map(|_| ())
            })
            .map_err(db_err)
    }
}

#[derive(Clone)]
pub struct SqliteSignedPreKeyStore {
    pub(crate) db: LocalDb,
}

#[async_trait(?Send)]
impl SignedPreKeyStore for SqliteSignedPreKeyStore {
    async fn get_signed_pre_key(&self, id: SignedPreKeyId) -> Result<SignedPreKeyRecord> {
        let blob: Option<Vec<u8>> = self
            .db
            .with(|c| {
                c.query_row(
                    "SELECT record FROM signed_prekeys WHERE id = ?1",
                    [u32::from(id)],
                    |r| r.get(0),
                )
                .optional()
            })
            .map_err(db_err)?;
        SignedPreKeyRecord::deserialize(&blob.ok_or(SignalProtocolError::InvalidSignedPreKeyId)?)
    }

    async fn save_signed_pre_key(
        &mut self,
        id: SignedPreKeyId,
        record: &SignedPreKeyRecord,
    ) -> Result<()> {
        let blob = record.serialize()?;
        self.db
            .with(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO signed_prekeys (id, record) VALUES (?1, ?2)",
                    params![u32::from(id), blob],
                )
                .map(|_| ())
            })
            .map_err(db_err)
    }
}

#[derive(Clone)]
pub struct SqliteKyberPreKeyStore {
    pub(crate) db: LocalDb,
}

#[async_trait(?Send)]
impl KyberPreKeyStore for SqliteKyberPreKeyStore {
    async fn get_kyber_pre_key(&self, id: KyberPreKeyId) -> Result<KyberPreKeyRecord> {
        let blob: Option<Vec<u8>> = self
            .db
            .with(|c| {
                c.query_row(
                    "SELECT record FROM kyber_prekeys WHERE id = ?1",
                    [u32::from(id)],
                    |r| r.get(0),
                )
                .optional()
            })
            .map_err(db_err)?;
        KyberPreKeyRecord::deserialize(&blob.ok_or(SignalProtocolError::InvalidKyberPreKeyId)?)
    }

    async fn save_kyber_pre_key(&mut self, id: KyberPreKeyId, record: &KyberPreKeyRecord) -> Result<()> {
        let blob = record.serialize()?;
        self.db
            .with(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO kyber_prekeys (id, record) VALUES (?1, ?2)",
                    params![u32::from(id), blob],
                )
                .map(|_| ())
            })
            .map_err(db_err)
    }

    async fn mark_kyber_pre_key_used(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        ec_prekey_id: SignedPreKeyId,
        base_key: &PublicKey,
    ) -> Result<()> {
        // Mirrors InMemKyberPreKeyStore: reusing the same (kyber, ec, base key)
        // combination indicates a replayed PreKey message.
        let inserted = self
            .db
            .with(|c| {
                c.execute(
                    "INSERT OR IGNORE INTO kyber_base_keys_seen (kyber_id, ec_id, base_key)
                     VALUES (?1, ?2, ?3)",
                    params![
                        u32::from(kyber_prekey_id),
                        u32::from(ec_prekey_id),
                        base_key.serialize().to_vec()
                    ],
                )
            })
            .map_err(db_err)?;
        if inserted == 0 {
            return Err(SignalProtocolError::InvalidMessage(
                CiphertextMessageType::PreKey,
                "kyber pre-key already used with this base key".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct SqliteIdentityStore {
    pub(crate) db: LocalDb,
}

impl SqliteIdentityStore {
    /// Loads the local identity, generating and persisting one if absent.
    pub fn ensure_identity(&self) -> anyhow::Result<()> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        if self.db.meta_get("identity_keypair")?.is_none() {
            let mut rng = crate::engine::os_rng();
            let pair = IdentityKeyPair::generate(&mut rng);
            let registration_id: u32 = rand::Rng::random_range(&mut rng, 1..16380);
            self.db
                .meta_set("identity_keypair", &b64.encode(pair.serialize()))?;
            self.db
                .meta_set("registration_id", &registration_id.to_string())?;
        }
        Ok(())
    }

    fn load_identity(&self) -> Result<IdentityKeyPair> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let stored = self
            .db
            .meta_get("identity_keypair")
            .map_err(db_err)?
            .ok_or_else(|| db_err("no local identity"))?;
        let bytes = b64.decode(stored).map_err(db_err)?;
        IdentityKeyPair::try_from(bytes.as_slice())
    }
}

#[async_trait(?Send)]
impl IdentityKeyStore for SqliteIdentityStore {
    async fn get_identity_key_pair(&self) -> Result<IdentityKeyPair> {
        self.load_identity()
    }

    async fn get_local_registration_id(&self) -> Result<u32> {
        self.db
            .meta_get("registration_id")
            .map_err(db_err)?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| db_err("no registration id"))
    }

    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
    ) -> Result<IdentityChange> {
        let existing = self.get_identity(address).await?;
        let change = match existing {
            Some(ref old) if old == identity => IdentityChange::NewOrUnchanged,
            Some(_) => IdentityChange::ReplacedExisting,
            None => IdentityChange::NewOrUnchanged,
        };
        self.db
            .with(|c| {
                c.execute(
                    "INSERT INTO identities (address, identity) VALUES (?1, ?2)
                     ON CONFLICT(address) DO UPDATE SET identity = excluded.identity",
                    params![addr_key(address), identity.serialize().to_vec()],
                )
                .map(|_| ())
            })
            .map_err(db_err)?;
        Ok(change)
    }

    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
        _direction: Direction,
    ) -> Result<bool> {
        // Trust on first use, like the in-memory store.
        Ok(match self.get_identity(address).await? {
            None => true,
            Some(known) => known == *identity,
        })
    }

    async fn get_identity(&self, address: &ProtocolAddress) -> Result<Option<IdentityKey>> {
        let blob: Option<Vec<u8>> = self
            .db
            .with(|c| {
                c.query_row(
                    "SELECT identity FROM identities WHERE address = ?1",
                    [addr_key(address)],
                    |r| r.get(0),
                )
                .optional()
            })
            .map_err(db_err)?;
        blob.map(|b| IdentityKey::decode(&b)).transpose()
    }
}
