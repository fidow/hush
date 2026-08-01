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
        reply: oneshot::Sender<Result<mpsc::Receiver<DecryptedMessage>, String>>,
    },
    Send {
        recipient: String,
        text: String,
        reply: oneshot::Sender<Result<(), String>>,
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

    /// Creates the account, publishes prekeys and opens the message stream.
    /// The returned channel yields decrypted incoming messages and closes if
    /// the server connection drops.
    pub async fn register(
        &self,
        server: &str,
        username: &str,
    ) -> Result<mpsc::Receiver<DecryptedMessage>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Register {
                server: server.to_string(),
                username: username.to_string(),
                reply,
            })
            .await
            .map_err(|_| "engine closed".to_string())?;
        rx.await.map_err(|_| "engine closed".to_string())?
    }

    /// Encrypts and sends `text`, establishing a session first if needed.
    pub async fn send_text(&self, recipient: &str, text: &str) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Send {
                recipient: recipient.to_string(),
                text: text.to_string(),
                reply,
            })
            .await
            .map_err(|_| "engine closed".to_string())?;
        rx.await.map_err(|_| "engine closed".to_string())?
    }
}

struct Session {
    engine: Engine,
    api: ApiClient,
}

async fn do_register(
    server: &str,
    username: &str,
) -> anyhow::Result<(Session, mpsc::Receiver<IncomingMessage>)> {
    let mut engine = Engine::new(username)?;
    let mut api = ApiClient::new(server.trim_end_matches('/'));
    api.register(
        username,
        engine.registration_id().await?,
        &engine.identity_key_b64().await?,
    )
    .await?;
    let keys = engine.generate_prekeys(20).await?;
    api.upload_keys(&keys).await?;
    let stream = api.stream().await?;
    Ok((Session { engine, api }, stream))
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
    let mut session: Option<Session> = None;
    let mut stream: Option<mpsc::Receiver<IncomingMessage>> = None;
    let mut events: Option<mpsc::Sender<DecryptedMessage>> = None;

    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                None => break,
                Some(Command::Register { server, username, reply }) => {
                    match do_register(&server, &username).await {
                        Ok((new_session, new_stream)) => {
                            let (tx, rx) = mpsc::channel(256);
                            session = Some(new_session);
                            stream = Some(new_stream);
                            events = Some(tx);
                            let _ = reply.send(Ok(rx));
                        }
                        Err(e) => {
                            let _ = reply.send(Err(e.to_string()));
                        }
                    }
                }
                Some(Command::Send { recipient, text, reply }) => {
                    let result = handle_send(&mut session, &recipient, &text).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
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
