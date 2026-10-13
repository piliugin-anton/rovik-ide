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
- **Global state outside `AppHandle`:** `overlay::chats` has about
  fifteen statics (`DATA_DIR`, `OPEN`, `GATES`, `CHILDREN`, `NOTES`,
  `RUNNING`, `DIRECT`, `BUSY`, `PEERS`, `STATE_LOCKS`, `SHOWN`…),
  `workload` has four (`SETTINGS_FILE`, `HISTORY`, `SHARED`, `STARTED`),
  plus `confine::available` and `vfs::macfuse::LIBRARY`. All but two are
  per-process caches keyed by chat id or path; only `DATA_DIR` and
  `workload::init(dir)` are set-once configuration. One daemon serves one
  user and one data directory, which is exactly what they assume, so
  they can stay statics (§2.2).
- **Settings and API keys already travel with every call.** The frontend
  owns `ai-settings.json` (`load_ai_settings`/`save_ai_settings` are
  opaque strings to the backend) and sends `ModelProfile` (with
  `api_key`) in every `StartRequest`, and `AgentConfig` (permissions,
  reviewer and subagent profiles, keys included) through
  `agent_configure` at startup and on every change. A run never reads
  the file. A daemon therefore needs nothing on disk to run a chat; it
  needs those same two calls from a connected client.
- **Streaming commands use `tauri::ipc::Channel`:** `send_chat_message`
  (agent.rs), `spawn_terminal`, the `vfs_*` subscriptions and `lsp_*`.
  Agent runs don't; they use app events. Of these only
  `send_chat_message` could need to go remote, so the protocol needs one
  streaming shape (§2.3), not a general one.
- **Other Tauri-managed state:** `ChatRequests` (cancel for
  `send_chat_message`), `WatcherState`, `LspState`, `TerminalState`. The
  last three stay local.
- **The web tools need Chrome** (`browser_tools.rs`: `eoka::find_chrome`,
  launched headless). On a server either Chromium is installed (setup
  script) or `browse_url`/`web_search` fail with a clear message; the
  connection test should say which (§7).
- **Command confinement needs unprivileged user namespaces**
  (`overlay/confine.rs`); Docker and most LXC images turn them off.
  Without them, commands that name the project's path are refused. With
  the `/dev/fuse` point below, a typical server has neither, so the
  server design shouldn't depend on either (§3.1).
- **Paths in stored data are absolute and local:** `projects.path`,
  `project_decisions.path`, the overlay store's `project.txt` (its base
  folder), the mount path (`$XDG_RUNTIME_DIR/rovik-ide/chats/<id>`) in
  tool results the model has seen, `abs_path` in `FileWritten` events. A
  moved chat needs them remapped at import, and the client needs the
  reverse map for events (§5).
- **A project's identity is a file in the project:** `.rovik/project.json`
  (`chat_db/projects.rs`). The server's copy has to carry the same file
  (or the daemon writes it from the exported `projects` row) so the chat
  finds its project there under the same id.
- **The e2e tests build a mock Tauri app** (`mock_builder().manage(...)`,
  about 20k lines across `e2e_tests.rs`, `subagent_e2e_tests.rs` and
  `repair_e2e_tests.rs`) and call the commands with `app.state()`. The
  harness changes with §2.2; the tests' bodies mostly don't. The eval
  scripts (`scripts/eval`) drive `rovik-headless`, so its CLI must stay.
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
    // Later, for a multi-tenant daemon: `overlays` and `workload` as
    // fields. Today they stay the statics they are (below).
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
- The statics in `overlay::chats` and `workload` **stay statics in
  phase 1**. A daemon is one process for one user and one data
  directory, which is what they assume. Only the set-once ones
  (`DATA_DIR`, `workload::init`, `set_chat_resolves`) become part of
  `Core::new(data_dir)`. Moving fifteen caches into `Core` is a week of
  plumbing that buys nothing until a multi-tenant daemon exists.
