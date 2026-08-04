//! Tauri commands bridging the UI to hush-core. All encryption happens in the
//! hush-core engine actor; the webview only ever sees the local user's own
//! plaintext.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use hush_core::{ClientEvent, ContactEntry, HushClient, ProfileInfo, StoredMessage};
// The tray, and closing to it, exist on desktop only: Android has neither.
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem};
#[cfg(desktop)]
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
#[cfg(desktop)]
use tauri::WindowEvent;
use tauri::{Emitter, Manager, State};

#[cfg(windows)]
mod identity;
#[cfg(desktop)]
mod single_instance;

/// There is no log to write to in a packaged build, so this only shows up when
/// the app is started from a console.
#[cfg(desktop)]
fn tracing_note(message: &str) {
    eprintln!("hush: {message}");
}

/// Whether closing the window hides the app instead of quitting it. Lives in
/// Rust because the close handler runs there, but the choice is the user's
/// and the UI stores it.
struct CloseToTray(Arc<AtomicBool>);

/// How the user wants to be alerted: "sound", "vibrate" or "none". The
/// interface owns the setting, but a message arriving in the background is
/// announced from here, so the choice has to be readable from Rust.
struct AlertMode(Arc<std::sync::Mutex<String>>);

/// Hush's own status bar icon. Without it the plugin falls back to Android's
/// generic information icon, which says nothing about who is calling.
#[cfg(target_os = "android")]
const NOTIFICATION_ICON: &str = "ic_notification";
/// The accent Android paints around that icon.
#[cfg(target_os = "android")]
const NOTIFICATION_COLOR: &str = "#584BC2";

/// The Android notification channel for that choice. Channels carry the sound
/// and vibration, and Android silences them itself when the phone is on
/// silent, which a tone played by the app would not respect.
#[cfg(target_os = "android")]
fn alert_channel(mode: &str) -> &'static str {
    match mode {
        "sound" => "hush-messages-sound",
        "vibrate" => "hush-messages-vibrate",
        _ => "hush-messages-silent",
    }
}

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
                    // On Android the webview is frozen while the app is off
                    // screen, so the notification cannot come from the
                    // interface: by the time it runs again the message is old
                    // news. When the app *is* on screen the interface knows
                    // whether that conversation is open, and handles it.
                    #[cfg(target_os = "android")]
                    if !app
                        .get_webview_window("main")
                        .and_then(|w| w.is_focused().ok())
                        .unwrap_or(false)
                    {
                        use tauri_plugin_notification::NotificationExt;
                        let preview = if msg.kind == "image" {
                            "📷".to_string()
                        } else {
                            msg.text.chars().take(120).collect()
                        };
                        let mode = app
                            .state::<AlertMode>()
                            .0
                            .lock()
                            .map(|m| m.clone())
                            .unwrap_or_else(|_| "sound".to_string());
                        let _ = app
                            .notification()
                            .builder()
                            .title(&msg.sender)
                            .body(preview)
                            .icon(NOTIFICATION_ICON)
                            .icon_color(NOTIFICATION_COLOR)
                            .channel_id(alert_channel(&mode))
                            .show();
                    }
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
                ClientEvent::MessageDeleted { id } => {
                    let _ = app.emit("hush://deleted", serde_json::json!({ "id": id }));
                }
                // Written on another device of this account: same shape as an
                // incoming message, but ours.
                ClientEvent::OwnMessage(msg) => {
                    let _ = app.emit(
                        "hush://own-message",
                        serde_json::json!({
                            "id": msg.id,
                            "contact": msg.sender,
                            "kind": msg.kind,
                            "text": msg.text,
                            "created_at": msg.created_at,
                        }),
                    );
                }
                ClientEvent::MessageResent { old_id, new_id } => {
                    let _ = app.emit(
                        "hush://resent",
                        serde_json::json!({ "old_id": old_id, "new_id": new_id }),
                    );
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

/// Rejects a request, cancels one we sent, removes a contact, or unblocks.
#[tauri::command]
async fn remove_contact(client: State<'_, HushClient>, username: String) -> Result<(), String> {
    client.remove_contact(&username).await
}

/// Blocks a peer: they stop being a contact and cannot reach us again.
#[tauri::command]
async fn block_contact(client: State<'_, HushClient>, username: String) -> Result<(), String> {
    client.block_contact(&username).await
}

/// Deletes a message; with `forEveryone` the other side deletes it too.
#[tauri::command]
async fn delete_message(
    client: State<'_, HushClient>,
    id: String,
    for_everyone: bool,
) -> Result<(), String> {
    client.delete_message(&id, for_everyone).await
}

/// Whether closing the window hides the app to the tray instead of quitting.
#[tauri::command]
fn set_close_to_tray(state: State<'_, CloseToTray>, enabled: bool) {
    state.0.store(enabled, Ordering::Relaxed);
}

/// One contact as the interface reads it. Written out field by field because
/// `ContactEntry` is not serialisable, which is also why anything left out
/// here silently disappears on its way to the webview.
fn contact_json(c: ContactEntry) -> serde_json::Value {
    serde_json::json!({
        "username": c.username,
        "alias": c.alias,
        "state": c.state,
        "status": c.status,
        "last_seen": c.last_seen,
        "avatar": c.avatar,
    })
}

/// The contact list with state and presence for each entry.
#[tauri::command]
async fn get_contacts(client: State<'_, HushClient>) -> Result<Vec<serde_json::Value>, String> {
    Ok(client.contacts().await?.into_iter().map(contact_json).collect())
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

/// Tray icon with a minimal menu. Clicking it brings the window back, which
/// is what people try first after closing to the tray.
#[cfg(desktop)]
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Hush", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::with_id("hush")
        .icon(app.default_window_icon().expect("bundled icon").clone())
        .tooltip("Hush")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button, .. } = event {
                if button == tauri::tray::MouseButton::Left {
                    show_main_window(tray.app_handle());
                }
            }
        })
        .build(app)?;
    Ok(())
}

