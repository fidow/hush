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
use crate::{ApiClient, Engine, IncomingMessage};

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
        reply: oneshot::Sender<Result<mpsc::Receiver<DecryptedMessage>, String>>,
    },
    Send {
        recipient: String,
        kind: String,
        content: String,
        reply: oneshot::Sender<Result<StoredMessage, String>>,
    },
    AddContact {
        username: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Contacts {
        reply: oneshot::Sender<Result<Vec<(String, String)>, String>>,
    },
    History {
        contact: String,
        reply: oneshot::Sender<Result<Vec<StoredMessage>, String>>,
    },
    UpdateMe {
        alias: Option<String>,
        status: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Presence {
        reply: oneshot::Sender<Result<Vec<(String, String)>, String>>,
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
    pub async fn connect(&self) -> Result<mpsc::Receiver<DecryptedMessage>, String> {
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

    /// Validates the user exists, stores it as contact, returns its alias.
    pub async fn add_contact(&self, username: &str) -> Result<String, String> {
        let username = username.to_string();
        self.request(|reply| Command::AddContact { username, reply })
            .await
    }

    pub async fn contacts(&self) -> Result<Vec<(String, String)>, String> {
        self.request(|reply| Command::Contacts { reply }).await
    }

    pub async fn history(&self, contact: &str) -> Result<Vec<StoredMessage>, String> {
        let contact = contact.to_string();
        self.request(|reply| Command::History { contact, reply })
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

    /// Presence of every stored contact, as `username -> status`.
    pub async fn presence(&self) -> Result<Vec<(String, String)>, String> {
        self.request(|reply| Command::Presence { reply }).await
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
    events: Option<mpsc::Sender<DecryptedMessage>>,
    /// Key protecting the history archive; absent until the user provides
    /// their history passphrase on this device.
    archive_key: Option<ArchiveKey>,
}

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
                    // Remember the contact too, so restored chats are listed.
                    let _ = self.db.upsert_contact(&msg.contact, &msg.contact);
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

    async fn handle_connect(&mut self) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
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

    async fn handle_send(
        &mut self,
        recipient: &str,
        kind: &str,
        content: &str,
    ) -> anyhow::Result<StoredMessage> {
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
        let stored = StoredMessage {
            id,
            contact: recipient.to_string(),
            mine: true,
            kind: kind.to_string(),
            text: content.to_string(),
            created_at: now_ms(),
        };
        self.db.add_message(&stored)?;
        self.db.upsert_contact(recipient, &remote.alias)?;
        Ok(stored)
    }

    /// Returns the stored message so the caller can archive it once the
    /// session borrow is released.
    async fn handle_incoming(&mut self, msg: IncomingMessage) -> Option<StoredMessage> {
        let session = self.session.as_mut()?;
        match session.engine.decrypt(&msg.sender, &msg.body).await {
            Ok(plain) => {
                let _ = session.api.ack_message(&msg.id).await;
                let (kind, text) = decode_content(&plain);
                let stored = StoredMessage {
                    id: msg.id,
                    contact: msg.sender.clone(),
                    mine: false,
                    kind,
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
                    .any(|(u, _)| u == &msg.sender);
                if !known {
                    let alias = session
                        .api
                        .fetch_profile(&msg.sender)
                        .await
                        .map(|p| p.alias)
                        .unwrap_or_else(|_| msg.sender.clone());
                    let _ = self.db.upsert_contact(&msg.sender, &alias);
                }
                if let Some(events) = &self.events {
                    let _ = events
                        .send(DecryptedMessage {
                            id: stored.id.clone(),
                            sender: stored.contact.clone(),
                            kind: stored.kind.clone(),
                            text: stored.text.clone(),
                            created_at: stored.created_at,
                        })
                        .await;
                }
                Some(stored)
            }
            Err(e) => {
                tracing::warn!("failed to decrypt message {} from {}: {e}", msg.id, msg.sender);
                // Ack anyway: an undecryptable message (stale session from a
                // previous device) would otherwise be redelivered forever.
                let _ = session.api.ack_message(&msg.id).await;
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
    };
    let mut stream: Option<mpsc::Receiver<IncomingMessage>> = None;

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
                Some(Command::AddContact { username, reply }) => {
                    let username = username.to_lowercase();
                    let result = match actor.session.as_ref() {
                        None => Err("no session".to_string()),
                        Some(s) => match s.api.fetch_profile(&username).await {
                            Ok(p) => actor
                                .db
                                .upsert_contact(&username, &p.alias)
                                .map(|_| p.alias)
                                .map_err(|e| e.to_string()),
                            Err(e) => Err(e.to_string()),
                        },
                    };
                    let _ = reply.send(result);
                }
                Some(Command::Contacts { reply }) => {
                    let _ = reply.send(actor.db.contacts().map_err(|e| e.to_string()));
                }
                Some(Command::History { contact, reply }) => {
                    let _ = reply.send(actor.db.history(&contact).map_err(|e| e.to_string()));
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
                Some(Command::Presence { reply }) => {
                    let result = match actor.session.as_ref() {
                        None => Err("no_session".to_string()),
                        Some(s) => {
                            let names: Vec<String> = actor
                                .db
                                .contacts()
                                .unwrap_or_default()
                                .into_iter()
                                .map(|(u, _)| u)
                                .collect();
                            if names.is_empty() {
                                Ok(Vec::new())
                            } else {
                                s.api.presence(&names).await.map_err(|e| e.to_string())
                            }
                        }
                    };
                    let _ = reply.send(result);
                }
            },
            msg = async { stream.as_mut().expect("guarded by if").recv().await }, if stream.is_some() => {
                match msg {
                    Some(msg) => {
                        if let Some(stored) = actor.handle_incoming(msg).await {
                            actor.archive_message(&stored).await;
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
