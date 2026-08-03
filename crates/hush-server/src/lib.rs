//! Hush server: a "dumb" relay/mailbox. It stores public key bundles and
//! queues of opaque encrypted blobs; it can never read message contents.

pub mod mail;
pub mod ratelimit;

use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::HashMap, convert::Infallible, sync::Arc};

use crate::ratelimit::RateLimiter;

/// Abuse limits. Windows in milliseconds.
const MINUTE: i64 = 60 * 1000;
const QUARTER_HOUR: i64 = 15 * MINUTE;
const HOUR: i64 = 60 * MINUTE;
/// Wrong verification codes accepted before the code is burned.
const MAX_VERIFY_ATTEMPTS: i64 = 5;
/// Caps that keep one account from filling the server's disk.
const MAX_QUEUED_MESSAGES: i64 = 10_000;
const MAX_ARCHIVE_ENTRIES: i64 = 200_000;
const MAX_PREKEYS_PER_UPLOAD: usize = 200;
const MAX_FIELD_BYTES: usize = 64 * 1024;
const MAX_ALIAS_BYTES: usize = 64;
/// Encrypted envelopes carrying inline images can be several MB.
const MAX_BODY_BYTES: usize = 15 * 1024 * 1024;
/// Contact requests one account may send per hour, and how many of those may
/// name someone who does not exist.
const MAX_CONTACT_REQUESTS_PER_HOUR: usize = 20;
const MAX_UNKNOWN_LOOKUPS_PER_HOUR: usize = 5;
/// Presence a user can choose. Being reachable at all is derived from the
/// live SSE connections, so "offline" is reported, never set.
const SETTABLE_STATUSES: [&str; 3] = ["online", "away", "busy"];

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

use axum::{
    extract::{FromRequestParts, Path, Query, State},
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
    -- Salt for the client-side history key. Public by design: without the
    -- user's passphrase it is useless.
    verified        INTEGER NOT NULL DEFAULT 0,
    verify_code     TEXT,
    verify_expires  INTEGER,
    -- Presence the user picked; whether they are reachable at all is derived
    -- from the live SSE connections, not from this column.
    status          TEXT NOT NULL DEFAULT 'online',
    -- When the account last held an open stream, for "last seen" on
    -- contacts who are offline.
    last_seen       INTEGER,
    verify_attempts INTEGER NOT NULL DEFAULT 0,
    reset_code      TEXT,
    reset_expires   INTEGER,
    reset_attempts  INTEGER NOT NULL DEFAULT 0,
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
-- Client-side encrypted history. The server stores opaque blobs and can
-- never read them: they are encrypted with a key derived from the user's
-- history passphrase, which never leaves their devices.
CREATE TABLE IF NOT EXISTS archive (
    username   TEXT NOT NULL REFERENCES accounts(username),
    id         TEXT NOT NULL,
    blob       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (username, id)
);
CREATE INDEX IF NOT EXISTS idx_archive_user ON archive(username, created_at);
-- Bookkeeping for read receipts. A message row disappears once the recipient
-- acknowledges it, so this remembers who to tell when it is later read. Rows
-- are dropped as soon as the read receipt goes out, and pruned by age
-- otherwise.
CREATE TABLE IF NOT EXISTS deliveries (
    id         TEXT PRIMARY KEY,
    sender     TEXT NOT NULL,
    recipient  TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
-- Contact list. Every link is stored from both sides so each account can be
-- listed on its own: A sees 'outgoing' while B sees 'incoming', and both
-- flip to 'accepted' together.
-- No CHECK on `state`: it is validated in code, and a constraint here would
-- mean rebuilding the table every time a state is added.
CREATE TABLE IF NOT EXISTS contacts (
    owner      TEXT NOT NULL REFERENCES accounts(username),
    peer       TEXT NOT NULL REFERENCES accounts(username),
    state      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (owner, peer)
);
"#;

#[derive(Clone, Serialize, Debug)]
pub struct OutMessage {
    pub id: String,
    pub sender: String,
    pub body: String,
    pub created_at: i64,
}

/// Something to push down an open SSE stream.
#[derive(Clone, Debug)]
enum Push {
    Message(OutMessage),
    /// The contact list changed; the client should re-fetch it.
    ContactsChanged,
    /// A message we sent reached the recipient's device, or was read.
    Receipt {
        id: String,
        state: &'static str,
        at: i64,
    },
}

/// A live SSE listener. The id lets a stream remove *its own* entry on
/// disconnect without evicting a newer connection from the same account.
struct Listener {
    id: u64,
    tx: mpsc::Sender<Push>,
}

type LiveMap = Arc<Mutex<HashMap<String, Listener>>>;

#[derive(Clone)]
pub struct AppState {
    db: SqlitePool,
    live: LiveMap,
    limits: Arc<RateLimiter>,
    next_listener_id: Arc<AtomicU64>,
}

pub async fn connect_db(url: &str) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new().connect(url).await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    // Migration for databases created before per-account verify throttling.
    let _ = sqlx::query("ALTER TABLE accounts ADD COLUMN verify_attempts INTEGER NOT NULL DEFAULT 0")
        .execute(&pool)
        .await;
    let _ = sqlx::query("ALTER TABLE accounts ADD COLUMN status TEXT NOT NULL DEFAULT 'online'")
        .execute(&pool)
        .await;
    // Password recovery, added later: migrate rather than recreate.
    for column in [
        "reset_code TEXT",
        "reset_expires INTEGER",
        "reset_attempts INTEGER NOT NULL DEFAULT 0",
        "last_seen INTEGER",
    ] {
        let _ = sqlx::query(&format!("ALTER TABLE accounts ADD COLUMN {column}"))
            .execute(&pool)
            .await;
    }

    // Blocking added a contact state, and the original table pinned the
    // allowed values with a CHECK. SQLite cannot drop a constraint, so the
    // table is rebuilt once, keeping its rows.
    let contacts_sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'contacts'",
    )
    .fetch_optional(&pool)
    .await?
    .flatten();
    if contacts_sql.is_some_and(|sql| sql.contains("CHECK")) {
        tracing::info!("migrating the contacts table to allow blocking");
        sqlx::raw_sql(
            "BEGIN;
             CREATE TABLE contacts_migrated (
                 owner      TEXT NOT NULL REFERENCES accounts(username),
                 peer       TEXT NOT NULL REFERENCES accounts(username),
                 state      TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 PRIMARY KEY (owner, peer)
             );
             INSERT INTO contacts_migrated SELECT owner, peer, state, created_at FROM contacts;
             DROP TABLE contacts;
             ALTER TABLE contacts_migrated RENAME TO contacts;
             COMMIT;",
        )
        .execute(&pool)
        .await?;
    }

    Ok(pool)
}

