use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use hush_server::mail;

/// Timestamps in local time. The default formatter writes UTC, which means the
/// log cannot be lined up with Apache's or the Windows event log without doing
/// the offset arithmetic by hand.
struct LocalTime;

impl tracing_subscriber::fmt::time::FormatTime for LocalTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f %:z"))
    }
}

/// Daily rotation on *local* dates, writing `<name>.<yyyy-mm-dd>`.
///
/// `tracing_appender::rolling::daily` rotates on the UTC date, so on a UTC+2
/// machine the file named for a day actually starts at 02:00 local time and
/// the first two hours of each day land in the previous day's file.
struct LocalDailyFile {
    directory: PathBuf,
    name: OsString,
    /// The day currently open, and its handle.
    current: Option<(String, std::fs::File)>,
}

impl LocalDailyFile {
    fn new(directory: PathBuf, name: OsString) -> Self {
        Self {
            directory,
            name,
            current: None,
        }
    }

    /// The file for today, reopening it when the local date has moved on.
    fn today(&mut self) -> std::io::Result<&mut std::fs::File> {
        self.file_for(&chrono::Local::now().format("%Y-%m-%d").to_string())
    }

    fn file_for(&mut self, today: &str) -> std::io::Result<&mut std::fs::File> {
        if self.current.as_ref().is_none_or(|(day, _)| day != today) {
            let mut path = self.name.clone();
            path.push(format!(".{today}"));
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.directory.join(path))?;
            self.current = Some((today.to_string(), file));
        }
        // Just assigned above when it was missing.
        Ok(&mut self.current.as_mut().expect("file is open").1)
    }
}

