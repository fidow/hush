//! High-level client actor.
//!
//! libsignal's async functions take `&mut dyn ...Store` without a `Send`
//! bound, so futures touching the [`Engine`] cannot cross threads. This
//! module runs the engine on a dedicated thread with a single-threaded
//! runtime; [`HushClient`] is a cheap, `Send + Clone` handle that talks to it
//! over channels, usable from any async context (e.g. tauri commands).

use std::path::PathBuf;

use tokio::sync::{mpsc, oneshot};

use crate::archive::{ArchiveEntry, ArchiveKey};
use crate::db::{LocalDb, Profile, StoredMessage};
use crate::{ApiClient, ContactEntry, Engine, IncomingMessage, ServerEvent};

/// What the UI is told about, over the event channel.
#[derive(Clone, Debug)]
pub enum ClientEvent {
    Message(DecryptedMessage),
    /// The other side deleted a message for everyone.
    MessageDeleted { id: String },
    /// A message was sent again after rebuilding a session, so it now travels
    /// under a different id.
    MessageResent { old_id: String, new_id: String },
    /// The contact list changed (a request arrived, was accepted, …).
    ContactsChanged,
    /// One of our messages was delivered or read.
    Receipt { id: String, state: String, at: i64 },
}

/// A decrypted incoming message, ready for display.
#[derive(Clone, Debug)]
pub struct DecryptedMessage {
    pub id: String,
    pub sender: String,
    /// "text" or "image" (image content is a data URL).
    pub kind: String,
    pub text: String,
    pub created_at: i64,
}

/// Wire format inside the encrypted envelope. Plain (non-JSON) payloads from
/// older clients are treated as text.
fn encode_content(kind: &str, content: &str) -> Vec<u8> {
    serde_json::json!({ "kind": kind, "content": content })
        .to_string()
        .into_bytes()
}

fn decode_content(plain: &[u8]) -> (String, String) {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(plain) {
        if let (Some(kind), Some(content)) = (v["kind"].as_str(), v["content"].as_str()) {
            return (kind.to_string(), content.to_string());
        }
    }
    ("text".to_string(), String::from_utf8_lossy(plain).into_owned())
}

/// The locally stored account, as reported to the UI.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProfileInfo {
    pub username: String,
    pub alias: String,
    pub server: String,
    /// Presence the user last chose on this device.
    pub status: String,
}