pub fn app(db: SqlitePool) -> Router {
    let state = AppState {
        db,
        live: Arc::new(Mutex::new(HashMap::new())),
        limits: Arc::new(RateLimiter::new()),
        next_listener_id: Arc::new(AtomicU64::new(0)),
    };
    Router::new()
        .route("/", get(landing))
        .route("/v1/accounts", post(register))
        .route("/v1/accounts/verify", post(verify_account))
        .route("/v1/sessions", post(login))
        .route("/v1/password/forgot", post(forgot_password))
        .route("/v1/password/reset", post(reset_password))
        .route("/v1/profile/{username}", get(get_profile))
        .route("/v1/me", axum::routing::patch(update_me))
        .route("/v1/presence", post(query_presence))
        .route("/v1/contacts", get(list_contacts))
        .route(
            "/v1/contacts/{peer}",
            post(request_contact).delete(remove_contact),
        )
        .route("/v1/contacts/{peer}/accept", post(accept_contact))
        .route("/v1/contacts/{peer}/block", post(block_contact))
        .route("/v1/keys", put(upload_keys))
        .route("/v1/keys/{username}", get(fetch_bundle))
        .route("/v1/messages/stream", get(message_stream))
        .route("/v1/messages/{target}", put(send_message).delete(ack_message))
        .route("/v1/messages/{id}/read", post(mark_read))
        .route("/v1/archive", get(list_archive))
        .route(
            "/v1/archive/{id}",
            put(put_archive).delete(delete_archive_entry),
        )
        // Updates are public: a client that cannot sign in still has to be
        // able to update, and the installer is signed anyway.
        .route(
            "/v1/update/{target}/{arch}/{current}",
            get(update_manifest),
        )
        .route("/v1/update/download/{file}", get(update_download))
        // Encrypted envelopes carrying inline images can be several MB.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        // Outermost, so it runs before routing: see `read_body_first`.
        .layer(axum::middleware::from_fn(read_body_first))
        .with_state(state)
}

/// An API error. The server speaks English; `code` is the stable identifier
/// clients use to show the message in the user's own language.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({ "code": self.code, "message": self.message })),
        )
            .into_response()
    }
}

fn err(status: StatusCode, code: &'static str, message: &'static str) -> ApiError {
    ApiError {
        status,
        code,
        message,
    }
}

fn bad_request(code: &'static str, message: &'static str) -> ApiError {
    err(StatusCode::BAD_REQUEST, code, message)
}

fn not_found() -> ApiError {
    err(StatusCode::NOT_FOUND, "user_not_found", "No such user")
}

fn unauthorized() -> ApiError {
    err(StatusCode::UNAUTHORIZED, "invalid_session", "Invalid session")
}

/// Logs the real cause and returns an opaque message: database and
/// serialization errors must never reach a client.
fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!("internal error: {e}");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "Internal server error",
    )
}

fn too_many() -> ApiError {
    err(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "Too many attempts, try again later",
    )
}

/// Comparison whose duration does not depend on where the mismatch is, so a
/// verification code cannot be recovered byte by byte.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Best-effort client address, used to throttle unauthenticated endpoints.
/// `X-Forwarded-For` is honoured only when `HUSH_TRUST_PROXY=1`, because the
/// header is trivially spoofable when the server is exposed directly.
pub struct ClientIp(pub String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Infallible> {
        if std::env::var("HUSH_TRUST_PROXY").is_ok_and(|v| v == "1") {
            if let Some(forwarded) = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                return Ok(ClientIp(forwarded.to_string()));
            }
        }
        Ok(ClientIp(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<SocketAddr>>()
                .map(|ci| ci.0.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        ))
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Reads the request body before anything else can answer.
///
/// Responses produced before the body is read — a 404 for an unknown route, a
/// 401 from the auth extractor — leave the client still uploading, and the
/// connection is then closed mid-send. A reverse proxy in front reports that
/// as a bare 502 (Apache: `AH01084: pass request body failed`), hiding the
/// real status: someone uploading a large image would see a gateway error
/// instead of "session expired". Buffering first costs nothing extra, since
/// every handler that takes a body already buffers it, and the size is capped
/// either way.
async fn read_body_first(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let (parts, body) = req.into_parts();
    match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => {
            let req = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
            next.run(req).await
        }
        // Over the cap: nothing to do but refuse. This is the one case where
        // the body still goes unread, exactly as any server behaves.
        Err(_) => err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Request body too large",
        )
        .into_response(),
    }
}

