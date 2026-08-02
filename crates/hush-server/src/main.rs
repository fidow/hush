use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use hush_server::mail;

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
            let (writer, guard) =
                tracing_appender::non_blocking(tracing_appender::rolling::daily(&directory, &name));
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                // Escape codes would end up as noise in a file.
                .with_ansi(false)
                .with_writer(writer)
                .init();
            tracing::info!("logging to {}", directory.join(&name).display());
            start_log_cleanup(directory, name);
            Some(guard)
        }
        _ => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
            None
        }
    };

    let db_url = std::env::var("HUSH_DB").unwrap_or_else(|_| "sqlite://hush.sqlite3?mode=rwc".into());
    let addr = std::env::var("HUSH_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let public = !addr.starts_with("127.") && !addr.starts_with("localhost");
    if public {
        // Loud, because each of these silently weakens a deployment that is
        // reachable from outside the machine.
        if std::env::var("HUSH_ECHO_CODE").is_ok() {
            tracing::warn!("HUSH_ECHO_CODE está definida: solo para desarrollo local");
        }
        if mail::config_missing() {
            tracing::warn!(
                "SMTP sin configurar: nadie podrá verificar su cuenta (define HUSH_SMTP_HOST)"
            );
        }
        if std::env::var("HUSH_LOG").is_ok_and(|v| v.contains("debug")) {
            tracing::warn!("HUSH_LOG=debug registra metadatos de mensajes; usa info en producción");
        }
        tracing::info!("recuerda terminar TLS delante del servidor (proxy inverso en :443)");
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
