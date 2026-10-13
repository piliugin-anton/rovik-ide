# Running the agent in the cloud — options and plan

Goal: the user configures a remote host in Settings (host/IP, port, SSH
port, SSH password or key), tests the connection, and the IDE deploys the
agent there. Any chat can be moved to that host at any moment, with its
history, context and settings, and keeps running while the laptop sleeps.

This document describes what the backend looks like today, what has to
change whichever way we go, and the real choices (transport, project
sync, session migration, secrets), each with pros and cons and a
recommendation.

---

## 1. Where the backend is today

Facts that shape the design (paths under `src-tauri/src/`):

- **One crate, Tauri all the way down.** ~112k lines of Rust in one
  `rovik-ide` crate. 145 commands are registered in `lib.rs`; the
  frontend calls them from 25 files through `invoke`.
- **The agent is already generic over `tauri::Runtime`.** `agent_run`,
  `agent.rs`, `chat_request.rs`, `clm` take `AppHandle<R>`. That's why
  `headless.rs` (`rovik-headless`) can run the agent on Tauri's *mock*
  runtime (`features = ["tauri/test"]`). It proves the agent runs without
  a window, but it's a test harness, not something to ship on a server.
- **What the agent actually uses `AppHandle` for** is small:
  - sending events: one function, `agent_run::events::emit`, sends the
    `agent-event` enum (`AgentEvent`). Two more events come from the file
    watcher (`fs-changes`, `git-changed`);
  - looking up shared state: `app.state::<AgentState>()` (29×),
    `DbState` (25×), `VfsState` (11×), `BrowserState` (a few);
  - `app.path().app_data_dir()` and `tauri::async_runtime::spawn_blocking`.
- **Global state outside `AppHandle`:** `overlay::chats` (`DATA_DIR`,
  `OPEN`, `GATES`, `CHILDREN`, `NOTES` statics) and `workload::init(dir)`.
  These assume one process = one user's data directory, which is fine on
  a dedicated server but has to be passed in explicitly, not set once.
- **Chat state is split in three places:**
  1. SQLite `<app data>/chats.db`: `dialogs`, `messages`,
     `message_bodies`, `attachments` + `attachments/` blobs,
     `tool_call_extras`, `clm_state`, `clm_revisions`,
     `composer_drafts`, `projects`, `project_paths`, `project_decisions`.
  2. Files: `<app data>/overlays/<chat id>/` (the chat's private,
     isolated changes), `ai-settings.json` (profiles **with API keys**,
     written 0600 by `atomic_file::write_atomic_private`, but plain text),
     the project's own settings (`project_profile`).
  3. Memory only: `AgentState` (run controls, pending approvals and
     questions, `carries` = fix-round state waiting on approval).
