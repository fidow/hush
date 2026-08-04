//! Outgoing email. Fully configured via environment variables so any SMTP
//! relay can be used:
//!
//! - `HUSH_SMTP_HOST`     relay host (unset = no email; codes are logged)
//! - `HUSH_SMTP_PORT`     default `25`
//! - `HUSH_SMTP_FROM`     sender address, e.g. `hush@example.com`
//! - `HUSH_SMTP_USER` / `HUSH_SMTP_PASS`  optional credentials
//! - `HUSH_SMTP_STARTTLS` set to `1` to use STARTTLS
//! - `HUSH_SMTP_TLS`      set to `1` for TLS from the first byte (port 465)
//!
//! Credentials are only ever sent over an encrypted connection: with neither
//! `HUSH_SMTP_TLS` nor `HUSH_SMTP_STARTTLS` set, a configured user and
//! password would cross the network in the clear, so the send is refused
//! instead.

use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};

#[derive(Clone)]
pub struct MailConfig {
    host: String,
    port: u16,
    from: String,
    user: Option<String>,
    pass: Option<String>,
    starttls: bool,
    /// TLS from the first byte, as port 465 expects.
    tls: bool,
}

/// Whether email delivery is unconfigured (verification codes cannot be sent).
pub fn config_missing() -> bool {
    MailConfig::from_env().is_none()
}

impl MailConfig {
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("HUSH_SMTP_HOST").ok()?;
        Some(Self {
            host,
            port: std::env::var("HUSH_SMTP_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(25),
            from: {
                let from =
                    std::env::var("HUSH_SMTP_FROM").unwrap_or_else(|_| "hush@localhost".into());
                // Bare addresses get the app's display name: `Hush <addr>`.
                if from.contains('<') {
                    from
                } else {
                    format!("Hush <{from}>")
                }
            },
            user: std::env::var("HUSH_SMTP_USER").ok(),
            pass: std::env::var("HUSH_SMTP_PASS").ok(),
            starttls: std::env::var("HUSH_SMTP_STARTTLS").is_ok_and(|v| v == "1"),
            tls: std::env::var("HUSH_SMTP_TLS").is_ok_and(|v| v == "1"),
        })
    }

    /// Whether credentials are configured but the connection carrying them
    /// would not be encrypted.
    pub fn credentials_in_the_clear(&self) -> bool {
        self.user.is_some() && self.pass.is_some() && !self.starttls && !self.tls
    }

    /// Blocking; call from `spawn_blocking`.
    pub fn send_verification(&self, to: &str, username: &str, code: &str) -> anyhow::Result<()> {
        self.send(
            to,
            "Your Hush verification code",
            &format!(
                "Hi {username},\n\n\
                 Your code to confirm the account is: {code}\n\n\
                 It expires in 24 hours. If you did not create this account, ignore this message.\n"
            ),
        )
    }

    /// Blocking; call from `spawn_blocking`.
    pub fn send_password_reset(&self, to: &str, username: &str, code: &str) -> anyhow::Result<()> {
        self.send(
            to,
            "Reset your Hush password",
            &format!(
                "Hi {username},\n\n\
                 Your password reset code is: {code}\n\n\
                 It expires in 1 hour. If you did not ask for this, ignore this message —\n\
                 your password stays unchanged.\n\n\
                 Note: this does not affect your recovery key, which is what restores your\n\
                 message history on a new device.\n"
            ),
        )
    }

    fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        let email = Message::builder()
            .from(self.from.parse()?)
            .to(to.parse()?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())?;

        // Refused rather than sent: handing the relay's password to whatever
        // is between us and it, once, is enough to lose it. A relay that
        // wants no encryption also wants no credentials.
        if self.credentials_in_the_clear() {
            anyhow::bail!(
                "refusing to send SMTP credentials over an unencrypted connection: \
                 set HUSH_SMTP_TLS=1 (port 465) or HUSH_SMTP_STARTTLS=1"
            );
        }

        tracing::info!("sending email to {to} via {}:{}", self.host, self.port);
        let mut builder = if self.tls {
            SmtpTransport::relay(&self.host)?
        } else if self.starttls {
            SmtpTransport::starttls_relay(&self.host)?
        } else {
            SmtpTransport::builder_dangerous(&self.host)
        }
        .port(self.port)
        // Fail fast so problems show up in the log within seconds.
        .timeout(Some(std::time::Duration::from_secs(10)));
        if let (Some(user), Some(pass)) = (&self.user, &self.pass) {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }
        builder.build().send(&email)?;
        Ok(())
    }
}
