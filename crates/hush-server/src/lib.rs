//! Hush server: a "dumb" relay/mailbox. It stores public key bundles and
//! queues of opaque encrypted blobs; it can never read message contents.

pub mod mail;

use std::{collections::HashMap, convert::Infallible, sync::Arc};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post, put},
    Json, Router,
};
use base64::Engine;
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
    username        TEXT PRIMARY KEY,
    alias           TEXT NOT NULL DEFAULT '',
    email           TEXT NOT NULL DEFAULT '',
    password_hash   TEXT NOT NULL DEFAULT '',
    verified        INTEGER NOT NULL DEFAULT 0,
    verify_code     TEXT,
    verify_expires  INTEGER,
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
        .route("/v1/accounts/verify", post(verify_account))
        .route("/v1/sessions", post(login))
        .route("/v1/profile/{username}", get(get_profile))
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
        let row = sqlx::query("SELECT username FROM accounts WHERE token = ? AND verified = 1")
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
    #[serde(default)]
    alias: String,
    email: String,
    password: String,
    registration_id: i64,
    identity_key: String,
}

fn new_token() -> String {
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let bad = |msg: &str| (StatusCode::BAD_REQUEST, msg.to_string());
    if req.username.is_empty()
        || req.username.len() > 32
        || !req.username.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(bad("username must be 1-32 chars of [a-zA-Z0-9_]"));
    }
    if req.alias.len() > 64 {
        return Err(bad("alias too long (max 64)"));
    }
    if !req.email.contains('@') || req.email.len() > 254 {
        return Err(bad("invalid email"));
    }
    if req.password.len() < 8 {
        return Err(bad("password must be at least 8 chars"));
    }

    let mut salt_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(internal)?;
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(internal)?
        .to_string();
    let token = new_token();
    let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
    let expires = now() + 24 * 60 * 60 * 1000;

    let existing = sqlx::query("SELECT verified FROM accounts WHERE username = ?")
        .bind(&req.username)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    match existing {
        Some(row) if row.get::<i64, _>(0) != 0 => {
            return Err((StatusCode::CONFLICT, "username already taken".into()));
        }
        Some(_) => {
            // Unverified leftover: allow re-registering (fresh code and keys).
            sqlx::query(
                "UPDATE accounts SET alias=?, email=?, password_hash=?, verify_code=?,
                 verify_expires=?, token=?, registration_id=?, identity_key=? WHERE username=?",
            )
            .bind(&req.alias)
            .bind(&req.email)
            .bind(&password_hash)
            .bind(&code)
            .bind(expires)
            .bind(&token)
            .bind(req.registration_id)
            .bind(&req.identity_key)
            .bind(&req.username)
            .execute(&state.db)
            .await
            .map_err(internal)?;
        }
        None => {
            sqlx::query(
                "INSERT INTO accounts (username, alias, email, password_hash, verify_code,
                 verify_expires, token, registration_id, identity_key, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&req.username)
            .bind(&req.alias)
            .bind(&req.email)
            .bind(&password_hash)
            .bind(&code)
            .bind(expires)
            .bind(&token)
            .bind(req.registration_id)
            .bind(&req.identity_key)
            .bind(now())
            .execute(&state.db)
            .await
            .map_err(internal)?;
        }
    }

    tracing::info!(username = %req.username, email = %req.email, "cuenta creada, pendiente de verificación");
    match mail::MailConfig::from_env() {
        Some(cfg) => {
            let (email, username, code) = (req.email.clone(), req.username.clone(), code.clone());
            tokio::task::spawn_blocking(move || {
                match cfg.send_verification(&email, &username, &code) {
                    Ok(()) => tracing::info!(%username, "email de verificación enviado"),
                    Err(e) => tracing::error!(%username, "fallo enviando email de verificación: {e}"),
                }
            });
        }
        None => {
            tracing::info!(username = %req.username, "SMTP no configurado; el email de verificación no se envió");
            // The code itself only reaches the log in debug mode (HUSH_LOG=debug).
            tracing::debug!(username = %req.username, "código de verificación (solo dev): {code}");
        }
    }

    let mut resp = serde_json::json!({ "status": "pending_verification" });
    if std::env::var("HUSH_ECHO_CODE").is_ok_and(|v| v == "1") {
        resp["dev_code"] = code.into();
    }
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct VerifyRequest {
    username: String,
    code: String,
}