- A cheaper route to the same place: make the context a **trait first**
  (`trait Host { fn db(&self) -> &DbState; fn agents(&self) ->
  &AgentState; … fn emit(&self, e: AgentEvent); }`), implement it for
  `AppHandle<R>` with `state::<X>()`, change the ~65 call sites to
  `host.db()`, and only then add the plain `Core` struct and swap the
  test harness over. Each step compiles on its own and the e2e tests run
  at every step, instead of one big-bang commit that moves the harness,
  the states and the call sites together.

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
`rovik-protocol` (JSON, `serde(tag = "method")`), with an `id` per call,
and make the protocol **one bidirectional message stream**: requests,
responses and notifications (events, and chunks for the one streaming
call) on a single ordered channel, JSON-RPC style. The same stream then
runs over a WebSocket (T2/T3), over an SSH exec channel's stdio (T1),
over a Unix socket for a local daemon in tests, and in-process in the
Tauri app. HTTP stays for what a stream is bad at: blob upload/download
(attachments) and `/health`. T3's "two layers to debug" becomes one
protocol plus a dumb byte pipe, and T1 is a free mode of it.

The Tauri commands and the server's handlers call the same core
functions; the remote-capable commands are generated from the enum (a
macro, or one `dispatch(Request) -> Response` the Tauri side wraps), so
there is one list to maintain, not two. On the frontend, introduce a
`backend` module that `chatApi.ts`, `agentRuns.ts` and the overlay code
use instead of calling `invoke`/`listen` directly. It picks the local
Tauri backend or a remote connection by the chat's `location` (§5) and
feeds remote events into the same `agent-event` handler, so the stores
and panels don't know where a chat runs.

Two things this costs, worth knowing up front:

- the chat list and search (`chat_list_dialogs`, `chat_search`, FTS) are
  local-database queries. A remote chat's new messages aren't in it
  unless mirrored; §5's mirror makes that a non-issue;
- `agent_configure` is global on each backend. The frontend has to send
  it to every connected backend, on connect and on every change, and a
  daemon that restarted has nothing until a client reconnects (§6).

---

## 3. Getting the project to the server (the hard part)

The agent needs the project's files and its toolchain on the server, and
its changes have to come back to the local editor.

### 3.1 Where the chat works on the server

Locally a chat works in a FUSE overlay and its commands are confined
with user namespaces. A server often has neither (`/dev/fuse` and
unprivileged user namespaces are both off in most containers), and the
existing fallback, "work in the project directly", is fine on a server
**as long as the checkout is the chat's own**. So:

- The daemon keeps one bare repository per project
  (`~/.rovik/projects/<project id>.git`) and gives each remote chat a
  **git worktree of its own** (`~/.rovik/work/<chat id>/`, checked out
  from the chat's snapshot commit, §3.2). The worktree is the chat's
  private copy: no overlay, no mount, no confinement needed, and
  `git diff <snapshot>` is the chat's change list. Worktrees share
  objects, so they're cheap in disk; build caches aren't shared, so a
  chat's first build on the server is cold. Keeping a project's last
  worktree and reusing it for the next chat on that project softens
  that.
- Where the driver is present, the overlay can later run over the
  worktree for the finer features (`overlay_undo_group`, redo, unmark,
  conflicts-to-chat). Not in v1: it doubles the server-side paths.
- Landing back (§3.2, down) needs the same three inputs either way: the
  base (snapshot commit), the chat's version (worktree, or overlay
  upper) and the project's file now (local). One `changes()` shape
  serves the local overlay and the server worktree.
- Pros: works in any container or VM with only `git` installed; the
  server's state is plain git, inspectable with git tools; crash
  recovery is `git status`.
