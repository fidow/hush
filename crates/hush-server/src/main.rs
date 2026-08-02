use hush_server::mail;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Debug mode is toggled via HUSH_LOG, e.g. HUSH_LOG=debug (default: info).
    // sqlx is capped at warn so successful queries stay out of the log; a
    // more specific directive (e.g. HUSH_LOG="debug,sqlx::query=debug") can
    // still bring them back.
    let base = std::env::var("HUSH_LOG").unwrap_or_else(|_| "info".into());
    let filter = tracing_subscriber::EnvFilter::new(format!("{base},sqlx=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

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
