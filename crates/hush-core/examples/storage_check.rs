//! Writes through the normal storage path, then the caller greps the raw file
//! to confirm nothing sensitive survives in the clear.
//!
//! Run: `cargo run -p hush-core --example storage_check -- <path>`

use hush_core::db::{LocalDb, StoredMessage};

fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: storage_check <db path>"),
    );

    let db = LocalDb::open(&path)?;
    db.meta_set("archive_key", "SUPERSECRETARCHIVEKEY")?;
    db.upsert_contact("bob", "RobertoSecreto", "accepted")?;
    db.add_message(&StoredMessage {
        id: "1".into(),
        contact: "bob".into(),
        mine: true,
        kind: "text".into(),
        text: "TEXTOCONFIDENCIAL".into(),
        state: "sent".into(),
        delivered_at: None,
        read_at: None,
        created_at: 1,
    })?;

    // Reading it back through the app must still give plain values.
    println!("message read by the app: {}", db.history("bob")?[0].text);
    println!("alias read by the app:   {}", db.contacts()?[0].1);
    println!("meta read by the app:    {:?}", db.meta_get("archive_key")?);
    Ok(())
}