- Cons: no fine-grained undo groups on the server in v1; disk per
  worktree for build output; a chat's `.gitignore`d by-products stay in
  the worktree (that's what tiers.rs skips at landing anyway).

### 3.2 Option P1: git carries the project both ways (recommended)

The desktop already links libgit2 with its SSH transport (`git2` with
the `ssh` feature; libssh2 is in Cargo.lock). Git is the transfer
format, with libgit2's negotiation: only objects the other side lacks
travel, in both directions.

- **Up:** the client makes a **snapshot commit** of the working tree
  (index plus untracked, not-ignored files, through a temporary index;
  parent `HEAD`; the user's branches and index are never touched) and
  pushes it with libgit2 over SSH to the project's bare repository on
  the server, as `refs/rovik/chats/<chat id>/snapshot`. The first push
  carries the history (a clone's worth, once); later ones only the new
  blobs. The daemon checks the snapshot out into the chat's worktree.
  "Sync now", and the automatic sync before each user message, is
  another snapshot push: seconds.
- **Down:** when the chat lands (the user asks, or the run finished and
  auto-landing applies, `agent_run/landing.rs`), the daemon commits the
  worktree as `refs/rovik/chats/<chat id>/result` (parent: the
  snapshot). The client fetches it over SSH and lands it through the
  existing landing code with base = snapshot tree, chat = result tree,
  project = the local file now: the same three-way merge, journal,
  conflicts and undo as today. Optionally the daemon also pushes a branch
  to the user's remote.
- **Non-git projects:** the client keeps a **shadow repository** in app
  data (`<app data>/shadow/<project id>.git` with `core.worktree` set to
  the project folder) and snapshots into it. The user never sees it;
  cloud chats just work, and the project's `.gitignore` files are still
  honoured by the snapshot.
- Pros: no bundle format, no tarball, no new sync engine, no server-side
  credentials (the client pushes; the server only receives); incremental
  by construction; the snapshot id doubles as the merge base and as a
  record of exactly what the chat saw; move-back (§5) is "fetch the
  result ref"; the server works while the laptop is offline (the result
  waits as a ref).
- Cons: needs SSH for git as well as for the tunnel (same credentials,
  but a second SSH stack on the client, libssh2 next to russh, with its
  own host-key check to keep consistent); the first push of a large
  history is as slow as a clone; large untracked binaries become git
  objects in the shadow or bare repository (ignore rules keep build
  output and dependencies out); files the user edits locally after the
  move aren't seen until the next sync.

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

**Recommendation:** P1 with the worktree-per-chat server mode (§3.1).
Design the protocol so P3 can be added later by putting the
FS/git/terminal groups behind the same `backend` abstraction. Require git
for cloud chats only in the sense that the shadow repository provides it
where the project has none.

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
- With the protocol as one message stream (§2.3), a `rovik-agentd
  attach --stdio` mode that bridges the stream to the daemon's Unix
  socket is a hundred lines, so T1 comes for free for hosts where port
  forwarding is disabled (`AllowTcpForwarding no`).
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
| Project identity | `projects`, `project_paths` | Export the row; the server maps it to the chat's worktree (`project_decisions` stay local) |
| Model, mode, context mode | `dialogs` columns, `chat_set_model` | Part of the dialog row |
| Private changes | `overlays/<chat id>/` | Simplest: land locally first, then move a clean chat. Otherwise the overlay's `upper/` files are part of the snapshot commit (§3.2) and `base/` is the parent tree, so the worktree starts with the chat's changes |
| Agent settings | `AgentConfig` (permissions, approval profiles, subagents), project settings | Sent with the move; the daemon's `agent_configure` per chat |
| Model profiles + API keys | Frontend memory (`ai-settings.json` is the frontend's file) | Already in `StartRequest` and `AgentConfig`; sent to the daemon the same way (§6) |
| Fix-round state waiting on approval | `AgentState.carries` (memory) | Not moved: the stop drops it and the checks run again on the other side (below) |
| Pending approvals/questions | `AgentState` (memory) | Not moved: they're re-asked after `Resume` |

### Option M1: Ownership transfer at a turn boundary (recommended)

1. User clicks "Move to cloud". If a run is going, the client asks it to
   **stop at the next safe point**: after the current tool call
   finishes, before the next model request. (A new stop mode next to
   `agent_stop`; today's stop is immediate.)
2. The client exports a **chat bundle** (dialog + children rows as JSON,
   attachments, overlay store, settings subset), uploads the project
   delta (§3), and the server imports it in one transaction.
3. The local chat is marked `location = remote:<host id>` (new column).
   The local rows stay and become a **mirror**: every `AgentEvent` the
   client receives for the chat (`MessageStored`, patches, subagent
   messages, checklist) is also written to the local database, keyed by
   the message's `seq`, so a replay after a reconnect is idempotent. The
   server is the chat's only writer; the mirror only follows. The chat
   list, search, "open around a hit" and reading history offline keep
   working from the local database with no change to `chatStore`; the
   remote backend is asked only for what needs the owner (start, stop,
   approvals, questions, landing, CLM) and for a `since seq` catch-up on
   connect.
4. The server starts the chat with `StartKind::Resume` or `ContinueRun`,
   whichever applies; both already exist.
5. "Move back": stop at the safe point, catch the mirror up to the
   server's last `seq`, fetch the result ref (§3.2) and land it (or keep
   it as a local overlay), flip `location` back. With the mirror this is
   a small transfer, not a second bundle.

At import the daemon rewrites what is local to the laptop:
`projects.path` becomes the worktree path; `project_decisions` don't move
(they're about the laptop's folders); `.rovik/project.json` is written
into the worktree from the `projects` row; an overlay store, if one moves
rather than lands, gets a new `project.txt`. The model's history keeps
the old mount paths in old tool results; that's harmless (the system
prompt and every new tool result name the new root), but `abs_path` in
events needs the reverse map on the client.

Pros: one owner per chat at any time, so no sync conflicts; reuses
resume logic; the bundle format doubles as chat export/import and
backup; the mirror keeps the local UI code and offline reading as they
are.
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

### Approvals while the laptop is closed

"Keeps running while the laptop sleeps" holds only for runs that don't
wait on the user. A remote run that needs an approval waits (it must
never auto-approve because nobody is there), so:

- when moving a chat, show what its permissions will ask the user for
  and offer the cloud choice: the AI reviewer approves what the user
  would otherwise be asked (`approval_profiles` already exist), or "ask
  me and wait";
- a run waiting on an approval or a question is a visible state in the
  chat list (a badge, from `agent_status`), so the user finds it on
  return; a push or email notification from the daemon is a later
  option;
- `VerifyCarry` holds an `Instant` and an `Arc<Seen>`; it isn't
  serializable and needn't be: the move waits for the safe point, and a
  pending carry is dropped with the stop, with the chat told that the
  waiting checks will be run again on the other side.

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
  - **Sent per session, kept in memory** (recommended default, and how
    the desktop already works: keys arrive in `StartRequest` and
    `AgentConfig`, never from a file). The client sends `agent_configure`
    and the chat's profile when it moves a chat or reconnects. A daemon
    restart (reboot, upgrade) pauses remote runs until a client
    reconnects; the daemon reports "waiting for a client" as the run's
    status so it isn't mistaken for a hang. Nothing usable is left on
    disk.
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
  dependency, and already linked through `git2`) or shelling out to the
  system `ssh` (inherits `~/.ssh/config`, ProxyJump, hardware and FIDO
  keys, certificates; but password auth needs a pty, and output parsing
  is fragile). A reasonable mix: russh by default, plus a "use my system
  ssh" option (`ssh -W`/`-L` under our control, no output parsing needed
  for a tunnel) in the same phase, because the first users with jump
  hosts or hardware keys will hit russh's limits at once.