/// Download page. Embedded at compile time so the binary stays
/// self-contained: no asset directory to ship or keep in sync.
///
/// Deliberately not a web client — serving the code that performs the
/// encryption from the same server that relays the messages would mean
/// trusting it on every page load, which is exactly what the app avoids.
async fn landing(parts: axum::http::HeaderMap) -> axum::response::Html<String> {
    let spanish = parts
        .get("accept-language")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|langs| langs.to_lowercase().starts_with("es"));
    let page = if spanish {
        include_str!("../web/index.es.html")
    } else {
        include_str!("../web/index.en.html")
    };
    axum::response::Html(page.replace("{{VERSION}}", env!("CARGO_PKG_VERSION")))
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
            .ok_or_else(unauthorized)?;
        let row = sqlx::query("SELECT username FROM accounts WHERE token = ? AND verified = 1")
            .bind(token)
            .fetch_optional(&state.db)
            .await
            .map_err(internal)?
            .ok_or_else(unauthorized)?;
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
    ClientIp(ip): ClientIp,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Throttle by source and by target address: registration both creates
    // rows and makes the server send mail, so it is an abuse vector twice
    // over (account spam and using us to bomb someone's inbox).
    let now_ms = now();
    if !state.limits.allow(&format!("reg-ip:{ip}"), 5, HOUR, now_ms)
        || !state
            .limits
            .allow(&format!("reg-mail:{}", req.email.to_lowercase()), 3, HOUR, now_ms)
    {
        tracing::warn!(%ip, "registration throttled after too many attempts");
        return Err(too_many());
    }
    // Usernames are case-insensitive: stored and matched in lowercase.
    let username = req.username.to_lowercase();
    if username.is_empty()
        || username.len() > 32
        || !username.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(bad_request(
            "invalid_username",
            "Username must be 1-32 characters of letters, digits or _",
        ));
    }
    if req.alias.len() > MAX_ALIAS_BYTES {
        return Err(bad_request("alias_too_long", "Display name is too long"));
    }
    if !req.email.contains('@') || req.email.len() > 254 {
        return Err(bad_request("invalid_email", "Invalid email address"));
    }
    if req.password.len() < 8 {
        return Err(bad_request(
            "weak_password",
            "Password must be at least 8 characters",
        ));
    }
    // Argon2 hashes any length, but an unbounded password is free CPU for an
    // attacker; the same cap keeps the other opaque fields sane.
    if req.password.len() > 1024 {
        return Err(bad_request("password_too_long", "Password is too long"));
    }
    if req.identity_key.len() > MAX_FIELD_BYTES {
        return Err(bad_request("invalid_request", "Invalid registration data"));
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
        .bind(&username)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    match existing {
        Some(row) if row.get::<i64, _>(0) != 0 => {
            return Err(err(
                StatusCode::CONFLICT,
                "username_taken",
                "That username is already taken",
            ));
        }
        Some(_) => {
            // Unverified leftover: allow re-registering (fresh code and keys).
            sqlx::query(
                "UPDATE accounts SET alias=?, email=?, password_hash=?,
                 verify_code=?, verify_expires=?, verify_attempts=0, token=?,
                 registration_id=?, identity_key=? WHERE username=?",
            )
            .bind(&req.alias)
            .bind(&req.email)
            .bind(&password_hash)
            .bind(&code)
            .bind(expires)
            .bind(&token)
            .bind(req.registration_id)
            .bind(&req.identity_key)
            .bind(&username)
            .execute(&state.db)
            .await
            .map_err(internal)?;
        }
        None => {
            sqlx::query(
                "INSERT INTO accounts (username, alias, email, password_hash,
                 verify_code, verify_expires, token, registration_id, identity_key, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&username)
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

    // The address is not logged: it is the one piece of an account that ties
    // it to a person outside Hush.
    tracing::info!(username = %username, "account created, pending verification");
    match mail::MailConfig::from_env() {
        Some(cfg) => {
            let (email, username, code) = (req.email.clone(), username.clone(), code.clone());
            tokio::task::spawn_blocking(move || {
                match cfg.send_verification(&email, &username, &code) {
                    Ok(()) => tracing::info!(%username, "verification email sent"),
                    Err(e) => tracing::error!(%username, "failed to send verification email: {e}"),
                }
            });
        }
        None => {
            tracing::info!(username = %username, "SMTP not configured; verification email not sent");
            // The code itself only reaches the log in debug mode (HUSH_LOG=debug).
            tracing::debug!(username = %username, "verification code (dev only): {code}");
        }
    }

    let mut resp = serde_json::json!({ "status": "pending_verification" });
    // Development convenience, refused whenever mail actually works, so the
    // flag left set by accident cannot hand out codes on a real deployment.
    if std::env::var("HUSH_ECHO_CODE").is_ok_and(|v| v == "1") && mail::MailConfig::from_env().is_none()
    {
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
    ClientIp(ip): ClientIp,
    Json(req): Json<VerifyRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let username = req.username.to_lowercase();
    // A 6-digit code is only 10^6 possibilities: without throttling plus the
    // persistent attempt counter below it can be exhausted in minutes.
    let now_ms = now();
    if !state
        .limits
        .allow(&format!("verify:{username}"), 5, QUARTER_HOUR, now_ms)
        || !state.limits.allow(&format!("verify-ip:{ip}"), 20, QUARTER_HOUR, now_ms)
    {
        tracing::warn!(%username, %ip, "verification throttled after too many attempts");
        return Err(too_many());
    }

    let row = sqlx::query(
        "SELECT token, verify_code, verify_expires, verified, verify_attempts
         FROM accounts WHERE username = ?",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;

    if row.get::<i64, _>(3) != 0 {
        return Err(err(
            StatusCode::CONFLICT,
            "already_verified",
            "This account is already verified",
        ));
    }
    let stored_code: Option<String> = row.get(1);
    let expires: Option<i64> = row.get(2);
    let attempts: i64 = row.get(4);
    let bad_code = || bad_request("invalid_code", "Incorrect or expired code");
    if attempts >= MAX_VERIFY_ATTEMPTS {
        return Err(bad_code());
    }

    let valid = stored_code
        .as_deref()
        .is_some_and(|stored| constant_time_eq(stored, &req.code))
        && expires.is_some_and(|e| e > now_ms);
    if !valid {
        // Burn the code once the budget is spent: the account must be
        // registered again to get a fresh one.
        let spent = attempts + 1;
        let burn = spent >= MAX_VERIFY_ATTEMPTS;
        sqlx::query(
            "UPDATE accounts SET verify_attempts = ?,
             verify_code = CASE WHEN ? THEN NULL ELSE verify_code END
             WHERE username = ?",
        )
        .bind(spent)
        .bind(burn)
        .bind(&username)
        .execute(&state.db)
        .await
        .map_err(internal)?;
        if burn {
            tracing::warn!(%username, "verification code burned after too many failures");
        }
        return Err(bad_code());
    }

    sqlx::query(
        "UPDATE accounts SET verified = 1, verify_code = NULL, verify_attempts = 0
         WHERE username = ?",
    )
    .bind(&username)
    .execute(&state.db)
    .await
    .map_err(internal)?;
    tracing::info!(username = %username, "account verified");
    Ok(Json(serde_json::json!({ "token": row.get::<String, _>(0) })))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

/// Argon2 hash of an unguessable value, used to equalise the cost of logging
/// into a non-existent account. Generated once at startup.
static DUMMY_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let mut raw = [0u8; 16];
    rand::rng().fill_bytes(&mut raw);
    let salt = SaltString::encode_b64(&raw).expect("valid salt");
    Argon2::default()
        .hash_password(b"unused", &salt)
        .expect("hashing works")
        .to_string()
});

async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let username = req.username.to_lowercase();
    // Throttling here stops password guessing and, just as importantly, stops
    // an attacker from pinning the CPU: every attempt runs Argon2.
    let now_ms = now();
    if !state
        .limits
        .allow(&format!("login:{username}"), 10, QUARTER_HOUR, now_ms)
        || !state.limits.allow(&format!("login-ip:{ip}"), 30, QUARTER_HOUR, now_ms)
    {
        tracing::warn!(%username, %ip, "login throttled after too many attempts");
        return Err(too_many());
    }

    let row = sqlx::query("SELECT token, password_hash, verified FROM accounts WHERE username = ?")
        .bind(&username)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    // Same error for unknown user and wrong password: don't leak which usernames exist.
    let denied = || {
        err(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "Incorrect username or password",
        )
    };
    let Some(row) = row else {
        // Verify against a throwaway hash so a missing account costs the same
        // time as a wrong password: otherwise the response time enumerates
        // which usernames exist.
        let _ = PasswordHash::new(&DUMMY_HASH)
            .map(|h| Argon2::default().verify_password(req.password.as_bytes(), &h));
        return Err(denied());
    };
    let hash_str: String = row.get(1);
    let parsed = PasswordHash::new(&hash_str).map_err(internal)?;
    Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed)
        .map_err(|_| denied())?;
    if row.get::<i64, _>(2) == 0 {
        return Err(err(
            StatusCode::FORBIDDEN,
            "not_verified",
            "Account not verified; check your email",
        ));
    }
    state.limits.reset(&format!("login:{username}"));
    tracing::info!(username = %username, "login succeeded");
    Ok(Json(serde_json::json!({ "token": row.get::<String, _>(0) })))
}

#[derive(Deserialize)]
struct ForgotRequest {
    username: String,
}

/// Emails a reset code. Always answers 200: telling the caller whether an
/// account exists would turn this into a username oracle.
async fn forgot_password(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<ForgotRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let username = req.username.to_lowercase();
    let now_ms = now();
    if !state
        .limits
        .allow(&format!("forgot:{username}"), 3, HOUR, now_ms)
        || !state.limits.allow(&format!("forgot-ip:{ip}"), 10, HOUR, now_ms)
    {
        return Err(too_many());
    }

    let row = sqlx::query("SELECT email FROM accounts WHERE username = ? AND verified = 1")
        .bind(&username)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    let Some(row) = row else {
        tracing::info!(%username, "password reset requested for unknown account");
        return Ok(Json(serde_json::json!({ "status": "sent" })));
    };

    let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
    sqlx::query(
        "UPDATE accounts SET reset_code = ?, reset_expires = ?, reset_attempts = 0
         WHERE username = ?",
    )
    .bind(&code)
    .bind(now_ms + 60 * 60 * 1000)
    .bind(&username)
    .execute(&state.db)
    .await
    .map_err(internal)?;

    let email: String = row.get(0);
    tracing::info!(%username, "password reset code generated");
    match mail::MailConfig::from_env() {
        Some(cfg) => {
            let (email, username, code) = (email, username.clone(), code.clone());
            tokio::task::spawn_blocking(move || {
                match cfg.send_password_reset(&email, &username, &code) {
                    Ok(()) => tracing::info!(%username, "password reset email sent"),
                    Err(e) => tracing::error!(%username, "failed to send password reset email: {e}"),
                }
            });
        }
        None => {
            tracing::debug!(%username, "password reset code (dev only): {code}");
        }
    }

    let mut resp = serde_json::json!({ "status": "sent" });
    if std::env::var("HUSH_ECHO_CODE").is_ok_and(|v| v == "1") && mail::MailConfig::from_env().is_none()
    {
        resp["dev_code"] = code.into();
    }
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct ResetRequest {
    username: String,
    code: String,
    password: String,
}

/// Sets a new password from a reset code and rotates the session token, so a
/// reset also evicts whoever might have had the old password.
async fn reset_password(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<ResetRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let username = req.username.to_lowercase();
    let now_ms = now();
    if !state
        .limits
        .allow(&format!("reset:{username}"), 5, QUARTER_HOUR, now_ms)
        || !state.limits.allow(&format!("reset-ip:{ip}"), 20, QUARTER_HOUR, now_ms)
    {
        return Err(too_many());
    }
    if req.password.len() < 8 {
        return Err(bad_request(
            "weak_password",
            "Password must be at least 8 characters",
        ));
    }
    if req.password.len() > 1024 {
        return Err(bad_request("password_too_long", "Password is too long"));
    }

    let row = sqlx::query(
        "SELECT reset_code, reset_expires, reset_attempts FROM accounts WHERE username = ?",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(|| bad_request("invalid_code", "Incorrect or expired code"))?;

    let stored: Option<String> = row.get(0);
    let expires: Option<i64> = row.get(1);
    let attempts: i64 = row.get(2);
    let bad_code = || bad_request("invalid_code", "Incorrect or expired code");
    if attempts >= MAX_VERIFY_ATTEMPTS {
        return Err(bad_code());
    }
    let valid = stored
        .as_deref()
        .is_some_and(|s| constant_time_eq(s, &req.code))
        && expires.is_some_and(|e| e > now_ms);
    if !valid {
        let spent = attempts + 1;
        sqlx::query(
            "UPDATE accounts SET reset_attempts = ?,
             reset_code = CASE WHEN ? THEN NULL ELSE reset_code END WHERE username = ?",
        )
        .bind(spent)
        .bind(spent >= MAX_VERIFY_ATTEMPTS)
        .bind(&username)
        .execute(&state.db)
        .await
        .map_err(internal)?;
        return Err(bad_code());
    }

    let mut salt_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(internal)?;
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(internal)?
        .to_string();
    let token = new_token();

    sqlx::query(
        "UPDATE accounts SET password_hash = ?, token = ?, reset_code = NULL,
         reset_expires = NULL, reset_attempts = 0 WHERE username = ?",
    )
    .bind(&password_hash)
    .bind(&token)
    .bind(&username)
    .execute(&state.db)
    .await
    .map_err(internal)?;

    state.limits.reset(&format!("login:{username}"));
    tracing::info!(%username, "password reset");
    Ok(Json(serde_json::json!({ "status": "reset" })))
}

async fn get_profile(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let username = username.to_lowercase();
    let row = sqlx::query(
        "SELECT alias, identity_key, status FROM accounts WHERE username = ? AND verified = 1",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;
    let connected = state.live.lock().await.contains_key(&username);
    Ok(Json(serde_json::json!({
        "username": username,
        "alias": row.get::<String, _>(0),
        "identity_key": row.get::<String, _>(1),
        "status": if connected { row.get::<String, _>(2) } else { "offline".to_string() },
    })))
}

#[derive(Deserialize)]
struct UpdateMeRequest {
    alias: Option<String>,
    status: Option<String>,
}

/// Updates the caller's own profile: display name and/or chosen presence.
async fn update_me(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<UpdateMeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(alias) = &req.alias {
        let alias = alias.trim();
        if alias.is_empty() || alias.len() > MAX_ALIAS_BYTES {
            return Err(bad_request(
                "alias_too_long",
                "Display name must be 1-64 characters",
            ));
        }
        sqlx::query("UPDATE accounts SET alias = ? WHERE username = ?")
            .bind(alias)
            .bind(&auth.username)
            .execute(&state.db)
            .await
            .map_err(internal)?;
        tracing::debug!(user = %auth.username, "display name updated");
    }
    if let Some(status) = &req.status {
        if !SETTABLE_STATUSES.contains(&status.as_str()) {
            return Err(bad_request("invalid_status", "Unknown status"));
        }
        sqlx::query("UPDATE accounts SET status = ? WHERE username = ?")
            .bind(status)
            .bind(&auth.username)
            .execute(&state.db)
            .await
            .map_err(internal)?;
        tracing::debug!(user = %auth.username, %status, "presence updated");
    }

    if req.alias.is_some() || req.status.is_some() {
        notify_watchers(&state.db, &state.live, &auth.username).await;
    }

    let row = sqlx::query("SELECT alias, status FROM accounts WHERE username = ?")
        .bind(&auth.username)
        .fetch_one(&state.db)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({
        "username": auth.username,
        "alias": row.get::<String, _>(0),
        "status": row.get::<String, _>(1),
    })))
}

/// Nudges a user's open stream so it re-fetches its contact list.
async fn notify_contacts_changed(state: &AppState, username: &str) {
    if let Some(listener) = state.live.lock().await.get(username) {
        let _ = listener.tx.try_send(Push::ContactsChanged);
    }
}

/// Records that the account was connected at this instant, so a contact who
/// is offline can still be shown when they were last around.
async fn touch_last_seen(db: &SqlitePool, username: &str) {
    let _ = sqlx::query("UPDATE accounts SET last_seen = ? WHERE username = ?")
        .bind(now())
        .bind(username)
        .execute(db)
        .await;
}

/// Nudges everyone who has `username` as an accepted contact. Used whenever
/// something they can see changes — presence, display name, connecting or
/// disconnecting — so their list updates at once instead of at the next poll.
async fn notify_watchers(db: &SqlitePool, live: &LiveMap, username: &str) {
    let peers = sqlx::query("SELECT peer FROM contacts WHERE owner = ? AND state = 'accepted'")
        .bind(username)
        .fetch_all(db)
        .await
        .unwrap_or_default();
    let map = live.lock().await;
    for row in peers {
        let peer: String = row.get(0);
        if let Some(listener) = map.get(&peer) {
            let _ = listener.tx.try_send(Push::ContactsChanged);
        }
    }
}

async fn link_state(db: &SqlitePool, owner: &str, peer: &str) -> Result<Option<String>, ApiError> {
    Ok(
        sqlx::query("SELECT state FROM contacts WHERE owner = ? AND peer = ?")
            .bind(owner)
            .bind(peer)
            .fetch_optional(db)
            .await
            .map_err(internal)?
            .map(|r| r.get::<String, _>(0)),
    )
}

async fn set_link(
    tx: &mut sqlx::SqliteConnection,
    owner: &str,
    peer: &str,
    state: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO contacts (owner, peer, state, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(owner, peer) DO UPDATE SET state = excluded.state",
    )
    .bind(owner)
    .bind(peer)
    .bind(state)
    .bind(now())
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    Ok(())
}

/// The caller's contact list: accepted contacts and pending requests in both
/// directions, with alias and current presence.
async fn list_contacts(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT c.peer, c.state, a.alias, a.status, a.last_seen
         FROM contacts c JOIN accounts a ON a.username = c.peer
         WHERE c.owner = ? ORDER BY c.created_at",
    )
    .bind(&auth.username)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;

    let live = state.live.lock().await;
    let contacts: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let peer: String = r.get(0);
            let connected = live.contains_key(&peer);
            serde_json::json!({
                "username": peer,
                "state": r.get::<String, _>(1),
                "alias": r.get::<String, _>(2),
                "status": if connected { r.get::<String, _>(3) } else { "offline".into() },
                // Only meaningful while they are away.
                "last_seen": if connected { None } else { r.get::<Option<i64>, _>(4) },
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "contacts": contacts })))
}

