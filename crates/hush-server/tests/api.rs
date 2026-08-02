use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use sqlx::Row;

async fn spawn_server() -> (String, sqlx::SqlitePool) {
    let db_path = std::env::temp_dir().join(format!("hush-test-{}.sqlite3", uuid::Uuid::new_v4()));
    let db_url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy().replace('\\', "/"));
    let pool = hush_server::connect_db(&db_url).await.expect("db");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app_pool = pool.clone();
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

/// Registers + verifies an account, returning its bearer token.
async fn register(
    client: &reqwest::Client,
    base: &str,
    pool: &sqlx::SqlitePool,
    username: &str,
    alias: &str,
) -> String {
    let res = client
        .post(format!("{base}/v1/accounts"))
        .json(&json!({
            "username": username,
            "alias": alias,
            "email": format!("{username}@example.com"),
            "password": "supersecreta",
            "registration_id": 42,
            "identity_key": "IKEY"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["status"], "pending_verification");

    let code = pending_code(pool, username).await;
    let res = client
        .post(format!("{base}/v1/accounts/verify"))
        .json(&json!({ "username": username, "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    res.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Links two accounts as accepted contacts, which messaging requires.
async fn befriend(client: &reqwest::Client, base: &str, from: &str, to: &str) {
    // The second request from the other side counts as an acceptance.
    for (requester, target) in [(from, "bob"), (to, "alice")] {
        let res = client
            .post(format!("{base}/v1/contacts/{target}"))
            .bearer_auth(requester)
            .send()
            .await
            .unwrap();
        assert!(res.status().is_success(), "befriend failed: {}", res.status());
    }
}

/// Reads the next `message` SSE event from a byte stream, with a timeout.
async fn next_sse_message(
    stream: &mut (impl futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
) -> Value {
    let mut buf = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let chunk = stream.next().await.expect("stream ended").expect("chunk");
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(end) = buf.find("\n\n") {
                let event: String = buf[..end].to_string();
                buf.drain(..end + 2);
                if let Some(data) = event
                    .lines()
                    .find_map(|l| l.strip_prefix("data: "))
                {
                    return serde_json::from_str(data).expect("json event");
                }
                // keep-alive comment or other event type: keep reading
            }
        }
    })
    .await
    .expect("timed out waiting for SSE event")
}

#[tokio::test]
async fn account_lifecycle() {
    let (base, pool) = spawn_server().await;
    let client = reqwest::Client::new();

    // Wrong verification code is rejected
    let res = client
        .post(format!("{base}/v1/accounts"))
        .json(&json!({
            "username": "carol", "alias": "Carol", "email": "carol@example.com",
            "password": "supersecreta", "registration_id": 1, "identity_key": "X"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = client
        .post(format!("{base}/v1/accounts/verify"))
        .json(&json!({ "username": "carol", "code": "000000x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    // Unverified accounts cannot authenticate or be messaged/profiled
    let alice = register(&client, &base, &pool, "alice", "Alicia").await;
    let res = client
        .get(format!("{base}/v1/profile/carol"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    // Duplicate of a verified username is rejected
    let res = client
        .post(format!("{base}/v1/accounts"))
        .json(&json!({
            "username": "alice", "alias": "Otra", "email": "otra@example.com",
            "password": "supersecreta", "registration_id": 2, "identity_key": "Y"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);

    // Weak password / bad email are rejected
    let res = client
        .post(format!("{base}/v1/accounts"))
        .json(&json!({
            "username": "dave", "alias": "", "email": "dave@example.com",
            "password": "corta", "registration_id": 1, "identity_key": "X"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    // Login: right and wrong password
    let res = client
        .post(format!("{base}/v1/sessions"))
        .json(&json!({ "username": "alice", "password": "supersecreta" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let relogin: Value = res.json().await.unwrap();
    assert_eq!(relogin["token"].as_str().unwrap(), alice);
    let res = client
        .post(format!("{base}/v1/sessions"))
        .json(&json!({ "username": "alice", "password": "incorrecta" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);

    // Profile returns the alias
    let profile: Value = client
        .get(format!("{base}/v1/profile/alice"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(profile["alias"], "Alicia");
}

/// A 6-digit code must not be guessable: attempts are throttled and the code
/// is burned before an attacker gets anywhere near 10^6 tries.
#[tokio::test]
async fn verification_code_cannot_be_brute_forced() {
    let (base, pool) = spawn_server().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/v1/accounts"))
        .json(&json!({
            "username": "victim", "alias": "V", "email": "victim@example.com",
            "password": "supersecreta", "registration_id": 1, "identity_key": "X"
        }))
        .send()
        .await
        .unwrap();
    let real_code = pending_code(&pool, "victim").await;

    for attempt in 0..5 {
        let res = client
            .post(format!("{base}/v1/accounts/verify"))
            .json(&json!({ "username": "victim", "code": "999999" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400, "intento {attempt} debería fallar");
    }

    // Even the *correct* code is now refused: the guesser is locked out.
    let res = client
        .post(format!("{base}/v1/accounts/verify"))
        .json(&json!({ "username": "victim", "code": real_code }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
}

/// Password guessing (and the Argon2 CPU burn that comes with it) is capped.
#[tokio::test]
async fn login_attempts_are_throttled() {
    let (base, pool) = spawn_server().await;
    let client = reqwest::Client::new();
    register(&client, &base, &pool, "alice", "Alicia").await;

    for attempt in 0..10 {
        let res = client
            .post(format!("{base}/v1/sessions"))
            .json(&json!({ "username": "alice", "password": "incorrecta" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401, "intento {attempt} debería ser 401");
    }
    let res = client
        .post(format!("{base}/v1/sessions"))
        .json(&json!({ "username": "alice", "password": "supersecreta" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429, "el 11º intento debe estar limitado");
}

/// Internal failures must not describe the database to the caller.
#[tokio::test]
async fn errors_do_not_leak_internals() {
    let (base, _pool) = spawn_server().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{base}/v1/keys/nobody"))
        .bearer_auth("token-inventado")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let body = res.text().await.unwrap();
    for leak in ["sqlx", "SELECT", "accounts", "sqlite"] {
        assert!(!body.contains(leak), "la respuesta filtra {leak}: {body}");
    }
}

/// Contact requests gate messaging: strangers are refused, and a link only
/// forms once the other side accepts.
#[tokio::test]
async fn contact_requests_gate_messaging() {
    let (base, pool) = spawn_server().await;
    let client = reqwest::Client::new();
    let alice = register(&client, &base, &pool, "alice", "Alicia").await;
    let bob = register(&client, &base, &pool, "bob", "Roberto").await;

    // Without a link, messaging is refused.
    let res = client
        .put(format!("{base}/v1/messages/bob"))
        .bearer_auth(&alice)
        .json(&json!({ "body": "CIPHERTEXT" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);

    // Alice asks; Bob sees an incoming request, Alice an outgoing one.
    let res = client
        .post(format!("{base}/v1/contacts/bob"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.json::<Value>().await.unwrap()["state"], "outgoing");

    let contacts = |token: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .get(format!("{base}/v1/contacts"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()["contacts"]
                .clone()
        }
    };
    assert_eq!(contacts(bob.clone()).await[0]["state"], "incoming");
    assert_eq!(contacts(alice.clone()).await[0]["state"], "outgoing");

    // Still no messaging while pending, in either direction.
    let res = client
        .put(format!("{base}/v1/messages/bob"))
        .bearer_auth(&alice)
        .json(&json!({ "body": "CIPHERTEXT" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);

    // Asking twice is rejected.
    let res = client
        .post(format!("{base}/v1/contacts/bob"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);

    // Bob accepts: both sides become contacts and messaging works.
    let res = client
        .post(format!("{base}/v1/contacts/alice/accept"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(contacts(alice.clone()).await[0]["state"], "accepted");
    assert_eq!(contacts(bob.clone()).await[0]["alias"], "Alicia");

    let res = client
        .put(format!("{base}/v1/messages/bob"))
        .bearer_auth(&alice)
        .json(&json!({ "body": "CIPHERTEXT" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    // Removing the contact closes the door again, for both.
    let res = client
        .delete(format!("{base}/v1/contacts/alice"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    assert_eq!(contacts(alice.clone()).await.as_array().unwrap().len(), 0);
    let res = client
        .put(format!("{base}/v1/messages/bob"))
        .bearer_auth(&alice)
        .json(&json!({ "body": "CIPHERTEXT" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);

    // You cannot add yourself.
    let res = client
        .post(format!("{base}/v1/contacts/alice"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn full_flow() {
    let (base, pool) = spawn_server().await;
    let client = reqwest::Client::new();

    let alice = register(&client, &base, &pool, "alice", "Alicia").await;
    let bob = register(&client, &base, &pool, "bob", "Roberto").await;
    befriend(&client, &base, &alice, &bob).await;

    // Auth required
    let unauth = client.get(format!("{base}/v1/keys/bob")).send().await.unwrap();
    assert_eq!(unauth.status(), 401);

    // Bob uploads his prekey bundle
    let res = client
        .put(format!("{base}/v1/keys"))
        .bearer_auth(&bob)
        .json(&json!({
            "bundle_static": { "signed_prekey": "SPK", "kyber_last_resort": "KLR" },
            "one_time_prekeys": [
                { "kind": "ec", "data": "EC1" },
                { "kind": "kyber", "data": "KYB1" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);

    // Alice fetches Bob's bundle; one-time prekeys are consumed
    let bundle: Value = client
        .get(format!("{base}/v1/keys/bob"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bundle["identity_key"], "IKEY");
    assert_eq!(bundle["one_time_prekey"], "EC1");
    assert_eq!(bundle["kyber_prekey"], "KYB1");

    let bundle2: Value = client
        .get(format!("{base}/v1/keys/bob"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(bundle2["one_time_prekey"].is_null());
    assert!(bundle2["kyber_prekey"].is_null());

    // Offline delivery: Alice sends while Bob is not connected
    let sent: Value = client
        .put(format!("{base}/v1/messages/bob"))
        .bearer_auth(&alice)
        .json(&json!({ "body": "CIPHERTEXT-1" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let msg_id = sent["id"].as_str().unwrap().to_string();

    // Bob connects and receives the backlog
    let res = client
        .get(format!("{base}/v1/messages/stream"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let mut stream = res.bytes_stream();
    let msg = next_sse_message(&mut stream).await;
    assert_eq!(msg["id"], msg_id.as_str());
    assert_eq!(msg["sender"], "alice");
    assert_eq!(msg["body"], "CIPHERTEXT-1");

    // Live delivery while connected
    client
        .put(format!("{base}/v1/messages/bob"))
        .bearer_auth(&alice)
        .json(&json!({ "body": "CIPHERTEXT-2" }))
        .send()
        .await
        .unwrap();
    let msg2 = next_sse_message(&mut stream).await;
    assert_eq!(msg2["body"], "CIPHERTEXT-2");

    // Ack deletes from the queue: a reconnect only replays message 2
    let res = client
        .delete(format!("{base}/v1/messages/{msg_id}"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    drop(stream);

    let res = client
        .get(format!("{base}/v1/messages/stream"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap();
    let mut stream = res.bytes_stream();
    let replay = next_sse_message(&mut stream).await;
    assert_eq!(replay["body"], "CIPHERTEXT-2");

    // Sending to an unknown user fails
    let res = client
        .put(format!("{base}/v1/messages/nobody"))
        .bearer_auth(&alice)
        .json(&json!({ "body": "X" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}
