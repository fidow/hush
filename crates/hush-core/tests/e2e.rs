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
    let mut engine = Engine::open(LocalDb::open_in_memory().unwrap(), username, 1).unwrap();
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

/// Conversations move to another device through an exported file, and only
/// with the password that made it. The server holds no history, so this is
/// the whole of the migration path.
#[tokio::test]
async fn conversations_move_between_devices_through_an_export() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-export-{}", uuid::Uuid::new_v4()));

    // Bob exists so Alice has someone to talk to.
    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();

    let old = HushClient::spawn(dir.join("old.db"));
    old.register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    old.verify(&pending_code(&pool, "alice").await).await.unwrap();
    old.connect().await.unwrap();
    bob.connect().await.unwrap();

    // Become contacts first: messaging strangers is refused.
    old.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();
    assert_eq!(
        old.contacts().await.unwrap()[0].state,
        "accepted",
        "the link must be visible from both sides"
    );

    old.send_text("bob", "mensaje que debe sobrevivir").await.unwrap();
    bob.send_text("alice", "y esta respuesta").await.unwrap();
    wait_for_text(&old, "bob", "y esta respuesta").await;

    let file = old.export_conversations("contraseña-del-backup").await.unwrap();
    assert!(
        !String::from_utf8_lossy(&file).contains("mensaje que debe sobrevivir"),
        "the file must not carry the conversation in the clear"
    );

    // The new device starts empty, and the wrong password leaves it that way.
    let new = HushClient::spawn(dir.join("new.db"));
    new.login(&base, "alice", "supersecreta").await.unwrap();
    assert!(new.history("bob").await.unwrap().is_empty());
    assert_eq!(
        new.import_conversations(file.clone(), "contraseña-equivocada")
            .await
            .unwrap_err(),
        "import_wrong_password"
    );
    assert!(new.history("bob").await.unwrap().is_empty());

    // With the right one the conversation comes back, both sides of it.
    assert_eq!(
        new.import_conversations(file.clone(), "contraseña-del-backup")
            .await
            .unwrap(),
        2
    );
    let history = new.history("bob").await.unwrap();
    let texts: Vec<&str> = history.iter().map(|m| m.text.as_str()).collect();
    assert!(texts.contains(&"mensaje que debe sobrevivir"));
    assert!(texts.contains(&"y esta respuesta"));
    assert!(
        history
            .iter()
            .find(|m| m.text == "mensaje que debe sobrevivir")
            .expect("ours is there")
            .mine,
        "who wrote what has to survive the trip"
    );

    // Importing the same file again changes nothing: the device already has
    // every message in it.
    assert_eq!(
        new.import_conversations(file, "contraseña-del-backup")
            .await
            .unwrap(),
        0
    );
}

/// Signing out ends the session without taking the conversations with it.
/// The server keeps no copy, so wiping them here would be destroying the only
/// one there is — and signing back in has to find them again.
#[tokio::test]
async fn signing_out_keeps_the_conversations() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-logout-{}", uuid::Uuid::new_v4()));

    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();

    let alice = HushClient::spawn(dir.join("alice.db"));
    alice
        .register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    alice.verify(&pending_code(&pool, "alice").await).await.unwrap();
    alice.connect().await.unwrap();
    bob.connect().await.unwrap();
    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();
    alice.send_text("bob", "antes de salir").await.unwrap();

    alice.logout().await.unwrap();
    assert!(
        alice.load_profile().await.unwrap().is_none(),
        "the app must come back to the sign-in screen"
    );
    assert!(
        alice.send_text("bob", "ya no").await.is_err(),
        "a signed-out client must not be able to write"
    );

    // Back in on the same device: the conversation is where it was left.
    alice.login(&base, "alice", "supersecreta").await.unwrap();
    let history = alice.history("bob").await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "antes de salir");
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

/// A profile picture reaches contacts without the server ever holding it: it
/// travels inside an encrypted message like anything else.
#[tokio::test]
async fn a_profile_picture_reaches_contacts_encrypted() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-avatar-{}", uuid::Uuid::new_v4()));

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

    // A one pixel PNG is enough to prove it travels.
    let picture = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";
    alice.set_avatar(Some(picture.to_string())).await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let seen = bob
            .contacts()
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.username == "alice")
            .and_then(|c| c.avatar);
        if seen.as_deref() == Some(picture) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the picture never reached the contact; got {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The server holds messages, not pictures: nothing readable is stored.
    let queued: i64 = sqlx::query("SELECT COUNT(*) FROM messages WHERE body LIKE ?")
        .bind("%iVBORw0KGgo%")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(queued, 0, "the picture must never reach the server in the clear");

    // And it must not show up as a message in the conversation.
    assert!(bob.history("alice").await.unwrap().is_empty());

    // Clearing it takes it away from them too.
    alice.set_avatar(None).await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let seen = bob
            .contacts()
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.username == "alice")
            .and_then(|c| c.avatar);
        if seen.is_none() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the picture was not cleared");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A picture set before anyone is a contact still reaches them once they are.
/// It is only sent when it changes, so without handing it over on acceptance
/// a new contact would never see it.
#[tokio::test]
async fn a_picture_set_before_the_contact_existed_still_reaches_them() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-avatar-late-{}", uuid::Uuid::new_v4()));

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

    // The picture comes first, with nobody to send it to.
    let picture = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";
    alice.set_avatar(Some(picture.to_string())).await.unwrap();

    // Only afterwards do they become contacts.
    alice.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        // Both sides refresh their lists, which is when the picture is handed
        // over.
        let _ = alice.contacts().await;
        let seen = bob
            .contacts()
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.username == "alice")
            .and_then(|c| c.avatar);
        if seen.as_deref() == Some(picture) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a contact accepted later never received the picture"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// An account is one device at a time: signing in somewhere else takes it
