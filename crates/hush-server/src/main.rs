#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Debug mode is toggled via HUSH_LOG, e.g. HUSH_LOG=debug (default: info).
    let filter = tracing_subscriber::EnvFilter::try_from_env("HUSH_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let db_url = std::env::var("HUSH_DB").unwrap_or_else(|_| "sqlite://hush.sqlite3?mode=rwc".into());
    let addr = std::env::var("HUSH_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let pool = hush_server::connect_db(&db_url).await?;
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("hush-server listening on {addr}");
    axum::serve(listener, hush_server::app(pool)).await?;
    Ok(())
}
