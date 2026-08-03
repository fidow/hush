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

/// Waits for the next message, skipping contact-list notifications.
async fn recv_one(
    rx: &mut tokio::sync::mpsc::Receiver<hush_core::ServerEvent>,
) -> hush_core::IncomingMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await.expect("stream closed") {
                hush_core::ServerEvent::Message(msg) => return msg,
                // Contact and receipt notifications are noise for this test.
                _ => continue,
            }
        }
    })
    .await
    .expect("timed out waiting for message")
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
    bob.connect().await.unwrap();

    // Become contacts first: messaging strangers is refused.
    device1.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();
    assert_eq!(
        device1.contacts().await.unwrap()[0].state,
        "accepted",
        "the link must be visible from both sides"
    );

    device1.send_text("bob", "mensaje que debe sobrevivir").await.unwrap();
    let recovery = device1.recovery_code().await.unwrap();
    assert!(recovery.contains('-'), "code is grouped for reading: {recovery}");

    // The second device signs in with no history and, crucially, no key of
    // its own: the recovery key belongs to the account, not the device.
    let device2 = HushClient::spawn(dir.join("device2.db"));
    device2.login(&base, "alice", "supersecreta").await.unwrap();
    assert!(device2.history("bob").await.unwrap().is_empty());
    assert_eq!(
        device2.recovery_code().await.unwrap_err(),
        "no_recovery_key",
        "a fresh device must not invent a second key"
    );

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
    assert!(device2
        .contacts()
        .await
        .unwrap()
        .iter()
        .any(|c| c.username == "bob"));

    // Both devices now report the same key, and messages sent from the second
    // one land in the same archive.
    assert_eq!(device2.recovery_code().await.unwrap(), recovery);
    device2.connect().await.unwrap();
    device2.send_text("bob", "desde el segundo dispositivo").await.unwrap();

    let device3 = HushClient::spawn(dir.join("device3.db"));
    device3.login(&base, "alice", "supersecreta").await.unwrap();
    assert_eq!(device3.restore_history(&recovery).await.unwrap(), 2);
    let texts: Vec<String> = device3
        .history("bob")
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert!(texts.contains(&"mensaje que debe sobrevivir".to_string()));
    assert!(texts.contains(&"desde el segundo dispositivo".to_string()));
}

