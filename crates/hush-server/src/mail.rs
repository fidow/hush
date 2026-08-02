//! Outgoing email. Fully configured via environment variables so any SMTP
//! relay can be used:
//!
//! - `HUSH_SMTP_HOST`     relay host (unset = no email; codes are logged)
//! - `HUSH_SMTP_PORT`     default `25`
//! - `HUSH_SMTP_FROM`     sender address, e.g. `hush@example.com`
//! - `HUSH_SMTP_USER` / `HUSH_SMTP_PASS`  optional credentials
//! - `HUSH_SMTP_STARTTLS` set to `1` to use STARTTLS

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
        })
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

        tracing::info!("sending email to {to} via {}:{}", self.host, self.port);
        let mut builder = if self.starttls {
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