/// Sends a contact request. If the peer already requested us, this accepts
/// instead, so the two directions cannot deadlock.
async fn request_contact(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let peer = peer.to_lowercase();
    if peer == auth.username {
        return Err(bad_request("self_contact", "You cannot add yourself"));
    }
    // Two separate budgets. The wide one caps spamming people who do exist;
    // the tight one caps *misses*, which is what walking a dictionary of
    // usernames looks like — that is the enumeration vector, since a request
    // to an unknown account is the one call that reveals non-existence.
    let now_ms = now();
    if !state
        .limits
        .allow(&format!("contact:{}", auth.username), MAX_CONTACT_REQUESTS_PER_HOUR, HOUR, now_ms)
    {
        tracing::warn!(user = %auth.username, "contact requests throttled");
        return Err(too_many());
    }

    let exists = sqlx::query("SELECT 1 FROM accounts WHERE username = ? AND verified = 1")
        .bind(&peer)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    if exists.is_none() {
        if !state.limits.allow(
            &format!("contact-miss:{}", auth.username),
            MAX_UNKNOWN_LOOKUPS_PER_HOUR,
            HOUR,
            now_ms,
        ) {
            tracing::warn!(user = %auth.username, "possible username enumeration, throttled");
            return Err(too_many());
        }
        return Err(not_found());
    }

    // Someone who blocked us must not learn that we tried, and must not get a
    // request. The attempt is recorded on our side only, so it simply stays
    // pending forever from here.
    if link_state(&state.db, &peer, &auth.username).await?.as_deref() == Some("blocked") {
        set_link(
            &mut *state.db.acquire().await.map_err(internal)?,
            &auth.username,
            &peer,
            "outgoing",
        )
        .await?;
        tracing::debug!(from = %auth.username, to = %peer, "contact request to a blocker, ignored");
        return Ok(Json(serde_json::json!({ "state": "outgoing" })));
    }

    let existing = link_state(&state.db, &auth.username, &peer).await?;
    let resulting_state = match existing.as_deref() {
        Some("accepted") => {
            return Err(err(
                StatusCode::CONFLICT,
                "already_contacts",
                "You are already contacts",
            ))
        }
        Some("outgoing") => {
            return Err(err(
                StatusCode::CONFLICT,
                "request_pending",
                "A request is already pending",
            ))
        }
        // They asked first: this is an acceptance.
        Some("incoming") => "accepted",
        _ => "outgoing",
    };

    let mut tx = state.db.begin().await.map_err(internal)?;
    set_link(&mut tx, &auth.username, &peer, resulting_state).await?;
    set_link(
        &mut tx,
        &peer,
        &auth.username,
        if resulting_state == "accepted" { "accepted" } else { "incoming" },
    )
    .await?;
    tx.commit().await.map_err(internal)?;

    // Who added whom is exactly the social graph, so it stays at debug like
    // message metadata: an info-level log must not record it.
    tracing::debug!(from = %auth.username, to = %peer, state = %resulting_state, "contact request");
    notify_contacts_changed(&state, &peer).await;
    Ok(Json(serde_json::json!({ "state": resulting_state })))
}

