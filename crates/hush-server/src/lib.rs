//! Hush server: a "dumb" relay/mailbox. It stores public key bundles and
//! queues of opaque encrypted blobs; it can never read message contents.

use std::{collections::HashMap, convert::Infallible, sync::Arc};

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post, put},
    Json, Router,
};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
    username        TEXT PRIMARY KEY,
    token           TEXT NOT NULL UNIQUE,
    registration_id INTEGER NOT NULL,
    identity_key    TEXT NOT NULL,
    bundle_static   TEXT,
    created_at      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS one_time_prekeys (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL REFERENCES accounts(username),
    kind     TEXT NOT NULL CHECK (kind IN ('ec', 'kyber')),
    data     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
    id         TEXT PRIMARY KEY,
    sender     TEXT NOT NULL,
    recipient  TEXT NOT NULL REFERENCES accounts(username),
    body       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_recipient ON messages(recipient, created_at);
"#;

#[derive(Clone, Serialize, Debug)]
pub struct OutMessage {
    pub id: String,
    pub sender: String,
    pub body: String,
    pub created_at: i64,
}

#[derive(Clone)]
pub struct AppState {
    db: SqlitePool,
    live: Arc<Mutex<HashMap<String, mpsc::Sender<OutMessage>>>>,
}

pub async fn connect_db(url: &str) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new().connect(url).await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    Ok(pool)
}

pub fn app(db: SqlitePool) -> Router {
    let state = AppState {
        db,
        live: Arc::new(Mutex::new(HashMap::new())),
    };
    Router::new()
        .route("/v1/accounts", post(register))
        .route("/v1/keys", put(upload_keys))
        .route("/v1/keys/{username}", get(fetch_bundle))
        .route("/v1/messages/stream", get(message_stream))
        .route("/v1/messages/{target}", put(send_message).delete(ack_message))
        .with_state(state)
}

type ApiError = (StatusCode, String);

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Authenticated user, resolved from the `Authorization: Bearer <token>` header.
pub struct AuthUser {
    pub username: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token".into()))?;
        let row = sqlx::query("SELECT username FROM accounts WHERE token = ?")
            .bind(token)
            .fetch_optional(&state.db)
            .await
            .map_err(internal)?
            .ok_or((StatusCode::UNAUTHORIZED, "invalid token".into()))?;
        Ok(AuthUser {
            username: row.get(0),
        })
    }
}

#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    registration_id: i64,
    identity_key: String,
}

#[derive(Serialize)]
struct RegisterResponse {
    token: String,
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, ApiError> {
    if req.username.is_empty()
        || req.username.len() > 32
        || !req.username.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "username must be 1-32 chars of [a-zA-Z0-9_]".into(),
        ));
    }
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);

    let res = sqlx::query(
        "INSERT INTO accounts (username, token, registration_id, identity_key, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&req.username)
    .bind(&token)
    .bind(req.registration_id)
    .bind(&req.identity_key)
    .bind(now())
    .execute(&state.db)
    .await;

    match res {
        Ok(_) => Ok(Json(RegisterResponse { token })),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err((StatusCode::CONFLICT, "username already taken".into()))
        }
        Err(e) => Err(internal(e)),
    }
}

#[derive(Deserialize)]
struct PrekeyUpload {
    kind: String,
    data: String,
}

#[derive(Deserialize)]
struct UploadKeysRequest {
    /// Opaque JSON: signed prekey, last-resort kyber prekey, signatures.
    bundle_static: serde_json::Value,
    one_time_prekeys: Vec<PrekeyUpload>,
}

