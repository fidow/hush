//! HTTP client for the Hush relay server (REST + SSE).

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

/// A message delivered by the server. `body` is an opaque envelope for
/// [`crate::engine::Engine::decrypt`].
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

    async fn check(res: reqwest::Response) -> Result<reqwest::Response> {
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            bail!("server returned {status}: {body}");
        }
        Ok(res)
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
            .await?;
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
            .await?;
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
            .await?;
        let body: Value = Self::check(res).await?.json().await?;
        self.token = Some(
            body["token"]
                .as_str()
                .context("no token in response")?
                .to_string(),
        );
        Ok(())
    }

    /// Public profile (alias) of a user.
    pub async fn fetch_profile(&self, username: &str) -> Result<String> {
        let req = self.auth(
            self.http
                .get(format!("{}/v1/profile/{username}", self.base)),
        )?;
        let body: Value = Self::check(req.send().await?).await?.json().await?;
        Ok(body["alias"].as_str().unwrap_or_default().to_string())
    }

    pub async fn upload_keys(&self, body: &Value) -> Result<()> {
        let req = self.auth(self.http.put(format!("{}/v1/keys", self.base)))?;
        Self::check(req.json(body).send().await?).await?;
        Ok(())
    }

    pub async fn fetch_bundle(&self, username: &str) -> Result<Value> {
        let req = self.auth(self.http.get(format!("{}/v1/keys/{username}", self.base)))?;
        Ok(Self::check(req.send().await?).await?.json().await?)
    }

    /// Sends an encrypted envelope; returns the server-assigned message id.
    pub async fn send_message(&self, recipient: &str, envelope: &str) -> Result<String> {
        let req = self.auth(
            self.http
                .put(format!("{}/v1/messages/{recipient}", self.base)),
        )?;
        let body: Value = Self::check(req.json(&json!({ "body": envelope })).send().await?)
            .await?
            .json()
            .await?;
        Ok(body["id"].as_str().context("no id in response")?.to_string())
    }

    pub async fn ack_message(&self, id: &str) -> Result<()> {
        let req = self.auth(self.http.delete(format!("{}/v1/messages/{id}", self.base)))?;
        Self::check(req.send().await?).await?;
        Ok(())
    }

    /// Opens the SSE stream. Returns a channel that yields the offline backlog
    /// first, then live messages, until the connection drops.
    pub async fn stream(&self) -> Result<mpsc::Receiver<IncomingMessage>> {
        let req = self.auth(
            self.http
                .get(format!("{}/v1/messages/stream", self.base)),
        )?;
        let res = Self::check(req.send().await?).await?;
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let mut stream = res.bytes_stream();
            let mut buf = String::new();
            while let Some(Ok(chunk)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buf.find("\n\n") {
                    let event: String = buf[..end].to_string();
                    buf.drain(..end + 2);
                    if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) {
                        if let Ok(msg) = serde_json::from_str::<IncomingMessage>(data) {
                            if tx.send(msg).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
        Ok(rx)
    }
}
