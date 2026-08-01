#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let db_url = std::env::var("HUSH_DB").unwrap_or_else(|_| "sqlite://hush.sqlite3?mode=rwc".into());
    let addr = std::env::var("HUSH_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let pool = hush_server::connect_db(&db_url).await?;
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("hush-server listening on {addr}");
    axum::serve(listener, hush_server::app(pool)).await?;
    Ok(())
}
