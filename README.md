# Hush

Private 1-to-1 messaging with **post-quantum end-to-end encryption**. The server
is a dumb relay/mailbox: it stores public key bundles and queues of opaque
encrypted blobs — it can never read message contents.

## Architecture

```
crates/
  hush-core     Shared client core: identity, PQXDH (X25519 + ML-KEM) session
                establishment and Double Ratchet via libsignal, local storage,
                HTTP+SSE API client. Reused by desktop now, mobile/web later.
  hush-server   Axum + SQLite relay: accounts, prekey bundles, encrypted
                message queues, SSE delivery.
apps/
  desktop       Tauri 2 desktop app (vanilla TypeScript + Vite UI).
```

- **Session establishment**: PQXDH (hybrid X25519 + ML-KEM/Kyber, FIPS 203).
- **Ongoing messaging**: Double Ratchet (forward secrecy + post-compromise security).
- **Transport**: HTTPS (REST + Server-Sent Events).
- Built on [libsignal](https://github.com/signalapp/libsignal) (AGPL-3.0).

## Development

Requirements: Rust (stable, via rustup), `protoc` (set the `PROTOC` env var if
it is not on `PATH`), Node.js (for the desktop app).

```sh
cargo test --workspace   # unit + integration tests
cargo run -p hush-server # relay on 127.0.0.1:8080 (HUSH_ADDR / HUSH_DB to override)

cd apps/desktop
npm install
npm run tauri dev        # desktop app against the local relay
```

To try a conversation locally, run the relay plus two instances of the app,
register a different username in each, add the other user as contact and chat.

## License

AGPL-3.0-only.
