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
    /// When they were last connected, in epoch millis. Only set while they
    /// are offline.
    pub last_seen: Option<i64>,
}

/// Anything the server pushes down the stream.
#[derive(Clone, Debug)]
pub enum ServerEvent {
    Message(IncomingMessage),
    ContactsChanged,
    /// A message we sent was delivered to, or read by, its recipient.
    Receipt { id: String, state: String, at: i64 },
}

#[derive(Clone, Debug, Deserialize)]
pub struct IncomingMessage {
    pub id: String,
    pub sender: String,
    /// Which of the sender's devices wrote it: each has its own session with
    /// us. Servers that predate devices do not send it.
    #[serde(default = "first_device")]
    pub sender_device: i64,
    pub body: String,
    pub created_at: i64,
}

fn first_device() -> i64 {
    1
}

pub struct ApiClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl ApiClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            // Identify ourselves: reqwest sends no User-Agent by default, and
            // filters in front of a server routinely reject requests that
            // arrive without one.
            http: reqwest::Client::builder()
                .user_agent(concat!("Hush/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("HTTP client builds with defaults"),
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
    /// Signs in. `device_id` keeps this device's place on the account, its
    /// queue and its sessions; without one the account gains a new device.
    /// Returns the device this session belongs to.
    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
        device_id: Option<i64>,
        device_name: &str,
    ) -> Result<i64> {
        let res = self
            .http
            .post(format!("{}/v1/sessions", self.base))
            .json(&json!({
                "username": username,
                "password": password,
                "device_id": device_id,
                "new_device": device_id.is_none(),
                "device_name": device_name,
            }))
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
        // A server that predates devices answers with a token alone.
        Ok(body["device_id"].as_i64().unwrap_or(1))
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

    /// Sends one encrypted copy per recipient device; returns the id the whole
    /// fan-out shares, so the sender still tracks a single message.
    pub async fn send_message(
        &self,
        recipient: &str,
        envelopes: &[(i64, String)],
    ) -> Result<String> {
        let envelopes: Vec<Value> = envelopes
            .iter()
            .map(|(device, body)| json!({ "device": device, "body": body }))
            .collect();
        let req = self.auth(
            self.http
                .put(format!("{}/v1/messages/{recipient}", self.base)),
        )?;
        let body: Value = Self::check(
            req.json(&json!({ "envelopes": envelopes }))
                .send()
                .await
                .map_err(Self::conn_err)?,
        )
        .await?
        .json()
        .await?;
        Ok(body["id"].as_str().context("no id in response")?.to_string())
    }

    /// Bundles for every device of `username`, each with the device it belongs
    /// to. Falls back to the single-device endpoint on an older server.
    pub async fn fetch_bundles(&self, username: &str) -> Result<Vec<(i64, Value)>> {
        let req = self.auth(
            self.http
                .get(format!("{}/v1/keys/{username}/devices", self.base)),
        )?;
        let res = req.send().await.map_err(Self::conn_err)?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            let bundle = self.fetch_bundle(username).await?;
            let device = bundle["device"].as_i64().unwrap_or(1);
            return Ok(vec![(device, bundle)]);
        }
        let body: Value = Self::check(res).await?.json().await?;
        let devices = body["devices"].as_array().context("no devices")?;
        Ok(devices
            .iter()
            .map(|d| (d["device"].as_i64().unwrap_or(1), d.clone()))
            .collect())
    }

    /// The account's devices, as shown in settings.
    pub async fn list_devices(&self) -> Result<Vec<Value>> {
        let req = self.auth(self.http.get(format!("{}/v1/devices", self.base)))?;
        let body: Value = Self::check(req.send().await.map_err(Self::conn_err)?)
            .await?
            .json()
            .await?;
        Ok(body["devices"].as_array().cloned().unwrap_or_default())
    }

    /// Signs a device out for good: its token stops working and its queue goes.
    pub async fn revoke_device(&self, device: i64) -> Result<()> {
        let req = self.auth(self.http.delete(format!("{}/v1/devices/{device}", self.base)))?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    /// Tells the sender that we have read their message.
    pub async fn mark_read(&self, id: &str) -> Result<()> {
        let req = self.auth(self.http.post(format!("{}/v1/messages/{id}/read", self.base)))?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    pub async fn ack_message(&self, id: &str) -> Result<()> {
        let req = self.auth(self.http.delete(format!("{}/v1/messages/{id}", self.base)))?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    /// Drops a message we could not decrypt. Unlike an acknowledgement this
    /// tells the sender nothing, so their copy stays unsent and can be sent
    /// again once the session is rebuilt.
    pub async fn discard_message(&self, id: &str) -> Result<()> {
        let req = self.auth(
            self.http
                .delete(format!("{}/v1/messages/{id}?undecryptable=1", self.base)),
        )?;
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
                        last_seen: c["last_seen"].as_i64(),
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

    /// Blocks a peer: they stop being a contact and cannot reach us again.
    pub async fn block_contact(&self, peer: &str) -> Result<()> {
        let req = self.auth(
            self.http
                .post(format!("{}/v1/contacts/{peer}/block", self.base)),
        )?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
    }

    /// Removes one entry from our history archive.
    pub async fn delete_archive_entry(&self, id: &str) -> Result<()> {
        let req = self.auth(self.http.delete(format!("{}/v1/archive/{id}", self.base)))?;
        Self::check(req.send().await.map_err(Self::conn_err)?).await?;
        Ok(())
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
            // Logged on exit: knowing whether the body ended cleanly or the
            // read failed is the difference between a normal disconnect and a
            // reconnect loop.
            let mut reason = "body ended";
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(e) => {
                        reason = "read error";
                        tracing::warn!("event stream read failed: {e}");
                        break;
                    }
                };
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
                        ("receipt", Some(data)) => serde_json::from_str::<Value>(data)
                            .ok()
                            .and_then(|v| {
                                Some(ServerEvent::Receipt {
                                    id: v["id"].as_str()?.to_string(),
                                    state: v["state"].as_str()?.to_string(),
                                    at: v["at"].as_i64().unwrap_or_default(),
                                })
                            }),
                        ("message", Some(data)) => serde_json::from_str::<IncomingMessage>(data)
                            .ok()
                            .map(ServerEvent::Message),
                        // Keep-alive comments and unknown event types.
                        _ => None,
                    };
                    if let Some(event) = event {
                        if tx.send(event).await.is_err() {
                            // The receiver is gone: a newer stream replaced us.
                            return;
                        }
                    }
                }
            }
            tracing::info!("event stream closed ({reason})");
        });
        Ok(rx)
    }
}