/// over, the previous session's token stops working, and the conversation
/// carries on at the new place.
#[tokio::test]
async fn signing_in_elsewhere_takes_the_account_over() {
    use hush_core::HushClient;

    let (base, pool) = spawn_server().await;
    let dir = std::env::temp_dir().join(format!("hush-takeover-{}", uuid::Uuid::new_v4()));

    let laptop = HushClient::spawn(dir.join("laptop.db"));
    laptop
        .register(&base, "alice", "Alicia", "alice@example.com", "supersecreta")
        .await
        .unwrap();
    laptop.verify(&pending_code(&pool, "alice").await).await.unwrap();
    let bob = HushClient::spawn(dir.join("bob.db"));
    bob.register(&base, "bob", "Roberto", "bob@example.com", "supersecreta")
        .await
        .unwrap();
    bob.verify(&pending_code(&pool, "bob").await).await.unwrap();
    laptop.connect().await.unwrap();
    bob.connect().await.unwrap();

    laptop.request_contact("bob").await.unwrap();
    bob.accept_contact("alice").await.unwrap();
    laptop.send_text("bob", "desde el portátil").await.unwrap();
    wait_for_text(&bob, "alice", "desde el portátil").await;

    // Alice signs in on the phone. That is now where the account lives.
    let phone = HushClient::spawn(dir.join("phone.db"));
    phone.login(&base, "alice", "supersecreta").await.unwrap();
    phone.connect().await.unwrap();

    // The phone sends happily: it has never pinned a key for Bob, so the one
    // he publishes is the one it learns.
    phone.send_text("bob", "y ahora desde el teléfono").await.unwrap();

    // Bob is the interesting side. A new device means a new identity key, and
    // Bob pinned the old one. He cannot read what arrived and must not write
    // either, until somebody confirms it is still Alice — a relay able to
    // swap that key quietly is a relay able to read the conversation.
    assert_eq!(
        bob.send_text("alice", "te leo en el nuevo").await.unwrap_err(),
        "identity_changed",
        "a key that changed must stop the message, not be adopted quietly"
    );
    assert!(
        !bob.history("alice")
            .await
            .unwrap()
            .iter()
            .any(|m| m.text == "y ahora desde el teléfono"),
        "nothing under the new key may be read before it is accepted"
    );

    // Once Bob accepts the change the conversation carries on, and what the
    // phone sent meanwhile is sent again rather than lost.
    bob.accept_identity("alice").await.unwrap();
    bob.send_text("alice", "te leo en el nuevo").await.unwrap();
    wait_for_text(&phone, "bob", "te leo en el nuevo").await;
    wait_for_text(&bob, "alice", "y ahora desde el teléfono").await;

    // The laptop's token died the moment the phone signed in, so anything it
    // tries now is refused rather than quietly working on.
    let refused = laptop.send_text("bob", "sigo aquí").await;
    assert!(
        refused.is_err(),
        "the previous device must be signed out, not left running"
    );
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

    // And it must not come back through an export either: what was deleted
    // here is gone, and the control message that deleted it was never a
    // conversation line to begin with.
    let file = alice.export_conversations("contraseña-del-backup").await.unwrap();
    let device2 = HushClient::spawn(dir.join("device2.db"));
    device2.login(&base, "alice", "supersecreta").await.unwrap();
    assert_eq!(
        device2
            .import_conversations(file, "contraseña-del-backup")
            .await
            .unwrap(),
        0
    );
    assert!(
        device2.history("bob").await.unwrap().is_empty(),
        "an export must not carry control messages either"
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
    alice.ensure_session("bob", 1, &bundle).await.unwrap();
    let envelope = alice.encrypt("bob", 1, b"hola bob, esto es secreto").await.unwrap();
    assert!(!envelope.contains("secreto"), "envelope must not leak plaintext");
    alice_api.send_message("bob", &envelope).await.unwrap();

    // Bob comes online, gets the backlog, decrypts.
    let mut bob_rx = bob_api.stream().await.unwrap();
    let msg = recv_one(&mut bob_rx).await;
    assert_eq!(msg.sender, "alice");
    let plain = bob.decrypt("alice", 1, &msg.body).await.unwrap();
    assert_eq!(plain, b"hola bob, esto es secreto");
    bob_api.ack_message(&msg.id).await.unwrap();

    // Bob replies over the ratchet established by the prekey message.
    assert!(bob.has_session("alice", 1).await.unwrap());
    let envelope = bob.encrypt("alice", 1, b"hola alice, recibido").await.unwrap();
    bob_api.send_message("alice", &envelope).await.unwrap();

    let mut alice_rx = alice_api.stream().await.unwrap();
    let msg = recv_one(&mut alice_rx).await;
    assert_eq!(msg.sender, "bob");
    let plain = alice.decrypt("bob", 1, &msg.body).await.unwrap();
    assert_eq!(plain, b"hola alice, recibido");
    alice_api.ack_message(&msg.id).await.unwrap();

    // A second message from Alice uses the ratchet (no prekey consumption).
    let envelope = alice.encrypt("bob", 1, b"segundo mensaje").await.unwrap();
    assert!(envelope.contains("\"signal\""), "ratchet messages use type signal");
    alice_api.send_message("bob", &envelope).await.unwrap();
    let msg = recv_one(&mut bob_rx).await;
    let plain = bob.decrypt("alice", 1, &msg.body).await.unwrap();
    assert_eq!(plain, b"segundo mensaje");
}