/// Accepts a pending incoming request.
async fn accept_contact(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let peer = peer.to_lowercase();
    if link_state(&state.db, &auth.username, &peer).await?.as_deref() != Some("incoming") {
        return Err(bad_request("no_request", "There is no request to accept"));
    }
    let mut tx = state.db.begin().await.map_err(internal)?;
    set_link(&mut tx, &auth.username, &peer, "accepted").await?;
    set_link(&mut tx, &peer, &auth.username, "accepted").await?;
    tx.commit().await.map_err(internal)?;

    tracing::debug!(user = %auth.username, peer = %peer, "contact accepted");
    notify_contacts_changed(&state, &peer).await;
    Ok(Json(serde_json::json!({ "state": "accepted" })))
}

/// Blocks a peer: they stop being a contact, cannot message us, and a request
/// from them never reaches us. Unblocking is just removing the contact.
async fn block_contact(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let peer = peer.to_lowercase();
    if peer == auth.username {
        return Err(bad_request("self_contact", "You cannot block yourself"));
    }

    let mut tx = state.db.begin().await.map_err(internal)?;
    set_link(&mut tx, &auth.username, &peer, "blocked").await?;
    // Their side of the link goes entirely: the block should look like the
    // contact simply disappeared, not like a door with our name on it.
    sqlx::query("DELETE FROM contacts WHERE owner = ? AND peer = ?")
        .bind(&peer)
        .bind(&auth.username)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    // Anything already queued for us from them is dropped too.
    sqlx::query("DELETE FROM messages WHERE recipient = ? AND sender = ?")
        .bind(&auth.username)
        .bind(&peer)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;

    tracing::debug!(user = %auth.username, peer = %peer, "contact blocked");
    notify_contacts_changed(&state, &peer).await;
    Ok(Json(serde_json::json!({ "state": "blocked" })))
}

