//! High-level client actor.
//!
//! libsignal's async functions take `&mut dyn ...Store` without a `Send`
//! bound, so futures touching the [`Engine`] cannot cross threads. This
//! module runs the engine on a dedicated thread with a single-threaded
//! runtime; [`HushClient`] is a cheap, `Send + Clone` handle that talks to it
//! over channels, usable from any async context (e.g. tauri commands).

use std::path::PathBuf;

use tokio::sync::{mpsc, oneshot};

use crate::db::{LocalDb, Profile, StoredMessage};
use crate::transfer::{Export, ExportedContact, ExportedMessage};
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
    /// A contact is publishing an identity key that is not the one we pinned.
    ///
    /// Either they reinstalled, or somebody is standing between us. Nothing
    /// is sent to them and nothing of theirs is read until the person using
    /// the app decides which it was, so this goes to the interface rather
    /// than being resolved quietly. It is also written into the conversation,
    /// under `id`, so it is still there once the dialog is gone.
    IdentityChanged {
        id: String,
        contact: String,
        /// Fingerprints of the key we trusted and the one being offered, to
        /// be read out over some other channel and compared.
        known: String,
        published: String,
        at: i64,
    },
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

/// Why a message could not be encrypted for its recipient.
enum SendRefused {
    /// The contact is publishing a key we never agreed to. Held back until
    /// the user says whether it is really them.
    IdentityChanged { known: String, published: String },
    Other(anyhow::Error),
}

/// Wire format inside the encrypted envelope. Plain (non-JSON) payloads from
/// older clients are treated as text.
fn encode_content(kind: &str, content: &str) -> Vec<u8> {
    serde_json::json!({ "kind": kind, "content": content })
        .to_string()
        .into_bytes()
}

/// The kind of a conversation entry that nobody wrote: a note that the
/// contact's key changed, kept alongside the messages it sits between.
pub const KEY_CHANGED: &str = "keychange";

/// Largest profile picture we will accept from a contact, as a data URL.
/// Generous for a portrait, small enough that a contact cannot use it to fill
/// this device's storage.
const MAX_AVATAR_BYTES: usize = 512 * 1024;

/// Whether a string is a picture we are willing to put in an `<img>`.
///
/// A profile picture arrives from the contact, and the interface hands it
/// straight to the webview. Anything but an inline image would be fetched
/// from wherever it points, which turns the picture into a beacon: whoever
/// chose it learns the address of everyone who so much as opens their
/// conversation list, and roughly when. That is precisely the metadata this
/// app exists to keep to itself, so only `data:image/…` is allowed through.
pub(crate) fn is_picture(data_url: &str) -> bool {
    data_url.starts_with("data:image/") && data_url.len() <= MAX_AVATAR_BYTES
}