async fn upload_keys(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<UploadKeysRequest>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.db.begin().await.map_err(internal)?;
    sqlx::query("UPDATE accounts SET bundle_static = ? WHERE username = ?")
        .bind(req.bundle_static.to_string())
        .bind(&auth.username)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    sqlx::query("DELETE FROM one_time_prekeys WHERE username = ?")
        .bind(&auth.username)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    for pk in &req.one_time_prekeys {
        if pk.kind != "ec" && pk.kind != "kyber" {
            return Err((StatusCode::BAD_REQUEST, "prekey kind must be ec|kyber".into()));
        }
        sqlx::query("INSERT INTO one_time_prekeys (username, kind, data) VALUES (?, ?, ?)")
            .bind(&auth.username)
            .bind(&pk.kind)
            .bind(&pk.data)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
    }
    tx.commit().await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct BundleResponse {
    registration_id: i64,
    identity_key: String,
    bundle_static: serde_json::Value,
    one_time_prekey: Option<String>,
    kyber_prekey: Option<String>,
}

async fn pop_prekey(db: &SqlitePool, username: &str, kind: &str) -> Result<Option<String>, ApiError> {
    let row = sqlx::query(
        "DELETE FROM one_time_prekeys
         WHERE id = (SELECT id FROM one_time_prekeys WHERE username = ? AND kind = ? LIMIT 1)
         RETURNING data",
    )
    .bind(username)
    .bind(kind)
    .fetch_optional(db)
    .await
    .map_err(internal)?;
    Ok(row.map(|r| r.get(0)))
}

async fn fetch_bundle(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<Json<BundleResponse>, ApiError> {
    let row = sqlx::query(
        "SELECT registration_id, identity_key, bundle_static FROM accounts WHERE username = ?",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or((StatusCode::NOT_FOUND, "no such user".into()))?;

    let bundle_static: Option<String> = row.get(2);
    let bundle_static = bundle_static
        .ok_or((StatusCode::NOT_FOUND, "user has not uploaded keys".into()))?;

    Ok(Json(BundleResponse {
        registration_id: row.get(0),
        identity_key: row.get(1),
        bundle_static: serde_json::from_str(&bundle_static).map_err(internal)?,
        one_time_prekey: pop_prekey(&state.db, &username, "ec").await?,
        kyber_prekey: pop_prekey(&state.db, &username, "kyber").await?,
    }))
}

#[derive(Deserialize)]
struct SendMessageRequest {
    /// Opaque encrypted payload (base64), including its envelope/type.
    body: String,
}

#[derive(Serialize)]
struct SendMessageResponse {
    id: String,
}

async fn send_message(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(recipient): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, ApiError> {
    let exists = sqlx::query("SELECT 1 FROM accounts WHERE username = ?")
        .bind(&recipient)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    if exists.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such user".into()));
    }

    let msg = OutMessage {
        id: uuid::Uuid::new_v4().to_string(),
        sender: auth.username,
        body: req.body,
        created_at: now(),
    };
    sqlx::query("INSERT INTO messages (id, sender, recipient, body, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&msg.id)
        .bind(&msg.sender)
        .bind(&recipient)
        .bind(&msg.body)
        .bind(msg.created_at)
        .execute(&state.db)
        .await
        .map_err(internal)?;

    // Best-effort live push; the SSE backlog query covers anyone offline.
    if let Some(tx) = state.live.lock().await.get(&recipient) {
        let _ = tx.try_send(msg.clone());
    }
    Ok(Json(SendMessageResponse { id: msg.id }))
}

async fn message_stream(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let (tx, rx) = mpsc::channel::<OutMessage>(256);
    state
        .live
        .lock()
        .await
        .insert(auth.username.clone(), tx.clone());

    // Backlog first, then live pushes. Clients dedupe by message id: a message
    // arriving during the backlog query can be delivered twice.
    let rows = sqlx::query(
        "SELECT id, sender, body, created_at FROM messages WHERE recipient = ? ORDER BY created_at",
    )
    .bind(&auth.username)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;
    tokio::spawn(async move {
        for r in rows {
            let msg = OutMessage {
                id: r.get(0),
                sender: r.get(1),
                body: r.get(2),
                created_at: r.get(3),
            };
            if tx.send(msg).await.is_err() {
                return;
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(|msg| {
        Ok(Event::default()
            .event("message")
            .data(serde_json::to_string(&msg).expect("OutMessage serializes")))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn ack_message(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    sqlx::query("DELETE FROM messages WHERE id = ? AND recipient = ?")
        .bind(&id)
        .bind(&auth.username)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}
