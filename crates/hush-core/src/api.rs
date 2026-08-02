//! HTTP client for the Hush relay server (REST + SSE).

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

/// A message delivered by the server. `body` is an opaque envelope for
/// [`crate::engine::Engine::decrypt`].
/// Public profile of another user as served by the relay.
#[derive(Clone, Debug)]
pub struct RemoteProfile {
    pub alias: String,
    pub identity_key: String,
    /// "online", "away", "busy" or "offline".
    pub status: String,
}

/// One entry of the server-side contact list.
#[derive(Clone, Debug)]
pub struct ContactEntry {
    pub username: String,
    pub alias: String,
    /// "incoming", "outgoing" or "accepted".
    pub state: String,
    pub status: String,
}

/// Anything the server pushes down the stream.
#[derive(Clone, Debug)]
pub enum ServerEvent {
    Message(IncomingMessage),
    ContactsChanged,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IncomingMessage {
    pub id: String,
    pub sender: String,
    pub body: String,
    pub created_at: i64,
}

pub struct ApiClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl ApiClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into(),
            token: None,
        }
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn set_token(&mut self, token: impl Into<String>) {
        self.token = Some(token.into());
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let token = self.token.as_ref().context("not logged in")?;
        Ok(req.bearer_auth(token))
    }

    /// Turns error responses into a stable error code the UI can localise
    /// (the server answers in English with `{code, message}`). No HTTP jargon
    /// reaches the UI; unrecognised bodies fall back to a generic code.
    async fn check(res: reqwest::Response) -> Result<reqwest::Response> {
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let body = res.text().await.unwrap_or_default();
        if status.is_server_error() {
            tracing::error!("server error {status}: {body}");
            bail!("internal_error");
        }
        let code = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["code"].as_str().map(str::to_string));
        match code {
            Some(code) => bail!(code),
            None => {
                tracing::warn!("unparsable error body ({status}): {body}");
                bail!("request_failed")
            }
        }
    }

    /// Maps transport-level failures (server down, DNS, timeout) to a code.
    fn conn_err(e: reqwest::Error) -> anyhow::Error {
        tracing::warn!("connection error: {e}");
        anyhow::anyhow!("connection_failed")
    }

    /// Creates the account (pending email verification). Returns the dev
    /// verification code if the server echoes it (HUSH_ECHO_CODE=1).
    #[allow(clippy::too_many_arguments)]
    pub async fn register(
        &mut self,
        username: &str,
        alias: &str,
        email: &str,
        password: &str,
        registration_id: u32,
        identity_key_b64: &str,
    ) -> Result<Option<String>> {
        let res = self
            .http
            .post(format!("{}/v1/accounts", self.base))
            .json(&json!({
                "username": username,
                "alias": alias,
                "email": email,
                "password": password,
                "registration_id": registration_id,
                "identity_key": identity_key_b64,
            }))
            .send()
            .await
            .map_err(Self::conn_err)?;
        let body: Value = Self::check(res).await?.json().await?;
        Ok(body["dev_code"].as_str().map(str::to_string))
    }

    /// Confirms the account with the emailed code and stores the session token.
    pub async fn verify(&mut self, username: &str, code: &str) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/v1/accounts/verify", self.base))
            .json(&json!({ "username": username, "code": code }))
            .send()
            .await
            .map_err(Self::conn_err)?;
        let body: Value = Self::check(res).await?.json().await?;
        self.token = Some(
            body["token"]
                .as_str()
                .context("no token in response")?
                .to_string(),
        );
        Ok(())
    }

    /// Logs into an existing (verified) account and stores the session token.
    pub async fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/v1/sessions", self.base))
            .json(&json!({ "username": username, "password": password }))
            .send()
            .await
            .map_err(Self::conn_err)?;
        let body: Value = Self::check(res).await?.json().await?;
        self.token = Some(
            body["token"]
                .as_str()
                .context("no token in response")?
                .to_string(),
        );
        Ok(())
    }

    /// Uploads one encrypted history entry.
    pub async fn put_archive(&self, id: &str, blob: &str) -> Result<()> {
        let req = self.auth(self.http.put(format!("{}/v1/archive/{id}", self.base)))?;
        Self::check(
            req.json(&json!({ "blob": blob }))
                .send()
                .await
                .map_err(Self::conn_err)?,
        )
        .await?;
        Ok(())
    }

    /// Downloads the whole encrypted history archive as `(id, blob)` pairs.
    pub async fn list_archive(&self) -> Result<Vec<(String, String)>> {
        let req = self.auth(self.http.get(format!("{}/v1/archive", self.base)))?;
        let body: Value = Self::check(req.send().await.map_err(Self::conn_err)?)
            .await?
            .json()
            .await?;
        Ok(body["entries"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| {
                        Some((e["id"].as_str()?.to_string(), e["blob"].as_str()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Public profile (alias + current identity key) of a user.
    pub async fn fetch_profile(&self, username: &str) -> Result<RemoteProfile> {
        let req = self.auth(
            self.http
                .get(format!("{}/v1/profile/{username}", self.base)),
        )?;
        let body: Value = Self::check(req.send().await.map_err(Self::conn_err)?).await?.json().await?;
        Ok(RemoteProfile {
            alias: body["alias"].as_str().unwrap_or_default().to_string(),
            identity_key: body["identity_key"].as_str().unwrap_or_default().to_string(),
            status: body["status"].as_str().unwrap_or("offline").to_string(),
        })
    }

    /// Updates the caller's display name and/or presence.
    pub async fn update_me(&self, alias: Option<&str>, status: Option<&str>) -> Result<()> {
        let req = self.auth(self.http.patch(format!("{}/v1/me", self.base)))?;
        Self::check(
            req.json(&json!({ "alias": alias, "status": status }))
                .send()
                .await
                .map_err(Self::conn_err)?,
        )
        .await?;
        Ok(())
    }

    /// Presence for a set of users, as `username -> status`.
    pub async fn presence(&self, usernames: &[String]) -> Result<Vec<(String, String)>> {
        let req = self.auth(self.http.post(format!("{}/v1/presence", self.base)))?;
        let body: Value = Self::check(
            req.json(&json!({ "usernames": usernames }))
                .send()
                .await
                .map_err(Self::conn_err)?,
        )
        .await?
        .json()
        .await?;
        Ok(body["presence"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("offline").to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn upload_keys(&self, body: &Value) -> Result<()> {
        let req = self.auth(self.http.put(format!("{}/v1/keys", self.base)))?;
        Self::check(req.json(body).send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    pub async fn fetch_bundle(&self, username: &str) -> Result<Value> {
        let req = self.auth(self.http.get(format!("{}/v1/keys/{username}", self.base)))?;
        Ok(Self::check(req.send().await.map_err(Self::conn_err)?).await?.json().await?)
    }

    /// Sends an encrypted envelope; returns the server-assigned message id.
    pub async fn send_message(&self, recipient: &str, envelope: &str) -> Result<String> {
        let req = self.auth(
            self.http
                .put(format!("{}/v1/messages/{recipient}", self.base)),
        )?;
        let body: Value = Self::check(req.json(&json!({ "body": envelope })).send().await.map_err(Self::conn_err)?)
            .await?
            .json()
            .await?;
        Ok(body["id"].as_str().context("no id in response")?.to_string())
    }

    pub async fn ack_message(&self, id: &str) -> Result<()> {
        let req = self.auth(self.http.delete(format!("{}/v1/messages/{id}", self.base)))?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    /// Asks the server to email a password reset code. Succeeds even for
    /// unknown accounts, by design. Returns the dev code when echoed.
    pub async fn forgot_password(&self, username: &str) -> Result<Option<String>> {
        let res = self
            .http
            .post(format!("{}/v1/password/forgot", self.base))
            .json(&json!({ "username": username }))
            .send()
            .await
            .map_err(Self::conn_err)?;
        let body: Value = Self::check(res).await?.json().await?;
        Ok(body["dev_code"].as_str().map(str::to_string))
    }

    /// Sets a new password from an emailed reset code.
    pub async fn reset_password(&self, username: &str, code: &str, password: &str) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/v1/password/reset", self.base))
            .json(&json!({ "username": username, "code": code, "password": password }))
            .send()
            .await
            .map_err(Self::conn_err)?;
        Self::check(res).await?;
        Ok(())
    }

    /// The caller's contact list, including pending requests.
    pub async fn list_contacts(&self) -> Result<Vec<ContactEntry>> {
        let req = self.auth(self.http.get(format!("{}/v1/contacts", self.base)))?;
        let body: Value = Self::check(req.send().await.map_err(Self::conn_err)?)
            .await?
            .json()
            .await?;
        Ok(body["contacts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|c| ContactEntry {
                        username: c["username"].as_str().unwrap_or_default().to_string(),
                        alias: c["alias"].as_str().unwrap_or_default().to_string(),
                        state: c["state"].as_str().unwrap_or("accepted").to_string(),
                        status: c["status"].as_str().unwrap_or("offline").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Sends a contact request (or accepts, if they already asked).
    pub async fn request_contact(&self, peer: &str) -> Result<String> {
        let req = self.auth(self.http.post(format!("{}/v1/contacts/{peer}", self.base)))?;
        let body: Value = Self::check(req.send().await.map_err(Self::conn_err)?)
            .await?
            .json()
            .await?;
        Ok(body["state"].as_str().unwrap_or("outgoing").to_string())
    }

    pub async fn accept_contact(&self, peer: &str) -> Result<()> {
        let req = self.auth(
            self.http
                .post(format!("{}/v1/contacts/{peer}/accept", self.base)),
        )?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    /// Rejects a request, cancels one we sent, or removes a contact.
    pub async fn remove_contact(&self, peer: &str) -> Result<()> {
        let req = self.auth(self.http.delete(format!("{}/v1/contacts/{peer}", self.base)))?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    /// Opens the SSE stream. Returns a channel that yields the offline backlog
    /// first, then live events, until the connection drops.
    pub async fn stream(&self) -> Result<mpsc::Receiver<ServerEvent>> {
        let req = self.auth(
            self.http
                .get(format!("{}/v1/messages/stream", self.base)),
        )?;
        let res = Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let mut stream = res.bytes_stream();
            let mut buf = String::new();
            while let Some(Ok(chunk)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buf.find("\n\n") {
                    let frame: String = buf[..end].to_string();
                    buf.drain(..end + 2);
                    let name = frame
                        .lines()
                        .find_map(|l| l.strip_prefix("event: "))
                        .unwrap_or("message");
                    let data = frame.lines().find_map(|l| l.strip_prefix("data: "));
                    let event = match (name, data) {
                        ("contacts", _) => Some(ServerEvent::ContactsChanged),
                        ("message", Some(data)) => serde_json::from_str::<IncomingMessage>(data)
                            .ok()
                            .map(ServerEvent::Message),
                        // Keep-alive comments and unknown event types.
                        _ => None,
                    };
                    if let Some(event) = event {
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(rx)
    }
}
