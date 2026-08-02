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
| `HUSH_LOG_FILE` | *(unset)* | Write the log to this path instead of the console, rotated daily. Any absolute path, including another drive or a share; the directory is created |
| `HUSH_LOG_KEEP_DAYS` | `30` | Days of rotated logs to keep; `0` keeps them forever. Only files this server rotated are ever deleted |
| `HUSH_SMTP_HOST` | *(unset)* | SMTP relay for verification emails; unset = codes are logged instead |
| `HUSH_SMTP_PORT` | `25` | SMTP port |
| `HUSH_SMTP_FROM` | `hush@localhost` | From address; a bare address gets the `Hush <…>` display name |
| `HUSH_SMTP_USER` / `HUSH_SMTP_PASS` | *(unset)* | Optional SMTP credentials |
| `HUSH_SMTP_STARTTLS` | *(unset)* | Set to `1` to use STARTTLS |
| `HUSH_ECHO_CODE` | *(unset)* | Dev only: echo verification codes in the API response. Ignored whenever SMTP is configured |
| `HUSH_TRUST_PROXY` | *(unset)* | Set to `1` **only** behind a reverse proxy, so `X-Forwarded-For` is used for per-IP rate limiting |

### Hardening notes

The server throttles registration (per IP and per destination address),
verification and login (per account and per IP), message sending and archive
uploads. Verification codes are additionally burned after 5 wrong attempts,
compared in constant time, and a login for a non-existent account still runs
Argon2 so response times don't enumerate usernames. Per-account quotas cap the
undelivered queue and the history archive.

For a public deployment:

- Terminate TLS in front of the server (reverse proxy on `:443`) and keep the
  binary bound to `127.0.0.1`. Prefer a proxy offering the hybrid
  `X25519MLKEM768` key exchange so the transport is post-quantum too.
- Set `HUSH_TRUST_PROXY=1` **only** when a proxy is actually in front:
  `X-Forwarded-For` is trivially spoofable otherwise, which would let an
  attacker bypass per-IP limits.
- Keep `HUSH_LOG=info`. At `debug` the log records who messaged whom and when.
- Never set `HUSH_ECHO_CODE`.

Accounts require a username, a public alias, an email and a password
(argon2-hashed). New accounts must be confirmed with a 6-digit code sent by
email before they can log in or exchange messages.

### History across devices

Private keys can never travel through the server, so a new device starts with
fresh keys and no history. To avoid losing conversations, each device
re-encrypts every message it sends or receives under the account's **recovery
key** (Argon2id → XChaCha20-Poly1305) and uploads the result. Signing in
elsewhere and entering that key restores the full history; the server only ever
stores opaque blobs. Both primitives are symmetric, so this layer is
quantum-resistant on its own.

The recovery key is generated for the account, shown once at sign-up and
available again from settings. It is deliberately separate from the login
password, which the server does see during authentication. Losing it means
losing the archive — there is no recovery path by design.

### Local storage

The client database is not a place where plaintext survives either: message
text, contact names, the identity private key, the recovery key, the session
token and the whole libsignal store are sealed with XChaCha20-Poly1305 before
being written. The key is generated per device, kept beside the database and
wrapped by the operating system — DPAPI on Windows, bound to the user account —
so the files on their own are useless on another machine or to another user.

Databases created before this migrate on first open, in a transaction followed
by a `VACUUM` so the old plaintext does not linger in freed pages. What this
does not defend against is code already running as that user: it can ask the OS
to unwrap the key exactly as the app does.

## License

AGPL-3.0-only.