async fn verify_account(
    State(state): State<AppState>,
    Json(req): Json<VerifyRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = sqlx::query(
        "SELECT token, verify_code, verify_expires, verified FROM accounts WHERE username = ?",
    )
    .bind(&req.username)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or((StatusCode::NOT_FOUND, "no such user".into()))?;

    if row.get::<i64, _>(3) != 0 {
        return Err((StatusCode::CONFLICT, "account already verified".into()));
    }
    let stored_code: Option<String> = row.get(1);
    let expires: Option<i64> = row.get(2);
    let valid = stored_code.as_deref() == Some(req.code.as_str())
        && expires.is_some_and(|e| e > now());
    if !valid {
        return Err((StatusCode::BAD_REQUEST, "invalid or expired code".into()));
    }

    sqlx::query("UPDATE accounts SET verified = 1, verify_code = NULL WHERE username = ?")
        .bind(&req.username)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    tracing::info!(username = %req.username, "cuenta verificada");
    Ok(Json(serde_json::json!({ "token": row.get::<String, _>(0) })))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = sqlx::query("SELECT token, password_hash, verified FROM accounts WHERE username = ?")
        .bind(&req.username)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    // Same error for unknown user and wrong password: don't leak which usernames exist.
    let denied = || (StatusCode::UNAUTHORIZED, "invalid credentials".into());
    let row = row.ok_or_else(denied)?;
    let hash_str: String = row.get(1);
    let parsed = PasswordHash::new(&hash_str).map_err(internal)?;
    Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed)
        .map_err(|_| denied())?;
    if row.get::<i64, _>(2) == 0 {
        return Err((StatusCode::FORBIDDEN, "account not verified".into()));
    }
    tracing::info!(username = %req.username, "login correcto");
    Ok(Json(serde_json::json!({ "token": row.get::<String, _>(0) })))
}

async fn get_profile(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = sqlx::query(
        "SELECT alias, identity_key FROM accounts WHERE username = ? AND verified = 1",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or((StatusCode::NOT_FOUND, "no such user".into()))?;
    Ok(Json(serde_json::json!({
        "username": username,
        "alias": row.get::<String, _>(0),
        "identity_key": row.get::<String, _>(1),
    })))
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
    /// Present when the device (re)provisioned its identity, e.g. after
    /// logging in on a new device.
    identity_key: Option<String>,
    registration_id: Option<i64>,
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
    if let (Some(identity_key), Some(registration_id)) = (&req.identity_key, req.registration_id) {
        sqlx::query("UPDATE accounts SET identity_key = ?, registration_id = ? WHERE username = ?")
            .bind(identity_key)
            .bind(registration_id)
            .bind(&auth.username)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        tracing::info!(username = %auth.username, "identidad re-aprovisionada (nuevo dispositivo)");
    }
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

    tracing::debug!(from = %msg.sender, to = %recipient, id = %msg.id, "mensaje encolado");
    // Best-effort live push; the SSE backlog query covers anyone offline.
    if let Some(tx) = state.live.lock().await.get(&recipient) {
        let _ = tx.try_send(msg.clone());
        tracing::debug!(to = %recipient, id = %msg.id, "entrega en vivo (SSE)");
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
    tracing::debug!(user = %auth.username, backlog = rows.len(), "stream SSE abierto");
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
    tracing::debug!(user = %auth.username, id = %id, "mensaje confirmado y borrado");
    Ok(StatusCode::NO_CONTENT)
}
