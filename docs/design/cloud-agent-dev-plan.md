# Running the agent in the cloud: development plan

This plan builds what [`cloud-agent-plan.md`](cloud-agent-plan.md) decides:
a person adds a server in Settings, the IDE deploys the agent there, and any
chat can move to that server and back, with its history, its project and
its settings, and keep running while the laptop sleeps.

Read `cloud-agent-plan.md` first for the options and the reasons behind
them. This file says what to build, in which crates and files, in which
order, and how each step is tested. It also records the decisions taken
on 2026-10-14 after the plan was checked against the code (section 1), and
where that check found the plan incomplete (section 10).

Contents:

1. [Decisions](#1-decisions)
2. [What the code does today](#2-what-the-code-does-today)
3. [The shape of the system](#3-the-shape-of-the-system)
4. [The protocol](#4-the-protocol)
5. [The server](#5-the-server)
6. [How each flow works](#6-how-each-flow-works)
7. [Milestones](#7-milestones)
8. [Implementation order for an AI agent](#8-implementation-order-for-an-ai-agent)
9. [Testing](#9-testing)
10. [Gaps found in review](#10-gaps-found-in-review)
11. [Risks and known limits](#11-risks-and-known-limits)
12. [Changes to the design document](#12-changes-to-the-design-document)

---

## 1. Decisions

Decisions marked _(new)_ were taken on 2026-10-14, after the design was
checked against the code. The others restate the design document's
decisions in the form this plan builds them, or follow from them.

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Why                                                                                                                                                                                                                                                                                                                                                                                   |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | **A Cargo workspace under `src-tauri/`.** The app stays the root package (`rovik-ide`), so the Tauri CLI and `tauri.conf.json` don't move. Members: `crates/rovik-protocol`, `crates/rovik-core`, `crates/rovik-client`, `crates/rovik-agentd`, `crates/rovik-headless`.                                                                                                                                                                                                                   | The design's split (§2.1), plus two crates. `rovik-client` is the app's half of the protocol (connection, mirror, project transport) without Tauri, so it can be tested against a real daemon process. `rovik-headless` keeps the evaluation runner out of the core.                                                                                                                  |
| D2  | **`Core` replaces `AppHandle<R>` in all core code.** `Core` holds `Paths` (data folder and config folder), `DbState`, `AgentState`, `VfsState`, `BrowserState` and an `Arc<dyn EventSink>`. Tasks are spawned through `rovik_core::rt`, over one tokio runtime per process. The process-wide statics stay as they are.                                                                                                                                                                     | The design's §2.2, plus two things the code needs. Every run reads `project-settings.json` from the **config** folder (K03). `tauri::async_runtime` can spawn from any thread, but `tokio::spawn` can't (K04).                                                                                                                                                                        |
| D3  | **One protocol, one dispatcher.** `rovik-protocol` defines `Request`, `Response`, `Event` and the frames they travel in. `rovik_core::dispatch(&Core, Request)` serves both the daemon and the app's own local calls, so the list of operations a chat can do exists once. HTTP is used only for `/health`, attachments and git packs.                                                                                                                                                     | §2.3.                                                                                                                                                                                                                                                                                                                                                                                 |
| D4  | **Routing in the Rust command layer** _(new)_. The Tauri commands the frontend already calls look up where the chat is and send a remote chat's call to its host. Remote events come back as the same `agent-event`. `chatApi.ts`, `agentRuns.ts` and `overlay.ts` don't change; the frontend work is new interface.                                                                                                                                                                       | The webview's CSP allows connections only to IPC (K06), so the SSH tunnel and the protocol client have to live in Rust. Routing next to them keeps one place that knows where a chat runs. This replaces the frontend `backend` module of §2.3.                                                                                                                                       |
| D5  | **Transport T3, over a Unix socket.** The daemon listens on `~/.rovik/agentd.sock` (mode 0600, and the peer's uid is checked). The client opens one `direct-streamlocal@openssh.com` channel per WebSocket or HTTP connection. Where the server doesn't allow forwarding, the client instead runs `rovik-agentd attach --stdio` on an exec channel, a plain byte bridge to the socket. Every call must carry a token, and **only its hash is stored on the server**. There is no TCP port. | Team hosts (D10): a socket in the user's own 0700 folder can't be reached by other users and can't clash with their ports. A byte bridge carries WebSocket and HTTP the same way over both channel kinds. Because the token itself never touches the server's disk, the agent's own commands, which run as the same user, can't connect to the daemon and approve their own requests. |
| D6  | **Git carries the project (P1), as packs through the tunnel** _(new)_. The client builds a pack of the objects the server lacks (`git2::PackBuilder`) and posts it to the daemon. Results come back the same way. The client uses one SSH library, russh.                                                                                                                                                                                                                                  | There is no libssh2 next to russh, so there is one place for authentication and one pinned host key. The server needs no `git-receive-pack`. The whole flow can be tested against a local daemon, without SSH. This replaces "libgit2 over SSH" in §3.2 and §6.                                                                                                                       |
| D7  | **Every project gets a shadow repository** _(new)_ at `<app data>/shadow/<project id>.git`. Its work tree is the project folder (read, never checked out into), and it has its own persistent index. For a git project it borrows the project's objects through `objects/info/alternates`. Nothing is ever written to the project's `.git`.                                                                                                                                                | No `refs/rovik/*` show up in the user's repository, and its gc, hooks and Git panel are unaffected. The index's stat cache makes every snapshot after the first cheap. Git and non-git projects share one code path.                                                                                                                                                                  |
| D8  | **On the server, each chat works in a git worktree of its own** (§3.1), as a direct-mode run: `isolate: false`, with the worktree as its root. Git commands that change the repository are refused, as they are in isolated chats today. Commands that name `~/.rovik` always need approval.                                                                                                                                                                                               | The worktree is the chat's private copy. Without the refusal, a `git checkout` would become part of the chat's result.                                                                                                                                                                                                                                                                |
| D9  | **Ownership moves at a turn boundary (M1)**, at a new pause of kind `move`: the point where a checkpoint pause happens today. The local rows become a **mirror that follows the owner's stored rows**, using a revision counter (`rev`) and `changes_since`. They don't follow replayed events.                                                                                                                                                                                            | Events can't rebuild the database. `MessagePatched` has no `seq`, text streamed between two checkpoints is never stored, and a ring buffer can't cover a night's sleep (K01).                                                                                                                                                                                                         |
| D10 | **On a shared team server, each person has their own SSH account and runs their own daemon** _(new)_. Everything the daemon keeps is in `~/.rovik` (0700), its temporary files included (`TMPDIR=~/.rovik/tmp`). An admin can install the binary once for everyone and set host-wide limits.                                                                                                                                                                                               | Unix users keep people's chats, keys and worktrees apart. This is the single-user design plus a socket, limits and documentation for admins.                                                                                                                                                                                                                                          |
| D11 | **Provider keys are stored on the server** (decided in §6), in `~/.rovik/keys.json`, and refreshed by every start.                                                                                                                                                                                                                                                                                                                                                                         | As decided.                                                                                                                                                                                                                                                                                                                                                                           |
| D12 | **A remote result lands through the existing landing code.** The client imports the result commit into the chat's local overlay store, with base copies taken from the snapshot, and needs no mount for it. From there, landing, conflicts, the journal, undo and the isolation bar work as they do today.                                                                                                                                                                                 | An overlay store can be read and landed without its view mounted (`overlay::chats::changes` and `land_now_with` open it with `OverlayFs::open`). The client doesn't need a second landing path.                                                                                                                                                                                       |
| D13 | **Merges with the project's newer changes are checked on the server.** If the local project changed a file the chat also changed, the client pushes a fresh snapshot, the daemon merges it in and runs the project's checks (`WorkRecheck`), and only a result that passes lands.                                                                                                                                                                                                          | A local isolated chat never lands an unchecked merge (`landing.rs`: `take_project_changes`, `check_merged`), and the toolchain is on the server.                                                                                                                                                                                                                                      |
| D14 | **The project syncs before each message.** If the merge is clean, the worktree gets the user's newer files. If it conflicts, the existing _chat resolves conflicts_ setting decides: either the markers go to the chat, or the sync waits and the chat carries on with its older copy. A sync is all or nothing, so the chat's landing base stays correct.                                                                                                                                 | These are the rules the user already set for local isolated chats. A sync that applied only some files would need a separate base for each file.                                                                                                                                                                                                                                      |
| D15 | **Daemon binaries come from a release workflow.** A new GitHub Actions workflow (there is no CI today) builds `x86_64-` and `aarch64-unknown-linux-musl` with `cargo zigbuild` and signs them. The app downloads the build for its own version and checks the signature against a public key compiled into it. `ROVIK_AGENTD_BINARY` points at a local build for development. The daemon builds with a `release-daemon` profile, which keeps unwinding on panic.                           | People who build the desktop app from source can still deploy. Because the key is compiled in, the check doesn't depend on which was built first, the app or the daemon. With the app's `panic = "abort"`, one run's panic would end every other run on the daemon.                                                                                                                   |
| D16 | **Servers run Linux on x86_64 or aarch64. Clients run Linux, macOS or Windows.** A daemon serves the chats each of its person's computers moved there; a computer doesn't see another computer's chats in v1.                                                                                                                                                                                                                                                                              | §7. Adopting another computer's chats is the "every device sees every chat" design (M2) that wasn't chosen.                                                                                                                                                                                                                                                                           |

---

## 2. What the code does today

These facts were checked at commit `e8abd81`. The plan relies on them.
Where one contradicts the design document, section 10 says so and names
the task that handles it.

**Backend (`src-tauri/src`, about 114,000 lines in one crate, 152 commands in `lib.rs`)**

- **How core code depends on Tauri:**
  - `AppHandle<R>` and `R: Runtime` appear in 23 non-test files of core code, 18 of them in `agent_run/`.
  - `tauri::async_runtime::spawn_blocking` is called 64 times, `spawn` 6 times, `block_on` twice.
  - Commands take `tauri::State`; `send_chat_message`, `vfs_subscribe`, `lsp_*` and `spawn_terminal` take `tauri::ipc::Channel`.
  - Events go out through one function, `agent_run::events::emit`.
  - Non-test code calls `app.state::<…>()` only about 20 times, because `Run` keeps `AgentState` and `DbState` as fields. The e2e tests reach the mock app about 170 more times.
- **Events:**
  - `AgentEvent` has 19 variants.
  - `FileWritten`, `ReasoningLearned`, `ApiRouteLearned` and `Notice` carry no dialog id.
  - What the design calls `MessageStored` is `MessagesAdded` plus `MessagePatched`.
- **How replies are stored:** a reply in progress is written to the database every 2 seconds (`run.rs`, `CHECKPOINT_EVERY`) and finalized at the end. The `MessagePatched` events sent between two checkpoints are never stored. Messages are deleted only together with their chat. No table has a revision or an `updated_at` per message (`chat_db/schema.rs`, version 9).
- **Project settings:** every run reads the project's saved settings, such as corrected check commands, from `project-settings.json` in the app's **config** folder. They are keyed by the project's local path (`agent_run/project.rs::load` → `project_profile::load_project_settings`).
- **Imports from core into app modules:**
  - `git_sync` is used only for its `ConflictFile` type (`overlay/chats.rs`), so `git_sync.rs` and `git.rs` can stay in the app.
  - `filesystem::write_app_file` is used by `agent_run/plan.rs`.
  - `lsp::rust_analyzer::parse_assets` is used by `vfs/driver.rs`.
- **Isolated chats:** `ChatOverlay::command_problem` refuses git commands that change the repository. Where commands can't be confined, it also refuses commands that name the project's path.
- **Landing:**
  - When an isolated run ends, `landing.rs` merges the project's newer changes into the chat's view, runs the checks again, and only then lands.
  - It lands without asking only after the outcomes `done`, `verified`, `unverified` and `verify-unverified`.
  - An overlay's saved store can be listed and landed without being mounted.
- **Pauses:** a checkpoint pause happens once every tool call of a turn has its result. It stores `meta.paused = { kind, toolCalls }` (`run.rs`; `guard.rs::PauseInfo`), and `ContinueRun` continues counting from there.
- **Interrupted runs:** `StartKind::Resume` reruns unanswered tool calls, and an interrupted call is always confirmed by the user first (`execute.rs`). `ContinueReply` continues a stored partial reply.
- **CLM:** it needs the virtual-files mount (`clm::availability`). Without the mount, each request falls back to trimming (`agent.rs`).
- **Panics:** `agent_start` runs the run as a task inside a task, so a panic still releases the chat. The release profile's `panic = "abort"` makes that useless in release builds.
- **Test harness:** the e2e harness builds a mock Tauri app in one function (`agent_run/e2e_tests.rs::harness`), and `headless.rs` builds the same mock. The eval script builds the runner with `cargo build --features headless --bin rovik-headless` (`scripts/eval/eval.mjs`).
- **Statics:** the overlays' data folder and other caches are process-wide statics (`overlay::chats::DATA_DIR` and others). Two `Core`s with different data folders can't share one process (K18).
- **git2 and TLS:**
  - git2 0.21 has no default features. The core uses only local operations (`merge_file`, `Patch`, `discover`).
  - `PackBuilder::insert_walk`, `Odb::packwriter`, `Indexer` and `Repository::worktree` exist.
  - git2's `https` and `ssh` features pull in `openssl-sys`, and reqwest's rustls pulls in `aws-lc-rs`.
- **Helpers that already exist:**
  - `process::adopt_login_shell_path`: a desktop launcher's bare PATH is a systemd unit's problem too.
  - `atomic_file::write_atomic_private` writes files with mode 0600.
  - `api_log` never writes a key, but by default it writes to `/tmp/rovik-api-log.jsonl`.
  - The IDE writes `.rovik/.gitignore` with `project.json` in it, and plans go to `.rovik/plans/` (`agent_run/plan.rs::PLANS_DIR`).
- **CI:** there is none; `.github` doesn't exist.

**Frontend (`src/`)**

- **Backend calls:**
  - There are 152 `invoke` calls in 24 files and 3 `listen` calls, with no central wrapper.
  - The calls about chats, runs, overlays and CLM are in `chatApi.ts` (22), `overlay.ts` (11), `contextMode.ts` (7), `agentRuns.ts` (5, plus the one `agent-event` listener), `checklist.ts` (2), `approvals.ts`, `questions.ts`, `aiChat.ts` (`chat_context_estimate`) and `ChatPanel.tsx` (`workload_focus`).
- **`send_chat_message`** is used only for chat titles, with explicit messages and no `dialogId` (`oneShot.ts`). No streaming call has to go remote.
- **CSP:** `connect-src` is `'self' ipc: http://ipc.localhost` (`tauri.conf.json`).
- **Starting runs:**
  - `beginRun` (`ChatPanel.tsx`) flushes the chat store's writes before `agent_start`, because "the backend reads the chat from the database".
  - ChatPanel resumes by itself any chat whose last tool calls have no results (a `createEffect` that calls `beginRun({ kind: 'resume' })`).
- **Local paths:**
  - The editor context sent with a message names the active tab's absolute local path (`activeContext.ts::buildContextHeader`).
  - `fileWritten.absPath` and `approval.edit.absPath` are matched against local paths, and `meta.planFile` is read from the local disk.
  - `ClmStatus.realPath` is the local mount.
- **Indicators:** the chat list shows only running and unread. Nothing shows that a chat waits for an approval or an answer.
- **`agent_configure`** is sent only while ChatPanel is mounted. Every `StartRequest` carries the configuration anyway.
- **Tests** use `node:test`, with `invoke` mocked through `window.__TAURI_INTERNALS__` (`agentRuns.test.ts`).

---

## 3. The shape of the system

```
 Desktop app (one per computer)                     Server (one daemon per person's account)
 ──────────────────────────────                     ────────────────────────────────────────
 webview: invoke / listen('agent-event')            rovik-agentd serve        (systemd --user)
    │                   ▲                             Axum on ~/.rovik/agentd.sock (0600)
 Tauri commands (app crate)                             /rpc     WebSocket: the frame stream
   route(chat) ──► local:  rovik_core::dispatch          /packs   git packs, up and down
             └───► remote: rovik_client::Host ──┐        /blobs   attachments (screenshots)
                     ├ mirror → local chats.db   │        /health
                     ├ shadow repo, packs        │     rovik_core::Core  (~/.rovik/data)
                     └ events → 'agent-event'    │     ~/.rovik/projects/<project>.git  (bare)
                                                 │     ~/.rovik/work/<chat>/            (worktrees)
       russh, one connection per host:           │
       a direct-streamlocal channel per socket ──┴─SSH─► ~/.rovik/agentd.sock
       connection (or exec: agentd attach --stdio)
```

- **The webview** doesn't change how it talks to the backend. It calls the
  same commands and listens to the same `agent-event`. New interface
  (hosts, the move dialog, badges) uses new commands and a new
  `cloud-event` for connection and transfer progress.
- **The app crate** routes each chat-scoped command by the chat's location.
  `Hosts` (managed state) holds the connections and a cache of
  `dialog → location`; a subagent's chat goes with its root chat.
- **`rovik-client`** owns the SSH connection, the protocol, the mirror, the
  shadow repositories and the packs. It writes the mirror into the app's
  `Core` database. It re-emits a remote chat's events, changing what
  section 6.1 lists.
- **`rovik-agentd`** is a `Core` behind Axum, plus what only a server needs:
  the key store, the per-chat run settings, the projects' bare
  repositories, the worktrees, and restarting runs after a crash.
- **`rovik-core`** is today's agent, with no Tauri in it.

---

## 4. The protocol

`rovik-protocol` has serde types only; it depends on nothing heavier than
`serde` and `serde_json`. The core depends on it for `AgentEvent`,
`StartRequest` and the other shared types, which move there from
`agent_run`.

### 4.1 Frames

One WebSocket at `GET /rpc` carries JSON text frames in both directions:

```rust
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Frame {
    /// Client → daemon, first frame. `token` is the device's token (D5).
    Hello { protocol: u32, client: String, device: String, token: String },
    /// Daemon → client. `boot` changes on every daemon start, so a client
    /// knows its `since` is from another process.
    Welcome { protocol: u32, min_protocol: u32, daemon: String, boot: String, features: Vec<String> },
    Call { id: u64, req: Request },
    Reply { id: u64, res: Result<Response, Fail> },
    /// After `Subscribe`: every event, numbered per daemon process.
    Event { seq: u64, ev: Event },
    Subscribe { since: Option<(String, u64)> }, // (boot, seq)
    Subscribed { replayed: u64, complete: bool },
    Ping, Pong,
}

pub struct Fail { pub code: FailCode, pub message: String }
pub enum FailCode { Unreachable, NotOwner, Busy, Version, BadRequest, Internal }
```

A frame is at most 16 MB. Anything larger, such as attachments or packs,
goes over HTTP. The app turns each `Fail` into the `String` error its
command returns today. `Unreachable` reads: "<host> isn't reachable. This
chat is read-only until it is."

### 4.2 Requests

| Group | Requests                                                                                                                                                                                                                                        | Serves                                                                                                                                         |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Agent | `AgentStart(StartRequest)`, `AgentStop`, `AgentPause` (new, M03), `AgentResolveApproval`, `AgentAnswerQuestions`, `AgentStatus`, `AgentConfigure(AgentConfig)`, `AgentSetEffort`, `ChatChecklist`, `ChatDismissChecklist`, `WorkloadFocus`      | The commands of the same names. `StartRequest` gains `project_settings: Option<Value>` (K03) and the start kind `CheckMerged { files }` (D13). |
| Chat  | `ChatChangesSince { dialog_id, rev }` → `ChangeSet`, `ChatRename`, `ChatUpdateMessage`, `ChatMessagesAnswered`, `ChatSetModel`, `ChatDelete`, `ChatImport { bundle, move_id }`, `ChatOwner { dialog_id }`, `ChatRelease { dialog_id, move_id }` | The mirror (6.3), writes to a remote chat, the move (6.8).                                                                                     |
| CLM   | `ClmStatus`, `ClmSetMode`, `ClmReset`, `ClmListRevisions`, `ClmReadRevision`, `ClmRestoreRevision`                                                                                                                                              | The `clm_*` commands. `ClmStatus.real_path` is dropped for remote chats.                                                                       |
| Work  | `ProjectEnsure { project, settings }`, `WorkStatus`, `WorkFile { path }`, `WorkSync { snapshot }`, `WorkRecheck { snapshot }`, `WorkResult`, `WorkDiscard`, `WorkConflictsToChat { files }`                                                     | The project transport (6.4–6.7) and the isolation bar's remote half.                                                                           |
| Host  | `HostInfo` (versions, platform, virtual files, Chrome, user namespaces, linger, other users' daemons), `KeysList` (provider ids and base URLs, never keys), `KeysRemove`, `Storage`, `Gc`, `Drain`                                              | The hosts page, deploy and upgrade.                                                                                                            |

On both sides `rovik_core::dispatch` answers the Agent, Chat and CLM
groups. It refuses a request for a chat that isn't owned here: on the
desktop that is a chat with a `location` (K21), and on the daemon a chat
it released. Reading changes stays allowed, for the final catch-up. The
Work and Host groups are the daemon's own; the core can't answer them.

### 4.3 Events

```rust
pub enum Event {
    /// Today's enum, moved here. `FileWritten` and `Notice` gain
    /// `dialog_id: Option<String>` (K02).
    Agent(AgentEvent),
    /// Rows of the chat changed; the mirror pulls `ChatChangesSince`.
    ChatChanged { dialog_id: String, rev: i64 },
    /// A run ended and its result is committed (6.5).
    ResultReady { dialog_id: String, commit: String, outcome: String, pending: Option<String> },
    /// A sync's state, for the isolation bar (6.7).
    SyncState { dialog_id: String, state: SyncState, files: Vec<String> },
    HostNotice { kind: String, message: String },
}
```

The daemon keeps the last 2,000 events or 8 MB, whichever is less. When a
client subscribes with `(boot, seq)` from this process, it gets the events
it missed (`complete: true`). Otherwise it gets `complete: false` and
catches up from the database: `AgentStatus` for what is running and
waiting, and `ChatChangesSince` for every chat it mirrors. This follows
the rule in `events.rs`: "the database stays the source of truth for
anything it misses".

### 4.4 HTTP

| Endpoint                                               | Purpose                                                                                                                   |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------- |
| `GET /health`                                          | No auth. Returns `{ ok, protocol, version }`.                                                                             |
| `GET /rpc`                                             | The WebSocket of 4.1.                                                                                                     |
| `GET` and `PUT /blobs/<sha256>`                        | Attachments, addressed by content as in `chat_db/attachments.rs`. A `PUT` whose body doesn't hash to its name is refused. |
| `GET /packs/<project>/refs`                            | The tips the daemon has for the project: snapshots, results, branches.                                                    |
| `POST /packs/<project>?ref=<name>&old=<oid>&new=<oid>` | A pack for the daemon to index. The ref moves only if the new commit is complete and the ref still points at `old`.       |
| `POST /packs/<project>/fetch`                          | A JSON body of wants and haves; the response is a pack.                                                                   |

Every request except `/health` carries `Authorization: Bearer <token>`.

### 4.5 Versions

`PROTOCOL` starts at 1. Additive changes keep the number: new optional
fields, and new requests the client checks for in `Welcome.features`.
Anything else increases it. A daemon refuses a client older than its
`min_protocol`. The client refuses to move a chat to a daemon with a lower
protocol than its own and offers to upgrade it. When the daemon is newer
and still compatible, nothing needs to happen.

---

## 5. The server

### 5.1 Layout

```
~/.rovik/                     0700, owned by the user; the daemon refuses anything looser
  bin/rovik-agentd            or the admin's /opt/rovik/bin/rovik-agentd-<version> (5.5)
  agentd.sock                 0600
  agentd.lock                 one daemon per account
  config.toml                 token hashes (one per device), limits, log level
  state.json                  per chat: project, worktree, landing base, last start's settings
                              (no keys), running, restarts, move id, released;
                              and the last AgentConfig (no keys)
  keys.json                   0600; created by the first cloud start (D11)
  data/                       the daemon's Core: chats.db, attachments/, agent-log.jsonl, backups/
  config/project-settings.json  keyed by worktree path (K03)
  projects/<project id>.git   bare repository per project
  work/<chat id>/             worktree per chat
  results/<chat id>.index     the index results are committed through (6.5)
  tmp/                        TMPDIR for the daemon and the agent's commands (K13)
  logs/agentd.log             rotated at 5 MB, 3 kept
```

### 5.2 Start-up of `rovik-agentd serve`

1. Set `umask 077`. Call `process::adopt_login_shell_path()`. When `$SHELL`
   is unset, which systemd user units often are, take the shell from the
   passwd entry (K12). This runs before the runtime exists, because it
   sets an environment variable.
2. Check that `~/.rovik` is the user's own with mode 0700, and stop with a
   clear message if it isn't. Take `agentd.lock`.
3. Set `TMPDIR=~/.rovik/tmp`.
4. Build a multi-thread runtime, call `rt::init`, then `Core::new(paths, sink)`.
5. If `/dev/fuse` works, mount the virtual-files volume, so CLM chats can
   use it.
6. Restart interrupted runs (5.4).
7. Bind the socket. A stale socket file is removed only after connecting
   to it fails. Then serve.

### 5.3 Running as a service

Deploy writes a systemd user unit:

```ini
[Unit]
Description=Rovik agent daemon

[Service]
ExecStart=%h/.rovik/bin/rovik-agentd serve
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Then it runs `loginctl enable-linger`, so the daemon runs after logout and
comes back after a reboot. Some distributions let only an administrator
enable lingering; the connection test says so and names the command for
them. Without systemd, the client starts the daemon with `setsid -f` when
it connects and finds none running. Runs then come back after a reboot
only at the next connection.

### 5.4 Restarting interrupted runs

For each chat in `state.json` marked `running`, the daemon looks at its
stored rows and picks the start kind:

| Last stored state                                                | What the daemon does                                                                                                                                                                                                                            |
| ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A reply that isn't finalized: the process died while it streamed | Settles it the way `run.rs::store_failed_turn` settles a failed turn: with text, it becomes an interrupted reply; empty, it is marked failed. Then it starts `ContinueReply` or `Retry`, the choice the chat's Continue and Retry buttons make. |
| A finalized reply whose tool calls have no results               | `Resume`, which asks the user before running an interrupted call again                                                                                                                                                                          |
| A `paused` record                                                | Nothing (the run had paused)                                                                                                                                                                                                                    |

Each start is rebuilt from the chat's saved settings and `keys.json`. A
chat restarted twice within an hour isn't restarted a third time: it gets
a notice ("stopped after the server restarted twice") and waits for the
user. Approvals and questions that were pending are asked again by
`Resume`.

### 5.5 Team hosts (D10)

- Each user's daemon is independent, and nothing under `~/.rovik` is
  shared.
- **Shared binary:** if `/opt/rovik/bin/rovik-agentd-<version>` exists, is
  owned by root, can't be written by others and passes the signature
  check, the unit runs it and nothing is uploaded.
- **Admin limits:** `/etc/rovik/agentd.toml`, owned by the admin, can set
  `max_slots`, `max_worktrees` and `max_disk_gb`. It overrides the user's
  settings.
- **Heavy-work slots:** they default to 1 while other users' daemons run
  on the host (found through `/proc`). Otherwise the existing automatic
  rule applies.
- **CPU and memory caps** are the admin's, through systemd drop-ins on
  user slices. They are documented, not managed by the app.
- **What can read what:** the agent's commands run as the user and can
  read whatever the user can. Worktrees and keys sit under `~/.rovik`
  (0700), so other users can't read them, but root can. The hosts page
  says: "Your API keys are stored in your account on this server. The
  agent's commands there and the server's administrators can read them."

---

## 6. How each flow works

### 6.1 Routing

In the app crate, every chat-scoped command is one line of routing:

```rust
#[tauri::command]
async fn agent_stop(core: State<'_, Core>, hosts: State<'_, Hosts>, dialog_id: String)
    -> Result<bool, String> {
    route(&core, &hosts, &dialog_id, Request::AgentStop { dialog_id: dialog_id.clone() })
        .await?.into_bool()
}
```

`route` reads the chat's location from the `Hosts` cache, which is filled
from `dialogs.location`. A local chat goes to `rovik_core::dispatch`, a
remote one to its host's connection. For a remote chat each command does
the following:

| Command                                                                                                                                                                                      | For a remote chat                                                                                                                                                                                                                                                                                             |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `agent_start`                                                                                                                                                                                | Syncs if needed (6.7). In `current_user_content` and the message's `meta.sentContent`, replaces the local project root with nothing, so the editor context names `src/x.rs`, not `/home/me/p/src/x.rs` (K08). Adds the project's settings. Applies the host's approval policy (6.9). Then sends `AgentStart`. |
| `agent_stop`, `agent_resolve_approval`, `agent_answer_questions`, `chat_dismiss_checklist`, `chat_rename_dialog`, `chat_update_message`, `chat_messages_answered`, `chat_set_model`, `clm_*` | Forwarded to the owner; the mirror follows.                                                                                                                                                                                                                                                                   |
| `agent_status`                                                                                                                                                                               | The local status plus each connected host's.                                                                                                                                                                                                                                                                  |
| `agent_configure`, `agent_set_effort`                                                                                                                                                        | Applied locally and sent to every connected host. `agent_configure` goes without keys, and the daemon fills them in (D11).                                                                                                                                                                                    |
| `chat_get_messages*`, `chat_get_message_body`, `chat_list_dialogs`, `chat_search`, `chat_checklist`, `chat_context_estimate`, `chat_get_model`                                               | Read from the local mirror.                                                                                                                                                                                                                                                                                   |
| `chat_save_composer_draft`, `chat_get_composer_draft`, `chat_create_dialog`                                                                                                                  | Local only. Drafts belong to the device, and new chats start local.                                                                                                                                                                                                                                           |
| `chat_delete_dialog`                                                                                                                                                                         | The owner stops the run and deletes the worktree and the rows. The local rows go at once. If the host is unreachable, a tombstone is sent when it reconnects.                                                                                                                                                 |
| `overlay_status`                                                                                                                                                                             | The imported result's status (6.5) combined with `WorkStatus`: the files the chat changed in its worktree so far, and the sync state.                                                                                                                                                                         |
| `overlay_file`                                                                                                                                                                               | `WorkFile` gives the base and the chat's version; the project's version is read locally.                                                                                                                                                                                                                      |
| `overlay_land`, `overlay_undo`, `overlay_conflict_file`, `overlay_conflict_resolve`                                                                                                          | Local, on the imported result.                                                                                                                                                                                                                                                                                |
| `overlay_discard`                                                                                                                                                                            | `WorkDiscard` on the owner, and the local import is dropped.                                                                                                                                                                                                                                                  |
| `overlay_conflicts_to_chat`                                                                                                                                                                  | `WorkConflictsToChat`, with the local project's versions of the files.                                                                                                                                                                                                                                        |
| `overlay_redo_group`, `overlay_undo_merge`, `overlay_unmark`                                                                                                                                 | Refused for cloud chats in v1, with the reason.                                                                                                                                                                                                                                                               |
| `chat_read_plan` (new, U04)                                                                                                                                                                  | `WorkFile` on `.rovik/plans/…`. A local chat reads the file, as `readFileContent` does today.                                                                                                                                                                                                                 |
| `workload_focus`                                                                                                                                                                             | Applied locally and on the owner.                                                                                                                                                                                                                                                                             |

The client re-emits each remote `AgentEvent` on `agent-event` in the order
it came, with these exceptions:

- A remote chat's `FileWritten` is dropped. It names a path on the server,
  and the local project changes only when the result lands, which the
  file watcher then sees.
- `Notice` gets the host's name in its title.
- `ApprovalRequested` with an edit gets `remote: true`, so the editor
  shows the change in a compare view and not on the local file's tab
  (K09).
- `ContextUpdated` and CLM statuses lose `real_path`.

When a host connects, the client sends events that bring the frontend up
to date: `runStarted`, `approvalRequested` and `questionsAsked` for the
host's current state, and `runEnded` for runs that ended while it was
away.

### 6.2 A remote start, on the daemon

1. The daemon takes the keys out of the start: the profile, the approval
   profiles and the subagent kinds' models. It writes them to `keys.json`
   under each provider id (the part of the profile id before `::`) with
   the base URL each came with, replacing older entries.
2. It stores the start's settings, without keys, in `state.json`, and
   marks the chat `running`.
3. It sets `root` to the chat's worktree and `isolate` to `Some(false)`,
   and writes `project_settings` under the worktree's path in
   `config/project-settings.json`.
4. It calls `dispatch(AgentStart)`. When `RunEnded` arrives, it clears
   `running` and commits the result (6.5).

### 6.3 The mirror

- **Revisions:** `SCHEMA_V10` adds a `sync_clock` and a `rev` column to
  `dialogs`, `messages`, `clm_state` and `clm_revisions`. Triggers set
  `rev` on every insert and update. Writes to `message_bodies`,
  `message_attachments` and `tool_call_extras` raise the `rev` of the
  message they belong to. Triggers keep this right even for code added
  later that writes with plain SQL.
- **Changes:** `chat_db::sync::changes_since(conn, root, rev)` returns
  every row of the chat's subtree with a higher `rev`: dialogs, messages
  with their full bodies, attachment ids, tool-call extras, CLM state and
  CLM revisions. `apply` writes them as the owner had them: upsert by
  key, the owner's `seq` and `created_at`, search rows and previews
  rebuilt, and new child chats marked with the parent's location. Applying
  the same changes twice changes nothing.
- **When it pulls:** on `ChatChanged`, with 500 ms of debounce, on every
  connection, and on `RunEnded`. The daemon sends `ChatChanged` with its
  current `sync_clock` after any event about a chat (at most twice a
  second per chat) and after each write request it handles. A
  checkpoint write sends no event of its own, but the stream's
  `MessagePatched` events arrive with it, so it is still pulled. A
  streaming reply raises `rev` only at its 2-second checkpoints, so a
  long reply costs one small pull every 2 seconds at most.
- **Where the position is kept:** each chat's position is in
  `mirror_state(dialog_id, host, rev)`, updated in the same transaction
  as the rows.
- **Attachments** missing locally are fetched from `/blobs/<sha256>`
  before their rows are applied.
- **One writer:** `dispatch` refuses local writes to a chat that has a
  location, so the mirror is the only thing that writes its rows (K21).

### 6.4 Snapshots and packs

**Snapshot, on the client.**

- The shadow repository for the project has the project folder as its
  work tree (`core.worktree`). It is never checked out into.
  - For a git project, `objects/info/alternates` names the project's
    object folder (found through `Repository::discover` and `commondir`,
    so linked worktrees work).
  - The project's `info/exclude` is copied into the shadow's before each
    snapshot. The user's global excludes come from their git config, as
    usual.
  - `core.autocrlf` is false, and `core.symlinks` and `core.filemode`
    follow the OS.
- A snapshot updates the shadow index (`add_all`, then `update_all` for
  deletions) and writes its tree.
  - The parent commit is the project's `HEAD` when the project is a
    repository whose `HEAD` can be read. Otherwise it is the chat's
    previous snapshot.
  - The commit goes to `refs/rovik/snapshots/<chat>`.
- **The chat's overlay moves with it** (T09).
  - The chat's start is not the project's tree as it is now. It is that
    tree with every path the chat changed set to its overlay base copy
    (`S0'`). This is the landing base.
  - The chat's changes go in a commit on top of it (`C0`, with the
    overlay's upper files). The worktree checks out `S0'` and then writes
    `C0`'s files, so the agent's `git status` shows the chat's own
    changes.
  - Without this, a file the user edited after the chat did would come
    back without the user's edit.
- **Guards:**
  - Before the first push, the client lists untracked files over 50 MB
    and asks.
  - It also estimates the history pack. Above 1 GB it offers to send the
    snapshot without history (no parent).
  - Nested repositories and submodules aren't carried in v1, and the
    move dialog lists them (section 11).

**Packs.**

- To push, the client asks `GET /packs/<project>/refs`, hides the tips it
  also has locally, walks from the new commit, builds the pack, and
  streams it to `POST /packs/…`.
- The daemon writes the pack with `Odb::packwriter` and checks that the
  new commit and its tree are complete. Only then does it move the ref,
  compare-and-swap on `old`.
- Fetching works the same way in the other direction, into the shadow
  repository.
- Progress goes to `cloud-event`. A cut upload leaves no ref and is
  started over: packs can't be resumed.

**Worktrees, on the daemon.**

- `ProjectEnsure` creates `projects/<id>.git` (bare) and the project's
  row, with the same id as on the desktop.
- Each chat gets branch `rovik/<chat>` at its landing base and a worktree
  at `work/<chat>` (`Repository::worktree`). The daemon writes
  `.rovik/project.json` there from the project row; `.rovik/.gitignore`,
  which the snapshot carries, keeps it out of results.
- The worktree's path is added to `project_paths`, so
  `projects::chat_may_use` accepts it.

### 6.5 Results, and landing them

**On the daemon.**

- When a remote run ends, and on `WorkResult`, the daemon commits the
  worktree as `refs/rovik/chats/<chat>/result`. The commit's parent is
  the landing base, and its tree is the worktree minus ignored files.
- It builds the tree through its own index file
  (`results/<chat>.index`, set on its own `Repository` handle with
  `set_index`). The agent's `git status` in the worktree therefore never
  changes under it, and later results hash only the files that changed.
- `state.json` keeps the outcome and the pending reason. The mapping from
  outcome to reason is taken out of `landing.rs::try_land` into one
  function that both sides use. Then the daemon sends `ResultReady`.

**On the client, while connected.**

- The client fetches the result into the shadow repository. Then
  `overlay::import` builds the chat's local overlay store from the
  landing base tree and the result tree: uppers from the result, base
  copies from the base, `project.txt` set to the local project, and no
  mount.
- After that, everything is today's landing code.
  - It lands by itself under `landing.rs`'s rules: an outcome in
    `LANDS_AFTER`, no conflict markers, no conflicts, and the plan not
    `merged`, and only while the chat's project is the open project.
  - Conflicts go to the isolation bar, as they do now.
  - A `merged` plan with checks on goes to 6.6.
- A result that waits shows in the bar as "Ready to land" until the
  project is opened or the user lands it.

### 6.6 Checking a merge on the server (D13)

1. The client pushes a fresh snapshot `S2` and sends `WorkRecheck { snapshot: S2 }`.
2. The daemon merges `S2` into the worktree, using 6.7's rules.
3. With the merged files, it starts the chat with the new core start
   kind `CheckMerged { files }`. The run sets `merged_from_project` and
   runs `verify_at_end`, as `take_project_changes` does locally.
   - If a check fails, the run goes on as a fix round.
   - If the checks pass, a new result comes back and lands.
4. If the project changed the same files again meanwhile, the client
   tries once more. After two rounds the result waits with "changed
   again", as `LandError::Unchecked` does today.
5. Checks that need approval ask for it like any check: the user is there,
   because landing only happens while they are connected.

### 6.7 Syncing before a message (D14)

1. Before each `Send` to a remote chat, the client takes a snapshot. If
   the tree didn't change since the last sync, it stops here.
2. It pushes the snapshot and calls `WorkSync`. For each path, the daemon
   merges the old base, the new snapshot and the worktree, using
   `merge::merge_bytes`:
   - **Nothing conflicts:** every path is applied. The branch moves to the
     new snapshot and the worktree's index is reset to it (mixed), so
     `git status` still shows exactly the chat's changes. Merged files are
     listed for the bar.
   - **Conflicts, and the chat resolves conflicts:** every path is
     applied, with conflict markers in the files that conflict. The next
     start carries a note listing them. Landing is refused while markers
     remain (`markers_left`, made to work on a list of worktree files).
   - **Conflicts, and the setting is off:** nothing is applied, and the
     base stays where it was. A `SyncState` event tells the bar: "Sync
     waits: N files changed both here and in the cloud chat", with "Hand
     to the chat" and "Keep the older copy".
3. The run goes on in every case.

### 6.8 Moving a chat

```
local ─► preflight ─► pause ─► snapshot+upload ─► blobs ─► import ─► flip ─► start ─► remote
                        (any failure before "flip" leaves the chat local)
```

1. **Preflight** (`cloud_move_preflight`).
   - Checks that the host is connected and its protocol is compatible,
     and that the chat has a project; chats without one can't move in v1.
   - Works out what will wait for the user, from the permissions and the
     host's approval policy (6.9).
   - Notes that CLM becomes trimming if the host has no virtual files.
   - Notes that a parallel group's "Redo sequentially" won't be available
     after the move.
   - Checks that no conflicts are pending in the chat's overlay; they have
     to be resolved first.
2. **Pause.** If a run is going, the client sends `AgentPause` and waits
   for `runEnded` with outcome `paused-move`. While it waits, the dialog
   says what it is waiting for (a command, a subagent) and offers "Stop
   now".
3. **Record the move.** The client writes a `cloud_moves` row: the
   chat, the host, the `move_id` and the step reached.
4. **Snapshot and upload** (6.4), with the overlay folded in.
5. **Attachments** the host doesn't have are uploaded.
6. **Import.** `ChatImport { bundle, move_id }` carries
   `changes_since(0)` of the chat's subtree, the project row, the
   project's settings, the start and base commits and the overlay's
   pending reason. The daemon imports it in one transaction, creates the
   worktree and records `move_id`. A repeated `move_id` answers
   "already imported". A different one for a chat it owns is refused.
7. **Flip**, the last step. On the chat's subtree, the client sets
   `dialogs.location` to `host:<id>` and `mirror_state` to the import's
   `rev`. It removes the chat's local overlay store, whose content is now
   on the server, and deletes the `cloud_moves` row.
8. **Start.** If the run paused for the move, the client sends
   `ContinueRun`. If tool calls have no results, it sends `Resume`.
   Otherwise it starts nothing. The start carries the keys (6.2).

**After a crash during the move.** On start-up, a `cloud_moves` row makes
the client ask the host with `ChatOwner`.

- If the host owns the chat with this `move_id`, the client finishes the
  flip.
- If the host doesn't know the chat, the move is rolled back.

The daemon accepts no writes to an imported chat except through a client
that knows its `move_id`.

**Moving back.**

1. Pause on the daemon (`AgentPause`).
2. A final `ChatChangesSince`.
3. `WorkResult`, then fetch the result and import it as the chat's local
   overlay. The chat's changes now wait as a local isolated chat's
   pending changes. On a computer without the virtual-files driver, an
   isolated run can't use them, so they land first (or show their
   conflicts).
4. `ChatRelease { move_id }`. The daemon marks the chat released, refuses
   further writes, and removes its worktree after 7 days or on "Remove
   now".
5. Flip `location` back, and continue the run locally with `ContinueRun`
   if it paused for the move.

Moving changes no messages. The transcript shows where the chat moved,
because the `move` pause record names the host in its `reason`.

### 6.9 While nobody is there

- A remote run never approves anything by itself.
- Each host has an approval policy, chosen on the hosts page and shown in
  the move dialog:
  - "approvals go to the AI reviewer, then to me", or
  - "approvals wait for me".
- The client applies the policy to the `AgentConfig` it sends that host.
  It is per host rather than per chat because the daemon's configuration
  is global, like the desktop's (`agent_start` sets it from each request),
  and every chat on the host is one person's, running unattended.
- A chat waiting for an approval or an answer shows a badge in the chat
  list, whether it is local or remote (U03). When the app reconnects, one
  notice sums up what waits (U06).

### 6.10 Offline

- A remote chat whose host can't be reached is read-only. It reads from
  the mirror, and its composer says the host is offline.
- Stop, approvals and sending fail with the reason, and the chat list
  shows the host as offline.
- The client reconnects with backoff from 1 s to 60 s. It reconnects at
  once when a timer gap over 30 s shows the laptop slept.

---

## 7. Milestones

Each task names files and has a "Done when" with its tests. "e2e" means
the scripted provider of `agent_run/e2e_tests.rs`. "Daemon test" means a
test in `crates/rovik-agentd/tests/` that starts the real binary (9).

### Milestone C: the core without Tauri (phase 1, no visible change)

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Needs | Done when                                                                                                                                                                                                                                |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C01 | Move the code that crosses the line. `ConflictFile` goes from `git_sync.rs` to `merge.rs`, which already has `MergeSegment`. `filesystem::write_app_file` goes to `file_guard.rs`, since it takes a file lock. `lsp::rust_analyzer::parse_assets` goes to a new `release_assets.rs`, used by both `lsp` and `vfs/driver.rs`. Behaviour doesn't change.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | –     | `cargo test` passes, and no core module of the design's §2.1 table imports `filesystem`, `git`, `git_sync`, `lsp`, `terminal`, `formatter`, `typings`, `project_files` or `open_url` (the PR shows the grep).                            |
| C02 | Add `rt.rs` with `spawn`, `spawn_blocking` and `block_on` over a process-wide `tokio::runtime::Handle` (`rt::init`), falling back to `Handle::current()`. `main` builds a multi-thread runtime and hands it to Tauri (`tauri::async_runtime::set`) and to `rt::init`. Every `tauri::async_runtime::*` call in core modules goes through `rt`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | C01   | No `tauri::async_runtime` remains in core modules, the app starts and runs a chat, and `cargo test` passes. `#[tokio::test]` tests are served by the fallback.                                                                           |
| C03 | Add `core.rs`: `Paths { data_dir, config_dir }`; `EventSink` with `fn agent(&self, e: &AgentEvent)`; `Core`, an `Arc` of paths, `db`, `agents`, `vfs`, `browser` and `events`; and `Core::new(paths, sink)`, which does what `lib.rs`'s `setup` does (open the database, `workload::init`, `overlay::chats::init`) and sets the agent log's folder. The app manages one `Core` and implements `EventSink` with `app.emit`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | C02   | A unit test checks that `Core::new` on a temporary folder opens the database and the overlays' folder. The app starts with it.                                                                                                           |
| C04 | The mechanical change, in one PR. Every `AppHandle<R>` and `R: Runtime` in core code becomes `&Core`: `app.state::<X>()` → `core.x`, the app's path lookups → `core.paths`, `events::emit(&app, …)` → `core.emit(…)`, `log::write(&app, …)` → `log::write(&core, …)`. `project_profile`'s settings and agent-log functions take a folder. `attachments::serve` becomes `attachments::read`, and the app builds the HTTP response. `send_chat_message`'s core function takes a sender in place of the `Channel`. Every `#[tauri::command]` in a core module moves to `src-tauri/src/commands/<area>.rs` as a thin wrapper over `State<'_, Core>`. The e2e harness (`e2e_tests.rs::harness`) builds a `Core` with a temporary data folder and a sink that records events as JSON; `Harness.app` becomes `Harness.core`, and the test bodies change to match. `headless.rs` builds a `Core` the same way. | C03   | No `tauri` is left in the core modules except the moved commands. All e2e suites pass without `tauri::test`. The headless e2e test passes. Manual check: a chat runs, approvals work, Stop works, an isolated chat lands, and CLM works. |
| C05 | Split the workspace. `src-tauri/Cargo.toml` gets `[workspace]` (resolver 2) with the app as the root package. `crates/rovik-core` takes the modules of the design's table and C01's moves. `crates/rovik-headless` takes the runner's binary. The `headless` feature and the `tauri/test` dev-dependency go. `scripts/eval/eval.mjs` builds with `-p rovik-headless`. `rovik-core` depends on `git2` with `vendored-libgit2` only; the app keeps `https` and `ssh`. Add `[profile.release-daemon]`, which inherits from `release` with `panic = "unwind"`.                                                                                                                                                                                                                                                                                                                                             | C04   | `cargo tree -p rovik-core` shows no `tauri` and no `openssl-sys`. `cargo test --workspace` passes, `npm run tauri build` works, and the eval script builds and starts the runner.                                                        |

### Milestone P: the protocol and a daemon on the same machine (phase 2)

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                         | Needs | Done when                                                                                                                                                                                                                                      |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P01 | `rovik-protocol`: the frames, `Request` and `Response` for the Agent group, and `Event`. `AgentEvent` and its payload types (`OutMessage`, `Patch`, `ApprovalRequest`, …) move here from `agent_run/events.rs`, and the core re-exports them. Add `dialog_id: Option<String>` to `FileWritten` and `Notice` (K02), and add `PROTOCOL` and `Fail`.                                                                            | C05   | Round-trip tests of every variant. JSON fixtures of `AgentEvent` are read by a Rust test and by an `agentRuns.ts` test, so both sides agree on the shape.                                                                                      |
| P02 | `rovik_core::dispatch` for the Agent group. The app's commands for that group call it. The ownership check of 4.2 comes with M01's column.                                                                                                                                                                                                                                                                                   | P01   | The e2e suites pass through `dispatch`, with a unit test per request.                                                                                                                                                                          |
| P03 | `rovik-agentd`. CLI: `serve [--socket P] [--data DIR] [--same-machine]`, `attach --stdio [--socket P]`, `version`, `doctor`. Start-up as in 5.2. Axum on the socket (0600, peer uid equal to its own) with `/health` and `/rpc`. Token hashes in `config.toml`. `attach --stdio` is a byte bridge. Logs go to `~/.rovik/logs/agentd.log`. `--same-machine` runs a chat in the `root` it is given, for development until T04. | P02   | Daemon test: `Hello` then `Welcome`; a wrong token is refused; a connection from another uid is refused (skipped when the test isn't root); a WebSocket session works through `attach --stdio`.                                                |
| P04 | The daemon's `EventSink`: `seq` per process, a ring buffer (2,000 events or 8 MB), a broadcast to every connection, `Subscribe` with `since`, and `Subscribed { complete }`. A connection that lags behind is dropped and resubscribes.                                                                                                                                                                                      | P03   | Test: three clients get the same events in order; a client that resubscribes with `since` gets the gap; once the buffer has wrapped, `complete` is false.                                                                                      |
| P05 | The daemon's store (6.2, D11): `keys.json` (0600, atomic writes), `state.json` (each chat's start settings without keys, `running`, `restarts`), and the last `AgentConfig` without keys. `AgentConfigure` without keys is filled from `keys.json`. A key is used only with the base URL it came with.                                                                                                                       | P03   | Unit tests: keys appear nowhere but in `keys.json` (the other files and the log are scanned); a rotated key replaces the old one; a key stored for another base URL isn't used; the file and folder modes are 0600 and 0700.                   |
| P06 | Restart interrupted runs at start-up (5.4), including the two-restart limit and its notice.                                                                                                                                                                                                                                                                                                                                  | P05   | Daemon test: a `kill -9` during a scripted reply makes the restarted daemon continue it (`ContinueReply`); one during a tool call makes it `Resume`, which asks for approval; a chat whose run crashed twice isn't restarted.                  |
| P07 | `rovik-client`: a connection over any `AsyncRead + AsyncWrite` (a Unix socket now, SSH channels in H), `Hello`, calls with ids and timeouts, `Subscribe` with `since`, reconnection with backoff, and typed wrappers. `Hosts` keeps each host's state: `connecting`, `ready`, `offline` or `version-mismatch`.                                                                                                               | P04   | Daemon test: calls and events go through the real binary. Restarting the daemon under the client makes the client reconnect and resync (`complete: false`).                                                                                    |
| P08 | Routing in the app for the Agent group (6.1): `Hosts` as managed state, `route`, remote events re-emitted under 6.1's rules, `agent_status` merged, `agent_configure` and `agent_set_effort` sent to every host. Until M01 exists, the developer setting `ROVIK_REMOTE_CHATS=<dialog>=<socket>` marks chats remote.                                                                                                          | P07   | A Rust test checks the event rules: `FileWritten` dropped, `Notice` titled, an edit approval marked `remote`. Manual: a chat pointed at a daemon running with `--same-machine` runs from start to end in the app. `npm test` passes unchanged. |

### Milestone T: the project on the server (P1)

The design's phase 5, moved before the move itself: a moved chat needs its
project, and packs through the tunnel (D6) can be tested without SSH.

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                               | Needs    | Done when                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T01 | Shadow repositories and snapshots (`rovik-client::shadow`, 6.4), including the large-file and history guards.                                                                                                                                                                                                                                                                                      | C05      | Tests on fixture projects: untracked files that aren't ignored are in the snapshot, while ignored ones (the project's `.gitignore`, `info/exclude`, global excludes) are out; after one edit, the next snapshot hashes one file (counted through a test hook); the project's `.git` is byte for byte the same before and after; the project folder's files are untouched (contents and modification times); a non-git project works; CRLF files keep their bytes; a symlink stays a symlink on Unix. |
| T02 | Packs: `GET /packs/<project>/refs`, `POST /packs/<project>` (compare-and-swap, completeness checked before the ref moves) and `POST /packs/<project>/fetch`, with the client's negotiation and progress on `cloud-event`.                                                                                                                                                                          | T01, P03 | Daemon test: the first push of a three-commit fixture carries the history; after one edit, the next push carries one blob, the changed trees and one commit (counted in the pack); a truncated pack is refused and the ref doesn't move; a fetch gets only new objects.                                                                                                                                                                                                                              |
| T03 | Projects and worktrees on the daemon (6.4): `ProjectEnsure`, the worktree and branch per chat, `.rovik/project.json` from the project row, and the `project_paths` row. Deleting a chat or collecting garbage removes the worktree, the branch and the refs.                                                                                                                                       | T02      | Daemon test: the chat's worktree has the snapshot's files and `.rovik/project.json` with the project's id; `chat_may_use` accepts the worktree; after a delete, all of it is gone.                                                                                                                                                                                                                                                                                                                   |
| T04 | Worktree mode in the run (D8). The daemon's `AgentStart` sets `root` and `isolate`. Add `command_problem` for worktrees: git commands that change the repository are refused, reusing `git_changes_repo`, and a command naming `~/.rovik` needs approval whatever the settings say. Parallel implementers (`task{kind:"workers"}`) are refused for the same reason as in an unisolated local chat. | T03, P05 | e2e against the daemon: a scripted run writes a file in the worktree; `git checkout x` is refused; `cat ~/.rovik/keys.json` asks for approval even with "allow any command" on; `workers` is refused.                                                                                                                                                                                                                                                                                                |
| T05 | Results (6.5): the commit through the daemon's own index, `ResultReady`, and the outcome-to-reason function taken out of `landing.rs` and used on both sides.                                                                                                                                                                                                                                      | T04      | Tests: the result's tree is the worktree minus ignored files; the agent's `git status` in the worktree is the same before and after; outcomes map to the same reasons as local landing (one table test shared with `landing.rs`).                                                                                                                                                                                                                                                                    |
| T06 | `overlay::import` (core, 6.5): build a chat's overlay store from a base tree and a result tree, with no mount. The client fetches and imports on `ResultReady` and lands by itself under `landing.rs`'s rules.                                                                                                                                                                                     | T05      | Client and daemon e2e: a remote run's change lands in the local project; a conflicting local change shows in the bar's conflicts, where "Land anyway" and "Resolve" work; Undo puts the project back.                                                                                                                                                                                                                                                                                                |
| T07 | Sync before each message (6.7, D14): the snapshot and push before each `Send`, `WorkSync`, `SyncState`, and `markers_left` for worktree files.                                                                                                                                                                                                                                                     | T05      | e2e: a local edit to a file the chat didn't touch is in the worktree before the next turn; with "chat resolves conflicts" on, a conflicting edit puts markers in the file and sends the chat a note, and landing waits until the markers are gone; with it off, the sync waits, the chat goes on with the older copy, and the bar says why.                                                                                                                                                          |
| T08 | Merges checked on the server (6.6, D13): `WorkRecheck`, and the core start kind `CheckMerged { files }`, which sets `merged_from_project` and runs `verify_at_end`.                                                                                                                                                                                                                                | T06, T07 | e2e: the local project changed a file the remote chat also changed, on other lines → the daemon rechecks → the checks pass → the merge lands; when a check fails, the chat fixes it and lands afterwards; after two rounds of "changed again", the result waits.                                                                                                                                                                                                                                     |
| T09 | The overlay moves with the chat (6.4): the landing base `S0'` from the overlay's base copies, and `C0` with its upper files.                                                                                                                                                                                                                                                                       | T01      | Test: an overlay with an edited, a new, a deleted and a moved file gives the expected `S0'` and `C0` trees. After a remote run and the import, landing merges exactly as the local overlay would have, including a file the user edited after the chat did.                                                                                                                                                                                                                                          |

### Milestone M: moving chats, and the mirror (phase 3)

| ID  | Task                                                                                                                                                                                                                                                                                                                                           | Needs              | Done when                                                                                                                                                                                                                                                                                                                       |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M01 | `SCHEMA_V10` (6.3): `dialogs.location` (NULL means here, `host:<id>` is a host, `released` is used on the daemon), `sync_clock`, `rev` columns and their triggers, an index on `(dialog_id, rev)`, and the local tables `cloud_moves` and `mirror_state`. Also the ownership check in `dispatch` (4.2).                                        | P02                | A migration test from version 9. A test runs every write function of `chat_db/store.rs`, `clm/store.rs` and `chat_db/projects.rs` and checks that `rev` rose on the rows each touched. `EXPLAIN QUERY PLAN` shows the changes query using the index.                                                                            |
| M02 | `chat_db::sync`: `ChangeSet`, `changes_since` and `apply` (6.3).                                                                                                                                                                                                                                                                               | M01                | Tests: applying the changes in any batches gives the same database as one full export; applying twice changes nothing; a finalized body stored in `message_bodies` arrives whole; a child chat arrives with its parent's location.                                                                                              |
| M03 | The safe point. `AgentPause` in the core: a flag on `RunControl`, handed to the run with its `Controls` and checked where a checkpoint pause happens in `run.rs` and between verification rounds. The run pauses with `PauseInfo { kind: "move", reason }`. Running `task` children finish first, and the call returns when the run has ended. | P02                | e2e: a pause during a tool call stores the call's result and makes no next request, with outcome `paused-move`; `ContinueRun` then continues the count; a pause during a scout lets the scout finish first; Stop still stops at once.                                                                                           |
| M04 | Import and ownership on the daemon: `ChatImport` (one transaction, idempotent per `move_id`, the worktree at the start commit), `ChatOwner` and `ChatRelease`.                                                                                                                                                                                 | M02, T03, T09      | Daemon test: two imports with one `move_id` make one chat; another `move_id` for a chat the daemon owns is refused; after a release, writes are refused and reading changes still works.                                                                                                                                        |
| M05 | Moving to a host (6.8): `cloud_move_preflight` and `cloud_move_chat`, progress on `cloud-event`, and recovery from `cloud_moves` at start-up.                                                                                                                                                                                                  | M03, M04, T07, P08 | Client and daemon e2e with the scripted model: a move during a run lets the run continue on the daemon, its events reach the app, and the mirror ends with the same rows as the daemon. Killing the app between the import and the flip → the next start finishes the flip. Killing it before the import → the move rolls back. |
| M06 | The mirror's writer (`rovik-client::mirror`, 6.3): pulls, attachments, and `mirror_state`. "Re-sync this chat" in the chat menu pulls `changes_since(0)` and replaces the mirror's rows.                                                                                                                                                                                                                                                     | M02, P07           | Tests: after a run with many streaming checkpoints, the mirror equals the daemon's rows; the app offline during a run catches up in one pull when it reconnects; a remote chat read offline shows everything stored before the disconnection.                                                                                   |
| M07 | Moving back (6.8).                                                                                                                                                                                                                                                                                                                             | M05, T06           | e2e: a chat moved there and back has the rows the daemon had; its changes wait as the chat's pending changes; the run continues locally.                                                                                                                                                                                        |
| M08 | Writes to and deletion of remote chats (6.1): rename, model, answered marks and checklist dismissal go to the owner; deletion removes the local rows at once and queues a tombstone for a host that is offline.                                                                                                                                | M05                | e2e: a rename in the app shows on the daemon; a chat deleted while its host is offline is deleted there on reconnection.                                                                                                                                                                                                        |

### Milestone H: hosts, SSH and deploy (phase 4)

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Needs    | Done when                                                                                                                                                                                             |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| H01 | Host settings: `cloud-hosts.json` in the app's config folder (id, name, host, port, user, auth kind, key path, pinned host key, approval policy, deploy state). Secrets go in the OS keychain (`keyring`, with each platform's native store): password, key passphrase, device token. Where no Secret Service exists, a file encrypted with argon2id and XChaCha20-Poly1305 takes their place, with a passphrase asked once per session, and the interface says so.                                                                                                                                                                                                                                                                                | –        | Unit tests: no secret ever appears in the JSON; the fallback round-trips; a missing keychain entry gives a clear error.                                                                               |
| H02 | `rovik-client::ssh` with russh. Authentication: key file with passphrase, ssh-agent (`SSH_AUTH_SOCK`; on Windows the OpenSSH agent pipe or Pageant), password, keyboard-interactive. The host key is shown as SHA-256 on first use and pinned; a mismatch is a hard error. Keepalives every 15 s. One SSH connection per host, with a `direct-streamlocal` channel per WebSocket or HTTP connection, and exec channels for setup.                                                                                                                                                                                                                                                                                                                  | H01      | Tests against an in-process russh server (`russh::server`): each authentication method works; a changed host key is refused; a WebSocket session and an HTTP upload run at once on separate channels. |
| H03 | The tunnel's fallbacks. The socket's path comes from `$HOME`, asked for once. When streamlocal is refused, the client uses `exec ~/.rovik/bin/rovik-agentd attach --stdio`. The "Use my system ssh" option runs `ssh -N -L <local socket or port>:<remote socket> <host>` under the app's control, so jump hosts, FIDO keys and certificates come from the user's own SSH configuration.                                                                                                                                                                                                                                                                                                                                                           | H02, P03 | Test: a server that refuses streamlocal still connects through `attach`. Manual: the system-ssh option through a jump host.                                                                           |
| H04 | The connection test (design §7), one step at a time with a result for each: DNS and TCP; handshake and fingerprint; authentication; a probe script in one exec (`uname -sm`, free disk in `$HOME`, `git --version`, Chromium and whether it starts headless, user namespaces, `/dev/fuse`, `systemd --user` and lingering, other users' daemons); whether the providers' API hosts can be reached from the server; and the daemon's health and version once it is deployed.                                                                                                                                                                                                                                                                        | H03      | Unit tests of the probe parser on fixtures (Ubuntu, Debian, Alpine, a container without systemd). Manual run against a VPS.                                                                           |
| H05 | Deploy, upgrade and removal (5.3). The binary comes from the release (B02), from `ROVIK_AGENTD_BINARY`, or from the shared `/opt/rovik/bin` copy when it matches. Upload through an exec channel to a temporary name, check it, `chmod 700`, rename. A new device token is added to `config.toml`: its hash on the server, the token in the keychain. Then the systemd unit and lingering (or the fallback), start, `/health`, `Welcome`. Upgrade means `Drain` then restart: runs pause at the safe point and continue on the new binary. Removal stops and disables the daemon, then deletes `~/.rovik` after a confirmation that names what goes. Before removing a host that still has chats, the app offers to move them back or delete them. | H04, B02 | Test against a container image with sshd. Manual: a deploy, an upgrade while a run is going (it pauses, then continues), a second computer added to the same host, and a removal.                     |
| H06 | The desktop's API keys move into the keychain. `ai-settings.json` keeps a reference per provider, migrated at the first start. `load_ai_settings` returns the keys filled in, so the frontend doesn't change.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | H01      | Tests: migration of a version 2 settings file, with no key left in the file afterwards; a missing keychain falls back as in H01.                                                                      |
| H07 | Team hosts (5.5, D10): the daemon's ownership and mode checks, `TMPDIR`, the limits in `/etc/rovik/agentd.toml`, slots defaulting to 1 while others' daemons run, and `docs/cloud-hosts.md` for admins (accounts, a shared binary, lingering, systemd limits, what admins can read).                                                                                                                                                                                                                                                                                                                                                                                                                                                               | P03, H04 | Tests: a group-readable `~/.rovik` stops the daemon with a message; the admin file's `max_slots` wins over the user's; nothing the daemon writes lands in `/tmp`. The document is reviewed.           |

### Milestone U: the interface

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Needs    | Done when                                                                                                                                                           |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| U01 | Settings → Cloud hosts, a new `AiSettingsPage` wired the way Subagents was: the list; add and edit (the fields of design §7, without the agent port); Test connection with each step's result and the fingerprint to confirm; deploy, upgrade and remove; the approval policy (6.9); server storage; the providers with a key on the server, with "Remove keys from this server".                                                                                                          | H05, P05 | `npm test` covers the page's states (not tested, testing, a failed step, deployed, upgrade available) and checks that the key list never shows a key. Manual check. |
| U02 | Location and moving. The chat header shows a location chip (this computer, or the host's name, or offline). The chat menu has "Move to <host>…" and "Move back". The move dialog shows what moves, what will wait for you (from the permissions and the host's policy), CLM becoming trimming, the keys notice and the preflight's other notes, then the progress and errors from `cloud-event`. The `move` pause record shows in the transcript as "Moved to <host>" or "Moved back".     | M05, U01 | `npm test` for the dialog's text under different settings. Manual: a move and a move back.                                                                          |
| U03 | Chat list badges: a cloud badge on remote chats, a "waiting for you" badge on any chat with a pending approval or question (local or remote, from the approval and question events and `agent_status`), and an offline host on its chats.                                                                                                                                                                                                                                                  | P08      | `npm test`: the badges follow a scripted sequence of events.                                                                                                        |
| U04 | Guards for remote chats in the frontend. The auto-resume effect skips remote chats, which the daemon resumes (K07). Edit approvals marked `remote` show in a compare view built from `current` and `proposed`, never on the local file's tab (K09). A new routed command, `chat_read_plan` (6.1), reads plans in place of `readFileContent`, and a remote chat's plan opens in a read-only tab (K09). Without `realPath`, the CLM context file of a remote chat opens read-only from `clm_read_revision`. | P08, T04 | `npm test`: a remote chat with unanswered calls starts no `resume`; a remote edit approval doesn't open the local file; a remote plan opens from the owner.         |
| U05 | The isolation bar for remote chats: the files changed so far, from `WorkStatus`, while a run goes; the result's pending reason and conflicts, from the imported store; the sync state, with "Hand to the chat" and "Keep the older copy"; and "Sync now".                                                                                                                                                                                                                                  | T06, T07 | `npm test` for the bar's states. Manual check.                                                                                                                      |
| U06 | Notices: an approval or a question from a remote chat notifies as a local question does today. On reconnection, one notice sums up what waits.                                                                                                                                                                                                                                                                                                                                             | U03      | `npm test`.                                                                                                                                                         |

### Milestone B: building and releasing the daemon

| ID  | Task                                                                                                                                                                                                                                                                                                                | Needs    | Done when                                                                                                                 |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------- |
| B01 | Static builds: `cargo zigbuild --profile release-daemon -p rovik-agentd --target {x86_64,aarch64}-unknown-linux-musl`, run by `scripts/build-agentd.sh`. If `aws-lc-sys` won't build for musl, the daemon's rustls uses the `ring` provider (K20).                                                                  | C05, P03 | Both binaries build in a clean container, `file` reports them statically linked, and each starts on Alpine and on Ubuntu. |
| B02 | Release workflow `.github/workflows/agentd.yml`, on `v*` tags: build both binaries, sign them (minisign, with a key kept in the repository's secrets), and attach `rovik-agentd-<version>-<arch>` with its `.minisig` to the release. The app compiles in the public key and checks the signature before uploading. | B01      | A unit test with a test key refuses a tampered binary. The workflow publishes on a test tag.                              |
| B03 | The development override: `ROVIK_AGENTD_BINARY`, and a "local build" field in a host's advanced settings. The daemon reports its version and git hash.                                                                                                                                                              | B01      | Manual: deploy a locally built daemon.                                                                                    |

### Milestone R: running it for real

| ID  | Task                                                                                                                                                                                                                                                                                                                                         | Needs    | Done when                                                                                                  |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------- |
| R01 | Storage and garbage collection. `Storage` reports the sizes of `projects/`, `work/`, `data/` and `tmp/`. Worktrees of released and deleted chats are removed after 7 days. Bare repositories get `git gc --auto` when git is installed (libgit2 has no gc). The existing attachment cleanup runs too, and the hosts page has "Clean up now". | M07      | Daemon test: a deleted chat's worktree and refs are gone after collection, and the sizes are reported.     |
| R02 | Version skew (design §10.6): the protocol check on connection; the client refuses to move a chat to an older daemon and offers to upgrade; a newer compatible daemon is fine; an upgrade drains first.                                                                                                                                       | H05, M03 | Daemon test: an upgrade while a run is going gives `paused-move`, and the run continues on the new binary. |
| R03 | `rovik-agentd doctor` prints what the connection test needs (versions, mounts, lingering, slots, storage), and H04 uses it once the daemon is deployed. Log rotation.                                                                                                                                                                        | P03      | A snapshot test of the output on a fixture.                                                                |
| R04 | Windows and macOS clients. Local paths, such as drive letters in `projects.path`, never reach the daemon: it gets project ids and relative paths. Snapshots keep CRLF bytes. ssh-agent works through the named pipe. Importing on a case-insensitive file system handles two paths that differ only in case.                                 | T09, H02 | Tests on Windows and macOS where they are available. Manual check on one Windows laptop.                   |

### Later (phase 6 and beyond, not planned in detail)

- The public HTTPS mode (T2), with a pinned self-signed certificate or
  ACME and a token per device.
- A remote terminal in a chat's worktree.
- The overlay over the worktree where FUSE exists: undo groups, and
  parallel implementers on servers.
- Commands in a container or as a separate Unix user, for teams that want
  more isolation than Unix accounts give.
- A shared object cache for a team's bare repositories.
- Carrying submodules and nested repositories in snapshots.
- Push or email notices when a remote run waits.
- Listing and adopting another computer's chats on the same host.
- Remote-first projects (P3).

---

## 8. Implementation order for an AI agent

### Rules for every task

1. **One task per branch and pull request.** Don't start the next task
   until the current one is merged.
2. **Read first:** this plan's sections 1 to 6, the design document's
   section for the milestone, and the files the task names. Follow the
   surrounding code's style: module doc comments that explain _why_,
   small functions, and tests next to the code.
3. **Keep the app working.** Nothing changes for someone who hasn't added
   a host. Local chats behave exactly as they do today, and the existing
   suites pass unchanged, except for how the harness is built in C04.
4. **Checks before every push:**
   - `cd src-tauri && cargo check --workspace && cargo test --workspace`
     (`cargo check && cargo test` until C05)
   - `npm test`
   - `npm run typecheck`
   - `npx prettier --check <the files you changed>` (the repository as a
     whole has docs that aren't formatted)
5. **Tests in the same task.** Each task's "Done when" names them.
6. **When a task doesn't fit reality,** don't improvise a new design. Add
   a note under "Notes from implementation" below, take the smallest safe
   path, and say so in the pull request.
7. **Protocol changes** are additive, or they raise `PROTOCOL` and the
   pull request says why. Every new request gets a round-trip test.
8. **Tauri commands:** a new or changed command is registered in `lib.rs`
   and gets a typed wrapper in its `src/lib` module in the same task.
9. **Secrets:** never log or store a provider key, password, passphrase
   or token outside the keychain, `keys.json` and the start request in
   memory. Tests scan the JSON files and the logs.

### Order at a glance

```
C:  C01 ─► C02 ─► C03 ─► C04 ─► C05                           no visible change
P:  P01 ─► P02 ─► P03 ─► P04 ─► P05 ─► P06
                          └─► P07 ─► P08                      a chat on a same-machine daemon
T:  T01 (after C05) ─► T02 ─► T03 ─► T04 ─► T05 ─► T06 ─► T07 ─► T08;  T09 after T01
M:  M01 (after P02) ─► M02 ─► M03 ─► M04 ─► M05 ─► M06 ─► M07 ─► M08
B:  B01 (after P03) ─► B02 ─► B03
H:  H01 ─► H02 ─► H03 ─► H04 ─► H05 (needs B02) ─► H07;  H06 any time after H01
U:  U03 after P08;  U04 after T04;  U05 after T07;  U01 after H05;  U02 after M05 and U01;  U06 after U03
R:  R03 after P03;  R01 after M07;  R02 after H05;  R04 after T09 and H02
```

After C, P, T and M, a chat moves to a daemon on the same machine, over a
Unix socket, and back again, with its project, through the real protocol.
That is the whole feature except SSH. H then puts a tunnel where the
socket was. This is why T and M come before H, unlike the design's phases,
where SSH was phase 4 and the project transport phase 5: packs through the
tunnel (D6) make the transfer testable without SSH.

C04 is the riskiest single change: a large mechanical diff with the
harness in it. T and M carry the most new logic. H depends most on the
environments it meets. B can run alongside T and M once P03 exists.

---

## 9. Testing

- **Unit tests** next to the code, as today.
- **Core e2e:** the scripted provider of `agent_run/e2e_tests.rs`. Its
  model server moves to a `testing` module of `rovik-core`, behind a
  feature, so the daemon's tests can start one too.
- **Daemon tests** (`crates/rovik-agentd/tests/`) start the real binary
  (`CARGO_BIN_EXE_rovik-agentd`) on a temporary socket and data folder.
  The daemon runs in its own process, so its statics never meet the test
  process's (K18).
- **Client and daemon e2e** tests run the client's `Core` in the test
  process and the daemon as a child process. They cover moving, the
  mirror, landing and syncs. Tests that use the test process's overlays
  take `overlay::chats::TEST_DATA`, as isolated tests do today.
- **SSH tests** run against an in-process russh server, with no sshd.
- **Frontend tests** use `node:test`, with `invoke` mocked through
  `window.__TAURI_INTERNALS__` as in `agentRuns.test.ts`.
- **Faults to inject:** `kill -9` of the daemon during a model reply,
  during a tool call, during an import and during a pack upload; the SSH
  connection cut in the middle of an upload; a socket closed without a
  FIN, which is what a sleeping laptop looks like; a server clock an hour
  off. Ordering uses `seq` and `rev`, never time.
- **Manual matrix:**
  - Servers: an Ubuntu 24.04 x86_64 VPS; Debian on aarch64; an Alpine
    container without systemd, FUSE or user namespaces; a team host with
    two accounts working at once.
  - Clients: Linux; macOS without macFUSE; Windows without WinFsp.

---

## 10. Gaps found in review

A check of the design against the code found 21 gaps. Each is now part of
a decision or a task.

| ID  | Gap                                                                                                                                                                                      | Resolved in                                                                                                |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| K01 | A mirror keyed by `seq` can't follow patches: `MessagePatched` changes rows by id, the text streamed between checkpoints is never stored, and a ring buffer can't cover a night's sleep. | D9, M01, M02, M06                                                                                          |
| K02 | `FileWritten`, `ReasoningLearned`, `ApiRouteLearned` and `Notice` carry no dialog id, so events can't all be told apart by chat.                                                         | P01 (a dialog id on `FileWritten` and `Notice`; the two "learned" events apply to settings, not to a chat) |
| K03 | Every run reads the project's settings from the app's config folder, keyed by the local path. A daemon wouldn't have them, and `Core` had no config folder.                              | D2, 6.2, M04, P05                                                                                          |
| K04 | `tauri::async_runtime` can spawn from any thread, but `tokio::spawn` panics outside a runtime.                                                                                           | D2, C02                                                                                                    |
| K05 | The design moves `git_sync` and three helpers from `git.rs` into the core, but the core needs only `ConflictFile`.                                                                       | C01                                                                                                        |
| K06 | The webview's CSP allows connections only to IPC, so a frontend `backend` module couldn't reach the tunnel.                                                                              | D4                                                                                                         |
| K07 | ChatPanel resumes by itself any chat with unanswered tool calls. It would resume a remote chat that the daemon is running, or that it resumes itself.                                    | U04                                                                                                        |
| K08 | The editor context in a message names the active file's absolute local path, which means nothing on the server.                                                                          | 6.1 (`agent_start`)                                                                                        |
| K09 | `meta.planFile` and the `absPath` of edit approvals are local paths read from the local disk.                                                                                            | 6.1 (`chat_read_plan`), U04                                                                                |
| K10 | Local auto-landing merges the project's newer changes and checks them before landing. Landing a remote result needs the same.                                                            | D13, T08                                                                                                   |
| K11 | Isolated chats refuse git commands that change the repository. Without the same rule in worktrees, a branch switch would become the chat's result.                                       | D8, T04                                                                                                    |
| K12 | A systemd user unit starts with a bare `PATH` and often no `$SHELL`, so the agent wouldn't find `cargo` or `npm`.                                                                        | 5.2                                                                                                        |
| K13 | `/tmp` is shared on team hosts: `api_log`'s default file, the agent's temporary files and Chrome's files would be visible to others, or open to symlink tricks.                          | D10, 5.2, H07                                                                                              |
| K14 | The design builds the daemon "in CI", but there is no CI.                                                                                                                                | D15, B01, B02                                                                                              |
| K15 | CLM needs the virtual-files mount, which most servers don't have.                                                                                                                        | 5.2 (mounted when FUSE works), U02 (the dialog says trimming will be used)                                 |
| K16 | The design relies on a "waiting for you" badge in the chat list, but none exists.                                                                                                        | U03                                                                                                        |
| K17 | Moving a chat whose overlay has changes, with the project tree as the base, would lose the user's later edits to the chat's files at landing.                                            | 6.4, T09                                                                                                   |
| K18 | Process-wide statics stop two `Core`s with different data folders from sharing one test process.                                                                                         | 9 (the daemon in its own process)                                                                          |
| K19 | The release profile's `panic = "abort"` would let one run's panic end every run on the daemon.                                                                                           | D15, C05                                                                                                   |
| K20 | reqwest's rustls uses `aws-lc-rs`, which needs CMake and a C toolchain to build for musl.                                                                                                | B01                                                                                                        |
| K21 | Nothing stops local code from writing to a chat whose owner is elsewhere.                                                                                                                | 4.2, 6.3, M01                                                                                              |

---

## 11. Risks and known limits

- **C04's size.** The compiler lists what is left at each step, and the
  suites are the safety net. It should go in one pull request, merged
  quickly, so that it doesn't conflict with other work for long.
- **russh's gaps** (unusual key types, Windows agents, jump hosts). The
  system-ssh option (H03) is the way out, and the connection test names
  it when russh fails.
- **musl builds.** `aws-lc-sys` and the tree-sitter grammars' C code need
  a C toolchain for musl, which `cargo zigbuild` provides. The `ring`
  provider is the fallback for the former.
- **libgit2's worktree support** has edge cases, such as locked worktrees
  and pruning. If they bite, the daemon uses the `git worktree` command,
  which the connection test reports when it is installed.
- **Disk on team hosts.** Each person's bare repository holds the
  project's whole history. Collection (R01) and the storage line help in
  v1; a shared object cache is a later option.
- **The mirror drifting from the owner.** M02's tests are the guard, and
  "Re-sync this chat" in the chat menu, a full `changes_since(0)`, is
  the way to recover.
- **Known limits of v1:**
  - Chats without a project don't move.
  - Submodules and nested repositories aren't carried.
  - Parallel implementers and fine-grained undo aren't available on
    servers.
  - "Redo sequentially" for a parallel group doesn't survive a move.
  - A computer sees only the chats it moved to a host.

---

## 12. Changes to the design document

This plan changes the design in these places. `cloud-agent-plan.md`
points here from each one.

| Design                                                                                     | Change                                                                                                                                         | Here                   |
| ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------- |
| §2.3, a frontend `backend` module                                                          | Routing in the Rust command layer, and no streaming call goes remote                                                                           | D4, 6.1                |
| §3.2 and §6, "pushes it with libgit2 over SSH", two SSH stacks                             | Packs through the tunnel, one SSH stack                                                                                                        | D6, 6.4                |
| §3.2, snapshots in the project's repository, a shadow repository only for non-git projects | A shadow repository for every project                                                                                                          | D7                     |
| §4, a localhost port or a Unix socket                                                      | A Unix socket, with `attach --stdio` as a byte bridge                                                                                          | D5                     |
| §5, a mirror written from events keyed by `seq`                                            | A mirror that follows stored rows by `rev`                                                                                                     | D9, 6.3                |
| §9, the phases                                                                             | Project transport and the move before SSH                                                                                                      | 8                      |
| Open questions                                                                             | Team servers with one daemon per person; containers and separate users later; the non-git shadow's disk cost accepted; v1 undo is landing undo | D10, 7 (Later), D7, 11 |

## Notes from implementation

None yet. Tasks add notes here when reality differs from the plan.