/// Waits for `text` to show up in the conversation with `contact`.
async fn wait_for_text(client: &hush_core::HushClient, contact: &str, text: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let history = client.history(contact).await.unwrap();
        if history.iter().any(|m| m.text == text) {
            return;
        }
        if std::time::Instant::now() > deadline {
            let seen: Vec<String> = history.into_iter().map(|m| m.text).collect();
            panic!("the conversation with {contact} never showed {text:?}; it holds {seen:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Blocking someone, lifting the block and adding them again has to leave a
/// working conversation: both sides must see what the other sends afterwards.
#[tokio::test]
async fn talking_again_after_a_block_works_both_ways() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-block-{}", uuid::Uuid::new_v4()));

    let alice = HushClient::spawn(dir.join("alice.db"));
    alice
        .register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    alice.verify(&pending_code(&pool, "alice").await).await.unwrap();
    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();
    alice.connect().await.unwrap();
    bob.connect().await.unwrap();

    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();
    alice.send_text("bob", "antes del bloqueo").await.unwrap();
    wait_for_text(&bob, "alice", "antes del bloqueo").await;
    bob.send_text("alice", "recibido").await.unwrap();
    wait_for_text(&alice, "bob", "recibido").await;

    // Bob blocks, thinks better of it, and they become contacts again.
    bob.block_contact("alice").await.unwrap();
    bob.remove_contact("alice").await.unwrap();
    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();

    alice.send_text("bob", "después del desbloqueo").await.unwrap();
    wait_for_text(&bob, "alice", "después del desbloqueo").await;
    bob.send_text("alice", "yo también te veo").await.unwrap();
    wait_for_text(&alice, "bob", "yo también te veo").await;
}

/// A device that lost its session state — a reinstall, a database recreated
/// by hand — still holds the account keys, so the other side keeps encrypting
/// under a ratchet that no longer exists there. Those messages cannot be
/// decrypted, and silently dropping them means the conversation is dead in one
/// direction for good, which is what "my messages never reach them" looks
/// like. The client has to notice and rebuild the session.
#[tokio::test]
async fn a_contact_who_lost_their_session_can_be_reached_again() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-lost-{}", uuid::Uuid::new_v4()));

    let alice = HushClient::spawn(dir.join("alice.db"));
    alice
        .register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    alice.verify(&pending_code(&pool, "alice").await).await.unwrap();
    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();
    alice.connect().await.unwrap();
    bob.connect().await.unwrap();

    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();

    // A full round trip, so Alice's session is established and she stops
    // attaching the handshake to every message.
    alice.send_text("bob", "hola").await.unwrap();
    wait_for_text(&bob, "alice", "hola").await;
    bob.send_text("alice", "hola alicia").await.unwrap();
    wait_for_text(&alice, "bob", "hola alicia").await;

    // Bob's session state disappears while his account keys stay.
    let bob_db = rusqlite::Connection::open(dir.join("bob.db")).unwrap();
    bob_db.execute("DELETE FROM sessions", []).unwrap();
    drop(bob_db);

    // Bob cannot read this one, but it must not be lost: once he rebuilds the
    // session, Alice sends again what he never acknowledged.
    let sent = alice.send_text("bob", "primero tras el incidente").await.unwrap();
    wait_for_text(&bob, "alice", "primero tras el incidente").await;

    // And it stays a single entry on Alice's side, under the id it was
    // finally sent with.
    let history = alice.history("bob").await.unwrap();
    let resent: Vec<&hush_core::StoredMessage> = history
        .iter()
        .filter(|m| m.text == "primero tras el incidente")
        .collect();
    assert_eq!(resent.len(), 1, "the resent message must not be duplicated");
    assert_ne!(resent[0].id, sent.id, "it travels under a new id");

    // The conversation works in both directions afterwards.
    alice.send_text("bob", "segundo tras el incidente").await.unwrap();
    wait_for_text(&bob, "alice", "segundo tras el incidente").await;
    bob.send_text("alice", "te vuelvo a leer").await.unwrap();
    wait_for_text(&alice, "bob", "te vuelvo a leer").await;
}

/// A message that never arrives can leave the sender holding a session the
/// receiver knows nothing about — blocking someone drops whatever they had
/// queued, which is exactly how that happens. Every later message would then
/// be undecryptable, so the two sides must be able to repair the session.
#[tokio::test]
async fn a_session_the_other_side_never_saw_gets_rebuilt() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-rekey-{}", uuid::Uuid::new_v4()));

    let alice = HushClient::spawn(dir.join("alice.db"));
    alice
        .register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    alice.verify(&pending_code(&pool, "alice").await).await.unwrap();
    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();
    alice.connect().await.unwrap();

    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();

    // Bob is offline, so this queues on the server: it carries the handshake
    // that would have set up his side of the session.
    alice.send_text("bob", "mientras estabas fuera").await.unwrap();

    // The block drops the queue, taking the handshake with it.
    bob.block_contact("alice").await.unwrap();
    bob.remove_contact("alice").await.unwrap();
    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();
    bob.connect().await.unwrap();

    // Alice still holds the session Bob never learned about.
    alice.send_text("bob", "hola otra vez").await.unwrap();
    wait_for_text(&bob, "alice", "hola otra vez").await;

    // And the repair has to work in both directions afterwards.
    bob.send_text("alice", "ahora sí te leo").await.unwrap();
    wait_for_text(&alice, "bob", "ahora sí te leo").await;
}

/// Deleting a message for everyone travels as a control message; it must not
/// end up in the sender's own conversation as a message showing an id.
#[tokio::test]
async fn deleting_for_everyone_leaves_no_trace_in_the_history() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-delete-{}", uuid::Uuid::new_v4()));

    let alice = HushClient::spawn(dir.join("alice.db"));
    alice
        .register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    alice.verify(&pending_code(&pool, "alice").await).await.unwrap();
    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();
    alice.connect().await.unwrap();
    bob.connect().await.unwrap();

    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();
    let sent = alice.send_text("bob", "esto se borra").await.unwrap();
    wait_for_text(&bob, "alice", "esto se borra").await;

    alice.delete_message(&sent.id, true).await.unwrap();
    assert!(
        alice.history("bob").await.unwrap().is_empty(),
        "the deleted message and the control message must both be gone"
    );

    // And it must not come back when another device restores the archive.
    let recovery = alice.recovery_code().await.unwrap();
    let device2 = HushClient::spawn(dir.join("device2.db"));
    device2.login(&base, "alice", "supersecreta").await.unwrap();
    device2.restore_history(&recovery).await.ok();
    assert!(
        device2.history("bob").await.unwrap().is_empty(),
        "the archive must not hold control messages either"
    );
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

    // Messaging requires an accepted contact link: the second request from
    // the other side accepts the first.
    alice_api.request_contact("bob").await.unwrap();
    bob_api.request_contact("alice").await.unwrap();

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