/// A name for this device, for the server's log. Nothing sensitive: the
/// computer's name is what makes one sign-in recognisable from another.
fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
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
    /// Our own profile picture as a data URL, kept locally and sent to
    /// contacts encrypted.
    pub avatar: Option<String>,
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
    /// Packs every conversation into an encrypted file.
    ExportConversations {
        password: String,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    /// Merges an exported file into this device's conversations.
    ImportConversations {
        bytes: Vec<u8>,
        password: String,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    /// Accepts a contact's new identity key after the user was shown that it
    /// changed and said it was really them.
    AcceptIdentity {
        contact: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Ends the session, here and on the server. Conversations stay.
    Logout {
        reply: oneshot::Sender<Result<(), String>>,
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
    /// Wipes a whole conversation here, and optionally asks the other side to
    /// drop our messages from theirs.
    DeleteConversation {
        contact: String,
        for_everyone: bool,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    /// Our own profile picture, handed to every accepted contact.
    SetAvatar {
        avatar: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
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
    /// Why the engine thread stopped, if it did. Without it every call after
    /// a crash reads "engine closed", which says nothing about the cause.
    fatal: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl HushClient {
    /// Starts the engine actor on its own thread. `db_path` is the SQLite
    /// file holding all local state (identity, sessions, history).
    pub fn spawn(db_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel(64);
        let fatal = std::sync::Arc::new(std::sync::Mutex::new(None));
        let reported = fatal.clone();
        std::thread::Builder::new()
            .name("hush-engine".into())
            .spawn(move || {
                // A panic in here would otherwise just close the channel, and
                // the app would report nothing but "engine closed".
                let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("build engine runtime");
                    rt.block_on(actor(db_path, rx));
                }));
                if let Err(panic) = crashed {
                    let reason = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "the engine stopped unexpectedly".to_string());
                    tracing::error!("engine thread panicked: {reason}");
                    *reported.lock().expect("fatal slot") = Some(reason);
                }
            })
            .expect("spawn engine thread");
        Self { tx, fatal }
    }

    /// What went wrong, when the engine is no longer answering.
    fn closed(&self) -> String {
        match self.fatal.lock().expect("fatal slot").clone() {
            Some(reason) => reason,
            None => "engine closed".to_string(),
        }
    }

    async fn request<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Command,
    ) -> Result<T, String> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(make(reply)).await.map_err(|_| self.closed())?;
        rx.await.map_err(|_| self.closed())?
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

    /// Packs every conversation on this device into an encrypted file,
    /// readable only with `password`.
    pub async fn export_conversations(&self, password: &str) -> Result<Vec<u8>, String> {
        let password = password.to_string();
        self.request(|reply| Command::ExportConversations { password, reply })
            .await
    }

    /// Merges a file produced by [`export_conversations`](Self::export_conversations)
    /// into this device. Returns how many messages it did not already have.
    pub async fn import_conversations(
        &self,
        bytes: Vec<u8>,
        password: &str,
    ) -> Result<usize, String> {
        let password = password.to_string();
        self.request(|reply| Command::ImportConversations {
            bytes,
            password,
            reply,
        })
        .await
    }

    /// Accepts the new identity key a contact is publishing, after the user
    /// has been shown the change and confirmed it was really them.
    pub async fn accept_identity(&self, contact: &str) -> Result<(), String> {
        let contact = contact.to_string();
        self.request(|reply| Command::AcceptIdentity { contact, reply })
            .await
    }

    /// Ends the session. The conversations on this device stay where they
    /// are: the server holds no copy, so wiping them would be destroying the
    /// only one there is.
    pub async fn logout(&self) -> Result<(), String> {
        self.request(|reply| Command::Logout { reply }).await
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

    /// Deletes the whole conversation with `contact` from this device. With
    /// `for_everyone`, our own messages are withdrawn from their device too;
    /// theirs stay where they are, which is not ours to decide.
    /// Returns how many messages were removed here.
    pub async fn delete_conversation(
        &self,
        contact: &str,
        for_everyone: bool,
    ) -> Result<usize, String> {
        let contact = contact.to_string();
        self.request(|reply| Command::DeleteConversation {
            contact,
            for_everyone,
            reply,
        })
        .await
    }

    /// Sets our profile picture, or clears it with `None`, and sends it to
    /// every accepted contact.
    pub async fn set_avatar(&self, avatar: Option<String>) -> Result<(), String> {
        self.request(|reply| Command::SetAvatar { avatar, reply }).await
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
}

struct Actor {
    db: LocalDb,
    pending: Option<Pending>,
    session: Option<Session>,
    events: Option<mpsc::Sender<ClientEvent>>,
    /// When we last rebuilt the session with each contact, so a backlog of
    /// unreadable messages does not trigger one repair per message.
    repairs: std::collections::HashMap<String, i64>,
    /// Contacts whose published identity does not match the one we pinned,
    /// and which key they are offering. Nothing is sent to them until the
    /// user has looked at it, and this stops the interface being asked the
    /// same question once per message.
    disputed: std::collections::HashMap<String, String>,
}

/// How long to wait before rebuilding the session with the same contact again.
const REPAIR_INTERVAL_MS: i64 = 60_000;
/// An account has one device, and it is this one.
const THE_DEVICE: u32 = 1;

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
        let engine = Engine::open(self.db.clone(), username, THE_DEVICE)?;
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
        self.session = Some(Session {
            engine: pending.engine,
            api: pending.api,
        });
        Ok(())
    }

    /// Packs every conversation on this device into an encrypted file.
    ///
    /// The server keeps no history, so this is the only way conversations
    /// move to another device — and the only thing standing between the file
    /// and whoever picks it up is the password, which is why the packing is
    /// deliberately slow. See [`crate::transfer`].
    fn export_conversations(&self, password: &str) -> anyhow::Result<Vec<u8>> {
        let messages: Vec<ExportedMessage> = self
            .db
            .all_messages()?
            .iter()
            .filter(|m| !Self::is_control(&m.kind))
            .map(ExportedMessage::from)
            .collect();
        let contacts: Vec<ExportedContact> = self
            .db
            .contacts()?
            .into_iter()
            .map(|(username, alias, _)| ExportedContact {
                avatar: self.db.contact_avatar(&username).unwrap_or(None),
                username,
                alias,
            })
            .collect();
        let username = self.db.profile()?.map(|p| p.username).unwrap_or_default();

        tracing::info!("exporting {} messages", messages.len());
        crate::transfer::export(
            &Export {
                username,
                exported_at: now_ms(),
                messages,
                contacts,
            },
            password,
        )
    }

    /// Merges an exported file into this device, returning how many messages
    /// were new. Anything already here is left alone: the local copy knows
    /// its own delivery state, and the file is a snapshot of an older one.
    fn import_conversations(&self, bytes: &[u8], password: &str) -> anyhow::Result<usize> {
        let opened = crate::transfer::import(bytes, password)?;
        let before = self.db.all_messages()?.len();
        for message in opened.messages {
            let stored = StoredMessage::from(message);
            if let Err(e) = self.db.add_message(&stored) {
                tracing::warn!("cannot store an imported message: {e}");
            }
        }
        // Pictures and names travel too, so an import on a device that has
        // never spoken to these people still shows something recognisable.
        for contact in opened.contacts {
            if let Some(avatar) = contact.avatar.as_deref().filter(|a| is_picture(a)) {
                let _ = self.db.set_contact_avatar(&contact.username, Some(avatar));
            }
        }
        let added = self.db.all_messages()?.len().saturating_sub(before);
        tracing::info!("imported {added} new messages");
        Ok(added)
    }

    /// Adopts `code` as the history key: pulls the archive down, merges it
    /// into the local database, and re-uploads anything this device had
    /// archived under its previous key so nothing is orphaned.
    /// Ends the session here and on the server.
    ///
    /// The conversations stay: the server keeps no copy of them, so deleting
    /// them on the way out would be destroying the only one that exists —
    /// that is what the export is for, and it is a separate decision.
    async fn handle_logout(&mut self) -> anyhow::Result<()> {
        if let Some(session) = self.session.as_ref() {
            // Best effort: being unable to reach the server is not a reason
            // to leave the user signed in on their own machine.
            if let Err(e) = session.api.logout().await {
                tracing::warn!("the server did not confirm the sign-out: {e}");
            }
        }
        self.session = None;
        self.events = None;
        self.pending = None;
        self.disputed.clear();
        self.repairs.clear();
        self.db.forget_token()?;
        tracing::info!("signed out");
        Ok(())
    }

    async fn handle_login(
        &mut self,
        server: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<ProfileInfo> {
        let server = server.trim_end_matches('/');
        // Whether this device already belongs to the account, which is not
        // the same as it holding a session: signing out drops the token but
        // keeps the conversations, and signing back in must find them.
        let same_account = self
            .db
            .account()?
            .is_some_and(|(user, host)| user == username && host == server);
        if !same_account {
            // Different account (or first login on this device): clean slate.
            self.db.clear_all()?;
            self.session = None;
        }

        // Signing in here signs out wherever the account was before: it is one
        // device at a time, and this is now the one.
        let mut api = ApiClient::new(server);
        api.login(username, password, &device_name()).await?;
        let mut engine = Engine::open(self.db.clone(), username, THE_DEVICE)?;

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

        Ok(ProfileInfo {
            username: profile.username,
            alias: profile.alias,
            server: profile.server,
            status: self
                .db
                .meta_get("status")?
                .unwrap_or_else(|| "online".into()),
            avatar: self.db.meta_get("avatar")?,
        })
    }

    async fn handle_connect(&mut self) -> anyhow::Result<mpsc::Receiver<ServerEvent>> {
        let profile = self
            .db
            .profile()?
            .ok_or_else(|| anyhow::anyhow!("no account on this device"))?;
        if self.session.is_none() {
            let engine = Engine::open(self.db.clone(), &profile.username, THE_DEVICE)?;
            let mut api = ApiClient::new(&profile.server);
            api.set_token(&profile.token);
            self.session = Some(Session { engine, api });
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
        matches!(kind, "delete" | "rekey" | "avatar")
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
    ///
    /// Only the ratchet is dropped. The contact's pinned identity survives,
    /// which matters more than it looks: an unreadable message is something
    /// anyone relaying for us can produce at will, and if that were enough to
    /// forget who the contact is, the next bundle would be trusted on sight
    /// and the relay would have chosen when to be believed. Rebuilding under
    /// the same identity needs nothing more than this; a genuinely new key
    /// goes to the user instead, through [`Self::dispute`].
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
            if let Err(e) = session.engine.reset_session(peer, THE_DEVICE) {
                tracing::warn!("cannot drop the stale session with {peer}: {e}");
                return;
            }
        }
        tracing::info!("rebuilding the session with {peer} after an unreadable message");
        if let Err(e) = self.handle_send(peer, "rekey", "").await {
            tracing::warn!("cannot rebuild the session with {peer}: {e}");
        }
    }

    /// Accepts the key a contact is now publishing, after the user confirmed
    /// the change was really them. Everything held for the old key goes, and
    /// the next message negotiates afresh.
    async fn accept_identity(&mut self, contact: &str) -> anyhow::Result<()> {
        self.disputed.remove(contact);
        if let Some(session) = self.session.as_mut() {
            session.engine.forget_identity(contact, THE_DEVICE)?;
        }
        tracing::warn!("the user accepted a new identity key for {contact}");
        // Nudge the conversation back to life under the new key.
        if let Err(e) = self.handle_send(contact, "rekey", "").await {
            tracing::warn!("cannot rebuild the session with {contact}: {e}");
        }
        Ok(())
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
        let payload = encode_content(kind, content);
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no session; log in first"))?;

        // Refused outright while the contact's key is in dispute: sending
        // would mean encrypting to whoever holds the new one.
        if self.disputed.contains_key(recipient) {
            anyhow::bail!("identity_changed");
        }

        let remote = session.api.fetch_profile(recipient).await?;
        let envelope = Self::encrypt_for(session, recipient, &payload).await;
        let envelope = match envelope {
            Ok(envelope) => envelope,
            Err(SendRefused::IdentityChanged { known, published }) => {
                self.dispute(recipient, &known, &published).await;
                anyhow::bail!("identity_changed");
            }
            Err(SendRefused::Other(e)) => return Err(e),
        };
        let id = session.api.send_message(recipient, &envelope).await?;
        self.db.upsert_contact(recipient, &remote.alias, "accepted")?;
        Ok(id)
    }

    /// Records that a contact's key changed and puts it to the user.
    ///
    /// It also goes into the conversation itself, and stays there. The dialog
    /// can be dismissed and forgotten; a line in the chat is still there
    /// tomorrow, next to the messages that came before and after it, which is
    /// where somebody would go looking if they later wondered.
    async fn dispute(&mut self, contact: &str, known: &str, published: &str) {
        if self
            .disputed
            .insert(contact.to_string(), published.to_string())
            .as_deref()
            == Some(published)
        {
            // Already asked about this exact key; asking again per message
            // would bury the question under its own repetitions.
            return;
        }
        tracing::warn!("the identity key of {contact} changed; waiting for the user to decide");

        let at = now_ms();
        let fingerprint = Engine::fingerprint(published);
        // Local only: it is never sent, and its id is ours rather than the
        // server's, so re-reading the conversation does not duplicate it.
        let notice = StoredMessage {
            id: format!("keychange-{contact}-{at}"),
            contact: contact.to_string(),
            mine: false,
            kind: KEY_CHANGED.to_string(),
            text: fingerprint.clone(),
            state: "delivered".to_string(),
            delivered_at: Some(at),
            read_at: None,
            created_at: at,
        };
        if let Err(e) = self.db.add_message(&notice) {
            tracing::warn!("cannot record the key change of {contact}: {e}");
        }

        if let Some(events) = &self.events {
            let _ = events
                .send(ClientEvent::IdentityChanged {
                    id: notice.id,
                    contact: contact.to_string(),
                    known: Engine::fingerprint(known),
                    published: fingerprint,
                    at,
                })
                .await;
        }
    }

    /// Encrypts `payload` for `peer`, establishing the session if there is
    /// none yet.
    ///
    /// A bundle whose identity key is not the one we pinned stops here. It may
    /// be an honest reinstall, or it may be the server handing us a key of its
    /// own so that it can read what we send next; nothing in the bundle can
    /// tell the two apart, and only the person using the app can. Adopting it
    /// quietly — which is what this used to do — meant the server could arrange
    /// to be trusted whenever it liked.
    async fn encrypt_for(
        session: &mut Session,
        peer: &str,
        payload: &[u8],
    ) -> Result<String, SendRefused> {
        let bundle = session
            .api
            .fetch_bundle(peer)
            .await
            .map_err(SendRefused::Other)?;

        if let (Some(known), Some(published)) = (
            session
                .engine
                .known_identity_b64(peer, THE_DEVICE)
                .await
                .map_err(SendRefused::Other)?,
            bundle["identity_key"].as_str(),
        ) {
            if !published.is_empty() && known != published {
                return Err(SendRefused::IdentityChanged {
                    known,
                    published: published.to_string(),
                });
            }
        }

        if !session
            .engine
            .has_session(peer, THE_DEVICE)
            .await
            .map_err(SendRefused::Other)?
        {
            session
                .engine
                .ensure_session(peer, THE_DEVICE, &bundle)
                .await
                .map_err(SendRefused::Other)?;
        }
        session
            .engine
            .encrypt(peer, THE_DEVICE, payload)
            .await
            .map_err(SendRefused::Other)
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
                    if let Some(events) = &self.events {
                        let _ = events
                            .send(ClientEvent::MessageResent {
                                old_id: msg.id.clone(),
                                new_id: id.clone(),
                            })
                            .await;
                    }
                    msg.id = id;
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

    /// Wipes the conversation with `contact` from this device, and with
    /// `for_everyone` asks them to drop the messages we sent.
    ///
    /// Only our own can be withdrawn: theirs are theirs. Each withdrawal is an
    /// encrypted control message, so a long conversation costs one message per
    /// entry, sent before anything is removed here — once it is gone locally
    /// there is nothing left to name.
    async fn handle_delete_conversation(
        &mut self,
        contact: &str,
        for_everyone: bool,
    ) -> anyhow::Result<usize> {
        let history = self.db.history(contact)?;

        if for_everyone {
            for message in history.iter().filter(|m| m.mine) {
                if let Err(e) = self.handle_send(contact, "delete", &message.id).await {
                    tracing::warn!("cannot withdraw {} from {contact}: {e}", message.id);
                }
            }
        }

        for message in &history {
            let _ = self.db.delete_message(&message.id);
        }
        tracing::info!("deleted {} messages of the conversation with {contact}", history.len());
        Ok(history.len())
    }

    /// Sets our own profile picture and hands it to every accepted contact.
    ///
    /// It travels as an encrypted message like any other, so the server never
    /// sees it. An empty picture clears it, on our side and on theirs.
    async fn handle_set_avatar(&mut self, avatar: Option<String>) -> anyhow::Result<()> {
        match &avatar {
            Some(data) => self.db.meta_set("avatar", data)?,
            None => self.db.meta_delete("avatar")?,
        }
        let payload = avatar.unwrap_or_default();

        let contacts: Vec<String> = self
            .db
            .contacts()?
            .into_iter()
            .filter(|(_, _, state)| state == "accepted")
            .map(|(username, _, _)| username)
            .collect();
        for contact in contacts {
            if let Err(e) = self.handle_send(&contact, "avatar", &payload).await {
                // One contact being unreachable must not stop the rest.
                tracing::warn!("cannot send our picture to {contact}: {e}");
            }
        }
        Ok(())
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
    /// Also reports which contacts have just become accepted, since that is
    /// the moment they can be handed our profile picture: before it, sending
    /// anything to them is refused.
    async fn sync_contacts(&self) -> Option<(Vec<ContactEntry>, Vec<String>)> {
        let mut entries = self.session.as_ref()?.api.list_contacts().await.ok()?;
        let known: std::collections::HashMap<String, String> = self
            .db
            .contacts()
            .unwrap_or_default()
            .into_iter()
            .map(|(username, _, state)| (username, state))
            .collect();
        let newly_accepted: Vec<String> = entries
            .iter()
            .filter(|e| {
                e.state == "accepted"
                    && known.get(&e.username).is_none_or(|state| state != "accepted")
            })
            .map(|e| e.username.clone())
            .collect();
        let cached: Vec<(String, String, String)> = entries
            .iter()
            .map(|c| (c.username.clone(), c.alias.clone(), c.state.clone()))
            .collect();
        if let Err(e) = self.db.replace_contacts(&cached) {
            tracing::warn!("cannot cache contacts: {e}");
        }
        for entry in &mut entries {
            entry.avatar = self.db.contact_avatar(&entry.username).unwrap_or(None);
        }
        Some((entries, newly_accepted))
    }

    /// Hands our profile picture to contacts that have just become reachable.
    /// Without this a picture set before someone accepted us would never
    /// arrive: it is only ever sent when it changes.
    async fn share_avatar_with(&mut self, contacts: &[String]) {
        if contacts.is_empty() {
            return;
        }
        let Ok(Some(avatar)) = self.db.meta_get("avatar") else {
            return;
        };
        for contact in contacts {
            if let Err(e) = self.handle_send(contact, "avatar", &avatar).await {
                tracing::warn!("cannot send our picture to {contact}: {e}");
            }
        }
    }

    /// The contact list: the server's when reachable, the cache otherwise.
    async fn contacts(&mut self) -> Result<Vec<ContactEntry>, String> {
        if let Some((entries, newly_accepted)) = self.sync_contacts().await {
            self.share_avatar_with(&newly_accepted).await;
            return Ok(entries);
        }
        self.db
            .contacts()
            .map(|rows| {
                rows.into_iter()
                    .map(|(username, alias, state)| ContactEntry {
                        avatar: self.db.contact_avatar(&username).unwrap_or(None),
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

    /// Returns the stored message so the caller can act on it once the
    /// session borrow is released.
    async fn handle_incoming(&mut self, msg: IncomingMessage) -> Option<StoredMessage> {
        let session = self.session.as_mut()?;
        let device = msg.sender_device as u32;
        match session.engine.decrypt(&msg.sender, device, &msg.body).await {
            Ok(plain) => {
                let _ = session.api.ack_message(&msg.id).await;
                let (kind, text) = decode_content(&plain);

                // Decrypting it already adopted the sender's new session, so
                // what we send next is readable on their side — including
                // whatever they never managed to receive.
                if kind == "rekey" {
                    tracing::info!("{}/{} rebuilt the session with us", msg.sender, device);
                    let sender = msg.sender.clone();
                    self.resend_undelivered(&sender).await;
                    return None;
                }

                // Their profile picture, which reaches us the same way a
                // message does and never passes through the server in the
                // clear.
                if kind == "avatar" {
                    // Only an inline image. The interface hands this to an
                    // <img>, so anything pointing elsewhere would be fetched
                    // from there — see `is_picture`.
                    let avatar = match text.as_str() {
                        "" => None,
                        picture if is_picture(picture) => Some(picture),
                        _ => {
                            tracing::warn!(
                                "{} sent something that is not an inline picture; ignored",
                                msg.sender
                            );
                            return None;
                        }
                    };
                    if let Err(e) = self.db.set_contact_avatar(&msg.sender, avatar) {
                        tracing::warn!("cannot store the picture of {}: {e}", msg.sender);
                    }
                    if let Some(events) = &self.events {
                        let _ = events.try_send(ClientEvent::ContactsChanged);
                    }
                    return None;
                }

                // A control message, not something to show: the sender
                // deleted a message and wants our copy gone too.
                if kind == "delete" {
                    let _ = self.db.delete_message(&text);
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
                tracing::warn!(
                    "failed to decrypt message {} from {}/{}: {e}",
                    msg.id,
                    msg.sender,
                    device
                );
                // Drop it rather than acknowledge it: it would otherwise be
                // redelivered forever, but telling the sender it arrived would
                // also stop them from sending it again once we can read them.
                let _ = session.api.discard_message(&msg.id).await;

                // Two very different things look the same from here: a ratchet
                // one of us lost, or somebody else holding the other end. The
                // published key tells them apart, and only the first is ours
                // to fix — rebuilding on a key we never agreed to would be
                // adopting whoever sent this.
                let sender = msg.sender.clone();
                let published = session
                    .api
                    .fetch_profile(&sender)
                    .await
                    .ok()
                    .map(|p| p.identity_key);
                let known = session
                    .engine
                    .known_identity_b64(&sender, THE_DEVICE)
                    .await
                    .ok()
                    .flatten();
                if let (Some(known), Some(published)) = (known, published) {
                    if !published.is_empty() && known != published {
                        self.dispute(&sender, &known, &published).await;
                        return None;
                    }
                }
                self.repair_session(&sender).await;
                None
            }
        }
    }
}

/// Answers every command with the same failure, so a broken start shows the
/// user what is wrong instead of an empty channel.
async fn report_fatal(mut commands: mpsc::Receiver<Command>, reason: String) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::LoadProfile { reply } => drop(reply.send(Err(reason.clone()))),
            Command::Register { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::Verify { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::Login { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::ForgotPassword { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::ResetPassword { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::Connect { reply } => drop(reply.send(Err(reason.clone()))),
            Command::Send { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::RequestContact { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::AcceptContact { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::RemoveContact { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::BlockContact { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::DeleteMessage { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::Contacts { reply } => drop(reply.send(Err(reason.clone()))),
            Command::DeleteConversation { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::SetAvatar { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::ExportConversations { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::ImportConversations { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::AcceptIdentity { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::Logout { reply } => drop(reply.send(Err(reason.clone()))),
            Command::History { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::MarkRead { reply, .. } => drop(reply.send(Err(reason.clone()))),
            Command::UpdateMe { reply, .. } => drop(reply.send(Err(reason.clone()))),
        }
    }
}

async fn actor(db_path: PathBuf, mut commands: mpsc::Receiver<Command>) {
    let db = match LocalDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            // Dying here would close the channel, and every call would come
            // back as "engine closed" with no hint of what actually went
            // wrong. Stay up and hand the real reason to whoever asks.
            let reason = format!("cannot open local storage at {}: {e:#}", db_path.display());
            tracing::error!("{reason}");
            report_fatal(commands, reason).await;
            return;
        }
    };
    let mut actor = Actor {
        db,
        pending: None,
        session: None,
        events: None,
        repairs: std::collections::HashMap::new(),
        disputed: std::collections::HashMap::new(),
    };
    let mut stream: Option<mpsc::Receiver<ServerEvent>> = None;

    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                None => break,
                Some(Command::LoadProfile { reply }) => {
                    let status = actor.db.meta_get("status").ok().flatten();
                    let avatar = actor.db.meta_get("avatar").unwrap_or(None);
                    let result = actor.db.profile().map(|p| p.map(|p| ProfileInfo {
                        username: p.username,
                        alias: p.alias,
                        server: p.server,
                        status: status.unwrap_or_else(|| "online".into()),
                        avatar,
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
                Some(Command::ExportConversations { password, reply }) => {
                    let result = actor.export_conversations(&password);
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::ImportConversations { bytes, password, reply }) => {
                    let result = actor.import_conversations(&bytes, &password);
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::AcceptIdentity { contact, reply }) => {
                    let result = actor.accept_identity(&contact.to_lowercase()).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::Logout { reply }) => {
                    let result = actor.handle_logout().await;
                    // The event stream belonged to the session that just
                    // ended; leaving it open would keep pushing into a
                    // conversation nobody is signed in to.
                    stream = None;
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
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::RequestContact { username, reply }) => {
                    let username = username.to_lowercase();
                    let result = match actor.session.as_ref() {
                        None => Err("no_session".to_string()),
                        Some(s) => s.api.request_contact(&username).await.map_err(|e| e.to_string()),
                    };
                    if result.is_ok() {
                        if let Some((_, newly_accepted)) = actor.sync_contacts().await {
                            actor.share_avatar_with(&newly_accepted).await;
                        }
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
                        if let Some((_, newly_accepted)) = actor.sync_contacts().await {
                            actor.share_avatar_with(&newly_accepted).await;
                        }
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
                        if let Some((_, newly_accepted)) = actor.sync_contacts().await {
                            actor.share_avatar_with(&newly_accepted).await;
                        }
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
                        if let Some((_, newly_accepted)) = actor.sync_contacts().await {
                            actor.share_avatar_with(&newly_accepted).await;
                        }
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
                Some(Command::DeleteConversation { contact, for_everyone, reply }) => {
                    let result = actor
                        .handle_delete_conversation(&contact.to_lowercase(), for_everyone)
                        .await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::SetAvatar { avatar, reply }) => {
                    let result = actor.handle_set_avatar(avatar).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
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
                        actor.handle_incoming(msg).await;
                    }
                    Some(ServerEvent::ContactsChanged) => {
                        if let Some((_, newly_accepted)) = actor.sync_contacts().await {
                            actor.share_avatar_with(&newly_accepted).await;
                        }
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
