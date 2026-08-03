//! Local client database (SQLite via rusqlite): identity and protocol state,
//! account profile, contacts and message history.
//!
//! Everything worth stealing — message text, contact names, the identity
//! private key, the archive recovery key, the session token, and the whole
//! libsignal store — is sealed with this device's key before it touches the
//! disk. See [`crate::keystore`].

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS identities (
    address  TEXT PRIMARY KEY,
    identity BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    address TEXT PRIMARY KEY,
    record  BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS prekeys (
    id     INTEGER PRIMARY KEY,
    record BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS signed_prekeys (
    id     INTEGER PRIMARY KEY,
    record BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS kyber_prekeys (
    id     INTEGER PRIMARY KEY,
    record BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS kyber_base_keys_seen (
    kyber_id INTEGER NOT NULL,
    ec_id    INTEGER NOT NULL,
    base_key BLOB NOT NULL,
    PRIMARY KEY (kyber_id, ec_id, base_key)
);
CREATE TABLE IF NOT EXISTS contacts (
    username TEXT PRIMARY KEY,
    alias    TEXT NOT NULL,
    -- Mirror of the server's list: 'incoming', 'outgoing' or 'accepted'.
    state    TEXT NOT NULL DEFAULT 'accepted'
);
CREATE TABLE IF NOT EXISTS messages (
    id         TEXT PRIMARY KEY,
    contact    TEXT NOT NULL,
    mine       INTEGER NOT NULL,
    kind       TEXT NOT NULL DEFAULT 'text',
    text       TEXT NOT NULL,
    -- For our own messages: 'sent', 'delivered' or 'read', with the instant
    -- each step was reported.
    state        TEXT NOT NULL DEFAULT 'sent',
    delivered_at INTEGER,
    read_at      INTEGER,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_contact ON messages(contact, created_at);
"#;

/// A locally stored chat message (either direction).
#[derive(Clone, Debug, serde::Serialize)]
pub struct StoredMessage {
    pub id: String,
    pub contact: String,
    pub mine: bool,
    /// "text" or "image" (image content is a data URL).
    pub kind: String,
    pub text: String,
    /// Delivery state of our own messages: "sent", "delivered" or "read".
    pub state: String,
    /// When each step was reported, for the message info panel.
    pub delivered_at: Option<i64>,
    pub read_at: Option<i64>,
    pub created_at: i64,
}

/// The locally saved account profile.
#[derive(Clone, Debug)]
pub struct Profile {
    pub username: String,
    pub alias: String,
    pub server: String,
    pub token: String,
}

/// Values that must never sit on disk in the clear. `crypto` is excluded on
/// purpose: it is the marker saying the rest of the file is sealed.
fn is_sealed_meta(key: &str) -> bool {
    key != "crypto"
}

#[derive(Clone)]
pub struct LocalDb {
    conn: Rc<RefCell<Connection>>,
    /// Seals message text, contact names, stored keys and tokens. Absent only
    /// in tests that do not care about storage.
    key: Option<crate::keystore::DeviceKey>,
}

impl LocalDb {
    /// Encrypts a value for storage, or passes it through when this database
    /// has no device key (tests).
    pub(crate) fn seal(&self, plaintext: &str) -> Result<String> {
        match &self.key {
            Some(key) => key.seal_str(plaintext),
            None => Ok(plaintext.to_string()),
        }
    }

    pub(crate) fn seal_bytes(&self, plaintext: &[u8]) -> Result<String> {
        match &self.key {
            Some(key) => key.seal(plaintext),
            None => Ok(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                plaintext,
            )),
        }
    }

    /// Decrypts a stored value. Anything that fails to open is returned as-is
    /// so a half-migrated database still reads.
    pub(crate) fn unseal(&self, stored: &str) -> String {
        match &self.key {
            Some(key) => key.open_str(stored).unwrap_or_else(|_| stored.to_string()),
            None => stored.to_string(),
        }
    }

    /// Whether a stored value is already sealed with this device's key.
    ///
    /// The migration has to be able to run twice: it may be interrupted, and a
    /// database can hold sealed rows while the marker that says so is missing.
    /// Sealing an already sealed value would make it unreadable for good.
    pub(crate) fn already_sealed(&self, stored: &str) -> bool {
        match &self.key {
            Some(key) => key.open(stored).is_ok(),
            None => false,
        }
    }

    pub(crate) fn open_bytes(&self, stored: &str) -> Result<Vec<u8>> {
        match &self.key {
            Some(key) => key.open(stored),
            None => Ok(base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                stored,
            )?),
        }
    }
}

impl LocalDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("create data dir")?;
        }
        let conn = Connection::open(path).context("open local db")?;
        // Overwrite freed pages instead of leaving old plaintext in the slack
        // space of the file.
        conn.execute_batch("PRAGMA secure_delete = ON")?;
        conn.execute_batch(SCHEMA)?;
        // Migration for local dbs created before message kinds existed.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'text'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE contacts ADD COLUMN state TEXT NOT NULL DEFAULT 'accepted'",
            [],
        );
        for column in [
            "state TEXT NOT NULL DEFAULT 'sent'",
            "delivered_at INTEGER",
            "read_at INTEGER",
        ] {
            let _ = conn.execute(&format!("ALTER TABLE messages ADD COLUMN {column}"), []);
        }
        // Earlier versions stored the "delete for everyone" instruction as a
        // message, so conversations showed a line with the id of the message
        // that had just been deleted.
        let _ = conn.execute("DELETE FROM messages WHERE kind = 'delete'", []);
        // An encrypted database whose key file is gone would otherwise get a
        // brand new key and read back as garbage, so say what happened.
        let sealed: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'crypto'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if sealed.is_some() && !path.with_extension("key").exists() {
            bail!(
                "the local data in {} is encrypted with a device key that is missing ({}). \
                 Move that folder aside to start over; the history can be restored with the recovery key.",
                path.display(),
                path.with_extension("key").display()
            );
        }

        let db = Self {
            conn: Rc::new(RefCell::new(conn)),
            key: Some(crate::keystore::DeviceKey::load_or_create(path)?),
        };
        db.encrypt_existing_rows()?;
        Ok(db)
    }

    /// In-memory database with no encryption, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Rc::new(RefCell::new(conn)),
            key: None,
        })
    }

    /// One-off migration of a database written before storage was encrypted.
    /// Without it the old plaintext rows would simply stay readable. It runs
    /// inside a transaction: a partially sealed database would be unreadable.
    fn encrypt_existing_rows(&self) -> Result<()> {
        if self.meta_get_raw("crypto")?.is_some() {
            return Ok(());
        }
        self.with(|c| c.execute_batch("BEGIN IMMEDIATE"))?;
        match self.seal_existing_rows() {
            Ok(sealed) => {
                self.with(|c| c.execute_batch("COMMIT"))?;
                // The updated rows land on new pages, and the old plaintext
                // would survive in the freed ones. A database that had nothing
                // to seal has nothing to reclaim, and a VACUUM that fails —
                // it needs somewhere to write a temporary file, which is not a
                // given on every platform — must not stop the app: the data is
                // already encrypted by this point.
                if sealed > 0 {
                    if let Err(e) = self.with(|c| c.execute_batch("VACUUM")) {
                        tracing::warn!("could not compact the database after encrypting it: {e}");
                    }
                    tracing::info!("local storage encrypted with this device's key");
                }
                Ok(())
            }
            Err(e) => {
                let _ = self.with(|c| c.execute_batch("ROLLBACK"));
                Err(e.context("encrypt existing local data"))
            }
        }
    }

    /// Returns how many rows were sealed, so the caller knows whether the
    /// file holds freed pages worth reclaiming.
    fn seal_existing_rows(&self) -> Result<usize> {
        let messages: Vec<(String, String)> = self.with(|c| {
            c.prepare("SELECT id, text FROM messages")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })?;
        let contacts: Vec<(String, String)> = self.with(|c| {
            c.prepare("SELECT username, alias FROM contacts")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })?;
        let meta: Vec<(String, String)> = self.with(|c| {
            c.prepare("SELECT key, value FROM meta")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })?;
        let stores = self.collect_store_rows()?;
        let mut sealed_rows = 0;

        for (id, text) in messages {
            if self.already_sealed(&text) {
                continue;
            }
            let sealed = self.seal(&text)?;
            self.with(|c| {
                c.execute(
                    "UPDATE messages SET text = ?1 WHERE id = ?2",
                    params![sealed, id],
                )
                .map(|_| ())
            })?;
            sealed_rows += 1;
        }
        for (username, alias) in contacts {
            if self.already_sealed(&alias) {
                continue;
            }
            let sealed = self.seal(&alias)?;
            self.with(|c| {
                c.execute(
                    "UPDATE contacts SET alias = ?1 WHERE username = ?2",
                    params![sealed, username],
                )
                .map(|_| ())
            })?;
            sealed_rows += 1;
        }
        for (key, value) in meta {
            if !is_sealed_meta(&key) || self.already_sealed(&value) {
                continue;
            }
            let sealed = self.seal(&value)?;
            self.with(|c| {
                c.execute(
                    "UPDATE meta SET value = ?1 WHERE key = ?2",
                    params![sealed, key],
                )
                .map(|_| ())
            })?;
            sealed_rows += 1;
        }
        for (table, key_col, value_col, key, value) in stores {
            let plaintext = match value {
                // What the stores held before encryption: raw serialized
                // records.
                rusqlite::types::Value::Blob(bytes) => bytes,
                // Already sealed by a run that did not get to write the
                // marker, or by the app itself after an interrupted
                // migration.
                rusqlite::types::Value::Text(text) if self.already_sealed(&text) => continue,
                rusqlite::types::Value::Text(text) => text.into_bytes(),
                // Nothing else belongs in these columns.
                _ => continue,
            };
            let sealed = self.seal_bytes(&plaintext)?;
            self.with(|c| {
                c.execute(
                    &format!("UPDATE {table} SET {value_col} = ?1 WHERE {key_col} = ?2"),
                    params![sealed, key],
                )
                .map(|_| ())
            })?;
            sealed_rows += 1;
        }

        self.meta_set_raw("crypto", "v1")?;
        Ok(sealed_rows)
    }

    /// Rows of the libsignal stores, which hold private key material.
    ///
    /// The value comes back untyped: before encryption these columns held raw
    /// blobs, after it they hold sealed text, and a database can contain
    /// either when a migration was interrupted.
    #[allow(clippy::type_complexity)]
    fn collect_store_rows(
        &self,
    ) -> Result<
        Vec<(
            &'static str,
            &'static str,
            &'static str,
            rusqlite::types::Value,
            rusqlite::types::Value,
        )>,
    > {
        let tables: [(&'static str, &'static str, &'static str); 5] = [
            ("sessions", "address", "record"),
            ("identities", "address", "identity"),
            ("prekeys", "id", "record"),
            ("signed_prekeys", "id", "record"),
            ("kyber_prekeys", "id", "record"),
        ];

        let mut all = Vec::new();
        for (table, key_col, value_col) in tables {
            let rows: Vec<(rusqlite::types::Value, rusqlite::types::Value)> = self.with(|c| {
                c.prepare(&format!("SELECT {key_col}, {value_col} FROM {table}"))?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect()
            })?;
            all.extend(
                rows.into_iter()
                    .map(|(key, value)| (table, key_col, value_col, key, value)),
            );
        }
        Ok(all)
    }

    pub(crate) fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&self.conn.borrow())?)
    }

    // ---- meta ----

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let stored = self.meta_get_raw(key)?;
        Ok(match stored {
            Some(value) if is_sealed_meta(key) => Some(self.unseal(&value)),
            other => other,
        })
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let stored = if is_sealed_meta(key) {
            self.seal(value)?
        } else {
            value.to_string()
        };
        self.meta_set_raw(key, &stored)
    }

    fn meta_get_raw(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        })
    }

    fn meta_set_raw(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }

    // ---- profile ----

    pub fn profile(&self) -> Result<Option<Profile>> {
        let (username, alias, server, token) = (
            self.meta_get("username")?,
            self.meta_get("alias")?,
            self.meta_get("server")?,
            self.meta_get("token")?,
        );
        Ok(match (username, alias, server, token) {
            (Some(username), Some(alias), Some(server), Some(token)) => Some(Profile {
                username,
                alias,
                server,
                token,
            }),
            _ => None,
        })
    }

    pub fn save_profile(&self, p: &Profile) -> Result<()> {
        self.meta_set("username", &p.username)?;
        self.meta_set("alias", &p.alias)?;
        self.meta_set("server", &p.server)?;
        self.meta_set("token", &p.token)
    }

    /// Wipes everything (switching to a different account on this device).
    pub fn clear_all(&self) -> Result<()> {
        self.with(|c| {
            c.execute_batch(
                "DELETE FROM meta; DELETE FROM identities; DELETE FROM sessions;
                 DELETE FROM prekeys; DELETE FROM signed_prekeys; DELETE FROM kyber_prekeys;
                 DELETE FROM kyber_base_keys_seen; DELETE FROM contacts; DELETE FROM messages;",
            )
        })
    }

    // ---- contacts ----

    pub fn upsert_contact(&self, username: &str, alias: &str, state: &str) -> Result<()> {
        let alias = self.seal(alias)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO contacts (username, alias, state) VALUES (?1, ?2, ?3)
                 ON CONFLICT(username) DO UPDATE SET alias = excluded.alias,
                                                     state = excluded.state",
                params![username, alias, state],
            )
            .map(|_| ())
        })
    }

    /// Replaces the cached list with the server's, which owns the truth.
    pub fn replace_contacts(&self, contacts: &[(String, String, String)]) -> Result<()> {
        let sealed: Vec<(String, String, String)> = contacts
            .iter()
            .map(|(u, a, s)| Ok((u.clone(), self.seal(a)?, s.clone())))
            .collect::<Result<_>>()?;
        self.with(|c| {
            c.execute("DELETE FROM contacts", [])?;
            for (username, alias, state) in &sealed {
                c.execute(
                    "INSERT INTO contacts (username, alias, state) VALUES (?1, ?2, ?3)",
                    params![username, alias, state],
                )?;
            }
            Ok(())
        })
    }

    /// Cached contacts as `(username, alias, state)`.
    pub fn contacts(&self) -> Result<Vec<(String, String, String)>> {
        let rows: Vec<(String, String, String)> = self.with(|c| {
            c.prepare("SELECT username, alias, state FROM contacts ORDER BY username")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })?;
        Ok(rows
            .into_iter()
            .map(|(u, alias, s)| {
                let alias = self.unseal(&alias);
                (u, alias, s)
            })
            .collect())
    }

    // ---- messages ----

    pub fn add_message(&self, m: &StoredMessage) -> Result<()> {
        let text = self.seal(&m.text)?;
        let m = &StoredMessage {
            text,
            ..m.clone()
        };
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO messages
                 (id, contact, mine, kind, text, state, delivered_at, read_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    m.id,
                    m.contact,
                    m.mine as i64,
                    m.kind,
                    m.text,
                    m.state,
                    m.delivered_at,
                    m.read_at,
                    m.created_at
                ],
            )
            .map(|_| ())
        })
    }

    /// Every stored message, used to re-upload the archive under a new key.
    pub fn all_messages(&self) -> Result<Vec<StoredMessage>> {
        let rows: Vec<StoredMessage> = self.with(|c| {
            c.prepare(
                "SELECT id, contact, mine, kind, text, state, delivered_at, read_at, created_at
                 FROM messages
                 ORDER BY created_at",
            )?
            .query_map([], |r| {
                Ok(StoredMessage {
                    id: r.get(0)?,
                    contact: r.get(1)?,
                    mine: r.get::<_, i64>(2)? != 0,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    state: r.get(5)?,
                    delivered_at: r.get(6)?,
                    read_at: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?
            .collect()
        })?;
        Ok(rows.into_iter().map(|m| self.decrypt_message(m)).collect())
    }

    /// Advances the delivery state of one of our messages. States only move
    /// forward, so a late "delivered" cannot undo a "read".
    pub fn set_message_state(&self, id: &str, state: &str, at: Option<i64>) -> Result<()> {
        let rank = |s: &str| match s {
            "read" => 2,
            "delivered" => 1,
            _ => 0,
        };
        self.with(|c| {
            let current: Option<String> = c
                .query_row("SELECT state FROM messages WHERE id = ?1", [id], |r| r.get(0))
                .optional()?;
            if current.is_some_and(|c| rank(&c) >= rank(state)) {
                return Ok(());
            }
            // Reaching "read" implies delivery, even if that notice was lost.
            let column = match state {
                "read" => "read_at",
                "delivered" => "delivered_at",
                _ => {
                    return c
                        .execute("UPDATE messages SET state = ?1 WHERE id = ?2", params![state, id])
                        .map(|_| ())
                }
            };
            c.execute(
                &format!(
                    "UPDATE messages SET state = ?1, {column} = COALESCE(?2, {column}),
                     delivered_at = COALESCE(delivered_at, ?2) WHERE id = ?3"
                ),
                params![state, at, id],
            )
            .map(|_| ())
        })
    }

    /// Ids of messages received from `contact` that we have not reported as
    /// read yet.
    /// Our messages to `contact` that the other device never acknowledged,
    /// oldest first. Used to send them again once a broken session is rebuilt.
    pub fn undelivered_to(&self, contact: &str) -> Result<Vec<StoredMessage>> {
        let rows: Vec<StoredMessage> = self.with(|c| {
            c.prepare(
                "SELECT id, contact, mine, kind, text, state, delivered_at, read_at, created_at
                 FROM messages
                 WHERE contact = ?1 AND mine = 1 AND state = 'sent'
                 ORDER BY created_at",
            )?
            .query_map([contact], |r| {
                Ok(StoredMessage {
                    id: r.get(0)?,
                    contact: r.get(1)?,
                    mine: r.get::<_, i64>(2)? != 0,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    state: r.get(5)?,
                    delivered_at: r.get(6)?,
                    read_at: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?
            .collect()
        })?;
        Ok(rows.into_iter().map(|m| self.decrypt_message(m)).collect())
    }

    /// Points a stored message at the id the server gave it when it was sent
    /// again, so the conversation keeps one entry and receipts still land.
    pub fn reassign_message_id(&self, old: &str, new: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE messages SET id = ?1 WHERE id = ?2",
                params![new, old],
            )
            .map(|_| ())
        })
    }

    /// Which of the account's devices this installation is, once the server
    /// has assigned it one.
    pub fn device_id(&self) -> Result<Option<i64>> {
        Ok(self.meta_get("device_id")?.and_then(|v| v.parse().ok()))
    }

    pub fn set_device_id(&self, device: i64) -> Result<()> {
        self.meta_set("device_id", &device.to_string())
    }

    pub fn unread_from(&self, contact: &str) -> Result<Vec<String>> {
        self.with(|c| {
            c.prepare(
                "SELECT id FROM messages WHERE contact = ?1 AND mine = 0 AND state != 'read'",
            )?
            .query_map([contact], |r| r.get(0))?
            .collect()
        })
    }

    /// Turns a row read from disk back into a readable message.
    fn decrypt_message(&self, mut m: StoredMessage) -> StoredMessage {
        m.text = self.unseal(&m.text);
        m
    }

    /// Removes a message from the local history.
    pub fn delete_message(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM messages WHERE id = ?1", [id])
                .map(|_| ())
        })
    }

    /// Who a message was exchanged with, and whether we sent it.
    pub fn message_peer(&self, id: &str) -> Result<Option<(String, bool)>> {
        self.with(|c| {
            c.query_row(
                "SELECT contact, mine FROM messages WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .optional()
        })
    }

    pub fn history(&self, contact: &str) -> Result<Vec<StoredMessage>> {
        let rows: Vec<StoredMessage> = self.with(|c| {
            c.prepare(
                "SELECT id, contact, mine, kind, text, state, delivered_at, read_at, created_at
                 FROM messages
                 WHERE contact = ?1 ORDER BY created_at",
            )?
            .query_map([contact], |r| {
                Ok(StoredMessage {
                    id: r.get(0)?,
                    contact: r.get(1)?,
                    mine: r.get::<_, i64>(2)? != 0,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    state: r.get(5)?,
                    delivered_at: r.get(6)?,
                    read_at: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?
            .collect()
        })?;
        Ok(rows.into_iter().map(|m| self.decrypt_message(m)).collect())
    }

    /// Next free id for a prekey table (ids grow monotonically).
    pub(crate) fn next_id(&self, table: &str) -> Result<u32> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT COALESCE(MAX(id), 0) + 1 FROM {table}"),
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v as u32)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database whose rows are sealed but whose migration marker is gone —
    /// an interrupted first run leaves exactly that — must open, and must not
    /// encrypt everything a second time, which would make it unreadable.
    #[test]
    fn a_migration_that_runs_twice_leaves_the_data_readable() {
        let dir = std::env::temp_dir().join(format!("hush-remigrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hush.db");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("key"));

        {
            let db = LocalDb::open(&path).unwrap();
            db.meta_set("archive_key", "la clave").unwrap();
            db.upsert_contact("bob", "Roberto", "accepted").unwrap();
            db.add_message(&StoredMessage {
                id: "1".into(),
                contact: "bob".into(),
                mine: true,
                kind: "text".into(),
                text: "hola".into(),
                state: "sent".into(),
                delivered_at: None,
                read_at: None,
                created_at: 1,
            })
            .unwrap();
            // Something the libsignal stores would have written, sealed.
            let sealed = db.seal_bytes(b"un registro").unwrap();
            db.with(|c| {
                c.execute(
                    "INSERT INTO sessions (address, record) VALUES ('bob:1', ?1)",
                    params![sealed],
                )
                .map(|_| ())
            })
            .unwrap();
            // The marker is what a crash between sealing and committing loses.
            db.with(|c| c.execute("DELETE FROM meta WHERE key = 'crypto'", []).map(|_| ()))
                .unwrap();
        }

        let db = LocalDb::open(&path).unwrap();
        assert_eq!(db.meta_get("archive_key").unwrap().as_deref(), Some("la clave"));
        assert_eq!(db.contacts().unwrap()[0].1, "Roberto");
        assert_eq!(db.history("bob").unwrap()[0].text, "hola");
        let record: String = db
            .with(|c| c.query_row("SELECT record FROM sessions", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(db.open_bytes(&record).unwrap(), b"un registro");

        std::fs::remove_dir_all(&dir).ok();
    }
}