- Two SSH stacks on the client (russh for the tunnel and exec, libssh2
  for git) is the price of using git as transport; both verify the host
  against the one pinned key from the settings.

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
   free disk in `$HOME`, `git --version` (worktrees need 2.5+; receiving
   a push into a bare repository needs nothing special), Chromium for
   the web tools, unprivileged user namespaces and `/dev/fuse` +
   `fusermount3` (reported as "isolation extras", not required, §3.1),
   systemd user session and `loginctl enable-linger` (otherwise a
   fallback such as `nohup`/`setsid`), and that the provider's API host
   is reachable from the server (some providers geo-block or need a
   proxy; a run that fails only on the server is confusing).
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
   `rovik-agentd.service`; enable linger so it runs after logout. Create
   `~/.rovik/projects/` (bare repositories) and `~/.rovik/work/`
   (worktrees); the push in §3.2 targets a path under the first.
3. Start it; check `/health`; protocol version handshake. On a version
   mismatch the client offers to upgrade (the daemon finishes its runs
   first: drain, then restart).
4. Run the user's setup script, if any.

Build: add `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`
targets for `rovik-agentd` in CI (`cargo zigbuild` or `cross`). The core
already bundles SQLite and vendors libgit2, which helps static builds;
`fuser` doesn't link libfuse. Tree-sitter grammars are C, so a musl C
toolchain is needed; zigbuild handles that. Two things don't build
static as they are: `git2`'s `https` feature links the system OpenSSL
(`openssl-sys` is in Cargo.lock, `openssl-src` isn't), so the server
crate enables `git2/vendored-openssl` or drops `https`/`ssh` (the server
only receives pushes; it needs neither unless it pushes to the user's
remote); and `eoka` wants a Chrome on the host at run time, not at build
time, which is fine. `trash`, `portable-pty`, `tauri-plugin-*` and the
LSP downloader stay out of the server crate.

---

## 8. Summary of options

| | Effort | Survives disconnect | Attack surface | Future web/mobile | Verdict |
|---|---|---|---|---|---|
| **T1** SSH stdio | Low | Only with a daemon behind it | Minimal (SSH) | No | Fine for a prototype |
| **T2** Axum public HTTPS | High | Yes | Public port | Yes | Later, as an opt-in mode |
| **T3** Axum on localhost + SSH tunnel | Medium | Yes | Minimal (SSH) | Switchable to T2 | **Recommended** |

| | Effort | Offline-friendly | Reuses existing code | Verdict |
|---|---|---|---|---|
| **P1** git push of a snapshot up / result ref down, worktree per chat on the server | Medium | Yes | libgit2 + landing | **Recommended** |
| **P2** continuous two-way sync | High | Yes | Little | No |
| **P3** remote-first project | Very high | No | Little | Long-term option |

| | Effort | Conflicts | Verdict |
|---|---|---|---|
| **M1** move ownership at a turn boundary, local mirror follows | Medium | None (one owner) | **Recommended** |
| **M2** server owns all chats | High | None | No (breaks offline) |
| **M3** replicate the database | Very high | Many | No |

---

## 9. Suggested phases

1. **Core extraction (no user-visible change).** `Host` trait over
   `AppHandle<R>` first, then the plain `Core` + `EventSink`; the
   set-once statics into `Core::new`; workspace split last, once nothing
   in the core names `tauri`; `rovik-headless` on top of `rovik-core`
   without the mock runtime, same CLI. The existing e2e tests keep
   passing at every step; they're the safety net.
2. **Protocol + local daemon.** `rovik-protocol` enum as one message
   stream; `rovik-agentd serve` (Axum on a Unix socket/localhost) and
   `attach --stdio`; a frontend `backend` abstraction over
   `invoke`/`listen` that routes by chat location. Test by running the
   daemon locally and pointing a chat at it over the Unix socket.
3. **Chat bundle export/import** + `dialogs.location` + the mirror +
   "stop after this tool call". Testable locally (move to the local
   daemon, then back).
4. **SSH layer:** host settings page, keychain storage, connection test,
   deploy/upgrade, tunnel (russh, with the system-ssh option). Move API
   keys to the keychain.
5. **Project transport (P1):** snapshot commit + libgit2 push to the
   server's bare repository; worktree per chat; result ref fetched and
   landed locally; "sync now" before user messages; shadow repository
   for non-git projects.
6. **Optional:** public HTTPS mode (pinned self-signed cert or ACME),
   remote terminal, overlay over the worktree where FUSE exists, then
   remote-first projects (P3).

## 10. Challenges to expect

Things that will bite, in rough order of how early:

1. **The test harness rewrite is the real cost of phase 1.** Three e2e
   files of ~20k lines construct a mock Tauri app and read states from
   it. Do the `Host` trait step first so the harness changes once, at
   the end, not with every call-site change.
2. **Event routing on the frontend.** `agentRuns.ts` listens once to
   `agent-event` and assumes one backend. With two backends, an event's
   `dialog_id` must be enough to tell where it came from (it is, with
   `location`), and events from a remote chat must never be treated as
   local-file changes (`FileWritten` → `notifyFileWritten` refreshes
   editor tabs by `abs_path`; map or drop them).
3. **A chat that moved while a subagent was running.** Subagents are
   runs in `AgentState` with their own child dialogs; the safe point
   must wait for the whole group (`task` calls in flight) or move the
   children too. The export must include children (`parent_id`) and
   their overlay/worktree state, which for v1 means: don't move during a
   `task` call; stop after it.
4. **Two writers by accident.** After a failed move (upload succeeded,
   the local `location` flip didn't, or the reverse) both sides think
   they own the chat. Make the flip the last step, idempotent, and
   record the move in the bundle (a `move_id`) so the daemon rejects a
   second import and the client can ask "who owns this?" on start.
5. **Daemon restarts lose keys and approvals.** A reboot, an upgrade or
   an OOM kill drops in-memory keys and pending approvals; runs resume
   only when a client connects. The status "waiting for a client" and a
   `Resume` on reconnect cover it, but the user must see it.
6. **Version skew.** The desktop updates on its own schedule; the
   daemon updates when the desktop deploys it. The handshake carries a
   protocol version, the daemon drains (finishes current tool calls)
   before upgrading, and the client refuses to move chats to an older
   daemon.
7. **Approvals that nobody answers.** See §5: a run waiting hours for an
   approval looks like "the cloud doesn't work". The move dialog must
   say what will wait.
8. **Clock and locale.** `local_date` comes from the client (good);
   `created_at` and `seq` are the owner's; a server with a skewed clock
   produces out-of-order `created_at` in the mirror. Order by `seq`,
   never by time.
9. **Disk growth on the server.** Worktrees with build output, bare
   repositories with every snapshot, attachments. A `gc` on chat delete
   and a "server storage" line in the host settings (du of `~/.rovik`)
   from day one.
10. **Windows clients.** No `$XDG_RUNTIME_DIR`, paths with drive
    letters in `projects.path`, line endings in snapshot commits
    (`core.autocrlf` must be off for the shadow repository), ssh-agent
    is a named pipe. Test early with one Windows laptop.
11. **The server's egress.** The provider's API, GitHub for toolchains,
    npm/crates registries: a locked-down VPS fails in the middle of a
    run, not at the connection test, unless the test checks the
    provider host (§7).
12. **Trust.** Moving a chat sends the provider API keys to a machine
    the user administers; the settings page should say so plainly, and
    "keep API keys on server" stays off by default.
13. **Head-of-line blocking on one tunnel.** The first push of a big
    history shares the SSH connection with the event stream only if
    both go through one SSH session; open a second session (or a second
    channel) for git, so the UI stays live.

Open questions for you:

- Single-user servers only (one daemon per SSH user), or a shared server
  for a team? Multi-user changes auth, data separation and sandboxing.
- Should the server run the agent's commands inside a container (Docker
  or podman) per project for isolation and reproducible toolchains?
  Recommended if the server is shared or long-lived.
- Non-git projects: the shadow repository (§3.2) makes them work without
  the user noticing; is it acceptable that a cloud chat on a non-git
  project costs a private git repository's worth of disk in app data?
- How much undo does a remote chat need in v1? The worktree mode (§3.1)
  gives landing, conflicts and undo-of-landing locally, but not the
  per-group undo an overlay has while the chat is going.
