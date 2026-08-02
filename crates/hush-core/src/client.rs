//! High-level client actor.
//!
//! libsignal's async functions take `&mut dyn ...Store` without a `Send`
//! bound, so futures touching the [`Engine`] cannot cross threads. This
//! module runs the engine on a dedicated thread with a single-threaded
//! runtime; [`HushClient`] is a cheap, `Send + Clone` handle that talks to it
//! over channels, usable from any async context (e.g. tauri commands).

use tokio::sync::{mpsc, oneshot};

use crate::{ApiClient, Engine, IncomingMessage};

/// A decrypted incoming message, ready for display.
#[derive(Clone, Debug)]
pub struct DecryptedMessage {
    pub id: String,
    pub sender: String,
    pub text: String,
    pub created_at: i64,
}

enum Command {
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
        reply: oneshot::Sender<Result<mpsc::Receiver<DecryptedMessage>, String>>,
    },
    Send {
        recipient: String,
        text: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Profile {
        username: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
}

#[derive(Clone)]
pub struct HushClient {
    tx: mpsc::Sender<Command>,
}

impl HushClient {
    /// Starts the engine actor on its own thread.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel(64);
        std::thread::Builder::new()
            .name("hush-engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build engine runtime");
                rt.block_on(actor(rx));
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

    /// Creates the account (pending email verification). Returns the dev
    /// verification code when the server echoes it.
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

    /// Confirms the account with the emailed code, publishes prekeys and opens
    /// the message stream. The returned channel yields decrypted incoming
    /// messages and closes if the server connection drops.
    pub async fn verify(&self, code: &str) -> Result<mpsc::Receiver<DecryptedMessage>, String> {
        let code = code.to_string();
        self.request(|reply| Command::Verify { code, reply }).await
    }

    /// Encrypts and sends `text`, establishing a session first if needed.
    pub async fn send_text(&self, recipient: &str, text: &str) -> Result<(), String> {
        let (recipient, text) = (recipient.to_string(), text.to_string());
        self.request(|reply| Command::Send {
            recipient,
            text,
            reply,
        })
        .await
    }

    /// Fetches the public alias of a user (also validates that it exists).
    pub async fn fetch_alias(&self, username: &str) -> Result<String, String> {
        let username = username.to_string();
        self.request(|reply| Command::Profile { username, reply })
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
}

async fn do_register(
    server: &str,
    username: &str,
    alias: &str,
    email: &str,
    password: &str,
) -> anyhow::Result<(Pending, Option<String>)> {
    let engine = Engine::new(username)?;
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
    Ok((
        Pending {
            engine,
            api,
            username: username.to_string(),
        },
        dev_code,
    ))
}

async fn do_verify(
    pending: &mut Pending,
    code: &str,
) -> anyhow::Result<mpsc::Receiver<IncomingMessage>> {
    pending.api.verify(&pending.username, code).await?;
    let keys = pending.engine.generate_prekeys(20).await?;
    pending.api.upload_keys(&keys).await?;
    Ok(pending.api.stream().await?)
}

async fn handle_send(
    session: &mut Option<Session>,
    recipient: &str,
    text: &str,
) -> anyhow::Result<()> {
    let session = session
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no session; register first"))?;
    if !session.engine.has_session(recipient).await? {
        let bundle = session.api.fetch_bundle(recipient).await?;
        session.engine.ensure_session(recipient, &bundle).await?;
    }
    let envelope = session.engine.encrypt(recipient, text.as_bytes()).await?;
    session.api.send_message(recipient, &envelope).await?;
    Ok(())
}

async fn handle_incoming(
    session: &mut Option<Session>,
    events: &Option<mpsc::Sender<DecryptedMessage>>,
    msg: IncomingMessage,
) {
    let Some(session) = session.as_mut() else {
        return;
    };
    match session.engine.decrypt(&msg.sender, &msg.body).await {
        Ok(plain) => {
            let _ = session.api.ack_message(&msg.id).await;
            if let Some(events) = events {
                let _ = events
                    .send(DecryptedMessage {
                        id: msg.id,
                        sender: msg.sender,
                        text: String::from_utf8_lossy(&plain).into_owned(),
                        created_at: msg.created_at,
                    })
                    .await;
            }
        }
        Err(e) => {
            tracing::warn!("failed to decrypt message {} from {}: {e}", msg.id, msg.sender);
        }
    }
}

async fn actor(mut commands: mpsc::Receiver<Command>) {
    let mut pending: Option<Pending> = None;
    let mut session: Option<Session> = None;
    let mut stream: Option<mpsc::Receiver<IncomingMessage>> = None;
    let mut events: Option<mpsc::Sender<DecryptedMessage>> = None;

    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                None => break,
                Some(Command::Register { server, username, alias, email, password, reply }) => {
                    match do_register(&server, &username, &alias, &email, &password).await {
                        Ok((new_pending, dev_code)) => {
                            pending = Some(new_pending);
                            let _ = reply.send(Ok(dev_code));
                        }
                        Err(e) => {
                            let _ = reply.send(Err(e.to_string()));
                        }
                    }
                }
                Some(Command::Verify { code, reply }) => {
                    let result = match pending.as_mut() {
                        None => Err("no pending registration".to_string()),
                        Some(p) => match do_verify(p, &code).await {
                            Ok(new_stream) => {
                                let Pending { engine, api, .. } = pending.take().expect("checked");
                                let (tx, rx) = mpsc::channel(256);
                                session = Some(Session { engine, api });
                                stream = Some(new_stream);
                                events = Some(tx);
                                Ok(rx)
                            }
                            Err(e) => Err(e.to_string()),
                        },
                    };
                    let _ = reply.send(result);
                }
                Some(Command::Send { recipient, text, reply }) => {
                    let result = handle_send(&mut session, &recipient, &text).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
                Some(Command::Profile { username, reply }) => {
                    let result = match session.as_ref().map(|s| &s.api).or(pending.as_ref().map(|p| &p.api)) {
                        None => Err("no session".to_string()),
                        Some(api) => api.fetch_profile(&username).await.map_err(|e| e.to_string()),
                    };
                    let _ = reply.send(result);
                }
            },
            msg = async { stream.as_mut().expect("guarded by if").recv().await }, if stream.is_some() => {
                match msg {
                    Some(msg) => handle_incoming(&mut session, &events, msg).await,
                    None => {
                        // Server connection dropped: closing `events` tells the UI.
                        stream = None;
                        events = None;
                    }
                }
            }
        }
    }
}