- **Runs can already resume.** `StartKind::Resume` ("a chat whose last
  tool calls never ran"), `ContinueRun`, `ContinueReply`. A chat moved at
  a turn boundary can continue on the other side with what exists.
- **Isolation needs FUSE.** Overlays mount through `vfs` (FUSE on Linux).
  Without the driver, the chat works in the project directly. A server
  needs `/dev/fuse` (often missing in Docker/LXC unless enabled) or we
  accept the fallback.
- **The agent edits real files and runs real commands** (`bash`, checks,
  builds). In the cloud, **the project and its toolchain must be on the
  server.** This matters more than the HTTP-vs-SSH choice.

---

## 2. Refactor needed whichever option we choose

### 2.1 Split the crate (Cargo workspace)

```
src-tauri/                 (workspace)
  crates/
    rovik-core/            agent_run, agent, chat_request, chat_db, clm,
                           overlay, vfs, workload, tools, outline,
                           repo_map, git, code_search, project_profile…
                           No tauri dependency.
    rovik-protocol/        Serializable requests/responses/events
                           (AgentEvent, StartRequest, ApprovalDecision…),
                           the protocol version, shared by both sides.
    rovik-agentd/          The server binary: core + transport
                           (stdio and/or axum). Built static (musl).
  src/  (rovik-ide)        Tauri app: thin #[tauri::command] wrappers
                           over core, plus the remote client.
```

`rovik-headless` becomes a small binary on top of `rovik-core`, with no
mock runtime.

### 2.2 Replace `AppHandle<R>` with a context the core owns

```rust
/// Everything a run needs from its host, whichever host that is.
#[derive(Clone)]
pub struct Core {
    pub data_dir: PathBuf,
    pub db: DbState,
    pub agents: AgentState,
    pub vfs: VfsState,
    pub browser: BrowserState,
    pub overlays: Overlays,        // was the statics in overlay::chats
    pub workload: Workload,        // was workload::init
    pub events: Arc<dyn EventSink>,
}

pub trait EventSink: Send + Sync {
    fn agent(&self, e: &AgentEvent);
    fn fs_changes(&self, e: &FsChanges) {}
    fn git_changed(&self) {}
}
```

- The Tauri app implements `EventSink` with `app.emit(...)`; the server
  implements it as a broadcast channel that every connected client
  subscribes to (plus a ring buffer for replay, §4.3).
- `app.state::<X>()` becomes `core.x`. That's ~65 call sites, mostly
  mechanical. `R: Runtime` generics disappear from the core.
- `tauri::async_runtime::spawn_blocking` becomes
  `tokio::task::spawn_blocking`. The server owns a multi-thread runtime
  (tokio needs `rt-multi-thread` in the server crate).
- `app.path().app_data_dir()` becomes `core.data_dir`.

### 2.3 Make the command surface a protocol, not 145 Tauri commands

Not every command needs to go remote. Group them:

| Group | Examples | Remote? |
|---|---|---|
| Agent runs | `agent_start/stop/resolve_approval/answer_questions/status/configure/set_effort`, `chat_checklist` | **Yes**, core of the feature |
| Chat DB | `chat_get_messages*`, `chat_append_messages`, `chat_rename_dialog`, `chat_search`, `chat_set_model`, drafts | **Yes**, for remote chats |
| Overlay/landing | `overlay_status/file/land/discard/undo/conflict_*` | **Yes**, so changes come back (§3) |
| CLM | `clm_status/set_mode/list_revisions/…` | Yes |
| Editor/FS/git/LSP/terminal | `read_file_content`, `git_*`, `lsp_*`, `spawn_terminal` | **No, at first.** The editor stays on the local copy. A remote terminal is a later nice-to-have |

Define these as one `Request` enum + `Response` + `Event` in
`rovik-protocol` (JSON, `serde(tag = "method")`), with an `id` per call.
The Tauri commands and the server's handlers call the same core
functions. On the frontend, introduce a `backend` module that
`chatApi.ts`, `agentRuns.ts` and the overlay code use instead of calling
`invoke`/`listen` directly. It goes to the local Tauri backend or to the
remote connection depending on where the chat lives.

---

## 3. Getting the project to the server (the hard part)

The agent needs the project's files and its toolchain on the server, and
its changes have to come back to the local editor.

### Option P1: Git snapshot up, overlay diff down (recommended)

- **Up:** `git bundle` of the commits the server doesn't have yet (or a
  full clone on first use), plus a tarball of the uncommitted and
  untracked, not-ignored files. Sent over the connection; the server
  keeps one checkout per project (`~/.rovik/projects/<project id>`),
  matched by `projects.git_root_commit`, which already exists.
- The remote chat works in an overlay over that checkout, as it does
  locally.
- **Down:** when a remote chat lands, the server sends the overlay's
  change set (the same data `overlay_status`/`overlay_file` already
  expose). The client applies it to the local project through the
  existing landing code (conflicts and undo already handled). Optionally
  the server can also push a branch to the user's git remote.
- Pros: reuses overlay/landing (conflicts, undo, "a chat never overwrites
  you"); bandwidth proportional to changes; works when the laptop is
  offline (changes wait on the server); no new sync engine.
- Cons: non-git projects need a full tarball upload (fine for small
  projects, slow for big ones); files the user edits locally after the
  move aren't seen by the remote chat until a re-sync ("sync now" or
  automatic before each user message); large untracked assets are
  uploaded once.

### Option P2: Continuous two-way file sync (rsync-like / Mutagen-style)

