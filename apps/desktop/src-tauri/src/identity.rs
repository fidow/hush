//! Windows app identity for notifications.
//!
//! A toast is labelled with whatever is registered under the
//! AppUserModelID it carries. With none of our own, the toast library falls
//! back to PowerShell's — which is why notifications arrived looking like they
//! came from PowerShell, even though no PowerShell ever runs.
//!
//! Registering the id under HKCU with a display name and an icon makes Windows
//! attribute the toast to Hush instead. It is per-user and needs no installer.

use std::path::{Path, PathBuf};

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

/// The icon shown on the toast, written next to the app data so Windows has a
/// file to point at (it cannot read the one embedded in the executable).
const ICON: &[u8] = include_bytes!("../icons/128x128.png");

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Registers `app_id` for this user and adopts it for this process.
pub fn register(app_id: &str, display_name: &str, data_dir: &Path) {
    let icon = write_icon(data_dir);
    if let Err(e) = write_registry(app_id, display_name, icon.as_deref()) {
        // Not fatal: notifications still work, they just look like PowerShell.
        eprintln!("cannot register the app identity for notifications: {e}");
    }
    // Tells Windows which id this process's windows and toasts belong to.
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(wide(app_id).as_ptr());
    }
}

fn write_icon(data_dir: &Path) -> Option<PathBuf> {
    let path = data_dir.join("hush.png");
    if path.exists() {
        return Some(path);
    }
    std::fs::create_dir_all(data_dir).ok()?;
    std::fs::write(&path, ICON).ok()?;
    Some(path)
}

fn write_registry(app_id: &str, display_name: &str, icon: Option<&Path>) -> Result<(), String> {
    let subkey = wide(&format!("Software\\Classes\\AppUserModelId\\{app_id}"));
    let mut key: HKEY = std::ptr::null_mut();

    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!("cannot create the registry key (error {status})"));
    }

    let mut result = set_string(key, "DisplayName", display_name);
    if let Some(icon) = icon {
        result = result.and(set_string(key, "IconUri", &icon.to_string_lossy()));
    }
    unsafe { RegCloseKey(key) };
    result
}

fn set_string(key: HKEY, name: &str, value: &str) -> Result<(), String> {
    let name = wide(name);
    let value = wide(value);
    let bytes = std::mem::size_of_val(&value[..]) as u32;
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr() as PCWSTR,
            0,
            REG_SZ,
            value.as_ptr() as *const u8,
            bytes,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("cannot write the registry value (error {status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows refuses to show a toast under an id it does not know, so this
    /// both registers the id and proves Windows accepts it. It puts a real
    /// notification on screen: it should read "Hush", never "PowerShell".
    #[test]
    fn windows_accepts_our_app_id_for_toasts() {
        let dir = std::env::temp_dir().join("hush-identity-test");
        register("com.fidow.hush", "Hush", &dir);

        tauri_winrt_notification::Toast::new("com.fidow.hush")
            .title("Hush")
            .text1("Identidad de notificaciones registrada")
            .show()
            .expect("Windows should accept a registered app id");
    }
}
