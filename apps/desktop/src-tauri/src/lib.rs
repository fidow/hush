//! Tauri commands bridging the UI to hush-core. All encryption happens in the
//! hush-core engine actor; the webview only ever sees the local user's own
//! plaintext.

use hush_core::HushClient;
use tauri::{Emitter, Manager, State};

/// Registers a new account on `server`, publishes prekeys, and starts the
/// incoming message stream (delivered to the UI as `hush://message` events).
#[tauri::command]
async fn register(
    app: tauri::AppHandle,
    client: State<'_, HushClient>,
    server: String,
    username: String,
) -> Result<(), String> {
    let mut rx = client.register(&server, &username).await?;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(HushClient::spawn());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![register, send_message])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
