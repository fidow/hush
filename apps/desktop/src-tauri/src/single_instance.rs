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

// Raising the copy that is already running used to live here: the second
// process looked its window up by title and called SetForegroundWindow. It
// does not work, and could not — Hush usually sits hidden in the tray, and a
// hidden window is not something another process gets to show. The running
// copy raises itself now, from the single-instance callback in `lib.rs`.