impl Write for LocalDailyFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.today()?.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.current {
            Some((_, file)) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Days of rotated logs to keep. `HUSH_LOG_KEEP_DAYS=0` disables the cleanup
/// and keeps everything.
const DEFAULT_LOG_KEEP_DAYS: u64 = 30;
const DAY: u64 = 24 * 60 * 60;

/// Deletes rotated logs older than the retention window, then repeats daily.
/// Without this the directory grows forever, which matters most where the log
/// was pointed at a small volume or a share.
fn start_log_cleanup(directory: PathBuf, name: OsString) {
    let keep_days = std::env::var("HUSH_LOG_KEEP_DAYS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_LOG_KEEP_DAYS);
    if keep_days == 0 {
        tracing::info!("log cleanup disabled; rotated files are kept indefinitely");
        return;
    }
    tracing::info!("rotated logs older than {keep_days} days will be deleted");

    tokio::spawn(async move {
        loop {
            prune_old_logs(&directory, &name, keep_days);
            tokio::time::sleep(Duration::from_secs(DAY)).await;
        }
    });
}

fn prune_old_logs(directory: &Path, name: &OsString, keep_days: u64) {
    let Some(prefix) = name.to_str() else { return };
    let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(keep_days * DAY)) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        tracing::warn!("cannot read log directory {}", directory.display());
        return;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        // Only files this server rotated ("hush.log.2026-08-02"), never
        // anything else that happens to share the directory.
        if !file_name.starts_with(&format!("{prefix}.")) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|modified| modified < cutoff);
        if old {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => tracing::info!("deleted old log {file_name}"),
                Err(e) => tracing::warn!("cannot delete old log {file_name}: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_moves_to_a_new_file_when_the_day_changes() {
        let dir = std::env::temp_dir().join(format!("hush-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut log = LocalDailyFile::new(dir.clone(), "hush.log".into());

        log.file_for("2026-08-02").unwrap().write_all(b"late\n").unwrap();
        log.file_for("2026-08-03").unwrap().write_all(b"early\n").unwrap();
        // Same day again: keeps appending instead of truncating.
        log.file_for("2026-08-03").unwrap().write_all(b"later\n").unwrap();

        let read = |day: &str| std::fs::read_to_string(dir.join(format!("hush.log.{day}"))).unwrap();
        assert_eq!(read("2026-08-02"), "late\n");
        assert_eq!(read("2026-08-03"), "early\nlater\n");

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Debug mode is toggled via HUSH_LOG, e.g. HUSH_LOG=debug (default: info).
    // sqlx is capped at warn so successful queries stay out of the log; a
    // more specific directive (e.g. HUSH_LOG="debug,sqlx::query=debug") can
    // still bring them back.
    let base = std::env::var("HUSH_LOG").unwrap_or_else(|_| "info".into());
    let filter = tracing_subscriber::EnvFilter::new(format!("{base},sqlx=warn"));

    // HUSH_LOG_FILE sends the log to a file instead of the console, rotated
    // daily. Any absolute path works, including another drive or a UNC share;
    // running as a scheduled task there is nowhere for stdout to go, so
    // without this the log is simply lost. `_log_guard` must outlive main:
    // dropping it stops the writer thread and loses buffered lines.
    let _log_guard = match std::env::var("HUSH_LOG_FILE") {
        Ok(path) if !path.trim().is_empty() => {
            let path = std::path::PathBuf::from(path.trim());
            let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
            let directory = directory.map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf);
            let name = path
                .file_name()
                .map(std::ffi::OsStr::to_os_string)
                .unwrap_or_else(|| "hush-server.log".into());
            // Fail loudly rather than start with no log at all.
            std::fs::create_dir_all(&directory).with_context(|| {
                format!("cannot create log directory {}", directory.display())
            })?;
            let (writer, guard) = tracing_appender::non_blocking(LocalDailyFile::new(
                directory.clone(),
                name.clone(),
            ));
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                // Escape codes would end up as noise in a file.
                .with_ansi(false)
                .with_timer(LocalTime)
                .with_writer(writer)
                .init();
            tracing::info!("logging to {}", directory.join(&name).display());
            start_log_cleanup(directory, name);
            Some(guard)
        }
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_timer(LocalTime)
                .init();
            None
        }
    };

    // Required on purpose. Defaulting to a file in the working directory
    // means a misconfigured deployment starts happily against an empty
    // database in an arbitrary place, and nobody notices until the accounts
    // are gone.
    let db_url = std::env::var("HUSH_DB").map_err(|_| {
        anyhow::anyhow!(
            "HUSH_DB is not set. Point it at the database file, e.g.\n\
             \x20 HUSH_DB=sqlite://C:/hush/data/hush.sqlite3?mode=rwc"
        )
    })?;
    let addr = std::env::var("HUSH_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let public = !addr.starts_with("127.") && !addr.starts_with("localhost");
    if public {
        // Loud, because each of these silently weakens a deployment that is
        // reachable from outside the machine.
        if std::env::var("HUSH_ECHO_CODE").is_ok() {
            tracing::warn!("HUSH_ECHO_CODE is set: development only");
        }
        if mail::config_missing() {
            tracing::warn!(
                "SMTP not configured: nobody can verify their account (set HUSH_SMTP_HOST)"
            );
        }
        if mail::MailConfig::from_env().is_some_and(|c| c.credentials_in_the_clear()) {
            tracing::warn!(
                "SMTP credentials are set but the connection is not encrypted; \
                 mail will not be sent. Set HUSH_SMTP_TLS=1 (port 465) or HUSH_SMTP_STARTTLS=1"
            );
        }
        if std::env::var("HUSH_LOG").is_ok_and(|v| v.contains("debug")) {
            tracing::warn!("HUSH_LOG=debug records message metadata; use info in production");
        }
        tracing::info!("remember to terminate TLS in front of the server (reverse proxy on :443)");
    }

    let pool = hush_server::connect_db(&db_url).await?;
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("hush-server listening on {addr}");
    // ConnectInfo gives handlers the peer address for per-IP rate limiting.
    axum::serve(
        listener,
        hush_server::app(pool)
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