- A watcher on both sides, with a delta sync over the connection.
- Pros: the remote chat always sees the user's latest edits; works for
  non-git projects.
- Cons: a sync engine with conflict rules is a project of its own; it
  fights with the overlay model (which side is the source of truth?);
  edits the agent hasn't landed would leak into the user's tree, which
  is exactly what isolation prevents today.

### Option P3: The project lives on the server (remote-first, VS Code Remote-SSH style)

- The editor, file tree, git, terminal and LSP all work over the
  connection; the server's checkout is the only copy.
- Pros: no sync at all; the cleanest model long-term; the server can be
  stronger than the laptop for builds and LSP.
- Cons: every editor feature has to go remote (most of the 145
  commands, the file watcher, LSP, terminal); the IDE becomes unusable
  offline for that project; a much larger first step. Moving "the current
  session at any moment" means moving the project too.

**Recommendation:** P1 now. Design the protocol so P3 can be added later
by putting the FS/git/terminal groups behind the same `backend`
abstraction.

**Toolchain on the server:** the agent's checks (`cargo`, `npm`,
`pytest`…) must be installed there. Offer a "server setup script" field
in the host settings (run once over SSH), and report missing tools in
the connection test via `project_profile`, which already detects the
project's checks.

---

## 4. Transport: Axum over HTTPS vs. SSH stdin/stdout

### Option T1: SSH exec + JSON-RPC over stdin/stdout

The client runs `ssh host ~/.rovik/bin/rovik-agentd --stdio`; requests
and events are newline-delimited JSON (or length-prefixed) over the
channel.

Pros:
- **No new attack surface:** no open port, no TLS, no certificates, no
  tokens. Auth = SSH auth, which the user already configures.
- Works behind NAT and firewalls, and on any host where only port 22 is
  reachable.
- Host identity = SSH host key (pinned at first connect).
- Simplest to build: the stdio loop is about 100 lines; the same binary
  can also do SFTP-free uploads through the channel.

Cons:
- **The process lives as long as the SSH session.** Runs must survive
  disconnects (laptop sleep is the main reason to go to the cloud), so
  the stdio process cannot be the agent itself. It has to be a thin
  `--attach` client that connects to a long-lived daemon over a Unix
  socket. That makes it option T3 in practice.
- One TCP stream for everything: a large file transfer delays events
  (head-of-line blocking), unless multiplexed with separate SSH channels.
- Every client device needs SSH credentials to the server; no
  browser/mobile access later.
- Reconnect, keepalive and resume are ours to handle (SSH keepalives
  help).

### Option T2: Axum HTTPS + WebSocket, exposed on a port

A daemon (systemd user unit) listens on `host:port`; REST for
request/response, one WebSocket for events.

Pros:
- Daemon independent of any client connection; several clients
  (desktop, a future web/mobile view) can watch the same run.
- Standard tooling: HTTP/2, compression, `tower` middleware, rate
  limiting, health checks, metrics.
- Uploads (project bundles, attachments) on separate requests, without
  blocking events.

Cons:
- **A public port running an agent that executes shell commands** is a
  high-value target: TLS, auth, brute-force protection and updates
  become security-critical.
- Certificates: a domain + ACME (Let's Encrypt, needs port 80 or 443
  reachable) or self-signed. Certificates for bare IPs exist only on
  some CAs or profiles and are short-lived.
