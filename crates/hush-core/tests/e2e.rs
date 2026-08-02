//! End-to-end test: two clients exchange PQXDH/Double-Ratchet encrypted
//! messages through a real hush-server instance, including offline delivery.

use std::time::Duration;

use hush_core::{ApiClient, Engine, LocalDb};
use sqlx::Row;

async fn spawn_server() -> (String, sqlx::SqlitePool) {
    let db_path = std::env::temp_dir().join(format!("hush-e2e-{}.sqlite3", uuid::Uuid::new_v4()));
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );
    let pool = hush_server::connect_db(&db_url).await.expect("db");
    let app_pool = pool.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, hush_server::app(app_pool)).await.unwrap();
    });
    (format!("http://{addr}"), pool)
}

async fn pending_code(pool: &sqlx::SqlitePool, username: &str) -> String {
    sqlx::query("SELECT verify_code FROM accounts WHERE username = ?")
        .bind(username)
        .fetch_one(pool)
        .await
        .unwrap()
        .get(0)
}

async fn onboard(base: &str, pool: &sqlx::SqlitePool, username: &str) -> (Engine, ApiClient) {
    let mut engine = Engine::open(LocalDb::open_in_memory().unwrap(), username).unwrap();
    let mut api = ApiClient::new(base);
    api.register(
        username,
        &format!("Alias de {username}"),
        &format!("{username}@example.com"),
        "supersecreta",
        engine.registration_id().await.unwrap(),
        &engine.identity_key_b64().await.unwrap(),
    )
    .await
    .unwrap();
    api.verify(username, &pending_code(pool, username).await)
        .await
        .unwrap();
    let keys = engine.generate_prekeys(4).await.unwrap();
    api.upload_keys(&keys).await.unwrap();
    (engine, api)
}

async fn recv_one(
    rx: &mut tokio::sync::mpsc::Receiver<hush_core::IncomingMessage>,
) -> hush_core::IncomingMessage {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for message")
        .expect("stream closed")
}

/// A second device restores the conversation history from the encrypted
/// archive with the recovery key, and only with the right one. Restoring is
/// available at any time, not only while signing in.
#[tokio::test]
async fn history_follows_the_user_to_a_new_device() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-devices-{}", uuid::Uuid::new_v4()));

    // Bob exists so Alice has someone to talk to.
    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();

    // Alice's first device sends a message and reads its recovery key.
    let device1 = HushClient::spawn(dir.join("device1.db"));
    device1
        .register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    device1
        .verify(&pending_code(&pool, "alice").await)
        .await
        .unwrap();
    device1.connect().await.unwrap();
    device1.send_text("bob", "mensaje que debe sobrevivir").await.unwrap();
    let recovery = device1.recovery_code().await.unwrap();
    assert!(recovery.contains('-'), "code is grouped for reading: {recovery}");

    // The second device signs in with no history, then restores it.
    let device2 = HushClient::spawn(dir.join("device2.db"));
    device2.login(&base, "alice", "supersecreta").await.unwrap();
    assert!(device2.history("bob").await.unwrap().is_empty());

    // Somebody else's key cannot read the archive.
    let other = HushClient::spawn(dir.join("other.db"));
    other
        .register(&base, "mallory", "M", "m@example.com", "supersecreta")
        .await
        .unwrap();
    other
        .verify(&pending_code(&pool, "mallory").await)
        .await
        .unwrap();
    let err = device2
        .restore_history(&other.recovery_code().await.unwrap())
        .await
        .unwrap_err();
    assert_eq!(err, "wrong_recovery_key");

    // With the real key the conversation comes back.
    let restored = device2.restore_history(&recovery).await.unwrap();
    assert_eq!(restored, 1);
    let history = device2.history("bob").await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "mensaje que debe sobrevivir");
    assert!(history[0].mine);
    assert!(device2.contacts().await.unwrap().iter().any(|(u, _)| u == "bob"));
}

#[tokio::test]
async fn encrypted_roundtrip_with_offline_delivery() {
    let (base, pool) = spawn_server().await;
    let (mut alice, alice_api) = onboard(&base, &pool, "alice").await;
    let (mut bob, bob_api) = onboard(&base, &pool, "bob").await;

    // Aliases are public profile data served to authenticated users.
    assert_eq!(
        alice_api.fetch_profile("bob").await.unwrap().alias,
        "Alias de bob"
    );

    // Alice establishes a PQXDH session from Bob's public bundle and sends
    // while Bob is offline.
    let bundle = alice_api.fetch_bundle("bob").await.unwrap();
    alice.ensure_session("bob", &bundle).await.unwrap();
    let envelope = alice.encrypt("bob", b"hola bob, esto es secreto").await.unwrap();
    assert!(!envelope.contains("secreto"), "envelope must not leak plaintext");
    alice_api.send_message("bob", &envelope).await.unwrap();

    // Bob comes online, gets the backlog, decrypts.
    let mut bob_rx = bob_api.stream().await.unwrap();
    let msg = recv_one(&mut bob_rx).await;
    assert_eq!(msg.sender, "alice");
    let plain = bob.decrypt("alice", &msg.body).await.unwrap();
    assert_eq!(plain, b"hola bob, esto es secreto");
    bob_api.ack_message(&msg.id).await.unwrap();

    // Bob replies over the ratchet established by the prekey message.
    assert!(bob.has_session("alice").await.unwrap());
    let envelope = bob.encrypt("alice", b"hola alice, recibido").await.unwrap();
    bob_api.send_message("alice", &envelope).await.unwrap();

    let mut alice_rx = alice_api.stream().await.unwrap();
    let msg = recv_one(&mut alice_rx).await;
    assert_eq!(msg.sender, "bob");
    let plain = alice.decrypt("bob", &msg.body).await.unwrap();
    assert_eq!(plain, b"hola alice, recibido");
    alice_api.ack_message(&msg.id).await.unwrap();

    // A second message from Alice uses the ratchet (no prekey consumption).
    let envelope = alice.encrypt("bob", b"segundo mensaje").await.unwrap();
    assert!(envelope.contains("\"signal\""), "ratchet messages use type signal");
    alice_api.send_message("bob", &envelope).await.unwrap();
    let msg = recv_one(&mut bob_rx).await;
    let plain = bob.decrypt("alice", &msg.body).await.unwrap();
    assert_eq!(plain, b"segundo mensaje");
}
