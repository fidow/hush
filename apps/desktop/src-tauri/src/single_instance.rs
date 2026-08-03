//! One running copy per profile.
//!
//! Every instance opens the same local database and the same account, and two
//! of them writing the same ratchet state is a good way to end up unable to
//! read your own messages. The lock is per profile rather than per machine so
//! `HUSH_PROFILE` can still run a second account side by side, which is how
//! two ends of a conversation are tested on one desktop.

use std::fs::File;
use std::path::Path;

/// Holds the lock for as long as the app runs; dropping it releases it, and
/// the operating system releases it anyway if the process dies.
pub struct ProfileLock(#[allow(dead_code)] File);

/// Takes the lock for this profile, or reports that somebody else has it.
pub fn acquire(data_dir: &Path, profile: &str) -> Option<ProfileLock> {
    if std::fs::create_dir_all(data_dir).is_err() {
        // Nowhere to put the lock: better to run than to refuse to start.
        return None;
    }
    let path = data_dir.join(format!("{profile}.lock"));

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // share_mode 0: nobody else may open it while we hold it.
        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(0)
            .open(&path)
        {
            Ok(file) => Some(ProfileLock(file)),
            Err(_) => None,
        }
    }
    #[cfg(not(windows))]
    {
        // Without an equivalent everywhere, the file is created but not
        // exclusive; the check below then always lets the app run.
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .ok()
            .map(ProfileLock)
    }
}

/// Brings the copy that is already running to the front, so a second launch
/// looks like clicking the app rather than doing nothing.
#[cfg(windows)]
pub fn raise_running_instance(window_title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    let title: Vec<u16> = window_title
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the string is nul-terminated and outlives the call.
    unsafe {
        let handle = FindWindowW(std::ptr::null(), title.as_ptr());
        if !handle.is_null() {
            ShowWindow(handle, SW_RESTORE);
            SetForegroundWindow(handle);
        }
    }
}

#[cfg(not(windows))]
pub fn raise_running_instance(_window_title: &str) {}