- Firewall and cloud security-group setup (the user has to open the
  port; we can't always do it for them).
- More code: TLS automation, token issuing and revocation, CORS if a
  browser client comes later.

### Option T3: Axum daemon on localhost, reached through an SSH tunnel (recommended)

The daemon (`rovik-agentd serve`) runs as a systemd user service bound
to `127.0.0.1:<port>` or a Unix socket. The client opens an SSH
connection and forwards a local port to it (`direct-tcpip` channel,
like `ssh -L`), then speaks HTTP + WebSocket to it.

Pros:
- Daemon outlives connections (runs continue while the laptop is
  closed); reconnect = new tunnel + resume events from a sequence number.
- **No public port and no certificates needed**: SSH encrypts and
  authenticates. Still token-auth the HTTP layer (defense in depth
  against other local users on the server). The token is generated at
  deploy time and delivered over SSH.
- Same Axum code as T2. Exposing it publicly later is a config switch
  (`--listen 0.0.0.0:443 --tls auto`), not a rewrite.
- One credential set (SSH), which the settings page already collects.

Cons:
- Still requires SSH reachability from the client (but so does deploy).
- Two layers (SSH + HTTP) to debug.
- A future browser/mobile client needs T2 mode anyway.

### HTTPS/SSL automation (for T2, or T3 with public mode on)

What "automatic HTTP and SSL" means in each case:

1. **Domain name given:** the daemon gets a certificate itself with
   `rustls-acme` (TLS-ALPN-01 on 443, no port 80 needed), renews it, and
   serves Axum with `axum-server` + rustls. The connection test checks
   that the DNS A/AAAA record resolves to the host.
2. **IP only (most common for "a VPS"):** at deploy time the client,
   over SSH, has the daemon generate a self-signed certificate (`rcgen`)
   and read back its SHA-256 fingerprint. The client **pins** that
   fingerprint (stored with the host settings). This is as secure as a
   CA certificate for our purposes, because the fingerprint arrives over
   the SSH channel the user already trusts, and it needs no domain, no
   port 80 and no renewal. This is the recommended default for T2.
3. **Auth on the HTTP layer:** a random 256-bit token per client device,
   created at deploy over SSH, stored hashed (argon2/sha256) on the
   server and in the OS keychain on the client; sent as
   `Authorization: Bearer`. Revocable per device. Alternative: mTLS with
   a client certificate generated at deploy (stronger, slightly more
   work; rustls supports it directly).

---

## 5. Moving a session to the cloud

"Move this chat to <host>" at any moment. What has to travel:

| What | Where it lives | How it moves |
|---|---|---|
| The chat and its subagent chats | `dialogs`, `messages`, `message_bodies`, `tool_call_extras` (children via `overlay::chats::CHILDREN` / dialog parent links) | Export rows |
| Context-management state | `clm_state`, `clm_revisions` | Export rows |
| Screenshots | `attachments` + `attachments/<sha256>` | Upload blobs the server doesn't have (content-addressed, dedups) |
| Project identity | `projects`, `project_paths`, `project_decisions` | Export the row; the server maps it to its own checkout path |
| Model, mode, context mode | `dialogs` columns, `chat_set_model` | Part of the dialog row |
| Private changes | `overlays/<chat id>/` | Tarball of the overlay store, applied over the server checkout (§3) |
| Agent settings | `AgentConfig` (permissions, approval profiles, subagents), project settings | Sent with the move; the daemon's `agent_configure` per chat |
| Model profiles + API keys | `ai-settings.json` | See §6. Only the profiles the chat (and its subagents/reviewers) uses |
| Fix-round state waiting on approval | `AgentState.carries` (memory) | Serialize `VerifyCarry` or don't move while one is pending (below) |
| Pending approvals/questions | `AgentState` (memory) | Not moved: they're re-asked after `Resume` |

### Option M1: Ownership transfer at a turn boundary (recommended)

1. User clicks "Move to cloud". If a run is going, the client asks it to
   **stop at the next safe point**: after the current tool call
   finishes, before the next model request. (A new stop mode next to
   `agent_stop`; today's stop is immediate.)
2. The client exports a **chat bundle** (dialog + children rows as JSON,
   attachments, overlay store, settings subset), uploads the project
   delta (§3), and the server imports it in one transaction.
3. The local chat is marked `location = remote:<host id>` (new column)
   and becomes read-only locally. The UI reads it through the remote
   backend from then on.
4. The server starts the chat with `StartKind::Resume` or `ContinueRun`,
   whichever applies; both already exist.
5. "Move back" is the same in reverse; the overlay's changes land
   locally (or move back as an overlay).

Pros: one owner per chat at any time, so no sync conflicts; reuses
resume logic; the bundle format doubles as chat export/import and backup.
Cons: "at any moment" means "within one tool call" (seconds, or as long
as a running build takes). A truly mid-request move would lose the
in-flight model response anyway.

### Option M2: The server becomes the source of truth for all chats

All chats always live on the server; the desktop keeps a cache.

Pros: nothing to move; every device sees every chat.
Cons: the IDE stops working offline; local-only users pay for a design
they don't use; a far bigger change to `chat_db`.

### Option M3: Replicate the database (e.g. CRDT/litestream-style)

Both sides write, and the changes merge.

Pros: seamless in theory.
Cons: the chat log has strict ordering (`seq`) and tool-call/result
pairing; two writers would corrupt runs. Not worth it.

**Recommendation:** M1. Add a `location` column to `dialogs`, a bundle
export/import in `chat_db` (useful by itself), and a graceful
"stop after this tool call" mode.

### Live view while remote

- The daemon numbers every `AgentEvent` per chat (`seq`) and keeps the
  last N in memory; the client resubscribes with "since seq X" after a
  reconnect. Anything older comes from the database, which matches the
  existing rule in `events.rs`: "the database stays the source of truth
  for anything it misses".
- `FileWritten` events carry server paths; the client maps
  `server checkout path` → `local project path` (or ignores them until
  landing, since the remote chat writes into its overlay, not the user's
  tree).
- Edit approvals already carry `current` and `proposed` content in the
  event, so the local diff view works unchanged.

---

## 6. Secrets

### On the client

- **SSH password/private key, the HTTP token, the pinned certificate
  fingerprint and host key:** stored in the OS keychain with the
  `keyring` crate (macOS Keychain, Windows Credential Manager, Linux
  Secret Service/KWallet). `ai-settings.json`-style files keep only a
  reference (`host id`). Where Secret Service isn't available (headless
  Linux), fall back to a file encrypted with a key derived from a user
  passphrase (argon2id + XChaCha20-Poly1305), and say so in the UI.
- Prefer **key auth and ssh-agent** over passwords: let the user pick a
  key file (stored by path, passphrase in the keychain) or "use
  ssh-agent". Offer "generate a key for this host" and install the public
  key with the password once, then forget the password.
- Side effect worth doing at the same time: move the **API keys** that
  are currently plain text in `ai-settings.json` into the keychain too.

### On the server

- API keys for the model providers: two choices.
  - **Sent per session, kept in memory** (recommended default): the
    client sends the keys a chat needs when it moves or reconnects. A
    daemon restart (reboot, upgrade) pauses remote runs until a client
    reconnects. Nothing usable is left on disk.
  - **Stored on the server**, encrypted at rest (key from a passphrase,
    or the plain 0600 file the desktop uses today), so runs survive
    reboots without a client. Opt-in, with a clear warning.
- The daemon runs as the SSH user, never as root; files under
  `~/.rovik/` are 0700.
- The agent on the server still obeys the permission rules
  (`agent_run::permissions`, `is_secret_path`), but approvals now arrive
  remotely, so an unattended remote run waits rather than auto-approves.

### SSH library

- **`russh`** (pure Rust, async, tokio): supports keys, passwords,
  ssh-agent, port forwarding and exec channels; no C dependency; good
  fit with tokio. Recommended.
- Alternatives: `ssh2` (libssh2 bindings; mature, blocking, C build
  dependency) or shelling out to the system `ssh` (inherits
  `~/.ssh/config`, ProxyJump, hardware keys; but output parsing is
  fragile and Windows has to have OpenSSH installed). A reasonable mix:
  russh by default, plus a "use my system ssh config" option later.

---

## 7. Settings page, connection test and deployment

### Settings → Cloud hosts

Fields: name, host/IP, SSH port (22), SSH user, auth (key file / ssh-agent
/ password), agent port (used for T2 public mode; for T3 it's a
localhost port on the server), optional domain (enables ACME), setup
script, "keep API keys on server" toggle.

### "Test connection" does, in order, and reports each step

1. DNS resolve (if a hostname) and TCP connect to the SSH port, with a
   timeout.
2. SSH handshake: show the host key fingerprint for the user to confirm
   on first connect (TOFU), then pin it; a later mismatch is a hard error.
3. Authenticate.
4. Detect platform: `uname -sm` (x86_64/aarch64 Linux supported first),
   free disk in `$HOME`, `git --version`, `/dev/fuse` and `fusermount3`
   (isolation available or not), systemd user session and `loginctl
   enable-linger` (otherwise a fallback such as `nohup`/`setsid`).
5. If T2/public mode: TCP connect to `host:port` from the client
   (firewall check), and, if a domain is given, that it resolves to this
   host.
6. If already deployed: daemon version and `/health` through the tunnel.

### Deploy

1. Upload `rovik-agentd-<version>-<arch>` (static musl binary, shipped
   with the app or downloaded from GitHub releases with a checksum) to
   `~/.rovik/bin/`, over SFTP or an exec channel.
2. Write `~/.rovik/config.toml` (listen address, data dir, token hash,
   TLS mode) and install a systemd **user** unit
   `rovik-agentd.service`; enable linger so it runs after logout.
3. Start it; check `/health`; protocol version handshake. On a version
   mismatch the client offers to upgrade (the daemon finishes its runs
   first: drain, then restart).
4. Run the user's setup script, if any.

Build: add `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`
targets for `rovik-agentd` in CI (`cargo zigbuild` or `cross`). The core
already bundles SQLite and vendors libgit2, which helps static builds;
`fuser` doesn't link libfuse. Tree-sitter grammars are C, so a musl C
toolchain is needed; zigbuild handles that.

---

## 8. Summary of options

| | Effort | Survives disconnect | Attack surface | Future web/mobile | Verdict |
|---|---|---|---|---|---|
| **T1** SSH stdio | Low | Only with a daemon behind it | Minimal (SSH) | No | Fine for a prototype |
| **T2** Axum public HTTPS | High | Yes | Public port | Yes | Later, as an opt-in mode |
| **T3** Axum on localhost + SSH tunnel | Medium | Yes | Minimal (SSH) | Switchable to T2 | **Recommended** |

| | Effort | Offline-friendly | Reuses existing code | Verdict |
|---|---|---|---|---|
| **P1** git snapshot up / overlay diff down | Medium | Yes | Overlay + landing | **Recommended** |
| **P2** continuous two-way sync | High | Yes | Little | No |
| **P3** remote-first project | Very high | No | Little | Long-term option |

| | Effort | Conflicts | Verdict |
|---|---|---|---|
| **M1** move ownership at a turn boundary | Medium | None (one owner) | **Recommended** |
| **M2** server owns all chats | High | None | No (breaks offline) |
| **M3** replicate the database | Very high | Many | No |

---

## 9. Suggested phases

1. **Core extraction (no user-visible change).** Workspace split;
   `Core` + `EventSink` instead of `AppHandle<R>`; overlay/workload
   statics into `Core`; `rovik-headless` on top of `rovik-core` without
   the mock runtime. The existing e2e tests keep passing; they're the
   safety net for this step.
2. **Protocol + local daemon.** `rovik-protocol` enum; `rovik-agentd
   serve` (Axum on a Unix socket/localhost); a frontend `backend`
   abstraction over `invoke`/`listen`. Test by running the daemon
   locally and pointing a chat at it.
3. **Chat bundle export/import** + `dialogs.location` + "stop after
   this tool call". Testable locally (move to the local daemon).
4. **SSH layer:** host settings page, keychain storage, connection test,
   deploy/upgrade, tunnel (russh). Move API keys to the keychain.
5. **Project sync (P1):** git bundle + working-tree snapshot up; remote
   landing applied locally; "sync now" before user messages.
6. **Optional:** public HTTPS mode (pinned self-signed cert or ACME),
   remote terminal, then remote-first projects (P3).

Open questions for you:

- Single-user servers only (one daemon per SSH user), or a shared server
  for a team? Multi-user changes auth, data separation and sandboxing.
- Should the server run the agent's commands inside a container (Docker
  or podman) per project for isolation and reproducible toolchains?
  Recommended if the server is shared or long-lived.
- Non-git projects: support them with full uploads, or require git for
  cloud chats at first?