enum Command {
    LoadProfile {
        reply: oneshot::Sender<Result<Option<ProfileInfo>, String>>,
    },
    Register {
        server: String,
        username: String,
        alias: String,
        email: String,
        password: String,
        reply: oneshot::Sender<Result<Option<String>, String>>,
    },
    Verify {
        code: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Login {
        server: String,
        username: String,
        password: String,
        reply: oneshot::Sender<Result<ProfileInfo, String>>,
    },
    ForgotPassword {
        server: String,
        username: String,
        reply: oneshot::Sender<Result<Option<String>, String>>,
    },
    ResetPassword {
        server: String,
        username: String,
        code: String,
        password: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// The recovery key held by this device, shown to the user on request.
    RecoveryCode {
        reply: oneshot::Sender<Result<String, String>>,
    },
    /// Adopts a recovery key and pulls the archive down. Usable at any time,
    /// not just right after signing in.
    RestoreHistory {
        code: String,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    Connect {
        reply: oneshot::Sender<Result<mpsc::Receiver<ClientEvent>, String>>,
    },
    Send {
        recipient: String,
        kind: String,
        content: String,
        reply: oneshot::Sender<Result<StoredMessage, String>>,
    },
    /// Sends a contact request (or accepts one already waiting from them).
    RequestContact {
        username: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    AcceptContact {
        username: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Rejects a request, cancels ours, removes a contact, or lifts a block.
    RemoveContact {
        username: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    BlockContact {
        username: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Deletes a message locally and, when `for_everyone`, asks the other
    /// side to delete their copy too.
    DeleteMessage {
        id: String,
        for_everyone: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Contacts {
        reply: oneshot::Sender<Result<Vec<ContactEntry>, String>>,
    },
    History {
        contact: String,
        reply: oneshot::Sender<Result<Vec<StoredMessage>, String>>,
    },
    /// Reports every unread message from `contact` as read.
    MarkRead {
        contact: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    UpdateMe {
        alias: Option<String>,
        status: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Clone)]
pub struct HushClient {
    tx: mpsc::Sender<Command>,
}

impl HushClient {
    /// Starts the engine actor on its own thread. `db_path` is the SQLite
    /// file holding all local state (identity, sessions, history).
    pub fn spawn(db_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel(64);
        std::thread::Builder::new()
            .name("hush-engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build engine runtime");
                rt.block_on(actor(db_path, rx));
            })
            .expect("spawn engine thread");
        Self { tx }
    }

    async fn request<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Command,
    ) -> Result<T, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(make(reply))
            .await
            .map_err(|_| "engine closed".to_string())?;
        rx.await.map_err(|_| "engine closed".to_string())?
    }

    /// The account stored on this device, if any.
    pub async fn load_profile(&self) -> Result<Option<ProfileInfo>, String> {
        self.request(|reply| Command::LoadProfile { reply }).await
    }

    /// Creates a new account (pending email verification). Wipes any previous
    /// local state. Returns the dev verification code if the server echoes it.
    pub async fn register(
        &self,
        server: &str,
        username: &str,
        alias: &str,
        email: &str,
        password: &str,
    ) -> Result<Option<String>, String> {
        let (server, username, alias, email, password) = (
            server.to_string(),
            username.to_string(),
            alias.to_string(),
            email.to_string(),
            password.to_string(),
        );
        self.request(|reply| Command::Register {
            server,
            username,
            alias,
            email,
            password,
            reply,
        })
        .await
    }

    /// Confirms the account with the emailed code, publishes prekeys and
    /// saves the profile locally. Call [`Self::connect`] afterwards.
    pub async fn verify(&self, code: &str) -> Result<(), String> {
        let code = code.to_string();
        self.request(|reply| Command::Verify { code, reply }).await
    }

    /// Logs into an existing account. If this device already holds keys for
    /// that username they are reused; otherwise fresh keys are generated and
    /// published (contacts will renegotiate sessions transparently). History
    /// is restored separately with [`Self::restore_history`].
    pub async fn login(
        &self,
        server: &str,
        username: &str,
        password: &str,
    ) -> Result<ProfileInfo, String> {
        let (server, username, password) =
            (server.to_string(), username.to_string(), password.to_string());
        self.request(|reply| Command::Login {
            server,
            username,
            password,
            reply,
        })
        .await
    }

    /// Requests a password reset code by email.
    pub async fn forgot_password(
        &self,
        server: &str,
        username: &str,
    ) -> Result<Option<String>, String> {
        let (server, username) = (server.to_string(), username.to_string());
        self.request(|reply| Command::ForgotPassword {
            server,
            username,
            reply,
        })
        .await
    }

    /// Sets a new password using the emailed code.
    pub async fn reset_password(
        &self,
        server: &str,
        username: &str,
        code: &str,
        password: &str,
    ) -> Result<(), String> {
        let (server, username, code, password) = (
            server.to_string(),
            username.to_string(),
            code.to_string(),
            password.to_string(),
        );
        self.request(|reply| Command::ResetPassword {
            server,
            username,
            code,
            password,
            reply,
        })
        .await
    }

    /// The recovery key of this device, formatted for the user to copy.
    pub async fn recovery_code(&self) -> Result<String, String> {
        self.request(|reply| Command::RecoveryCode { reply }).await
    }

    /// Adopts `code` as the history key and downloads the archive. Returns
    /// how many messages were restored.
    pub async fn restore_history(&self, code: &str) -> Result<usize, String> {
        let code = code.to_string();
        self.request(|reply| Command::RestoreHistory { code, reply })
            .await
    }

    /// Opens the message stream for the locally stored account. The returned
    /// channel yields decrypted incoming messages and closes on disconnect.
    pub async fn connect(&self) -> Result<mpsc::Receiver<ClientEvent>, String> {
        self.request(|reply| Command::Connect { reply }).await
    }

    /// Encrypts and sends `text`, establishing or renegotiating the session
    /// as needed. Returns the stored message for display.
    pub async fn send_text(&self, recipient: &str, text: &str) -> Result<StoredMessage, String> {
        self.send_content(recipient, "text", text).await
    }

    /// Encrypts and sends an image given as a data URL.
    pub async fn send_image(&self, recipient: &str, data_url: &str) -> Result<StoredMessage, String> {
        if !data_url.starts_with("data:image/") {
            return Err("El contenido pegado no es una imagen".to_string());
        }
        self.send_content(recipient, "image", data_url).await
    }

    async fn send_content(
        &self,
        recipient: &str,
        kind: &str,
        content: &str,
    ) -> Result<StoredMessage, String> {
        let (recipient, kind, content) =
            (recipient.to_string(), kind.to_string(), content.to_string());
        self.request(|reply| Command::Send {
            recipient,
            kind,
            content,
            reply,
        })
        .await
    }

    /// Sends a contact request; returns the resulting state ("outgoing", or
    /// "accepted" when the other side had already asked).
    pub async fn request_contact(&self, username: &str) -> Result<String, String> {
        let username = username.to_string();
        self.request(|reply| Command::RequestContact { username, reply })
            .await
    }

    pub async fn accept_contact(&self, username: &str) -> Result<(), String> {
        let username = username.to_string();
        self.request(|reply| Command::AcceptContact { username, reply })
            .await
    }

    /// Rejects a request, cancels one we sent, or removes a contact.
    pub async fn remove_contact(&self, username: &str) -> Result<(), String> {
        let username = username.to_string();
        self.request(|reply| Command::RemoveContact { username, reply })
            .await
    }

    /// Blocks a peer: they stop being a contact and cannot reach us again.
    pub async fn block_contact(&self, username: &str) -> Result<(), String> {
        let username = username.to_string();
        self.request(|reply| Command::BlockContact { username, reply })
            .await
    }

    /// Deletes a message. With `for_everyone` the other side is asked to
    /// delete their copy as well.
    pub async fn delete_message(&self, id: &str, for_everyone: bool) -> Result<(), String> {
        let id = id.to_string();
        self.request(|reply| Command::DeleteMessage {
            id,
            for_everyone,
            reply,
        })
        .await
    }

    /// The contact list, refreshed from the server when reachable.
    pub async fn contacts(&self) -> Result<Vec<ContactEntry>, String> {
        self.request(|reply| Command::Contacts { reply }).await
    }

    pub async fn history(&self, contact: &str) -> Result<Vec<StoredMessage>, String> {
        let contact = contact.to_string();
        self.request(|reply| Command::History { contact, reply })
            .await
    }

    /// Reports every unread message from `contact` as read.
    pub async fn mark_read(&self, contact: &str) -> Result<(), String> {
        let contact = contact.to_string();
        self.request(|reply| Command::MarkRead { contact, reply })
            .await
    }

    /// Changes the display name and/or presence of the local account.
    pub async fn update_me(
        &self,
        alias: Option<String>,
        status: Option<String>,
    ) -> Result<(), String> {
        self.request(|reply| Command::UpdateMe {
            alias,
            status,
            reply,
        })
        .await
    }

}

struct Session {
    engine: Engine,
    api: ApiClient,
}

/// Account created but not yet verified: keys exist locally, no token yet.
struct Pending {
    engine: Engine,
    api: ApiClient,
    username: String,
    alias: String,
    server: String,
    archive_key: ArchiveKey,
}

struct Actor {
    db: LocalDb,
    pending: Option<Pending>,
    session: Option<Session>,
    events: Option<mpsc::Sender<ClientEvent>>,
    /// Key protecting the history archive; absent until the user provides
    /// their recovery key on this device.
    archive_key: Option<ArchiveKey>,
    /// When we last rebuilt the session with each contact, so a backlog of
    /// unreadable messages does not trigger one repair per message.
    repairs: std::collections::HashMap<String, i64>,
}

/// How long to wait before rebuilding the session with the same contact again.
const REPAIR_INTERVAL_MS: i64 = 60_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

impl Actor {
    async fn handle_register(
        &mut self,
        server: &str,
        username: &str,
        alias: &str,
        email: &str,
        password: &str,
    ) -> anyhow::Result<Option<String>> {
        // A new account means a clean slate on this device.
        self.db.clear_all()?;
        self.session = None;
        self.archive_key = None;
        // The history key is random, never derived from anything the user
        // typed, and shown to them later as a recovery code.
        let archive_key = ArchiveKey::generate();
        let engine = Engine::open(self.db.clone(), username)?;
        let mut api = ApiClient::new(server.trim_end_matches('/'));
        let dev_code = api
            .register(
                username,
                alias,
                email,
                password,
                engine.registration_id().await?,
                &engine.identity_key_b64().await?,
            )
            .await?;
        self.pending = Some(Pending {
            engine,
            api,
            username: username.to_string(),
            alias: alias.to_string(),
            server: server.trim_end_matches('/').to_string(),
            archive_key,
        });
        Ok(dev_code)
    }

    async fn handle_verify(&mut self, code: &str) -> anyhow::Result<()> {
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no pending registration"))?;
        pending.api.verify(&pending.username, code).await?;
        let keys = pending.engine.generate_prekeys(20).await?;
        pending.api.upload_keys(&keys).await?;
        let pending = self.pending.take().expect("checked");
        self.db.save_profile(&Profile {
            username: pending.username,
            alias: pending.alias,
            server: pending.server,
            token: pending.api.token().unwrap_or_default().to_string(),
        })?;
        self.db
            .meta_set("archive_key", &pending.archive_key.to_b64())?;
        self.archive_key = Some(pending.archive_key);
        self.session = Some(Session {
            engine: pending.engine,
            api: pending.api,
        });
        Ok(())
    }

    /// Re-encrypts a message under the history key and uploads it. Best
    /// effort: a failure here must never break sending or receiving.
    async fn archive_message(&self, msg: &StoredMessage) {
        if Self::is_control(&msg.kind) {
            return;
        }
        let (Some(key), Some(session)) = (self.archive_key.as_ref(), self.session.as_ref()) else {
            return;
        };
        let blob = match key.encrypt_entry(&ArchiveEntry::from(msg)) {
            Ok(blob) => blob,
            Err(e) => {
                tracing::warn!("cannot encrypt history entry: {e}");
                return;
            }
        };
        if let Err(e) = session.api.put_archive(&msg.id, &blob).await {
            tracing::warn!("cannot upload history entry: {e}");
        }
    }

    /// Adopts `code` as the history key: pulls the archive down, merges it
    /// into the local database, and re-uploads anything this device had
    /// archived under its previous key so nothing is orphaned.
    async fn handle_restore(&mut self, code: &str) -> anyhow::Result<usize> {
        let key = ArchiveKey::from_recovery_code(code)?;
        let restored = self.restore_archive(&key).await?;
        self.db.meta_set("archive_key", &key.to_b64())?;
        self.archive_key = Some(key);
        // Back-fill: anything this device holds but never archived (because
        // it had no key) goes up now.
        for msg in self.db.all_messages()? {
            self.archive_message(&msg).await;
        }
        Ok(restored)
    }

    /// Downloads the encrypted archive and merges it into the local database.
    /// Returns how many messages were restored.
    async fn restore_archive(&self, key: &ArchiveKey) -> anyhow::Result<usize> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no session"))?;
        let entries = session.api.list_archive().await?;
        let mut restored = 0;
        let mut failed = 0;
        for (_, blob) in &entries {
            match key.decrypt_entry(blob) {
                Ok(entry) => {
                    let msg = StoredMessage::from(entry);
                    // Control messages were archived by versions that treated
                    // them as ordinary ones; drop them here and clean up.
                    if Self::is_control(&msg.kind) {
                        let _ = session.api.delete_archive_entry(&msg.id).await;
                        continue;
                    }
                    // Remember the contact too, so restored chats are listed.
                    let _ = self.db.upsert_contact(&msg.contact, &msg.contact, "accepted");
                    if self.db.add_message(&msg).is_ok() {
                        restored += 1;
                    }
                }
                Err(_) => failed += 1,
            }
        }
        // All entries failing means the key is wrong, not that data is bad.
        if restored == 0 && failed > 0 {
            anyhow::bail!("wrong_recovery_key");
        }
        tracing::info!("restored {restored} archived messages ({failed} unreadable)");
        Ok(restored)
    }

    async fn handle_login(
        &mut self,
        server: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<ProfileInfo> {
        let server = server.trim_end_matches('/');
        let same_account = self
            .db
            .profile()?
            .is_some_and(|p| p.username == username && p.server == server);
        if !same_account {
            // Different account (or first login on this device): clean slate.
            self.db.clear_all()?;
            self.session = None;
            self.archive_key = None;
        }

        let mut engine = Engine::open(self.db.clone(), username)?;
        let mut api = ApiClient::new(server);
        api.login(username, password).await?;

        if !same_account {
            // Fresh keys for this device; publishing them also updates our
            // identity on the server so contacts renegotiate sessions.
            let keys = engine.generate_prekeys(20).await?;
            api.upload_keys(&keys).await?;
        }

        let alias = api.fetch_profile(username).await?.alias;
        let profile = Profile {
            username: username.to_string(),
            alias,
            server: server.to_string(),
            token: api.token().unwrap_or_default().to_string(),
        };
        self.db.save_profile(&profile)?;
        self.session = Some(Session { engine, api });

        // The recovery key belongs to the *account*, not the device: it is
        // created once at registration. A device that doesn't hold it yet
        // archives nothing, because entries written under a second key would
        // be unreadable with the key the user actually kept. Restoring with
        // the real code adopts it and back-fills anything received meanwhile.
        self.archive_key = match self.db.meta_get("archive_key")? {
            Some(stored) => Some(ArchiveKey::from_b64(&stored)?),
            None => {
                tracing::info!("no recovery key on this device; history archiving is paused");
                None
            }
        };

        Ok(ProfileInfo {
            username: profile.username,
            alias: profile.alias,
            server: profile.server,
            status: self
                .db
                .meta_get("status")?
                .unwrap_or_else(|| "online".into()),
        })
    }

    async fn handle_connect(&mut self) -> anyhow::Result<mpsc::Receiver<ServerEvent>> {
        let profile = self
            .db
            .profile()?
            .ok_or_else(|| anyhow::anyhow!("no account on this device"))?;
        if self.session.is_none() {
            let engine = Engine::open(self.db.clone(), &profile.username)?;
            let mut api = ApiClient::new(&profile.server);
            api.set_token(&profile.token);
            self.session = Some(Session { engine, api });
        }
        if self.archive_key.is_none() {
            if let Some(stored) = self.db.meta_get("archive_key")? {
                self.archive_key = ArchiveKey::from_b64(&stored).ok();
            }
        }
        // Re-assert the chosen presence: the server only reports it while a
        // stream is open, so it must be refreshed on every reconnect.
        if let (Some(status), Some(session)) = (self.db.meta_get("status")?, self.session.as_ref())
        {
            let _ = session.api.update_me(None, Some(&status)).await;
        }
        let session = self.session.as_ref().expect("just set");
        Ok(session.api.stream().await?)
    }

    /// Instructions for the other device rather than something a person wrote.
    /// They travel as ordinary encrypted messages so the server cannot tell
    /// them apart, but they belong in no conversation.
    fn is_control(kind: &str) -> bool {
        matches!(kind, "delete" | "rekey")
    }

    /// Rebuilds the session with `peer` after a message we could not decrypt.
    ///
    /// That happens when they still hold a ratchet we no longer have — their
    /// device kept the session while ours lost it, or a handshake never
    /// arrived. Dropping those messages silently, as we must, would leave the
    /// conversation dead in that direction for good. Sending anything back
    /// carries a fresh handshake, which replaces the session on their side
    /// too, so the next message they send is readable.
    ///
    /// The message that triggered this is still lost: it was encrypted for a
    /// ratchet that no longer exists.
    async fn repair_session(&mut self, peer: &str) {
        let now = now_ms();
        if let Some(last) = self.repairs.get(peer) {
            // A burst of undecryptable messages is one broken session, not one
            // per message.
            if now.saturating_sub(*last) < REPAIR_INTERVAL_MS {
                return;
            }
        }
        self.repairs.insert(peer.to_string(), now);

        if let Some(session) = self.session.as_mut() {
            if let Err(e) = session.engine.reset_session(peer) {
                tracing::warn!("cannot drop the stale session with {peer}: {e}");
                return;
            }
        }
        tracing::info!("rebuilding the session with {peer} after an unreadable message");
        if let Err(e) = self.handle_send(peer, "rekey", "").await {
            tracing::warn!("cannot rebuild the session with {peer}: {e}");
        }
    }

    /// Encrypts and sends, returning the id the server assigned. Whether the
    /// message belongs in the conversation is the caller's business: a resend
    /// updates the row it already has.
    async fn send_envelope(
        &mut self,
        recipient: &str,
        kind: &str,
        content: &str,
    ) -> anyhow::Result<String> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no session; log in first"))?;

        // Detect a contact who re-provisioned keys (e.g. new device): their
        // published identity no longer matches the one we have on record.
        let remote = session.api.fetch_profile(recipient).await?;
        if let Some(known) = session.engine.known_identity_b64(recipient).await? {
            if !remote.identity_key.is_empty() && known != remote.identity_key {
                tracing::info!("identity of {recipient} changed; renegotiating session");
                session.engine.reset_session(recipient)?;
            }
        }
        if !session.engine.has_session(recipient).await? {
            let bundle = session.api.fetch_bundle(recipient).await?;
            session.engine.ensure_session(recipient, &bundle).await?;
        }

        let envelope = session
            .engine
            .encrypt(recipient, &encode_content(kind, content))
            .await?;
        let id = session.api.send_message(recipient, &envelope).await?;
        self.db.upsert_contact(recipient, &remote.alias, "accepted")?;
        Ok(id)
    }

    async fn handle_send(
        &mut self,
        recipient: &str,
        kind: &str,
        content: &str,
    ) -> anyhow::Result<StoredMessage> {
        let id = self.send_envelope(recipient, kind, content).await?;
        let stored = StoredMessage {
            id,
            contact: recipient.to_string(),
            mine: true,
            kind: kind.to_string(),
            text: content.to_string(),
            // It reached the server; the recipient's device confirms later.
            state: "sent".to_string(),
            delivered_at: None,
            read_at: None,
            created_at: now_ms(),
        };
        // A control message would otherwise show up in our own conversation as
        // a line containing the id of the message it refers to.
        if !Self::is_control(kind) {
            self.db.add_message(&stored)?;
        }
        Ok(stored)
    }

    /// Sends again what `peer` never acknowledged.
    ///
    /// Called once their session has been rebuilt: those messages were
    /// encrypted for a ratchet they no longer have, so they would never
    /// arrive. The stored message follows the id the server gives the new
    /// copy, keeping one entry in the conversation and letting receipts land.
    async fn resend_undelivered(&mut self, peer: &str) {
        let pending = match self.db.undelivered_to(peer) {
            Ok(pending) => pending,
            Err(e) => {
                tracing::warn!("cannot look up undelivered messages for {peer}: {e}");
                return;
            }
        };
        if pending.is_empty() {
            return;
        }
        tracing::info!("sending {} unacknowledged messages to {peer} again", pending.len());

        for mut msg in pending {
            match self.send_envelope(peer, &msg.kind, &msg.text).await {
                Ok(id) => {
                    if let Err(e) = self.db.reassign_message_id(&msg.id, &id) {
                        tracing::warn!("cannot follow the resent message: {e}");
                        continue;
                    }
                    if let Some(session) = self.session.as_ref() {
                        let _ = session.api.delete_archive_entry(&msg.id).await;
                    }
                    if let Some(events) = &self.events {
                        let _ = events
                            .send(ClientEvent::MessageResent {
                                old_id: msg.id.clone(),
                                new_id: id.clone(),
                            })
                            .await;
                    }
                    msg.id = id;
                    self.archive_message(&msg).await;
                }
                Err(e) => {
                    // The session is broken again, or the server is gone;
                    // either way the rest will not fare better.
                    tracing::warn!("cannot resend to {peer}: {e}");
                    return;
                }
            }
        }
    }

    /// Deletes a message here and, for `for_everyone`, tells the other side.
    ///
    /// The instruction travels as an ordinary encrypted message with
    /// `kind = "delete"`, so the server never learns which message was
    /// deleted, and it queues like any other if they are offline.
    async fn handle_delete(&mut self, id: &str, for_everyone: bool) -> anyhow::Result<()> {
        let peer = self
            .db
            .message_peer(id)?
            .ok_or_else(|| anyhow::anyhow!("message_not_found"))?;
        let (contact, mine) = peer;
        if for_everyone && !mine {
            anyhow::bail!("not_your_message");
        }

        self.db.delete_message(id)?;
        // Drop it from our archive too, or restoring history would bring it
        // back on the next device.
        if let Some(session) = self.session.as_ref() {
            let _ = session.api.delete_archive_entry(id).await;
        }

        if for_everyone {
            self.handle_send(&contact, "delete", id).await?;
        }
        Ok(())
    }

    /// Reports the unread messages from `contact` as read, both locally and
    /// to the sender.
    async fn handle_mark_read(&self, contact: &str) -> Result<(), String> {
        let session = self.session.as_ref().ok_or("no_session")?;
        let unread = self.db.unread_from(contact).map_err(|e| e.to_string())?;
        for id in unread {
            // A failed receipt is not worth failing the read for; the message
            // is marked locally either way so we don't retry forever.
            let _ = session.api.mark_read(&id).await;
            let _ = self.db.set_message_state(&id, "read", Some(now_ms()));
        }
        Ok(())
    }

    /// Pulls the contact list from the server into the local cache. Failures
    /// are tolerated: the cache keeps the app usable while offline.
    async fn sync_contacts(&self) -> Option<Vec<ContactEntry>> {
        let entries = self.session.as_ref()?.api.list_contacts().await.ok()?;
        let cached: Vec<(String, String, String)> = entries
            .iter()
            .map(|c| (c.username.clone(), c.alias.clone(), c.state.clone()))
            .collect();
        if let Err(e) = self.db.replace_contacts(&cached) {
            tracing::warn!("cannot cache contacts: {e}");
        }
        Some(entries)
    }

    /// The contact list: the server's when reachable, the cache otherwise.
    async fn contacts(&self) -> Result<Vec<ContactEntry>, String> {
        if let Some(entries) = self.sync_contacts().await {
            return Ok(entries);
        }
        self.db
            .contacts()
            .map(|rows| {
                rows.into_iter()
                    .map(|(username, alias, state)| ContactEntry {
                        username,
                        alias,
                        state,
                        // The cache only knows who they are, not where.
                        status: "offline".into(),
                        last_seen: None,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    /// Returns the stored message so the caller can archive it once the
    /// session borrow is released.
    async fn handle_incoming(&mut self, msg: IncomingMessage) -> Option<StoredMessage> {
        let session = self.session.as_mut()?;
        match session.engine.decrypt(&msg.sender, &msg.body).await {
            Ok(plain) => {
                let _ = session.api.ack_message(&msg.id).await;
                let (kind, text) = decode_content(&plain);

                // Decrypting it already adopted the sender's new session, so
                // what we send next is readable on their side — including
                // whatever they never managed to receive.
                if kind == "rekey" {
                    tracing::info!("{} rebuilt the session with us", msg.sender);
                    let sender = msg.sender.clone();
                    self.resend_undelivered(&sender).await;
                    return None;
                }

                // A control message, not something to show: the sender
                // deleted a message and wants our copy gone too.
                if kind == "delete" {
                    let _ = self.db.delete_message(&text);
                    if let Some(session) = self.session.as_ref() {
                        let _ = session.api.delete_archive_entry(&text).await;
                    }
                    if let Some(events) = &self.events {
                        let _ = events
                            .send(ClientEvent::MessageDeleted { id: text })
                            .await;
                    }
                    return None;
                }

                let stored = StoredMessage {
                    id: msg.id,
                    contact: msg.sender.clone(),
                    mine: false,
                    kind,
                    // Incoming messages become "read" when the user opens the
                    // conversation; until then this tracks what we owe.
                    state: "delivered".to_string(),
                    delivered_at: Some(now_ms()),
                    read_at: None,
                    text,
                    created_at: msg.created_at,
                };
                if let Err(e) = self.db.add_message(&stored) {
                    tracing::warn!("failed to store message: {e}");
                }
                let known = self
                    .db
                    .contacts()
                    .unwrap_or_default()
                    .iter()
                    .any(|(u, _, _)| u == &msg.sender);
                if !known {
                    // Only accepted contacts can reach us, so the cache is
                    // simply behind; fill it in.
                    let alias = session
                        .api
                        .fetch_profile(&msg.sender)
                        .await
                        .map(|p| p.alias)
                        .unwrap_or_else(|_| msg.sender.clone());
                    let _ = self.db.upsert_contact(&msg.sender, &alias, "accepted");
                }
                if let Some(events) = &self.events {
                    let _ = events
                        .send(ClientEvent::Message(DecryptedMessage {
                            id: stored.id.clone(),
                            sender: stored.contact.clone(),
                            kind: stored.kind.clone(),
                            text: stored.text.clone(),
                            created_at: stored.created_at,
                        }))
                        .await;
                }
                Some(stored)
            }
            Err(e) => {
                tracing::warn!("failed to decrypt message {} from {}: {e}", msg.id, msg.sender);
                // Drop it rather than acknowledge it: it would otherwise be
                // redelivered forever, but telling the sender it arrived would
                // also stop them from sending it again once we can read them.
                let _ = session.api.discard_message(&msg.id).await;
                self.repair_session(&msg.sender).await;
                None
            }
        }
    }
}

async fn actor(db_path: PathBuf, mut commands: mpsc::Receiver<Command>) {
    let db = match LocalDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            tracing::error!("cannot open local db {db_path:?}: {e}");
            return;
        }
    };
    let mut actor = Actor {
        db,
        pending: None,
        session: None,
        events: None,
        archive_key: None,
        repairs: std::collections::HashMap::new(),
    };
    let mut stream: Option<mpsc::Receiver<ServerEvent>> = None;

    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                None => break,
                Some(Command::LoadProfile { reply }) => {
                    let status = actor.db.meta_get("status").ok().flatten();
                    let result = actor.db.profile().map(|p| p.map(|p| ProfileInfo {
                        username: p.username,
                        alias: p.alias,
                        server: p.server,
                        status: status.unwrap_or_else(|| "online".into()),
                    }));
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::Register { server, username, alias, email, password, reply }) => {
                    let result = actor
                        .handle_register(&server, &username.to_lowercase(), &alias, &email, &password)
                        .await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::Verify { code, reply }) => {
                    let result = actor.handle_verify(&code).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::Login { server, username, password, reply }) => {
                    let result = actor
                        .handle_login(&server, &username.to_lowercase(), &password)
                        .await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::ForgotPassword { server, username, reply }) => {
                    // No session needed: this runs before signing in.
                    let api = ApiClient::new(server.trim_end_matches('/'));
                    let result = api
                        .forgot_password(&username.to_lowercase())
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                Some(Command::ResetPassword { server, username, code, password, reply }) => {
                    let api = ApiClient::new(server.trim_end_matches('/'));
                    let result = api
                        .reset_password(&username.to_lowercase(), &code, &password)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = reply.send(result);
                }
                Some(Command::RecoveryCode { reply }) => {
                    let result = actor
                        .archive_key
                        .as_ref()
                        .map(|k| k.to_recovery_code())
                        .ok_or_else(|| "no_recovery_key".to_string());
                    let _ = reply.send(result);
                }
                Some(Command::RestoreHistory { code, reply }) => {
                    let result = actor.handle_restore(&code).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::Connect { reply }) => {
                    match actor.handle_connect().await {
                        Ok(new_stream) => {
                            let (tx, rx) = mpsc::channel(256);
                            stream = Some(new_stream);
                            actor.events = Some(tx);
                            let _ = reply.send(Ok(rx));
                        }
                        Err(e) => {
                            let _ = reply.send(Err(e.to_string()));
                        }
                    }
                }
                Some(Command::Send { recipient, kind, content, reply }) => {
                    let result = actor
                        .handle_send(&recipient.to_lowercase(), &kind, &content)
                        .await;
                    if let Ok(stored) = &result {
                        actor.archive_message(stored).await;
                    }
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::RequestContact { username, reply }) => {
                    let username = username.to_lowercase();
                    let result = match actor.session.as_ref() {
                        None => Err("no_session".to_string()),
                        Some(s) => s.api.request_contact(&username).await.map_err(|e| e.to_string()),
                    };
                    if result.is_ok() {
                        actor.sync_contacts().await;
                    }
                    let _ = reply.send(result);
                }
                Some(Command::AcceptContact { username, reply }) => {
                    let username = username.to_lowercase();
                    let result = match actor.session.as_ref() {
                        None => Err("no_session".to_string()),
                        Some(s) => s.api.accept_contact(&username).await.map_err(|e| e.to_string()),
                    };
                    if result.is_ok() {
                        actor.sync_contacts().await;
                    }
                    let _ = reply.send(result);
                }
                Some(Command::RemoveContact { username, reply }) => {
                    let username = username.to_lowercase();
                    let result = match actor.session.as_ref() {
                        None => Err("no_session".to_string()),
                        Some(s) => s.api.remove_contact(&username).await.map_err(|e| e.to_string()),
                    };
                    if result.is_ok() {
                        actor.sync_contacts().await;
                    }
                    let _ = reply.send(result);
                }
                Some(Command::BlockContact { username, reply }) => {
                    let username = username.to_lowercase();
                    let result = match actor.session.as_ref() {
                        None => Err("no_session".to_string()),
                        Some(s) => s.api.block_contact(&username).await.map_err(|e| e.to_string()),
                    };
                    if result.is_ok() {
                        actor.sync_contacts().await;
                    }
                    let _ = reply.send(result);
                }
                Some(Command::DeleteMessage { id, for_everyone, reply }) => {
                    let result = actor.handle_delete(&id, for_everyone).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::Contacts { reply }) => {
                    let _ = reply.send(actor.contacts().await);
                }
                Some(Command::History { contact, reply }) => {
                    let _ = reply.send(actor.db.history(&contact).map_err(|e| e.to_string()));
                }
                Some(Command::MarkRead { contact, reply }) => {
                    let _ = reply.send(actor.handle_mark_read(&contact).await);
                }
                Some(Command::UpdateMe { alias, status, reply }) => {
                    let result = match actor.session.as_ref() {
                        None => Err("no_session".to_string()),
                        Some(s) => s
                            .api
                            .update_me(alias.as_deref(), status.as_deref())
                            .await
                            .map_err(|e| e.to_string()),
                    };
                    // Keep the locally stored profile in step with the server.
                    if result.is_ok() {
                        if let (Some(alias), Ok(Some(mut profile))) = (&alias, actor.db.profile()) {
                            profile.alias = alias.clone();
                            let _ = actor.db.save_profile(&profile);
                        }
                        if let Some(status) = &status {
                            let _ = actor.db.meta_set("status", status);
                        }
                    }
                    let _ = reply.send(result);
                }
            },
            event = async { stream.as_mut().expect("guarded by if").recv().await }, if stream.is_some() => {
                match event {
                    Some(ServerEvent::Message(msg)) => {
                        if let Some(stored) = actor.handle_incoming(msg).await {
                            actor.archive_message(&stored).await;
                        }
                    }
                    Some(ServerEvent::ContactsChanged) => {
                        actor.sync_contacts().await;
                        if let Some(events) = &actor.events {
                            let _ = events.send(ClientEvent::ContactsChanged).await;
                        }
                    }
                    Some(ServerEvent::Receipt { id, state, at }) => {
                        let _ = actor.db.set_message_state(&id, &state, Some(at));
                        if let Some(events) = &actor.events {
                            let _ = events.send(ClientEvent::Receipt { id, state, at }).await;
                        }
                    }
                    None => {
                        // Server connection dropped: closing `events` tells the UI.
                        stream = None;
                        actor.events = None;
                    }
                }
            }
        }
    }
}
