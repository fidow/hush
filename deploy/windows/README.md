# Deploying hush-server (Windows, behind Apache)

What this package contains:

| File | What it is |
|---|---|
| `hush-server.exe` | The server. A single binary, nothing to install. |
| `hush-server.cmd` | Its configuration: environment variables. **This is the only file you need to edit.** |
| `install-service.ps1` | Registers automatic startup and checks that the server answers. Optional. |

---

## What the server is

An HTTP process **without TLS** listening on `127.0.0.1:8080`. It exposes a
REST API under `/v1/...`, a download page at `/`, and a **Server-Sent Events**
endpoint (`/v1/messages/stream`) that holds an HTTP connection open
indefinitely for every connected user.

Everything is stored in a single SQLite file. No external database, runtime or
extra service is needed.

---

## What to do on the machine

**1. Copy the package** into a folder, for example `C:\hush\`.

**2. Edit `hush-server.cmd`**: the database path and the SMTP relay details.
Without working SMTP nobody can verify their account or recover a password, so
it has to work.

**3. Create the data folder** that `HUSH_DB` points at and grant write access
to the account that will run the process. That file holds the accounts and the
encrypted archive: **it is the only thing that needs backing up.** If `HUSH_DB`
is not set the server refuses to start, so it never ends up writing to an
unexpected location.

The log path is configured separately, in `HUSH_LOG_FILE`, and accepts any
absolute location — another drive or a network share — creating the folder if
needed. It rotates at local midnight (`hush.log.2026-08-02`, and the timestamps
inside carry the machine's UTC offset, so the log lines up with Apache's) and
cleans up after itself:
`HUSH_LOG_KEEP_DAYS` (30 by default, `0` to never delete) removes older rotated
files at startup and once a day. It only deletes files it generated, so it is
safe to point at a folder shared with other logs. Without `HUSH_LOG_FILE` the
server writes to the console, which as a scheduled task means losing the log.

**4. Start it at boot** and restart it if it dies. `install-service.ps1` does
this with Task Scheduler; NSSM or any other supervisor works just as well. It
is not a native Windows service — it is a plain executable, so it needs a
wrapper.

**5. Do not open port 8080 on the firewall.** External access comes in through
Apache.

---

## What it needs from the existing Apache

The `hush.villasante.es` virtualhost has to reverse-proxy to
`http://127.0.0.1:8080`, with these requirements:

**The whole domain goes to the backend.** There are no paths Apache should
serve itself: `/` is served by the server (the download page) and everything
else is the API.

**The `/v1/messages/stream` endpoint needs special treatment.** It is the only
delicate part of the deployment, and if it is wrong the app looks broken:

- **No buffering and no compression** on that path. If Apache accumulates the
  response (typically because of `mod_deflate`), messages arrive late or not at
  all.
- **A long timeout**, on the order of an hour. With the default value Apache
  cuts the connection after a minute and the client ends up in a continuous
  reconnect cycle.

**Forward the client IP.** The server rate-limits registration, login and
verification attempts per IP. Behind a proxy it only sees Apache's address, so
it needs `X-Forwarded-For` — which `mod_proxy` already adds — and
`HUSH_TRUST_PROXY=1` in `hush-server.cmd` (already set).

> That variable must **only** be enabled if there really is a proxy in front.
> If the server were reachable directly, anyone could forge the header and
> bypass the limits.

**Allow request bodies of about 20 MB.** Images travel inside the encrypted
message; the server already rejects anything over 15 MB on its own.

**Redirect HTTP to HTTPS.** The app always talks over HTTPS.

Optional but worthwhile: enable the post-quantum hybrid key exchange
(`X25519MLKEM768`) in Apache if the OpenSSL version supports it, so the
transport is post-quantum too. It is not essential — message encryption is
independent of TLS and is already quantum-resistant.

---

## Checking that it works

From the machine itself, `http://127.0.0.1:8080/` must return the download
page. From outside, `https://hush.villasante.es/` must return the same thing.

The test that really matters is the stream: opening
`https://hush.villasante.es/v1/messages/stream` in a browser must **hang
without returning anything** (it answers 401 without a session, but it must not
close or fail with a proxy error). If it responds immediately with a 502 or
504, the proxy is not configured correctly for SSE.

---

## Updates

Stop the process, replace `hush-server.exe`, start it again. The database
migrates itself at startup; nothing to delete and nothing to run.