/// Rejects a request, cancels one we sent, removes an existing contact, or
/// lifts a block.
async fn remove_contact(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(peer): Path<String>,
) -> Result<StatusCode, ApiError> {
    let peer = peer.to_lowercase();
    sqlx::query("DELETE FROM contacts WHERE (owner = ? AND peer = ?) OR (owner = ? AND peer = ?)")
        .bind(&auth.username)
        .bind(&peer)
        .bind(&peer)
        .bind(&auth.username)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    tracing::debug!(user = %auth.username, peer = %peer, "contact removed");
    notify_contacts_changed(&state, &peer).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PresenceRequest {
    usernames: Vec<String>,
}

/// Presence for a set of users: their chosen status while connected, or
/// "offline" when they hold no live stream.
async fn query_presence(
    _auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<PresenceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.usernames.len() > 500 {
        return Err(bad_request("invalid_request", "Too many usernames"));
    }
    let live = state.live.lock().await;
    let mut out = serde_json::Map::new();
    for username in &req.usernames {
        let username = username.to_lowercase();
        let status = if live.contains_key(&username) {
            sqlx::query("SELECT status FROM accounts WHERE username = ?")
                .bind(&username)
                .fetch_optional(&state.db)
                .await
                .map_err(internal)?
                .map(|r| r.get::<String, _>(0))
                .unwrap_or_else(|| "offline".into())
        } else {
            "offline".into()
        };
        out.insert(username, serde_json::Value::String(status));
    }
    Ok(Json(serde_json::json!({ "presence": out })))
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
    if req.one_time_prekeys.len() > MAX_PREKEYS_PER_UPLOAD {
        return Err(bad_request("too_many_prekeys", "Too many prekeys"));
    }
    let bundle = req.bundle_static.to_string();
    if bundle.len() > MAX_FIELD_BYTES
        || req.one_time_prekeys.iter().any(|pk| pk.data.len() > MAX_FIELD_BYTES)
        || req.identity_key.as_ref().is_some_and(|k| k.len() > MAX_FIELD_BYTES)
    {
        return Err(bad_request("invalid_keys", "Invalid key material"));
    }

    let mut tx = state.db.begin().await.map_err(internal)?;
    sqlx::query("UPDATE accounts SET bundle_static = ? WHERE username = ?")
        .bind(&bundle)
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
        tracing::info!(username = %auth.username, "identity re-provisioned (new device)");
    }
    sqlx::query("DELETE FROM one_time_prekeys WHERE username = ?")
        .bind(&auth.username)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    for pk in &req.one_time_prekeys {
        if pk.kind != "ec" && pk.kind != "kyber" {
            return Err(bad_request("invalid_keys", "Prekey kind must be ec or kyber"));
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
    let username = username.to_lowercase();
    let row = sqlx::query(
        "SELECT registration_id, identity_key, bundle_static FROM accounts WHERE username = ?",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await
    .map_err(internal)?
    .ok_or_else(not_found)?;

    let bundle_static: Option<String> = row.get(2);
    let bundle_static = bundle_static
        .ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                "no_keys",
                "That user cannot receive messages yet",
            )
        })?;

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
    let recipient = recipient.to_lowercase();
    let now_ms = now();
    if !state
        .limits
        .allow(&format!("send:{}", auth.username), 120, MINUTE, now_ms)
    {
        return Err(too_many());
    }

    let exists = sqlx::query("SELECT 1 FROM accounts WHERE username = ?")
        .bind(&recipient)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    if exists.is_none() {
        return Err(not_found());
    }
    // Messaging is contacts-only: an accepted link is what a contact request
    // buys you, and it also keeps strangers from filling anyone's mailbox.
    if link_state(&state.db, &recipient, &auth.username).await?.as_deref() != Some("accepted") {
        return Err(err(
            StatusCode::FORBIDDEN,
            "not_a_contact",
            "You can only message accepted contacts",
        ));
    }

    // Cap the undelivered queue so one sender cannot fill the disk (or a
    // recipient's memory on reconnect) by flooding an offline account.
    let queued: i64 = sqlx::query("SELECT COUNT(*) FROM messages WHERE recipient = ?")
        .bind(&recipient)
        .fetch_one(&state.db)
        .await
        .map_err(internal)?
        .get(0);
    if queued >= MAX_QUEUED_MESSAGES {
        tracing::warn!(to = %recipient, "mailbox full, delivery refused");
        return Err(err(
            StatusCode::INSUFFICIENT_STORAGE,
            "mailbox_full",
            "The recipient's mailbox is full",
        ));
    }

    let msg = OutMessage {
        id: uuid::Uuid::new_v4().to_string(),
        sender: auth.username,
        body: req.body,
        created_at: now_ms,
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

    tracing::debug!(from = %msg.sender, to = %recipient, id = %msg.id, "message queued");
    // Best-effort live push; the SSE backlog query covers anyone offline.
    if let Some(listener) = state.live.lock().await.get(&recipient) {
        let _ = listener.tx.try_send(Push::Message(msg.clone()));
        tracing::debug!(to = %recipient, id = %msg.id, "live delivery (SSE)");
    }
    Ok(Json(SendMessageResponse { id: msg.id }))
}

async fn message_stream(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // Opening streams is cheap per request but expensive in aggregate; a
    // client stuck in a reconnect loop must not be able to spin here.
    if !state
        .limits
        .allow(&format!("stream:{}", auth.username), 30, MINUTE, now())
    {
        tracing::warn!(user = %auth.username, "too many reconnections, stream refused");
        return Err(too_many());
    }

    let (tx, rx) = mpsc::channel::<Push>(256);
    let listener_id = state.next_listener_id.fetch_add(1, Ordering::Relaxed);
    state.live.lock().await.insert(
        auth.username.clone(),
        Listener {
            id: listener_id,
            tx: tx.clone(),
        },
    );
    // Without this the map grows for every connection ever made and keeps
    // senders alive for streams that are long gone.
    let cleanup = LiveGuard {
        live: state.live.clone(),
        db: state.db.clone(),
        username: auth.username.clone(),
        id: listener_id,
    };
    // Coming online is a presence change for everyone watching.
    touch_last_seen(&state.db, &auth.username).await;
    notify_watchers(&state.db, &state.live, &auth.username).await;

    // Backlog first, then live pushes. Clients dedupe by message id: a message
    // arriving during the backlog query can be delivered twice.
    let rows = sqlx::query(
        "SELECT id, sender, body, created_at FROM messages WHERE recipient = ? ORDER BY created_at",
    )
    .bind(&auth.username)
    .fetch_all(&state.db)
    .await
    .map_err(internal)?;
    tracing::debug!(user = %auth.username, backlog = rows.len(), "event stream opened");
    tokio::spawn(async move {
        for r in rows {
            let msg = OutMessage {
                id: r.get(0),
                sender: r.get(1),
                body: r.get(2),
                created_at: r.get(3),
            };
            if tx.send(Push::Message(msg)).await.is_err() {
                return;
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(move |push| {
        // Moving the guard into the closure ties its lifetime to the stream.
        let _keep = &cleanup;
        Ok(match push {
            Push::Message(msg) => Event::default()
                .event("message")
                .data(serde_json::to_string(&msg).expect("OutMessage serializes")),
            Push::ContactsChanged => Event::default().event("contacts").data("{}"),
            Push::Receipt { id, state, at } => Event::default().event("receipt").data(
                serde_json::json!({ "id": id, "state": state, "at": at }).to_string(),
            ),
        })
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Removes this stream's entry from the live map when the client disconnects,
/// unless a newer connection for the same account has replaced it.
struct LiveGuard {
    live: LiveMap,
    db: SqlitePool,
    username: String,
    id: u64,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        let (live, db, username, id) =
            (self.live.clone(), self.db.clone(), self.username.clone(), self.id);
        tokio::spawn(async move {
            let removed = {
                let mut map = live.lock().await;
                let mine = map.get(&username).is_some_and(|l| l.id == id);
                if mine {
                    map.remove(&username);
                    tracing::debug!(user = %username, "event stream closed");
                }
                mine
            };
            // Going offline is a presence change too, and fixes the moment
            // the contact was last around.
            if removed {
                touch_last_seen(&db, &username).await;
                notify_watchers(&db, &live, &username).await;
            }
        });
    }
}

/// Reports that the caller has read a message, telling its sender.
async fn mark_read(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if id.len() > 128 {
        return Err(bad_request("invalid_id", "Invalid identifier"));
    }
    let row = sqlx::query("SELECT sender FROM deliveries WHERE id = ? AND recipient = ?")
        .bind(&id)
        .bind(&auth.username)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    // Unknown or already reported: nothing to do, and nothing to leak.
    let Some(row) = row else {
        return Ok(StatusCode::NO_CONTENT);
    };
    let sender: String = row.get(0);

    sqlx::query("DELETE FROM deliveries WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(internal)?;

    if let Some(listener) = state.live.lock().await.get(&sender) {
        let _ = listener.tx.try_send(Push::Receipt {
            id: id.clone(),
            state: "read",
            at: now(),
        });
    }
    tracing::debug!(user = %auth.username, id = %id, "message read");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ArchiveEntryRequest {
    /// Opaque client-encrypted history entry.
    blob: String,
}

/// Stores one encrypted history entry. Overwrites on repeat so a client can
/// safely retry uploads.
async fn put_archive(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ArchiveEntryRequest>,
) -> Result<StatusCode, ApiError> {
    if id.len() > 128 {
        return Err(bad_request("invalid_id", "Invalid identifier"));
    }
    if !state
        .limits
        .allow(&format!("archive:{}", auth.username), 600, MINUTE, now())
    {
        return Err(too_many());
    }
    // Overwrites of an existing entry are always allowed; only growth counts
    // against the quota.
    let existing = sqlx::query("SELECT 1 FROM archive WHERE username = ? AND id = ?")
        .bind(&auth.username)
        .bind(&id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    if existing.is_none() {
        let count: i64 = sqlx::query("SELECT COUNT(*) FROM archive WHERE username = ?")
            .bind(&auth.username)
            .fetch_one(&state.db)
            .await
            .map_err(internal)?
            .get(0);
        if count >= MAX_ARCHIVE_ENTRIES {
            return Err(err(
                StatusCode::INSUFFICIENT_STORAGE,
                "archive_full",
                "Your history archive is full",
            ));
        }
    }

    sqlx::query(
        "INSERT INTO archive (username, id, blob, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(username, id) DO UPDATE SET blob = excluded.blob",
    )
    .bind(&auth.username)
    .bind(&id)
    .bind(&req.blob)
    .bind(now())
    .execute(&state.db)
    .await
    .map_err(internal)?;
    tracing::debug!(user = %auth.username, id = %id, "history entry archived");
    Ok(StatusCode::NO_CONTENT)
}

/// Drops one entry from the account's history archive, so a deleted message
/// does not come back the next time the history is restored.
async fn delete_archive_entry(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if id.len() > 128 {
        return Err(bad_request("invalid_id", "Invalid identifier"));
    }
    sqlx::query("DELETE FROM archive WHERE username = ? AND id = ?")
        .bind(&auth.username)
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(internal)?;
    tracing::debug!(user = %auth.username, id = %id, "history entry deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Returns the whole encrypted history archive for the account.
async fn list_archive(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = sqlx::query("SELECT id, blob FROM archive WHERE username = ? ORDER BY created_at")
        .bind(&auth.username)
        .fetch_all(&state.db)
        .await
        .map_err(internal)?;
    tracing::debug!(user = %auth.username, entries = rows.len(), "encrypted history downloaded");
    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| serde_json::json!({ "id": r.get::<String, _>(0), "blob": r.get::<String, _>(1) }))
        .collect();
    Ok(Json(serde_json::json!({ "entries": entries })))
}

#[derive(Deserialize)]
struct AckParams {
    /// Set when the recipient could not decrypt the message.
    undecryptable: Option<String>,
}

async fn ack_message(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<AckParams>,
) -> Result<StatusCode, ApiError> {
    if id.len() > 128 {
        return Err(bad_request("invalid_id", "Invalid identifier"));
    }

    // A message the recipient could not decrypt is dropped without telling
    // the sender it arrived: they still hold it as unsent, which is what lets
    // them send it again once the session is rebuilt.
    if params.undecryptable.is_some() {
        sqlx::query("DELETE FROM messages WHERE id = ? AND recipient = ?")
            .bind(&id)
            .bind(&auth.username)
            .execute(&state.db)
            .await
            .map_err(internal)?;
        tracing::debug!(user = %auth.username, id = %id, "undecryptable message discarded");
        return Ok(StatusCode::NO_CONTENT);
    }
    // Remember who sent it before the row goes, so a later read receipt still
    // knows where to go.
    let sender: Option<String> =
        sqlx::query("SELECT sender FROM messages WHERE id = ? AND recipient = ?")
            .bind(&id)
            .bind(&auth.username)
            .fetch_optional(&state.db)
            .await
            .map_err(internal)?
            .map(|r| r.get(0));

    sqlx::query("DELETE FROM messages WHERE id = ? AND recipient = ?")
        .bind(&id)
        .bind(&auth.username)
        .execute(&state.db)
        .await
        .map_err(internal)?;

    if let Some(sender) = sender {
        let now_ms = now();
        sqlx::query(
            "INSERT OR REPLACE INTO deliveries (id, sender, recipient, created_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&sender)
        .bind(&auth.username)
        .bind(now_ms)
        .execute(&state.db)
        .await
        .map_err(internal)?;
        // Opportunistic prune: receipts nobody claimed in a month.
        let _ = sqlx::query("DELETE FROM deliveries WHERE created_at < ?")
            .bind(now_ms - 30 * 24 * 60 * 60 * 1000)
            .execute(&state.db)
            .await;

        if let Some(listener) = state.live.lock().await.get(&sender) {
            let _ = listener.tx.try_send(Push::Receipt {
                id: id.clone(),
                state: "delivered",
                at: now_ms,
            });
        }
    }
    tracing::debug!(user = %auth.username, id = %id, "message acknowledged and deleted");
    Ok(StatusCode::NO_CONTENT)
}

// ---- Client updates -------------------------------------------------------
//
// The deployment drops the installer and its signature into HUSH_UPDATE_DIR;
// the server offers the newest one it finds there. Nothing here is trusted by
// the client on its own: the installer is verified against the public key
// built into the app, so a tampered file is refused.

/// `Hush_1.0.2_x64-setup.exe`, and nothing else: the name is used to build a
/// path, so it must not be able to point anywhere but that directory.
fn installer_version(file_name: &str) -> Option<(u64, u64, u64)> {
    let version = file_name
        .strip_prefix("Hush_")?
        .strip_suffix("_x64-setup.exe")?;
    parse_version(version)
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let parsed = (next()?, next()?, next()?);
    parts.next().is_none().then_some(parsed)
}

fn update_dir() -> Option<PathBuf> {
    let dir = std::env::var("HUSH_UPDATE_DIR").ok()?;
    let dir = dir.trim();
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

/// The newest installer available, as (version, file name).
fn newest_installer(dir: &FsPath) -> Option<((u64, u64, u64), String)> {
    let mut best: Option<((u64, u64, u64), String)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(version) = installer_version(&name) else {
            continue;
        };
        // Without the signature the client would refuse the download anyway.
        if !dir.join(format!("{name}.sig")).exists() {
            tracing::warn!("{name} has no signature and is ignored");
            continue;
        }
        if best.as_ref().is_none_or(|(best, _)| version > *best) {
            best = Some((version, name));
        }
    }
    best
}

/// Where clients should fetch the installer from. Built from the request so
/// the deployment needs no extra configuration, unless HUSH_PUBLIC_URL says
/// otherwise.
fn public_base(headers: &axum::http::HeaderMap) -> String {
    if let Ok(base) = std::env::var("HUSH_PUBLIC_URL") {
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() {
            return base.to_string();
        }
    }
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:8080");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or(if host.starts_with("127.0.0.1") || host.starts_with("localhost") {
            "http"
        } else {
            "https"
        });
    format!("{scheme}://{host}")
}

/// Tells the client whether a newer build exists. 204 means "you are current",
/// which is what the updater expects.
async fn update_manifest(
    Path((_target, _arch, current)): Path<(String, String, String)>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;

    let Some(dir) = update_dir() else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let Some(current) = parse_version(current.trim_start_matches('v')) else {
        return Err(bad_request("invalid_request", "Invalid version"));
    };
    let Some((version, file)) = newest_installer(&dir) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    if version <= current {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let signature = match std::fs::read_to_string(dir.join(format!("{file}.sig"))) {
        Ok(signature) => signature.trim().to_string(),
        Err(e) => {
            tracing::error!("cannot read the signature of {file}: {e}");
            return Err(internal(e));
        }
    };
    let notes = std::fs::read_to_string(dir.join("notes.txt"))
        .map(|n| n.trim().to_string())
        .unwrap_or_default();
    let (major, minor, patch) = version;

    tracing::debug!(%file, "offering an update");
    Ok(Json(serde_json::json!({
        "version": format!("{major}.{minor}.{patch}"),
        "notes": notes,
        "url": format!("{}/v1/update/download/{file}", public_base(&headers)),
        "signature": signature,
    }))
    .into_response())
}

async fn update_download(Path(file): Path<String>) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;

    let Some(dir) = update_dir() else {
        return Err(not_found());
    };
    // Only a name this server itself would have offered, so the path cannot
    // escape the directory.
    let signature = file.strip_suffix(".sig").unwrap_or(&file);
    if installer_version(signature).is_none() {
        return Err(not_found());
    }

    let bytes = std::fs::read(dir.join(&file)).map_err(|_| not_found())?;
    tracing::debug!(%file, "serving an update download");
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "application/octet-stream",
        )],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod update_tests {
    use super::*;

    #[test]
    fn only_our_own_installer_names_are_accepted() {
        assert_eq!(installer_version("Hush_1.0.2_x64-setup.exe"), Some((1, 0, 2)));
        assert_eq!(installer_version("Hush_10.20.30_x64-setup.exe"), Some((10, 20, 30)));

        // Anything else must not resolve to a file: the name becomes a path.
        for name in [
            "hush.sqlite3",
            "../hush.sqlite3",
            "Hush_1.0.2_x64-setup.exe.bak",
            r"Hush_../..\1.0.2_x64-setup.exe",
            "Hush_1.0_x64-setup.exe",
            "Hush_1.0.2.3_x64-setup.exe",
            "Hush__x64-setup.exe",
        ] {
            assert_eq!(installer_version(name), None, "{name} should be refused");
        }
    }

    #[test]
    fn the_newest_build_in_the_folder_wins() {
        let dir = std::env::temp_dir().join(format!("hush-updates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for version in ["1.0.2", "1.0.10", "1.1.0"] {
            let file = dir.join(format!("Hush_{version}_x64-setup.exe"));
            std::fs::write(&file, b"installer").unwrap();
            std::fs::write(format!("{}.sig", file.display()), b"signature").unwrap();
        }
        // Unsigned builds are ignored: the client would refuse them anyway.
        std::fs::write(dir.join("Hush_2.0.0_x64-setup.exe"), b"installer").unwrap();

        let (version, file) = newest_installer(&dir).unwrap();
        assert_eq!(version, (1, 1, 0));
        assert_eq!(file, "Hush_1.1.0_x64-setup.exe");

        std::fs::remove_dir_all(&dir).ok();
    }
}