/// Wipes a whole conversation, optionally withdrawing our own messages from
/// the other device too.
#[tauri::command]
async fn delete_conversation(
    client: State<'_, HushClient>,
    contact: String,
    for_everyone: bool,
) -> Result<usize, String> {
    client.delete_conversation(&contact, for_everyone).await
}

/// Sets our profile picture, or clears it, and hands it to every contact.
#[tauri::command]
async fn set_avatar(client: State<'_, HushClient>, avatar: Option<String>) -> Result<(), String> {
    client.set_avatar(avatar).await
}

/// The devices signed in to this account, so the user can revoke one.
#[tauri::command]
async fn get_devices(client: State<'_, HushClient>) -> Result<Vec<serde_json::Value>, String> {
    client.devices().await
}

/// Signs a device out for good.
#[tauri::command]
async fn revoke_device(client: State<'_, HushClient>, device: i64) -> Result<(), String> {
    client.revoke_device(device).await
}

/// Shows a desktop notification under Hush's own identity.
///
/// The notification plugin only registers that identity for an installed
/// build, so anything else — a copy run from disk, a dev build — ends up
/// labelled PowerShell. Sending it here keeps the name right everywhere.
#[tauri::command]
fn notify(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        tauri_winrt_notification::Toast::new(&app.config().identifier)
            .title(&title)
            .text1(&body)
            .show()
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_notification::NotificationExt;
        let mode = app
            .state::<AlertMode>()
            .0
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|_| "sound".to_string());
        app.notification()
            .builder()
            .title(title)
            .body(body)
            .icon(NOTIFICATION_ICON)
            .icon_color(NOTIFICATION_COLOR)
            .channel_id(alert_channel(&mode))
            .show()
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        use tauri_plugin_notification::NotificationExt;
        app.notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|e| e.to_string())
    }
}

/// Remembers how the user wants to be alerted, so a message arriving while the
/// interface is asleep is announced the way they asked.
#[tauri::command]
fn set_alert_mode(mode: State<'_, AlertMode>, value: String) {
    if let Ok(mut mode) = mode.0.lock() {
        *mode = value;
    }
}

#[cfg(desktop)]
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            #[cfg(windows)]
            identity::register(&app.config().identifier, "Hush", &dir);
            // HUSH_PROFILE lets several instances coexist on one machine
            // (useful to test two accounts locally).
            let profile = match std::env::var("HUSH_PROFILE") {
                Ok(p) if !p.is_empty() => p,
                _ => "hush".to_string(),
            };
            let file = format!("{profile}.db");

            // Two copies of the same profile share a database and a set of
            // ratchet sessions, and would end up unable to read each other's
            // conversations. The second one hands the screen to the first.
            #[cfg(desktop)]
            {
                match single_instance::acquire(&dir, &profile) {
                    Some(lock) => {
                        app.manage(lock);
                    }
                    None => {
                        tracing_note("another copy of this profile is already running");
                        single_instance::raise_running_instance("Hush");
                        app.handle().exit(0);
                        return Ok(());
                    }
                }
            }

            app.manage(HushClient::spawn(dir.join(file)));
            app.manage(Arc::new(AtomicU64::new(0)));

            // Closing hides to the tray by default, so messages keep arriving
            // and notifications still work. The UI can turn that off. On
            // Android there is no tray and the system owns the lifecycle.
            let close_to_tray = Arc::new(AtomicBool::new(true));
            app.manage(CloseToTray(close_to_tray.clone()));
            app.manage(AlertMode(Arc::new(std::sync::Mutex::new("sound".to_string()))));
            #[cfg(desktop)]
            {
                build_tray(app.handle())?;
                if let Some(window) = app.get_webview_window("main") {
                    let handle = window.clone();
                    window.on_window_event(move |event| {
                        if let WindowEvent::CloseRequested { api, .. } = event {
                            if close_to_tray.load(Ordering::Relaxed) {
                                api.prevent_close();
                                let _ = handle.hide();
                            }
                        }
                    });
                }
            }
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
            block_contact,
            delete_message,
            set_close_to_tray,
            get_contacts,
            get_history,
            mark_read,
            update_me,
            get_recovery_code,
            restore_history,
            forgot_password,
            reset_password,
            idle_seconds,
            notify,
            get_devices,
            revoke_device,
            set_avatar,
            set_alert_mode,
            delete_conversation
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The webview reads a fixed set of fields off each contact; one missing
    /// from the hand-written JSON reads as `undefined` there, with nothing to
    /// say it was ever dropped. A profile picture was lost exactly this way.
    #[test]
    fn a_contact_reaches_the_interface_with_every_field_it_reads() {
        let value = contact_json(ContactEntry {
            username: "alice".into(),
            alias: "Alicia".into(),
            state: "accepted".into(),
            status: "online".into(),
            last_seen: Some(1_700_000_000_000),
            avatar: Some("data:image/jpeg;base64,AAAA".into()),
        });
        // The fields declared by `interface ContactEntry` in main.ts.
        for field in ["username", "alias", "state", "status", "last_seen", "avatar"] {
            assert!(!value[field].is_null(), "the interface never sees {field}");
        }
        assert_eq!(value["avatar"], "data:image/jpeg;base64,AAAA");
    }
}
