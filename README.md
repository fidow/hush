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

### Server configuration (environment variables)

| Variable | Default | Purpose |
|---|---|---|
| `HUSH_ADDR` | `127.0.0.1:8080` | Listen address |
| `HUSH_DB` | `sqlite://hush.sqlite3?mode=rwc` | SQLite database URL |
| `HUSH_LOG` | `info` | Log level; set `debug` to trace users/messages (toggleable debug mode) |
| `HUSH_SMTP_HOST` | *(unset)* | SMTP relay for verification emails; unset = codes are logged instead |
| `HUSH_SMTP_PORT` | `25` | SMTP port |
| `HUSH_SMTP_FROM` | `hush@localhost` | From address (e.g. `hush@example.com`) |
| `HUSH_SMTP_USER` / `HUSH_SMTP_PASS` | *(unset)* | Optional SMTP credentials |
| `HUSH_SMTP_STARTTLS` | *(unset)* | Set to `1` to use STARTTLS |
| `HUSH_ECHO_CODE` | *(unset)* | Set to `1` (dev only!) to echo verification codes in the API response |

Accounts require a username, a public alias, an email and a password
(argon2-hashed). New accounts must be confirmed with a 6-digit code sent by
email before they can log in or exchange messages.

### History across devices

Private keys can never travel through the server, so a new device starts with
fresh keys and no history. To avoid losing conversations, each device
re-encrypts every message it sends or receives under a key derived from the
user's **history passphrase** (Argon2id → XChaCha20-Poly1305) and uploads the
result. Signing in elsewhere and entering that passphrase restores the full
history; the server only ever stores opaque blobs. Both primitives are
symmetric, so this layer is quantum-resistant on its own.

The passphrase is deliberately separate from the login password, which the
server does see during authentication. Losing it means losing the archive —
there is no recovery path by design.

## License

AGPL-3.0-only.
