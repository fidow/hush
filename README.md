# Hush

Private 1-to-1 messaging with **post-quantum end-to-end encryption**. The server
is a dumb relay/mailbox: it stores public key bundles and queues of opaque
encrypted blobs — it can never read message contents.

**[hush.villasante.es](https://hush.villasante.es)** — the live server the
client talks to. The Windows client is published in
[releases](https://github.com/fidow/hush/releases).

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
verification and login (per account and per IP), message sending, profile
lookups and key-bundle fetches. Verification codes are additionally burned
after 5 wrong attempts, compared in constant time, and a login for a
non-existent account still runs Argon2 so response times don't enumerate
usernames. Per-account quotas cap the undelivered queue, by message count and
by bytes.

Anything an account publishes — its key bundle, its presence — is only served
to people it accepted; a stranger gets the same answer as for a name that does
not exist. Looking a profile up cannot be limited that way, since it is how you
find somebody before adding them, so it is metered instead: a wide budget for
lookups and a much tighter one for misses, which is what walking a dictionary
of usernames looks like.

Request bodies are capped at 256 KB except when sending a message, which may
carry a picture; requests are bounded in number and in time, so the memory the
server can be made to hold does not depend on how many connections somebody
opens.

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

### History stays on the device

The server keeps no history at all: a message is deleted as soon as the
recipient acknowledges it. What a device holds is what it received, and nothing
of it exists anywhere else.

Moving conversations to another device is therefore something the user does
deliberately, from settings: **export** writes every conversation to a file
sealed with a password of their choosing, and **import** merges that file
elsewhere. The password is turned into a key with Argon2id (64 MiB, 8 passes)
and the contents sealed with XChaCha20-Poly1305; the cost is written into the
file and authenticated along with it, so it can be raised later without making
old exports unreadable, and cannot be edited down to something cheap to attack.
Both primitives are symmetric, so the file is quantum-resistant on its own.

A wrong password is indistinguishable from a damaged file. Losing it means
losing the file — there is no recovery path by design.

### Profile pictures

A picture never reaches the server. It is sent to each accepted contact inside
an encrypted message, exactly like anything else they receive, and stored
sealed on their device. That costs one message per contact whenever it changes,
which is why it is scaled down to a 256px thumbnail before being sent.

### One device at a time

An account lives in one place. Signing in somewhere else takes it over: the
previous session's token stops working and its open stream is dropped on the
spot, so it stops receiving immediately rather than at its next reconnect.

That new device publishes a new identity key, which contacts will notice — see
below, which is the point.

### When a contact's key changes

The client pins each contact's identity key the first time it sees one. If a
later bundle carries a different key, nothing is sent to that contact and
nothing of theirs is read: the app says so and shows both fingerprints, for the
two of them to compare over some channel it does not control.

This matters more than it looks. A contact reinstalling and a relay handing
over a key of its own are indistinguishable from here, and a relay that can get
a pinned key dropped — by, say, delivering one unreadable message — is a relay
that can arrange to be trusted whenever it likes. So an unreadable message
drops the ratchet and nothing else, and only the person using the app can
accept a new key.

### Updates

The client checks `/v1/update/{target}/{arch}/{version}` on start and offers
what the server publishes from `HUSH_UPDATE_DIR`. The installer is verified
against a minisign public key built into the app, so the server can only offer
builds the developer signed; a compromised server cannot push its own. The
private key lives outside the repository and signs the artefacts at build time
(`TAURI_SIGNING_PRIVATE_KEY`).

### Local storage

The client database is not a place where plaintext survives either: message
text, contact names, the identity private key, the session
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
