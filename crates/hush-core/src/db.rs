//! Local client database (SQLite via rusqlite): identity and protocol state,
//! account profile, contacts and message history.
//!
//! Note: message history is stored in plaintext — it is the local user's own
//! data on their own device. At-rest encryption (SQLCipher) is future work.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result};
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
    -- For our own messages: 'sent', 'delivered' or 'read'.
    state      TEXT NOT NULL DEFAULT 'sent',
    created_at INTEGER NOT NULL
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

#[derive(Clone)]
pub struct LocalDb {
    conn: Rc<RefCell<Connection>>,
}

impl LocalDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("create data dir")?;
        }
        let conn = Connection::open(path).context("open local db")?;
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
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN state TEXT NOT NULL DEFAULT 'sent'",
            [],
        );
        Ok(Self {
            conn: Rc::new(RefCell::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Rc::new(RefCell::new(conn)),
        })
    }

    pub(crate) fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&self.conn.borrow())?)
    }

    // ---- meta ----

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        })
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
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
        self.with(|c| {
            c.execute("DELETE FROM contacts", [])?;
            for (username, alias, state) in contacts {
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
        self.with(|c| {
            c.prepare("SELECT username, alias, state FROM contacts ORDER BY username")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })
    }

    // ---- messages ----

    pub fn add_message(&self, m: &StoredMessage) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO messages (id, contact, mine, kind, text, state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![m.id, m.contact, m.mine as i64, m.kind, m.text, m.state, m.created_at],
            )
            .map(|_| ())
        })
    }

    /// Every stored message, used to re-upload the archive under a new key.
    pub fn all_messages(&self) -> Result<Vec<StoredMessage>> {
        self.with(|c| {
            c.prepare(
                "SELECT id, contact, mine, kind, text, state, created_at FROM messages
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
                    created_at: r.get(6)?,
                })
            })?
            .collect()
        })
    }

    /// Advances the delivery state of one of our messages. States only move
    /// forward, so a late "delivered" cannot undo a "read".
    pub fn set_message_state(&self, id: &str, state: &str) -> Result<()> {
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
            c.execute(
                "UPDATE messages SET state = ?1 WHERE id = ?2",
                params![state, id],
            )
            .map(|_| ())
        })
    }

    /// Ids of messages received from `contact` that we have not reported as
    /// read yet.
    pub fn unread_from(&self, contact: &str) -> Result<Vec<String>> {
        self.with(|c| {
            c.prepare(
                "SELECT id FROM messages WHERE contact = ?1 AND mine = 0 AND state != 'read'",
            )?
            .query_map([contact], |r| r.get(0))?
            .collect()
        })
    }

    pub fn history(&self, contact: &str) -> Result<Vec<StoredMessage>> {
        self.with(|c| {
            c.prepare(
                "SELECT id, contact, mine, kind, text, state, created_at FROM messages
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
                    created_at: r.get(6)?,
                })
            })?
            .collect()
        })
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
