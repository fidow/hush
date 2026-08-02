//! Tauri commands bridging the UI to hush-core. All encryption happens in the
//! hush-core engine actor; the webview only ever sees the local user's own
//! plaintext.

use hush_core::HushClient;
use tauri::{Emitter, Manager, State};

/// Creates the account on `server` (pending email verification). Returns the
/// dev verification code when the server echoes it (HUSH_ECHO_CODE=1).
#[tauri::command]
async fn register(
    client: State<'_, HushClient>,
    server: String,
    username: String,
    alias: String,
    email: String,
    password: String,
) -> Result<Option<String>, String> {
    client
        .register(&server, &username, &alias, &email, &password)
        .await
}

/// Confirms the account with the emailed code, publishes prekeys and starts
/// the incoming message stream (delivered as `hush://message` events).
#[tauri::command]
async fn verify(
    app: tauri::AppHandle,
    client: State<'_, HushClient>,
    code: String,
) -> Result<(), String> {
    let mut rx = client.verify(&code).await?;
    tauri::async_runtime::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let _ = app.emit(
                "hush://message",
                serde_json::json!({
                    "id": msg.id,
                    "sender": msg.sender,
                    "text": msg.text,
                    "created_at": msg.created_at,
                }),
            );
        }
        let _ = app.emit("hush://disconnected", ());
    });
    Ok(())
}

/// Encrypts and sends `text` to `recipient`, establishing a PQXDH session
/// from their published bundle if none exists yet.
#[tauri::command]
async fn send_message(
    client: State<'_, HushClient>,
    recipient: String,
    text: String,
) -> Result<(), String> {
    client.send_text(&recipient, &text).await
}

/// Public alias of a user; also validates that the user exists.
#[tauri::command]
async fn get_profile(client: State<'_, HushClient>, username: String) -> Result<String, String> {
    client.fetch_alias(&username).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(HushClient::spawn());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            register,
            verify,
            send_message,
            get_profile
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
