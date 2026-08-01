use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};

async fn spawn_server() -> String {
    let db_path = std::env::temp_dir().join(format!("hush-test-{}.sqlite3", uuid::Uuid::new_v4()));
    let db_url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy().replace('\\', "/"));
    let pool = hush_server::connect_db(&db_url).await.expect("db");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, hush_server::app(pool)).await.unwrap();
    });
    format!("http://{addr}")
}

async fn register(client: &reqwest::Client, base: &str, username: &str) -> String {
    let res = client
        .post(format!("{base}/v1/accounts"))
        .json(&json!({ "username": username, "registration_id": 42, "identity_key": "IKEY" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    res.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
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
async fn full_flow() {
    let base = spawn_server().await;
    let client = reqwest::Client::new();

    // Registration + duplicate rejection
    let alice = register(&client, &base, "alice").await;
    let bob = register(&client, &base, "bob").await;
    let dup = client
        .post(format!("{base}/v1/accounts"))
        .json(&json!({ "username": "alice", "registration_id": 1, "identity_key": "X" }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), 409);

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
