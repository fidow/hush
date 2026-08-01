//! End-to-end test: two clients exchange PQXDH/Double-Ratchet encrypted
//! messages through a real hush-server instance, including offline delivery.

use std::time::Duration;

use hush_core::{ApiClient, Engine};

async fn spawn_server() -> String {
    let db_path = std::env::temp_dir().join(format!("hush-e2e-{}.sqlite3", uuid::Uuid::new_v4()));
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );
    let pool = hush_server::connect_db(&db_url).await.expect("db");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, hush_server::app(pool)).await.unwrap();
    });
    format!("http://{addr}")
}

async fn onboard(base: &str, username: &str) -> (Engine, ApiClient) {
    let mut engine = Engine::new(username).unwrap();
    let mut api = ApiClient::new(base);
    api.register(
        username,
        engine.registration_id().await.unwrap(),
        &engine.identity_key_b64().await.unwrap(),
    )
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

#[tokio::test]
async fn encrypted_roundtrip_with_offline_delivery() {
    let base = spawn_server().await;
    let (mut alice, alice_api) = onboard(&base, "alice").await;
    let (mut bob, bob_api) = onboard(&base, "bob").await;

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
