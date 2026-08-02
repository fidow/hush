//! Opens an existing database (running the at-rest encryption migration on it)
//! and prints what the app can still read, so the caller can compare it with
//! what a raw reader sees in the file.
//!
//! Run: `cargo run -p hush-core --example migration_check -- <path>`

use hush_core::{db::LocalDb, Engine};

fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: migration_check <db path>"),
    );

    let db = LocalDb::open(&path)?;

    println!("archive_key: {:?}", db.meta_get("archive_key")?);
    let username = db.meta_get("username")?.unwrap_or_default();
    println!("username:    {username}");
    let contacts = db.contacts()?;
    for (username, alias, state) in &contacts {
        let history = db.history(username)?;
        println!(
            "contact {username} / alias {alias} / {state} / {} msgs",
            history.len()
        );
        for m in history.iter().take(3) {
            println!("    [{}] {}", m.kind, m.text);
        }
    }

    // The libsignal stores hold the sealed key material: loading them proves
    // the migrated rows still deserialize.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let engine = Engine::open(db, &username)?;
        println!("identity key: {}", engine.identity_key_b64().await?);
        println!("registration id: {}", engine.registration_id().await?);
        for (contact, _, _) in &contacts {
            println!(
                "session with {contact}: {} / known identity: {}",
                engine.has_session(contact).await?,
                engine.known_identity_b64(contact).await?.is_some(),
            );
        }
        anyhow::Ok(())
    })
}
