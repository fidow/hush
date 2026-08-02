//! Tauri commands bridging the UI to hush-core. All encryption happens in the
//! hush-core engine actor; the webview only ever sees the local user's own
//! plaintext.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use hush_core::{ClientEvent, ContactEntry, HushClient, ProfileInfo, StoredMessage};
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

/// Asks the server to email a password reset code.
#[tauri::command]
async fn forgot_password(
    client: State<'_, HushClient>,
    server: String,
    username: String,
) -> Result<Option<String>, String> {
    client.forgot_password(&server, &username).await
}

/// Sets a new password from the emailed code.
#[tauri::command]
async fn reset_password(
    client: State<'_, HushClient>,
    server: String,
    username: String,
    code: String,
    password: String,
) -> Result<(), String> {
    client
        .reset_password(&server, &username, &code, &password)
        .await
}

/// Seconds since the user last touched keyboard or mouse *anywhere*, not just
/// in this window, so stepping away from the machine counts as idle.
/// `None` where the platform offers no such measure; the UI then falls back
/// to its own activity tracking.
#[tauri::command]
fn idle_seconds() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetTickCount;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is correctly sized and lives for the whole call.
        let ok = unsafe { GetLastInputInfo(&mut info) } != 0;
        if !ok {
            return None;
        }
        let elapsed_ms = unsafe { GetTickCount() }.wrapping_sub(info.dwTime);
        Some(u64::from(elapsed_ms) / 1000)
    }
    #[cfg(not(windows))]
    {
        None
    }
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
async fn connect(
    app: tauri::AppHandle,
    client: State<'_, HushClient>,
    generation: State<'_, Arc<AtomicU64>>,
) -> Result<(), String> {
    let mut rx = client.connect().await?;
    // Reconnecting replaces the event channel, which ends the previous task.
    // Only the newest one may report a disconnection, or a reconnect would
    // look like a fresh drop and restart the cycle.
    let generation = generation.inner().clone();
    let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                ClientEvent::Message(msg) => {
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
                ClientEvent::ContactsChanged => {
                    let _ = app.emit("hush://contacts", ());
                }
                ClientEvent::Receipt { id, state, at } => {
                    let _ = app.emit(
                        "hush://receipt",
                        serde_json::json!({ "id": id, "state": state, "at": at }),
                    );
                }
            }
        }
        if generation.load(Ordering::SeqCst) == mine {
            let _ = app.emit("hush://disconnected", ());
        }
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
    // Tauri maps the JS `dataUrl` argument onto this snake_case name.
    data_url: String,
) -> Result<StoredMessage, String> {
    client.send_image(&recipient, &data_url).await
}

/// Sends a contact request; returns the resulting state.
#[tauri::command]
async fn request_contact(
    client: State<'_, HushClient>,
    username: String,
) -> Result<String, String> {
    client.request_contact(&username).await
}

#[tauri::command]
async fn accept_contact(client: State<'_, HushClient>, username: String) -> Result<(), String> {
    client.accept_contact(&username).await
}

/// Rejects a request, cancels one we sent, or removes a contact.
#[tauri::command]
async fn remove_contact(client: State<'_, HushClient>, username: String) -> Result<(), String> {
    client.remove_contact(&username).await
}

/// The contact list with state and presence for each entry.
#[tauri::command]
async fn get_contacts(client: State<'_, HushClient>) -> Result<Vec<serde_json::Value>, String> {
    Ok(client
        .contacts()
        .await?
        .into_iter()
        .map(|c: ContactEntry| {
            serde_json::json!({
                "username": c.username,
                "alias": c.alias,
                "state": c.state,
                "status": c.status,
                "last_seen": c.last_seen,
            })
        })
        .collect())
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

/// Reports every unread message from `contact` as read.
#[tauri::command]
async fn mark_read(client: State<'_, HushClient>, contact: String) -> Result<(), String> {
    client.mark_read(&contact).await
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
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            // HUSH_PROFILE lets several instances coexist on one machine
            // (useful to test two accounts locally).
            let file = match std::env::var("HUSH_PROFILE") {
                Ok(p) if !p.is_empty() => format!("hush-{p}.db"),
                _ => "hush.db".to_string(),
            };
            app.manage(HushClient::spawn(dir.join(file)));
            app.manage(Arc::new(AtomicU64::new(0)));
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
            request_contact,
            accept_contact,
            remove_contact,
            get_contacts,
            get_history,
            mark_read,
            update_me,
            get_recovery_code,
            restore_history,
            forgot_password,
            reset_password,
            idle_seconds
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
