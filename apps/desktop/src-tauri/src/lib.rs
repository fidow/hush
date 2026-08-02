//! Tauri commands bridging the UI to hush-core. All encryption happens in the
//! hush-core engine actor; the webview only ever sees the local user's own
//! plaintext.

use hush_core::{HushClient, ProfileInfo, StoredMessage};
use tauri::{Emitter, Manager, State};

/// The account stored on this device, if any.
#[tauri::command]
async fn load_profile(client: State<'_, HushClient>) -> Result<Option<ProfileInfo>, String> {
    client.load_profile().await
}

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

/// Confirms the account with the emailed code and saves the profile locally.
/// The UI should call `connect` afterwards.
#[tauri::command]
async fn verify(client: State<'_, HushClient>, code: String) -> Result<(), String> {
    client.verify(&code).await
}

/// Logs into an existing account (re-provisioning keys if this is a new
/// device). The UI should call `connect` afterwards.
#[tauri::command]
async fn login(
    client: State<'_, HushClient>,
    server: String,
    username: String,
    password: String,
) -> Result<ProfileInfo, String> {
    client.login(&server, &username, &password).await
}

/// The recovery key of this device, for the user to copy and keep.
#[tauri::command]
async fn get_recovery_code(client: State<'_, HushClient>) -> Result<String, String> {
    client.recovery_code().await
}

/// Adopts a recovery key and pulls down the archived history. Returns how
/// many messages were restored.
#[tauri::command]
async fn restore_history(client: State<'_, HushClient>, code: String) -> Result<usize, String> {
    client.restore_history(&code).await
}

/// Opens the message stream; incoming messages arrive as `hush://message`
/// events until disconnect (`hush://disconnected`).
#[tauri::command]
async fn connect(app: tauri::AppHandle, client: State<'_, HushClient>) -> Result<(), String> {
    let mut rx = client.connect().await?;
    tauri::async_runtime::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let _ = app.emit(
                "hush://message",
                serde_json::json!({
                    "id": msg.id,
                    "sender": msg.sender,
                    "kind": msg.kind,
                    "text": msg.text,
                    "created_at": msg.created_at,
                }),
            );
        }
        let _ = app.emit("hush://disconnected", ());
    });
    Ok(())
}

/// Encrypts and sends `text` to `recipient`; returns the stored message.
#[tauri::command]
async fn send_message(
    client: State<'_, HushClient>,
    recipient: String,
    text: String,
) -> Result<StoredMessage, String> {
    client.send_text(&recipient, &text).await
}

/// Encrypts and sends a pasted image (data URL); returns the stored message.
#[tauri::command]
async fn send_image(
    client: State<'_, HushClient>,
    recipient: String,
    dataUrl: String,
) -> Result<StoredMessage, String> {
    client.send_image(&recipient, &dataUrl).await
}

/// Validates the user exists, stores it as a contact, returns its alias.
#[tauri::command]
async fn add_contact(client: State<'_, HushClient>, username: String) -> Result<String, String> {
    client.add_contact(&username).await
}

#[tauri::command]
async fn get_contacts(client: State<'_, HushClient>) -> Result<Vec<(String, String)>, String> {
    client.contacts().await
}

/// Changes the local account's display name and/or presence.
#[tauri::command]
async fn update_me(
    client: State<'_, HushClient>,
    alias: Option<String>,
    status: Option<String>,
) -> Result<(), String> {
    client.update_me(alias, status).await
}

/// Presence of every stored contact, as `[username, status]` pairs.
#[tauri::command]
async fn get_presence(client: State<'_, HushClient>) -> Result<Vec<(String, String)>, String> {
    client.presence().await
}

#[tauri::command]
async fn get_history(
    client: State<'_, HushClient>,
    contact: String,
) -> Result<Vec<StoredMessage>, String> {
    client.history(&contact).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            // HUSH_PROFILE lets several instances coexist on one machine
            // (useful to test two accounts locally).
            let file = match std::env::var("HUSH_PROFILE") {
                Ok(p) if !p.is_empty() => format!("hush-{p}.db"),
                _ => "hush.db".to_string(),
            };
            app.manage(HushClient::spawn(dir.join(file)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_profile,
            register,
            verify,
            login,
            connect,
            send_message,
            send_image,
            add_contact,
            get_contacts,
            get_history,
            update_me,
            get_presence,
            get_recovery_code,
            restore_history
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
