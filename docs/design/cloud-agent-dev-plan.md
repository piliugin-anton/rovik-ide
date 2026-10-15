# Running the agent in the cloud: development plan

This plan builds what [`cloud-agent-plan.md`](cloud-agent-plan.md) decides:
a person adds a server in Settings, the IDE deploys the agent there, and any
chat can move to that server and back, with its history, its project and
its settings, and keep running while the laptop sleeps.

Read `cloud-agent-plan.md` first for the options and the reasons behind
them. This file says what to build, in which crates and files, in which
order, and how each step is tested. It also records the decisions taken
on 2026-10-14 after the plan was checked against the code (section 1), and
where that check, and a review of this plan, found problems (section 10).

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

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Why                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | **A Cargo workspace under `src-tauri/`.** The app stays the root package (`rovik-ide`), so the Tauri CLI and `tauri.conf.json` don't move. Members: `crates/rovik-protocol`, `crates/rovik-core`, `crates/rovik-client`, `crates/rovik-agentd`, `crates/rovik-headless`.                                                                                                                                                                                                                                                                                                                                                                                                                           | The design's split (§2.1), plus two crates. `rovik-client` is the app's half of the protocol (connection, mirror, project transport) without Tauri, so it can be tested against a real daemon process. `rovik-headless` keeps the evaluation runner out of the core.                                                                                                                                                                                                                                                                                                                                                              |
| D2  | **`Core` replaces `AppHandle<R>` in all core code.** `Core` holds `Paths` (data folder and config folder), `DbState`, `AgentState`, `VfsState`, `BrowserState` and an `Arc<dyn EventSink>`. Tasks are spawned through `rovik_core::rt`, over one tokio runtime per process. The process-wide statics stay as they are.                                                                                                                                                                                                                                                                                                                                                                             | The design's §2.2, plus two things the code needs. Every run reads `project-settings.json` from the **config** folder (K03). `tauri::async_runtime` can spawn from any thread, but `tokio::spawn` can't (K04).                                                                                                                                                                                                                                                                                                                                                                                                                    |
| D3  | **One protocol, one dispatcher.** `rovik-protocol` defines `Request`, `Response`, `Event` and the frames they travel in. `rovik_core::dispatch(&Core, Request)` serves both the daemon and the app's own local calls, so the list of operations a chat can do exists once. HTTP carries `/health`, attachments, git packs, the bundle that moves a chat, and calls too big for a frame (4.1).                                                                                                                                                                                                                                                                                                      | §2.3.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| D4  | **Routing in the Rust command layer** _(new)_. The Tauri commands the frontend already calls look up where the chat is and send a remote chat's call to its host. Remote events come back as the same `agent-event`. `chatApi.ts`, `agentRuns.ts` and `overlay.ts` don't change; the frontend work is new interface and a few guards (U04).                                                                                                                                                                                                                                                                                                                                                        | The webview's CSP allows connections only to IPC (K06), so the SSH tunnel and the protocol client have to live in Rust. Routing next to them keeps one place that knows where a chat runs. This replaces the frontend `backend` module of §2.3.                                                                                                                                                                                                                                                                                                                                                                                   |
| D5  | **Transport T3, over a Unix socket.** The daemon listens on `~/.rovik/agentd.sock` (mode 0600, and the peer's uid is checked). The client opens one `direct-streamlocal@openssh.com` channel per WebSocket or HTTP connection. Where the server doesn't allow forwarding, the client instead runs `rovik-agentd attach --stdio` on an exec channel, a plain byte bridge to the socket. Every call carries the computer's token. The server keeps only the token's hash, which the daemon reads when it starts. There is no TCP port.                                                                                                                                                               | Team hosts (D10): a socket in the user's own 0700 folder can't be reached by other users and can't clash with their ports. A byte bridge carries WebSocket and HTTP the same way over both channel kinds. The token never touches the server's disk, so the agent's own commands, which run as the same user, can't simply connect and approve their own requests. A command set on tampering with the daemon's files still could. D8's rule asks the user about any command that names them, and the client checks the daemon's binary and unit on every connection (H05).                                                       |
| D6  | **Git carries the project (P1), as packs through the tunnel** _(new)_. The client builds a pack of the objects the server lacks (`git2::PackBuilder`) and posts it to the daemon. Results come back the same way. The client uses one SSH library, russh.                                                                                                                                                                                                                                                                                                                                                                                                                                          | There is no libssh2 next to russh, so there is one place for authentication and one pinned host key. The server needs no `git-receive-pack`. The whole flow can be tested against a local daemon, without SSH. This replaces "libgit2 over SSH" in §3.2 and §6.                                                                                                                                                                                                                                                                                                                                                                   |
| D7  | **Every project gets a shadow repository** _(new)_ at `<app data>/shadow/<project id>.git`. Its work tree is the project's repository folder (read, never checked out into), and it has its own persistent index, seeded from the project's `HEAD`. For a git project it borrows the project's objects through `objects/info/alternates`. Nothing is ever written to the project's `.git`.                                                                                                                                                                                                                                                                                                         | No `refs/rovik/*` show up in the user's repository, and its gc, hooks and Git panel are unaffected. The index's stat cache makes every snapshot after the first cheap. Git and non-git projects share one code path.                                                                                                                                                                                                                                                                                                                                                                                                              |
| D8  | **On the server, each chat works in a git worktree of its own** (§3.1), as a direct-mode run: `isolate: false`, with the worktree as its root. The worktrees and the commands' temporary files live in `~/.local/share/rovik/`, apart from the daemon's own files in `~/.rovik/`. Git commands that change the repository are refused, as they are in isolated chats today. A command that names `.rovik` in any spelling (`~/.rovik`, `$HOME/.rovik`, its absolute path, `../.rovik`) needs the user's own approval, never the AI reviewer's, whatever the settings say.                                                                                                                          | The worktree is the chat's private copy. Without the refusal, a `git checkout` would become part of the chat's result. With the work kept outside `~/.rovik`, ordinary commands never trip the rule, even though their paths and compiler output name the worktree. The rule is a text match: it stops accidents, not a determined command (design §6).                                                                                                                                                                                                                                                                           |
| D9  | **Ownership moves at a turn boundary (M1)**, at a new pause of kind `move`: the point where a checkpoint pause happens today. The local rows become a **mirror that follows the owner's stored rows**, using a revision counter (`rev`) and `changes_since`. They don't follow replayed events. Rows that `MessagesAdded` announces are written to the mirror before the event goes on, and every pull ends with events for the rows it changed.                                                                                                                                                                                                                                                   | Events can't rebuild the database. `MessagePatched` has no `seq`, text streamed between two checkpoints is never stored, and a ring buffer can't cover a night's sleep (K01). And the frontend must never show less than the database has (K22).                                                                                                                                                                                                                                                                                                                                                                                  |
| D10 | **On a shared team server, each person has their own SSH account and runs their own daemon** _(new)_. Everything the daemon keeps is in `~/.rovik` (0700). What the agent works in, its worktrees and temporary files (`TMPDIR`), is in `~/.local/share/rovik` (0700). Rovik is installed once per server (D17), and each person's daemon is an instance of its system unit.                                                                                                                                                                                                                                                                                                                       | Unix users keep people's chats, keys and worktrees apart. This is the single-user design plus a socket, limits and documentation for admins.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| D11 | **Provider keys are stored on the server** (decided in §6), in `~/.rovik/keys.json`, and refreshed by every start.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | As decided.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| D12 | **A remote result lands through the existing landing code.** The client imports the result commit into the chat's local overlay store, with base copies taken from the landing base, and needs no mount for it. From there, landing, conflicts, the journal, undo and the isolation bar work as they do today. A landing is reported to the daemon (`WorkLanded`), which then counts those changes as part of the project.                                                                                                                                                                                                                                                                         | An overlay store can be read and landed without its view mounted (`overlay::chats::changes` and `land_now_with` open it with `OverlayFs::open`). The client doesn't need a second landing path. Without the report, an Undo of the landing would be undone by the next result (K34).                                                                                                                                                                                                                                                                                                                                              |
| D13 | **Merges with the project's newer changes are checked on the server.** If the local project changed a file the chat also changed, the client pushes a fresh snapshot, the daemon merges it in and runs the project's checks (`WorkRecheck`), and only a result that passes lands.                                                                                                                                                                                                                                                                                                                                                                                                                  | A local isolated chat never lands an unchecked merge (`landing.rs`: `take_project_changes`, `check_merged`), and the toolchain is on the server.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| D14 | **The project syncs before each message.** If the merge is clean, the worktree gets the user's newer files. If text conflicts, the existing _chat resolves conflicts_ setting decides: either the markers go to the chat, or the sync waits and the chat carries on with its older copy. Binary and delete conflicts always wait for the user. A sync is all or nothing, so the chat's landing base stays correct.                                                                                                                                                                                                                                                                                 | These are the rules the user already set for local isolated chats. A sync that applied only some files would need a separate base for each file.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| D15 | **Daemon binaries come from a release workflow.** A new GitHub Actions workflow (there is no CI today) builds `x86_64-` and `aarch64-unknown-linux-musl` with `cargo zigbuild` and signs them. The app downloads the build for its own version and checks the signature against a public key compiled into it. `ROVIK_AGENTD_BINARY` points at a local build for development. The daemon builds with a `release-daemon` profile, which keeps unwinding on panic.                                                                                                                                                                                                                                   | People who build the desktop app from source can still deploy. Because the key is compiled in, the check doesn't depend on which was built first, the app or the daemon. With the app's `panic = "abort"`, one run's panic would end every other run on the daemon.                                                                                                                                                                                                                                                                                                                                                               |
| D16 | **Servers run Linux on x86_64 or aarch64. Clients run Linux, macOS or Windows.** A daemon can serve several of its person's computers. Each chat belongs to the computer that moved it: the daemon sends a computer only its own chats' events and status, takes a chat's writes only from its computer, and keeps one `AgentConfig` per computer, which that computer's chats run with.                                                                                                                                                                                                                                                                                                           | §7. One computer's settings and approval policy must never change another's runs (K27). Adopting another computer's chats would be the "every device sees every chat" design (M2), which wasn't chosen.                                                                                                                                                                                                                                                                                                                                                                                                                           |
| D17 | **Rovik is installed once per server, by its first user, who becomes its administrator** _(new)_. On a server without `/etc/rovik/agentd.toml`, the first deploy installs with `sudo`: one root-owned copy of the binary in `/opt/rovik/bin`, its signature checked; the limits in `/etc/rovik/agentd.toml`; a system unit, `rovik-agentd@.service`, that runs each person's daemon as that person, in `rovik.slice`; and a sudoers rule that lets each user start only their own daemon, and the administrators run Rovik's admin commands. Without sudo, the deploy stops and shows the command for someone with root to run. The administrator sets the limits and upgrades Rovik for everyone. | Neither a user nor the agent's commands can swap a root-owned binary, and the binary already installed checks every upgrade's signature. Limits on system units can't be overridden by users, cover the daemon and everything it starts, and leave alone everything else the user runs. System units need no lingering, and give each daemon a private `/tmp`.                                                                                                                                                                                                                                                                    |
| D18 | **The agent's Chrome is driven over a pipe, not a port** _(new)_. Rovik carries a fork of `eoka` with a pipe transport (`--remote-debugging-pipe`), offered upstream, and uses it on Linux and macOS.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | eoka opens Chrome's DevTools on a `127.0.0.1` port with no password, which every user of a shared server can reach, and the browser runs as you (K52). The pipe leaves eoka's stealth as it is. The pipe switch turns on the same "automation controlled" flag that headless mode and eoka's port 0 already turn on, and eoka's `--disable-blink-features=AutomationControlled` turns it off after them (Chromium's `content/child/runtime_features.cc`). The rest of its stealth (patched strings in Chrome, injected scripts, the CDP methods it avoids) doesn't depend on the transport, and pages lose a local port to probe. |

---

## 2. What the code does today

These facts were checked at commit `e8abd81`, and again at `f3e8bf8`
after the chat input learned to attach images, PDFs and text files
(6.11). The plan relies on them.
Where one contradicts the design document, section 10 says so and names
the task that handles it.

**Backend (`src-tauri/src`, about 115,000 lines in one crate, 154 commands in `lib.rs`)**

- **How core code depends on Tauri:**
  - `AppHandle<R>` and `R: Runtime` appear in 23 non-test files of core code, 18 of them in `agent_run/`.
  - `tauri::async_runtime::spawn_blocking` is called 64 times, `spawn` 6 times, `block_on` twice.
  - Commands take `tauri::State`; `send_chat_message`, `vfs_subscribe`, `lsp_*` and `spawn_terminal` take `tauri::ipc::Channel`. `chat_store_attachment` reads the request's raw body (`tauri::ipc::Request`).
  - Events go out through one function, `agent_run::events::emit`.
  - Non-test code calls `app.state::<…>()` only about 20 times, because `Run` keeps `AgentState` and `DbState` as fields. The e2e tests reach the mock app about 170 more times.
- **Events:**
  - `AgentEvent` has 19 variants.
  - `FileWritten`, `ReasoningLearned`, `ApiRouteLearned` and `Notice` carry no dialog id.
  - What the design calls `MessageStored` is `MessagesAdded` plus `MessagePatched`.
  - A reply on screen is patched every 50 ms (`run.rs`, `UI_EVERY`), and each patch carries the whole text so far.
  - `Patch.status` is an `Option<Option<Status>>`, where `null` means "clear the status".
- **How replies are stored:**
  - A reply in progress is written to the database every 2 seconds (`CHECKPOINT_EVERY`) and finalized at the end. The patches sent between two checkpoints are never stored.
  - A failed turn is settled by `run.rs::store_failed_turn`: text becomes an interrupted reply (`meta.interrupted`), and an empty reply is marked failed.
  - Messages are deleted only together with their chat. No table has a revision or an `updated_at` per message (`chat_db/schema.rs`, version 9).
- **Attachments** (`chat_db/attachments.rs`, checked at `f3e8bf8`):
  - Screenshots, and the images (PNG, JPEG, WebP, GIF) and PDFs attached in the chat input, are files in `attachments/` named by their SHA-256, each with an `attachments` row. `message_attachments` links them to messages, with a foreign key to `attachments`.
  - The input stores a file as soon as it is pasted or picked, before its message exists. `chat_store_attachment` reads the type from the bytes, counts a PDF's pages, and refuses anything over 20 MB (`MAX_BYTES`).
  - `agent_start` takes the ids in `UserMessage.attachments` (at most 20) and refuses the message if one of them isn't stored. The message's `meta.pdfs` names its PDFs, with their pages.
  - `ModelProfile` has `vision` and `pdf`, so every `StartRequest` says which files the model takes.
  - Every request of a run reads its files from the folder again (`history::to_wire`). A file missing there is replaced by a note, with no error.
  - Cleanup removes a file that no message references once it is an hour old (`GRACE_MS`), unless the input's saved draft holds it (`composer_drafts`, which cascades from `dialogs`). It runs when the database opens and after a chat or a project is deleted.
- **Project settings:** every run reads the project's saved settings, such as corrected check commands, from `project-settings.json` in the app's **config** folder. They are keyed by the project's local path (`agent_run/project.rs::load` → `project_profile::load_project_settings`).
- **Projects:**
  - A project can be a subfolder of a git repository (`projects.git_subpath`).
  - `projects::merge` moves one project's chats to another.
- **Imports from core into app modules:**
  - `git_sync` is used only for its `ConflictFile` type (`overlay/chats.rs`), so `git_sync.rs` and `git.rs` can stay in the app.
  - `filesystem::write_app_file` is used by `agent_run/plan.rs`.
  - `lsp::rust_analyzer::parse_assets` is used by `vfs/driver.rs`.
- **Isolated chats:** `ChatOverlay::command_problem` refuses git commands that change the repository. Where commands can't be confined, it also refuses commands that name the project's path.
- **Landing:**
  - When an isolated run ends, `landing.rs` merges the project's newer changes into the chat's view, runs the checks again, and only then lands.
  - It lands without asking only after the outcomes `done`, `verified`, `unverified` and `verify-unverified`.
  - An overlay's saved store can be listed and landed without being mounted.
  - `overlay_land` refuses while the chat's run is going, judged from the local `AgentState` (`not_running`).
- **Conflict markers:**
  - They are written by `overlay::chats::mark_one`, with labels (`MARK_PROJECT`, …). `markers_left` recognises only those labels.
  - `merge::merge_bytes` gives no text on a conflict (`Ok(None)`) and refuses binary files.
- **Pauses:**
  - A checkpoint pause happens once every tool call of a turn has its result. It stores `meta.paused = { kind, toolCalls, reason }` (`guard.rs::PauseInfo`), and `ContinueRun` continues counting from there.
  - A pause with a `reason` first asks the model for a summary (`summarize_before_pause`).
- **Interrupted runs:** `StartKind::Resume` reruns unanswered tool calls, and an interrupted call is always confirmed by the user first (`execute.rs`). `ContinueReply` continues a stored partial reply.
- **CLM:** it needs the virtual-files mount (`clm::availability`). Without the mount, each request falls back to trimming (`agent.rs`). `clm/store.rs` deletes old CLM revisions.
- **Panics:** `agent_start` runs the run as a task inside a task, so a panic still releases the chat. The release profile's `panic = "abort"` makes that useless in release builds.
- **Test harness:** the e2e harness builds a mock Tauri app in one function (`agent_run/e2e_tests.rs::harness`), and `headless.rs` builds the same mock. The eval script builds the runner with `cargo build --features headless --bin rovik-headless` (`scripts/eval/eval.mjs`).
- **Statics:**
  - The overlays' data folder and other caches are process-wide statics (`overlay::chats::DATA_DIR` and others). Two `Core`s with different data folders can't share one process (K18).
  - `overlay::chats::TEST_DATA` exists only under `#[cfg(test)]`.
- **git2 and TLS:**
  - git2 0.21 has no default features. The core uses only local operations (`merge_file`, `Patch`, `discover`).
  - `PackBuilder::insert_walk`, `Odb::packwriter`, `Indexer`, `Repository::worktree`, `Repository::set_index`, `Index::read_tree` and `CheckoutBuilder::target_dir` exist.
  - `index.add_all` skips ignored files that aren't in the index yet.
  - git2's `https` and `ssh` features pull in `openssl-sys`, and reqwest's rustls pulls in `aws-lc-rs`.
- **Helpers that already exist:**
  - `process::adopt_login_shell_path` must run while the process is single-threaded. A desktop launcher's bare PATH is a systemd unit's problem too.
  - `atomic_file::write_atomic_private` writes files with mode 0600.
  - `api_log` never writes a key, but by default it writes to `/tmp/rovik-api-log.jsonl`.
  - The IDE writes `.rovik/.gitignore` with `project.json` in it, and plans go to `.rovik/plans/` (`agent_run/plan.rs::PLANS_DIR`).
- **Browser tools:** `browser_tools.rs` drives headless Chrome through `eoka` 0.5.14 (MIT). eoka starts it with `--remote-debugging-port=0`, so DevTools listens on a random `127.0.0.1` port with no password, and finds the port in `DevToolsActivePort` inside the profile, which it keeps under `TMPDIR`. Its CDP traffic is one browser connection with flat sessions. Its stealth is launch flags, patched strings in a copy of the Chrome binary, injected scripts, and CDP methods it avoids.
- **CI:** there is none; `.github` doesn't exist.

**Frontend (`src/`)**

- **Backend calls:**
  - There are 155 `invoke` calls in 25 files and 3 `listen` calls, with no central wrapper.
  - The calls about chats, runs, overlays and CLM are in `chatApi.ts` (23), `overlay.ts` (11), `contextMode.ts` (7), `agentRuns.ts` (5, plus the one `agent-event` listener), `checklist.ts` (2), `approvals.ts`, `questions.ts`, `aiChat.ts` (`chat_context_estimate`) and `ChatPanel.tsx` (`workload_focus`).
  - `chat_messages_answered` is a read.
- **`send_chat_message`** is used only for chat titles, with explicit messages and no `dialogId` (`oneShot.ts`). No streaming call has to go remote.
- **CSP:** `connect-src` is `'self' ipc: http://ipc.localhost` (`tauri.conf.json`).
- **The chat store:**
  - `chatStore.openChat` reuses a chat it has already loaded.
  - Events add messages only to chats that are open, and patches for unknown ids are dropped.
  - `runStarted` marks any dialog id as running.
- **Starting runs:**
  - `beginRun` (`ChatPanel.tsx`) flushes the chat store's writes before `agent_start`, because "the backend reads the chat from the database".
  - ChatPanel resumes by itself any chat whose last tool calls have no results (a `createEffect` that calls `beginRun({ kind: 'resume' })`).
  - At start-up, `recoverDraftsAfterRestart` marks as failed every sent message that the database shows unanswered while no run is known for it.
- **Attachments:**
  - A message can carry ten attachments (`attachmentFormats.ts`). Images and PDFs are stored at once through `chat_store_attachment` (`composerAttachments.ts`), and the saved draft keeps their ids. Sending clears that draft and keeps a copy in `localStorage` until the message is answered (`pendingDraft.ts`).
  - Text files are read by the webview and go into the message's text as `<attached_file>` blocks, up to 1 MB each. The start carries that text two or three times: in `content` and `currentUserContent`, and in `meta.sentContent` when the editor's context is added.
  - The chat shows images from `chat-attachment://localhost/<id>`, served from the local folder, and PDFs as chips with their names and pages.
- **Local paths:**
  - The editor context sent with a message names the active tab's absolute local path (`activeContext.ts::buildContextHeader`).
  - `fileWritten.absPath` and `approval.edit.absPath` are matched against local paths, and `meta.planFile` is read from the local disk.
  - `ClmStatus.realPath` is the local mount.
- **Indicators:** the chat list shows only running and unread. Nothing shows that a chat waits for an approval or an answer. A pause that isn't a checkpoint is shown as "the agent seems stuck".
- **`agent_configure`** is sent only while ChatPanel is mounted. Every `StartRequest` carries the configuration anyway.
- **Tests** use `node:test`, with `invoke` mocked through `window.__TAURI_INTERNALS__` (`agentRuns.test.ts`).

---

## 3. The shape of the system

```
 Desktop app (one per computer)                     Server (one daemon per person's account)
 ──────────────────────────────                     ────────────────────────────────────────
 webview: invoke / listen('agent-event')            rovik-agentd serve        (rovik-agentd@<user>)
    │                   ▲                             Axum on ~/.rovik/agentd.sock (0600)
 Tauri commands (app crate)                             /rpc     WebSocket: the frame stream
   route(chat) ──► local:  rovik_core::dispatch          /call    calls too big for a frame
             └───► remote: rovik_client::Host ──┐        /packs   git packs, up and down
                     ├ mirror → local chats.db   │        /chats   the bundle that moves a chat
                     ├ shadow repo, packs        │        /blobs   attachments: screenshots, images, PDFs
                     └ events → 'agent-event'    │        /health
                                                 │     rovik_core::Core  (~/.rovik/data)
                                                 │     ~/.rovik/projects/<project>.git  (bare)
       russh, one connection per host:           │     ~/.local/share/rovik/work/<chat>/ (worktrees)
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
  the key store, each chat's computer and run settings, the projects' bare
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
    /// Client → daemon, first frame. `device` names the computer, and
    /// `token` is its token (D5).
    Hello { protocol: u32, client: String, device: String, token: String },
    /// Daemon → client. `boot` changes on every daemon start, so a client
    /// knows its `since` is from another process.
    Welcome { protocol: u32, min_protocol: u32, daemon: String, boot: String, features: Vec<String> },
    Call { id: u64, req: Request },
    Reply { id: u64, res: Result<Response, Fail> },
    /// After `Subscribe`: the events of this computer's chats, numbered
    /// per daemon process.
    Event { seq: u64, ev: Event },
    Subscribe { since: Option<(String, u64)> }, // (boot, seq)
    Subscribed { replayed: u64, complete: bool },
    Ping, Pong,
}

pub struct Fail { pub code: FailCode, pub message: String }
pub enum FailCode { Unreachable, NotOwner, Busy, Version, BadRequest, TooLarge, Internal }
```

A frame is at most 1 MB, so that a big transfer never holds up events and
pings. Anything bigger goes over HTTP, each request on its own SSH channel
(K41):

- Bundles, attachments and packs always do (4.4).
- A call whose JSON is over 1 MB goes as `POST /call`, with the same
  `Call` as its body and the `Reply` as its response. A message with
  attached text files can be that big, since the start carries its text
  two or three times (section 2). `/call` takes up to 64 MB. Long pastes
  have no limit of their own, so a remote send can be bigger than that;
  it is refused with the reason: "This message is too big to send to
  <host>: N MB, 64 MB at most."
- A reply over 1 MB comes back as `TooLarge`, and the client repeats the
  call through `POST /call`. Only reads have replies that big
  (`AgentStatus`, whose edit approvals carry whole files,
  `ChatChangesSince`, `WorkFile`, `ClmReadRevision`), so repeating them
  is safe.
- An event is at most 1 MB too. In place of a bigger one, such as a
  `MessagesAdded` with a long message or an edit approval of a big file,
  the daemon sends `Resync { dialog_id }`. The client catches that chat
  up as it does after `complete: false` (4.3): `AgentStatus` for what is
  waiting, a pull for the rows, and the events the frontend is missing.
  The daemon never sends `FileWritten`, which carries whole files and
  which the client would drop (6.1).

The app turns each `Fail` into the `String` error its command returns
today. `Unreachable` reads: "<host> isn't reachable. This chat is
read-only until it is."

### 4.2 Requests

| Group | Requests                                                                                                                                                                                                                                                                                                                                                                 | Serves                                                                                                                                         |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Agent | `AgentStart(StartRequest)`, `AgentStop`, `AgentPause` (new, M03), `AgentResolveApproval`, `AgentAnswerQuestions`, `AgentStatus`, `AgentConfigure(AgentConfig)`, `AgentSetEffort`, `ChatChecklist`, `ChatDismissChecklist`, `WorkloadFocus`                                                                                                                               | The commands of the same names. `StartRequest` gains `project_settings: Option<Value>` (K03) and the start kind `CheckMerged { files }` (D13). |
| Chat  | `ChatChangesSince { dialog_id, rev, cursor }` → a page of a `ChangeSet` (at most 512 KB of rows, or one bigger row) and whether `more` follows; `ChatRename`; `ChatUpdateMessage`; `ChatSetModel`; `ChatDelete`; `ChatOwner { dialog_id }`; `ChatRelease { dialog_id }`; `ChatForget { dialog_id }`; `BlobsMissing { ids }` → the attachments the daemon has no file for | The mirror (6.3), writes to a remote chat, the move (6.8), attachments (6.11). The bundle that moves a chat goes over HTTP (4.4).              |
| CLM   | `ClmStatus`, `ClmSetMode`, `ClmReset`, `ClmListRevisions`, `ClmReadRevision`, `ClmRestoreRevision`                                                                                                                                                                                                                                                                       | The `clm_*` commands. `ClmStatus.real_path` is dropped for remote chats.                                                                       |
| Work  | `ProjectEnsure { project, settings, excludes }`, `WorkStatus`, `WorkFile { path }`, `WorkSync { snapshot, excludes }`, `WorkRecheck { snapshot }`, `WorkResult`, `WorkResults`, `WorkLanded { commit, paths }`, `WorkDiscard`, `WorkConflictsToChat { files }`                                                                                                           | The project transport (6.4–6.7), and the isolation bar's remote half. `WorkResults` lists the newest result of each of this computer's chats.  |
| Host  | `HostInfo` (versions, platform, virtual files, Chrome, user namespaces, the install and its administrators, other users' daemons), `KeysList` (provider ids and base URLs, never keys), `KeysRemove`, `Storage`, `Gc`, `Drain`                                                                                                                                           | The hosts page, deploy and upgrade.                                                                                                            |

On both sides `rovik_core::dispatch` answers the Agent, Chat and CLM
groups, and the daemon adds the Work and Host groups. Some chats are
refused with `NotOwner`:

- on the desktop, a chat that has a `location` (K21);
- on the daemon, a chat that another computer moved there;
- on the daemon, a chat it has released. Reading its changes stays
  allowed, for the final catch-up.

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
    /// In place of an event too big for a frame (4.1): catch this chat up.
    Resync { dialog_id: String },
}
```

- **What a client gets:** only the events of the chats its computer moved
  to the daemon, plus host notices.
- **Streaming patches are coalesced (K28).** For each message the daemon
  keeps the newest `MessagePatched` and sends at most four a second. The
  ring keeps only the newest patch per message, so a long reply neither
  floods a slow link nor pushes everything else out of the ring.
- **The ring and catch-up:**
  - The daemon keeps the last 2,000 events or 8 MB, whichever is less.
  - When a client subscribes with a `(boot, seq)` from this process, it
    gets the events it missed (`complete: true`).
  - Otherwise it gets `complete: false` and catches up from the database:
    `AgentStatus` for what is running and waiting, `ChatChangesSince` for
    every chat it mirrors, and `WorkResults` for results it hasn't
    imported.
  - This follows the rule in `events.rs`: "the database stays the source
    of truth for anything it misses".

### 4.4 HTTP

| Endpoint                                               | Purpose                                                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /health`                                          | No auth. Returns `{ ok, protocol, version }`.                                                                                                                                                                                                                                                             |
| `GET /rpc`                                             | The WebSocket of 4.1.                                                                                                                                                                                                                                                                                     |
| `POST /call`                                           | A call too big for a frame (4.1): the `Call` as JSON in, its `Reply` out. Its token names the computer, as on the WebSocket.                                                                                                                                                                              |
| `POST /chats/import?move_id=<id>`                      | The bundle that moves a chat (6.8), streamed and imported in one transaction.                                                                                                                                                                                                                             |
| `GET` and `PUT /blobs/<sha256>`                        | Attachments, addressed by content as in `chat_db/attachments.rs`: screenshots, and the images and PDFs attached in the chat input (6.11). A `PUT` is refused when its body doesn't hash to its name, and is stored as `chat_store_attachment` stores a file: 20 MB at most, the type read from the bytes. |
| `GET /packs/<project>/refs`                            | The tips the daemon has for the project: snapshots, results, branches.                                                                                                                                                                                                                                    |
| `POST /packs/<project>?ref=<name>&old=<oid>&new=<oid>` | A pack for the daemon to index. The ref moves only if the new commit is complete and the ref still points at `old`.                                                                                                                                                                                       |
| `POST /packs/<project>/fetch`                          | A JSON body of wants and haves; the response is a pack.                                                                                                                                                                                                                                                   |

Every request except `/health` carries `Authorization: Bearer <token>`.
Requests time out when no bytes have moved for 60 seconds, not after a
fixed time: a big upload over a slow link takes minutes.

### 4.5 Versions

`PROTOCOL` starts at 1. Additive changes keep the number: new optional
fields, and new requests the client checks for in `Welcome.features`.
Anything else increases it. A daemon refuses a client older than its
`min_protocol`. The client refuses to move a chat to a daemon with a lower
protocol than its own and offers to upgrade it. When the daemon is newer
and still compatible, nothing needs to happen.

A new field that changes what a request means needs a feature too, even
when it is optional: serde drops fields it doesn't know, so an older
daemon would act on the request without it. A message's attachments are
such a field: a daemon that didn't know them would send the message to
the model without its files (K46).

---

## 5. The server

### 5.1 Layout

```
~/.rovik/                     0700, the user's own; the daemon refuses anything looser.
                              Commands that name it need the user's approval (D8).
  agentd.sock                 0600
  agentd.lock                 one daemon per account
  config.toml                 one token hash per computer (read at start), limits, log level
  state.json                  per chat: its computer, project, worktree, landing base,
                              newest result, last start's settings (no keys, no message text), running,
                              restarts, released; and each computer's last AgentConfig (no keys)
  keys.json                   0600; created by the first cloud start (D11)
  data/                       the daemon's Core: chats.db, attachments/, agent-log.jsonl, backups/
  config/project-settings.json  keyed by worktree path (K03)
  projects/<project id>.git   bare repository per project; info/exclude holds the client's patterns
  results/<chat id>.index     the index results are committed through (6.5)
  logs/agentd.log             rotated at 5 MB, 3 kept
~/.local/share/rovik/         0700: what the agent works in
  work/<chat id>/             worktree per chat
  tmp/                        TMPDIR for the daemon and the agent's commands (K13)

/opt/rovik/bin/               root's, installed once per server (D17)
  rovik-agentd-<version>      its signature checked at install and at every upgrade
  rovik-agentd                a link to the current version
/etc/rovik/agentd.toml        root's: the administrators and the limits (5.5)
/etc/systemd/system/          rovik-agentd@.service, its limits drop-in, rovik.slice (5.3)
/etc/sudoers.d/rovik          `user` commands for everyone, `admin` commands for rovik-admin
```

### 5.2 Start-up of `rovik-agentd serve`

1. Set `umask 077`. Call `process::adopt_login_shell_path()`. When `$SHELL`
   is unset, take the shell from the passwd entry (K12). This runs first, while the process is still
   single-threaded, because it sets an environment variable.
2. Check that `~/.rovik` and `~/.local/share/rovik` are the user's own
   with mode 0700, and stop with a clear message if either isn't. Take
   `agentd.lock`.
3. Set `TMPDIR=~/.local/share/rovik/tmp`.
4. Read the token hashes from `config.toml`. They are read only here, so
   a hash added later takes a restart.
5. Build a multi-thread runtime, call `rt::init`, then `Core::new(paths, sink)`.
6. If `/dev/fuse` works, mount the virtual-files volume, so CLM chats can
   use it.
7. Restart interrupted runs (5.4).
8. Bind the socket. A stale socket file is removed only after connecting
   to it fails. Then serve.

### 5.3 Installing on a server, and running as a service

A server gets Rovik once, and its first user becomes Rovik's
administrator there (D17). A deploy to a server without
`/etc/rovik/agentd.toml`:

1. Uploads the release binary to a temporary name in the user's home,
   after checking its signature (B02).
2. Runs `sudo <that file> install --admin <user>`. If `sudo -n` isn't
   enough, it asks for the user's sudo password, which goes to that one
   command and is never stored. The installer:
   - copies the binary to `/opt/rovik/bin/rovik-agentd-<version>`, owned
     by root, and points `/opt/rovik/bin/rovik-agentd` at it;
   - writes `/etc/rovik/agentd.toml`, with the user as administrator and
     the limits the deploy dialog showed (5.5);
   - writes the unit below with its limits drop-in, and `rovik.slice`;
   - creates the `rovik-admin` group with the administrator in it, and
     writes `/etc/sudoers.d/rovik` once `visudo -c` accepts it;
   - enables and starts the user's instance.
3. Without sudo, the deploy stops and shows that command, for someone
   with root to run, with the release's public key and the `minisign -V`
   line that checks the file first. The deploy goes on once it has run.

A deploy to a server that has Rovik starts the user's own instance with
`sudo -n /opt/rovik/bin/rovik-agentd user start`. The sudoers rule lets
everyone run the `user` commands without a password, and they act only
on the instance of the user that sudo names (`SUDO_UID`). The daemon
never runs as root: over a root login, the deploy stops and asks for a
regular account, which an install from root can name as administrator.

```ini
[Unit]
Description=Rovik agent daemon for %i

[Service]
User=%i
ExecStart=/opt/rovik/bin/rovik-agentd serve
Slice=rovik.slice
PrivateTmp=yes
# A command killed for using too much memory doesn't stop the daemon,
# and the daemon is the last process picked: it sets its children's
# score back to 0, which needs no privilege.
OOMPolicy=continue
OOMScoreAdjust=-500
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

- **Limits** (5.5) go in a drop-in, `rovik-agentd@.service.d/limits.conf`,
  as `MemoryMax=`, `TasksMax=` and, when the administrator sets one,
  `CPUQuota=`. The cap on all of Rovik goes in `rovik.slice`. The agent's
  commands, Chrome and the language servers stay in their daemon's
  cgroup, so the limits cover them too.
- **No lingering:** the instance starts at boot. `PrivateTmp=yes` gives
  each person's daemon and commands their own `/tmp` (K13).
- **The administrator** changes limits and upgrades Rovik with
  `sudo -n /opt/rovik/bin/rovik-agentd admin …`, which the sudoers rule
  allows the `rovik-admin` group. For an upgrade, the installed binary
  checks the new release's signature with the key compiled into it,
  installs it beside the old one, drains every instance (R02), moves the
  link and restarts them. Removing Rovik from the server is an admin
  command too. A user's own removal stops their instance and deletes
  their folders.
- **Without systemd**, as in a container, the installer still places the
  binary and `agentd.toml`, and the client starts the user's daemon with
  `setsid -f` when it connects and finds none running. Runs then come
  back after a reboot only at the next connection, and only the daemon's
  own limits apply (slots, worktrees, disk). The connection test says so.

### 5.4 Restarting interrupted runs

For each chat in `state.json` marked `running`, the daemon looks at its
stored rows and picks what to do:

| Last stored state                                                                       | What the daemon does                                                                                                                                                                                                                            |
| --------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A reply that isn't finalized: the process died while it streamed                        | Settles it the way `run.rs::store_failed_turn` settles a failed turn: with text, it becomes an interrupted reply; empty, it is marked failed. Then it starts `ContinueReply` or `Retry`, the choice the chat's Continue and Retry buttons make. |
| A finalized reply whose tool calls have no results                                      | `Resume`, which asks the user before running an interrupted call again                                                                                                                                                                          |
| A `paused` record of kind `drain`: an upgrade paused it (R02)                           | `ContinueRun`                                                                                                                                                                                                                                   |
| Another `paused` record                                                                 | Nothing: the run had paused, or the chat moved away                                                                                                                                                                                             |
| Anything else: the process died during the end-of-run checks, or in a `CheckMerged` run | Clears `running`, commits the result with outcome `interrupted` and sends `ResultReady`. The result waits for the user.                                                                                                                         |

Each start is rebuilt from the chat's saved settings, its computer's
`AgentConfig` and `keys.json`. A chat restarted twice within an hour isn't
restarted a third time: it gets a notice ("stopped after the server
restarted twice") and waits for the user. Approvals and questions that
were pending are asked again by `Resume`.

### 5.5 Team hosts (D10, D17)

- Each user's daemon is independent, and nothing of theirs is shared.
- **One binary for everyone,** owned by root and checked at install and
  at every upgrade (5.3). On every connection, the client checks over SSH
  that the unit runs it and that its hash is the release's (H05).
- **Limits,** in `/etc/rovik/agentd.toml`, set by the administrator from
  the hosts page and binding on every user:
  - per user: `max_slots`, `max_worktrees` and `max_disk_gb`, which the
    daemon enforces, and memory, task and CPU caps on their daemon's
    cgroup (5.3);
  - for all of Rovik: memory and CPU caps on `rovik.slice`.
  - The installer sizes the defaults to the machine: per user, half the
    memory and an equal share of the CPUs; for all of Rovik, 80% of the
    memory. The deploy dialog shows them to the administrator.
- **Heavy-work slots:** they default to 1 while other users' daemons run
  on the host (found through `/proc`). Otherwise the existing automatic
  rule applies.
- **The administrator** is the first user, or whoever an install from
  root names. Admin commands change the limits, the version and the list
  of administrators, and nothing else: being Rovik's administrator gives
  no access to other users' files.
- **What can read what:** the agent's commands run as the user and can
  read whatever the user can. Worktrees and keys sit under 0700 folders,
  so other users can't read them, but root can. The hosts page says:
  "Your API keys are stored in your account on this server. The agent's
  commands there and anyone with root on it can read them."
- **Ports are shared.** A dev server, or anything else the agent's
  commands open on a port, can be reached by every user of the server,
  and two people's agents can clash on one port. Chrome's DevTools opens
  none (D18).

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
from `dialogs.location`. A command keyed by a message id finds its chat
from the message's row in the local database first. A local chat goes to
`rovik_core::dispatch`, a remote one to its host's connection. For a
remote chat each command does the following:

| Command                                                                                                                                                                  | For a remote chat                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `agent_start`                                                                                                                                                            | Checks `chat_may_use` locally as today. Syncs before a `Send` (6.7). In `current_user_content` and the message's `meta.sentContent`, replaces the local project root with nothing, so the editor context names `src/x.rs`, not `/home/me/p/src/x.rs` (K08). Adds the project's settings. Applies the host's approval policy (6.9). Uploads the message's images and PDFs that the host doesn't have (6.11). For a start other than `Send`, leaves out `current_user_content` when it is what the daemon already has (the stored `meta.sentContent`, else the text), so a Continue doesn't send a message's text files again (K50). Then sends `AgentStart`. |
| `agent_stop`, `agent_resolve_approval`, `agent_answer_questions`, `chat_dismiss_checklist`, `chat_rename_dialog`, `chat_update_message`, `chat_set_model`, `clm_*`       | Forwarded to the owner; the mirror follows. `agent_stop` first cancels a send whose files are still uploading (6.11).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `agent_status`                                                                                                                                                           | The local status plus each connected host's.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `agent_configure`, `agent_set_effort`                                                                                                                                    | Applied locally and sent to every connected host. `agent_configure` goes without keys, and the daemon fills them in (D11). It replaces only this computer's configuration there (D16).                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `chat_get_messages*`, `chat_get_message_body`, `chat_messages_answered`, `chat_list_dialogs`, `chat_search`, `chat_checklist`, `chat_context_estimate`, `chat_get_model` | Read from the local mirror.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `chat_save_composer_draft`, `chat_get_composer_draft`, `chat_create_dialog`, `chat_store_attachment`                                                                     | Local only. Drafts belong to the computer, and new chats start local. The input's files are stored here and go to the host with the message that sends them (6.11).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `chat_delete_dialog`                                                                                                                                                     | The owner stops the run and deletes the worktree and the rows. The local rows go at once. If the host is unreachable, a tombstone is sent when it reconnects.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `chat_delete_project`, and the project answers that merge two projects (`projects::merge`)                                                                               | Refused while the project has remote chats, with the reason (K38).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `overlay_status`                                                                                                                                                         | The imported result's status (6.5) combined with `WorkStatus`: the files the chat changed in its worktree so far, and the sync state.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `overlay_file`                                                                                                                                                           | `WorkFile` gives the base and the chat's version; the project's version is read locally.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `overlay_land`, `overlay_undo`, `overlay_conflict_file`, `overlay_conflict_resolve`                                                                                      | Local, on the imported result. "Is the run going?" is asked of the merged status, not only the local `AgentState`. A landing is reported with `WorkLanded` (6.5).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `overlay_discard`                                                                                                                                                        | `WorkDiscard` on the owner, and the local import is dropped.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `overlay_conflicts_to_chat`                                                                                                                                              | `WorkConflictsToChat`, with the local project's versions of the files.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `overlay_redo_group`, `overlay_undo_merge`, `overlay_unmark`                                                                                                             | Refused for cloud chats in v1, with the reason.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `chat_read_plan` (new, T10)                                                                                                                                              | `WorkFile` on `.rovik/plans/…`. A local chat reads the file, as `readFileContent` does today.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `workload_focus`                                                                                                                                                         | Applied locally and on the owner.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |

The client re-emits each remote `AgentEvent` on `agent-event` in the order
it came, with these exceptions:

- **`MessagesAdded`:** its rows are written into the mirror first, then
  the event goes on (K22). The rows are the owner's stored rows; the next
  pull completes them with bodies and tool-call extras. Attachments they
  name that have no file here are fetched first (6.3), so the rows go in
  whole: a patch can't add attachments later, and the chat would show a
  screenshot taken on the server as a broken image.
- **`FileWritten`** of a remote chat isn't sent (4.1). It names a path on
  the server, and the local project changes only when the result lands,
  which the file watcher then sees.
- **`Resync`** catches the chat up as after `complete: false` (4.3).
- **`Notice`** gets the host's name in its title.
- **`ApprovalRequested`** with an edit gets `remote: true`, so the editor
  shows the change in a compare view and not on the local file's tab
  (K09).
- **`ContextUpdated`** and CLM statuses lose `real_path`.
- **Events for a chat this computer doesn't mirror** are dropped. The
  daemon already filters by computer; this is a second guard.

When a host connects, the client sends events that bring the frontend up
to date: `runStarted`, `approvalRequested` and `questionsAsked` for the
host's current state, and `runEnded` for runs that ended while it was
away.

### 6.2 A remote start, on the daemon

1. The daemon takes the keys out of the start: the profile, the approval
   profiles and the subagent kinds' models. It writes them to `keys.json`
   under each provider id (the part of the profile id before `::`) with
   the base URL each came with, replacing older entries.
2. It stores the start's settings, without keys or the message's text,
   in `state.json`. The
   start's `config` becomes its computer's configuration, without keys.
   The chat is marked `running`.
3. It sets `root` to the chat's worktree, or to the project's subfolder in
   it (6.4), and `isolate` to `Some(false)`. It writes `project_settings`
   under that path in `config/project-settings.json`.
4. It calls `dispatch(AgentStart)`. The run reads its configuration with
   `AgentState::config_for(dialog)`, which on the daemon returns the chat's
   computer's configuration and on the desktop returns the one
   configuration. When `RunEnded` arrives, it clears `running` and commits
   the result (6.5).

### 6.3 The mirror

- **Revisions:** `SCHEMA_V10` adds a `sync_clock` and a `rev` column to
  `dialogs`, `messages`, `clm_state` and `clm_revisions`. Triggers set
  `rev` on every insert and update. Writes to `message_bodies`,
  `message_attachments` and `tool_call_extras` raise the `rev` of the
  message they belong to. Triggers keep this right even for code added
  later that writes with plain SQL.
- **Changes:**
  - `chat_db::sync::changes_since(conn, root, rev, cursor)` returns, in
    pages of at most 512 KB (one row when a row is bigger), every row of the
    chat's subtree with a higher `rev`: dialogs, messages with their full
    bodies, attachment ids, tool-call extras, CLM state and CLM revisions.
  - It also lists the CLM revision numbers that still exist, since the
    owner deletes old ones.
  - `apply` writes the rows as the owner had them: upsert by key, with the
    owner's `seq` and `created_at`, and search rows and previews rebuilt.
    It keeps the local `project_id` and `location`, and new child chats
    get the parent's. It deletes CLM revisions the owner no longer has.
  - Before writing, `apply` checks in the same transaction that the chat
    is still at that host, so a late pull can't overwrite a chat that has
    moved back.
  - Applying the same changes twice changes nothing.
- **When it pulls:**
  - On `ChatChanged`, with 500 ms of debounce, on every connection, and on
    `RunEnded`.
  - The daemon sends `ChatChanged` with its current `sync_clock` after any
    event about a chat (at most twice a second per chat) and after each
    write request it handles.
  - A checkpoint write sends no event of its own, but the stream's
    `MessagePatched` events arrive with it, so it is still pulled. A
    streaming reply raises `rev` only at its 2-second checkpoints, so a
    long reply costs one small pull every 2 seconds at most.
- **After each pull,** the client emits `messagesAdded` for new rows and a
  full `messagePatched` for changed ones. A chat the frontend opened
  between an event and its rows still ends up with what the database has
  (K22). The chat store already treats these as idempotent.
- **Where the position is kept:** `mirror_state(dialog_id, host, rev,
imported_result, imported_seq, unreported_landing)`, updated in the same
  transaction as the rows. The last three are 6.5's: the result imported
  last and its number, and a landing its host wasn't told of yet.
- **Attachments** with no file here are fetched from `/blobs/<sha256>` and
  stored, with their `attachments` rows, before the `message_attachments`
  rows that reference them are applied. A file the owner can't serve gets
  a row without a file (6.11), so one lost file never stops a pull.
- **One writer:** `dispatch` refuses local writes to a chat that has a
  location, so the mirror is the only thing that writes its rows (K21).

### 6.4 Snapshots and packs

**Snapshot, on the client.**

- The shadow repository has the project's repository folder as its work
  tree (`core.worktree`), and it is never checked out into.
  - For a git project, `objects/info/alternates` names the project's
    object folder (found through `Repository::discover` and `commondir`,
    so linked worktrees work).
  - A project that is a subfolder of a repository (`git_subpath`, K31)
    snapshots the whole repository, so the ignore rules above it apply and
    the history matches. On the server its chat's root is that subfolder
    of the worktree. A result's changes outside the subfolder are listed
    and never landed.
  - The project's `info/exclude` is copied into the shadow's before each
    snapshot. The user's global excludes come from their git config, as
    usual.
  - These client-only patterns are also sent to the daemon with
    `ProjectEnsure` and `WorkSync`, which writes them into the bare
    repository's `info/exclude`, so both sides ignore the same files
    (K32).
  - **Line endings (K40):** `core.symlinks` and `core.filemode` follow the
    OS. `core.autocrlf` and `.gitattributes` are taken from the project,
    so blobs hold what `git add` would store: text with LF where the
    project's settings normalise it. The server checks out with
    `core.autocrlf=false`.
- **The index is seeded from the project's `HEAD`** (`Index::read_tree`)
  when the shadow is created. Without that, `add_all` would leave out
  tracked files that match an ignore rule, and new entries would lose
  their modes (K30).
- A snapshot updates the shadow index (`add_all`, then `update_all` for
  deletions) and writes its tree.
  - The parent commit is the project's `HEAD` when the project is a
    repository whose `HEAD` and its objects can be read. Otherwise it is
    the chat's previous snapshot, or none.
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
    project without history. That choice is kept for the project: later
    snapshots chain onto the previous snapshot instead of `HEAD`.
  - Nested repositories and submodules aren't carried in v1, and the move
    dialog lists them (section 11).
  - Text files with CRLF that the project doesn't normalise are listed by
    the move dialog: shell scripts among them won't run on Linux.
- **Upkeep:** the shadow's loose objects are packed once a day.

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
  at `~/.local/share/rovik/work/<chat>` (`Repository::worktree`).
- The daemon writes `.rovik/project.json` there from the project row.
  `.rovik/.gitignore`, which the snapshot carries, keeps it out of
  results.
- The chat's root (the worktree, or its subfolder) is added to
  `project_paths`, so `projects::chat_may_use` accepts it.

### 6.5 Results, and landing them

**On the daemon.**

- When a remote run ends, and on `WorkResult`, the daemon commits the
  worktree as `refs/rovik/chats/<chat>/result`. The commit's parent is
  the landing base, and its tree is the worktree minus ignored files.
- It builds the tree through its own index file (`results/<chat>.index`,
  set on its own `Repository` handle with `set_index`).
  - The index is seeded from the landing base's tree. Otherwise a file in
    the base that an ignore rule matches would drop out, and the drop
    would land as a deletion of the user's file (K30).
  - The agent's `git status` in the worktree never changes under it, and
    later results hash only the files that changed.
- `state.json` keeps the newest result, its outcome and its pending
  reason. The mapping from outcome to reason is taken out of
  `landing.rs::try_land` into one function that both sides use. Then the
  daemon sends `ResultReady`.
- **`WorkLanded { commit, paths }`** tells the daemon that a result
  landed. For those paths, the landing base moves to the result's
  content, so `WorkStatus` stops listing them. An Undo of that landing
  then reaches the worktree at the next sync as the user's own change, and
  the next result doesn't land the undone change again (K34).

**On the client, while connected.**

- **Finding results:** `ResultReady` while connected, and `WorkResults` on
  every connection. Each chat's results are numbered (`seq`), and any
  result newer than the one `mirror_state` records is imported, so a
  result that ended while the laptop slept isn't lost (K24). An older
  `ResultReady` replayed after a newer result is passed over.
- **Importing:**
  - The client fetches the result into the shadow repository.
  - `overlay::import` builds the chat's local overlay store from the
    landing base tree and the result tree: uppers from the result, base
    copies from the base, `project.txt` set to the local project, and no
    mount.
  - The files are written through a libgit2 checkout into a temporary
    folder (`CheckoutBuilder::target_dir`), so line endings come out as
    the user's own checkout would make them.
  - The store also gets the list of files with conflict markers
    (`markers.json`) from the daemon's sync state (6.7).
- **An import over an existing store** replaces it, unless the user is
  resolving conflicts in it. Then the bar says a newer result is ready
  and lets the user choose.
- **Landing** is today's code from here on.
  - It lands by itself under `landing.rs`'s rules: an outcome in
    `LANDS_AFTER`, no conflict markers, no conflicts, and the plan not
    `merged`, and only while the chat's project is the open project.
  - Conflicts go to the isolation bar, as they do now.
  - A `merged` plan goes to 6.6 when the chat's checks would run, which
    the result says (`unchecked` when they wouldn't). Without checks, it
    lands merged, as a local chat's does.
  - After a landing, the client sends `WorkLanded`. A landing its host
    can't be told of yet waits in `mirror_state.unreported_landing` and is
    sent after the next connection, so an app restarted meanwhile still
    tells it (K34).
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
   `merge::merge_bytes`. That gives no text on a conflict and refuses
   binary files.
   - **Nothing conflicts:** every path is applied. The branch moves to the
     new snapshot and the worktree's index is reset to it (mixed), so
     `git status` still shows exactly the chat's changes. Merged files are
     listed for the bar.
   - **A binary conflict, or a file deleted on one side and changed on
     the other:** nothing is applied, whatever the setting. These stay
     with the user, as they do locally.
   - **Text conflicts, and the chat resolves conflicts:** every path is
     applied, with conflict markers in the files that conflict. The
     markers are written by the text builder taken out of
     `overlay::chats::mark_one`, with the same labels, so `markers_left`
     finds them (K33). The next start carries a note listing the files.
     The daemon keeps the list, and landing is refused while markers
     remain.
   - **Text conflicts, and the setting is off:** nothing is applied, and
     the base stays where it was. A `SyncState` event tells the bar:
     "Sync waits: N files changed both here and in the cloud chat", with
     "Hand to the chat" and "Keep the older copy".
3. The run goes on in every case.

### 6.8 Moving a chat

```
local ─► preflight ─► lock ─► pause ─► snapshot+upload ─► blobs ─► import ─► flip ─► start ─► remote
                 (until "flip", a failure or "Cancel move" makes the chat local again)
```

1. **Preflight** (`cloud_move_preflight`).
   - Checks that the host is connected and its protocol is compatible,
     and that the chat has a project; chats without one can't move in v1.
   - Works out what will wait for the user, from the permissions and the
     host's approval policy (6.9).
   - Notes that CLM becomes trimming if the host has no virtual files.
   - Lists what 6.4 lists: large files, nested repositories, and CRLF
     scripts.
   - Notes that a parallel group's "Redo sequentially" won't be available
     after the move.
   - Checks that no conflicts are pending in the chat's overlay; they have
     to be resolved first.
   - Counts the attachments the host doesn't have, with their size
     (`BlobsMissing`), for the dialog.
2. **Lock** (K25). The client sets `dialogs.location` to `moving:<host>`
   and writes a `cloud_moves` row: the chat, the host, a `move_id` and the
   step reached. `dispatch` refuses the chat from here on, and the
   interface shows it read-only, so no run can start in the middle of the
   move.
3. **Pause.** If a run is going, the client sends `AgentPause` and waits
   for `runEnded` with outcome `paused-move`. While it waits, the dialog
   says what it is waiting for (a command, a subagent) and offers "Stop
   now".
4. **Snapshot and upload** (6.4), with the overlay folded in.
5. **Attachments** the host doesn't have are uploaded: every file the
   subtree's messages reference, as 6.11 says.
6. **Import.**
   - The bundle goes to `POST /chats/import?move_id=…`. It carries
     `changes_since(0)` of the chat's subtree, the project row, the
     project's settings and exclude patterns, the start and base commits
     and the overlay's pending reason.
   - The daemon imports it in one transaction and records the chat's
     computer and `move_id`. It creates the worktree before it commits.
   - A repeated `move_id` answers "already imported".
   - A chat the daemon released earlier is cleared first: its rows,
     worktree, branch and refs (K36).
   - A chat another computer owns there is refused.
   - An attachment the bundle references that the daemon doesn't have
     gets a row without a file (6.11).
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
- If the host doesn't know the chat, the move is rolled back:
  `location` goes back to NULL.

If the host stays unreachable, "Cancel move" makes the chat local again
and queues a `ChatRelease` for that `move_id`, sent when the host comes
back.

**Moving back.**

1. Pause on the daemon (`AgentPause`).
2. `WorkResult` commits the worktree as it is.
3. `ChatRelease`. The daemon marks the chat released and refuses further
   writes. It removes the worktree after 7 days, or on "Remove now".
4. A final `ChatChangesSince`. It is complete, because nothing can change
   on the daemon any more.
5. Fetch the result and import it as the chat's local overlay. The chat's
   changes now wait as a local isolated chat's pending changes. On a
   computer without the virtual-files driver, an isolated run can't use
   them, so they land first (or show their conflicts).
6. Flip `location` back, and send `ChatForget`: the daemon deletes the
   chat's rows then, which frees their attachments for the cleanup (R01).
   Continue the run locally with `ContinueRun` if it paused for the move.

The move back has its `cloud_moves` row too (K51). After a crash past
`ChatRelease`, the next start finishes it from step 4: the daemon keeps
the rows until `ChatForget`.

Moving changes no messages. The transcript shows where the chat moved,
because the `move` pause record names the host in its own `host` field.
It is not in `reason`, which would make the run ask the model for a
summary (K29).

### 6.9 While nobody is there

- A remote run never approves anything by itself.
- Each host has an approval policy, chosen on the hosts page and shown in
  the move dialog:
  - "approvals go to the AI reviewer, then to me", or
  - "approvals wait for me".
- The client applies the policy to the `AgentConfig` it sends that host.
  The daemon keeps one configuration per computer (D16), so the policy
  covers that computer's chats on the host and nothing else.
- Commands that name `.rovik` always wait for the user (D8).
- A chat waiting for an approval or an answer shows a badge in the chat
  list, whether it is local or remote (U03). When the app reconnects, one
  notice sums up what waits (U06).

### 6.10 Offline

- A remote chat whose host can't be reached is read-only. It reads from
  the mirror, and its composer says the host is offline.
- Stop, approvals and sending fail with the reason, and the chat list
  shows the host as offline.
- At start-up, draft recovery leaves remote chats alone until their host
  has connected and the mirror has caught up. Only the owner knows
  whether a message was answered (K23).
- The client reconnects with backoff from 1 s to 60 s. It reconnects at
  once when a timer gap over 30 s shows the laptop slept.

### 6.11 Attachments

The chat input attaches images, PDFs and text files (section 2). A text
file is part of its message's text and travels with it; only its size
needs the rules of 4.1. Images and PDFs are files of their own, stored on
the computer where they were attached, while a remote chat's requests to
the model are built on the server, from the server's files.

- **Attaching stays local.** `chat_store_attachment` stores the file on
  this computer, as today. The input shows it from the local folder, and
  the local draft keeps it from the local cleanup. Nothing goes to the
  host before the message is sent.
- **Sending uploads first** (K42). For a remote `Send` with attachments,
  `agent_start` checks the ids locally, with the error the daemon would
  give, and renews their grace period here: sending cleared the draft
  that kept them. Then it calls `BlobsMissing { ids }`. The daemon
  answers with the ids it has no file for, a row alone not counting
  (K47), and renews the grace period of the others, so its cleanup can't
  remove one between this answer and the start (K43). The client `PUT`s
  each missing file, with progress on `cloud-event`, then sends
  `AgentStart`. A failed upload fails the send with its reason, and the
  input gets its draft back (U04).
- **Stop works during the upload** (K48). The chat shows as running from
  the moment the message is sent, so Stop is offered while its files
  upload. The client keeps the pending send: Stop, or moving the chat
  back, cancels the upload and sends no `AgentStart`, and the send fails,
  so the input gets the message back.
- **The daemon checks what it receives.** `PUT /blobs` stores through
  `attachments::store_attached`, as `chat_store_attachment` does: at most
  20 MB, with the type read from the bytes. The body must hash to the
  name.
- **The model gets what it would get locally.** `vision` and `pdf` come
  with the profile in every start, the PDFs' names and pages come in the
  message's `meta.pdfs`, and the daemon builds requests with the same
  `history::to_wire`.
- **Coming back.** The message's row brings its attachment ids and
  `meta.pdfs` to the mirror, and the files are already here. Screenshots
  taken on the server are fetched before their event's rows are written
  (6.1), or before a pull's (6.3).
- **Moving** uploads every file the chat's messages reference that the
  host doesn't have (6.8).
- **A lost file never blocks** (K44). When an event's rows, a move's
  import or a pull meet an attachment the other side couldn't send, a row
  without a file is added for it. The foreign key from `message_attachments` holds, and requests
  say the file is no longer shown, as they do locally when a file is
  gone.
- **Cleanup on the daemon** has no drafts to look at. The grace period,
  renewed by `BlobsMissing`, covers a file until its message is stored.
  The cleanup runs daily, and a released chat's rows go once its
  computer has them all (`ChatForget`, 6.8), so their files go too (R01).

---

## 7. Milestones

Each task names files and has a "Done when" with its tests. "e2e" means
the scripted provider of `agent_run/e2e_tests.rs`. "Daemon test" means a
test in `crates/rovik-agentd/tests/` that starts the real binary (9).

### Milestone C: the core without Tauri (phase 1, no visible change)

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Needs | Done when                                                                                                                                                                                                                                                                                                |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C01 | Move the code that crosses the line. `ConflictFile` goes from `git_sync.rs` to `merge.rs`, which already has `MergeSegment`. `filesystem::write_app_file` goes to `file_guard.rs`, since it takes a file lock. `lsp::rust_analyzer::parse_assets` goes to a new `release_assets.rs`, used by both `lsp` and `vfs/driver.rs`. Behaviour doesn't change.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | –     | `cargo test` passes, and no core module of the design's §2.1 table imports `filesystem`, `git`, `git_sync`, `lsp`, `terminal`, `formatter`, `typings`, `project_files` or `open_url` (the PR shows the grep).                                                                                            |
| C02 | Add `rt.rs` with `spawn`, `spawn_blocking` and `block_on` over a process-wide `tokio::runtime::Handle` (`rt::init`), falling back to `Handle::current()`. `main` calls `process::adopt_login_shell_path` first, while it is still single-threaded, then builds a multi-thread runtime and hands it to Tauri (`tauri::async_runtime::set`) and to `rt::init`. Every `tauri::async_runtime::*` call in core modules goes through `rt`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | C01   | No `tauri::async_runtime` remains in core modules, the app starts and runs a chat with the login shell's `PATH`, and `cargo test` passes. `#[tokio::test]` tests are served by the fallback.                                                                                                             |
| C03 | Add `core.rs`: `Paths { data_dir, config_dir }`; `EventSink` with `fn agent(&self, e: &AgentEvent)`; `Core`, an `Arc` of paths, `db`, `agents`, `vfs`, `browser` and `events`; and `Core::new(paths, sink)`, which does what `lib.rs`'s `setup` does (open the database, `workload::init`, `overlay::chats::init`) and sets the agent log's folder. The app manages one `Core` and implements `EventSink` with `app.emit`. The app's `setup` keeps building the main window itself, with clipboard access (`create: false` in `tauri.conf.json`): pasting an image into the chat input needs it.                                                                                                                                                                                                                                                                                                                                                                                                                                  | C02   | A unit test checks that `Core::new` on a temporary folder opens the database and the overlays' folder. The app starts with it.                                                                                                                                                                           |
| C04 | The mechanical change, in one PR. Every `AppHandle<R>` and `R: Runtime` in core code becomes `&Core`: `app.state::<X>()` → `core.x`, the app's path lookups → `core.paths`, `events::emit(&app, …)` → `core.emit(…)`, `log::write(&app, …)` → `log::write(&core, …)`. `project_profile`'s settings and agent-log functions take a folder. `attachments::serve` becomes `attachments::read`, and the app builds the HTTP response. `chat_store_attachment` stays a wrapper in the app: it reads the raw request body and calls `attachments::store_attached`. `send_chat_message`'s core function takes a sender in place of the `Channel`. Every `#[tauri::command]` in a core module moves to `src-tauri/src/commands/<area>.rs` as a thin wrapper over `State<'_, Core>`. The e2e harness (`e2e_tests.rs::harness`) builds a `Core` with a temporary data folder and a sink that records events as JSON; `Harness.app` becomes `Harness.core`, and the test bodies change to match. `headless.rs` builds a `Core` the same way. | C03   | No `tauri` is left in the core modules except the moved commands. All e2e suites pass without `tauri::test`. The headless e2e test passes. Manual check: a chat runs, approvals work, Stop works, an isolated chat lands, CLM works, and a pasted image and an attached PDF are sent with their message. |
| C05 | Split the workspace. `src-tauri/Cargo.toml` gets `[workspace]` (resolver 2) with the app as the root package. `crates/rovik-core` takes the modules of the design's table and C01's moves. `crates/rovik-headless` takes the runner's binary. The `headless` feature and the `tauri/test` dev-dependency go. `scripts/eval/eval.mjs` builds with `-p rovik-headless`. `rovik-core` depends on `git2` with `vendored-libgit2` only; the app keeps `https` and `ssh`. Add `[profile.release-daemon]`, which inherits from `release` with `panic = "unwind"`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | C04   | `cargo tree -p rovik-core` shows no `tauri` and no `openssl-sys`. `cargo test --workspace` passes, `npm run tauri build` works, and the eval script builds and starts the runner.                                                                                                                        |
| C06 | Test support for other crates: a `testing` feature of `rovik-core` exposes the scripted model server (moved out of `e2e_tests.rs`), a `Core` built on a temporary folder, and `overlay::chats::TEST_DATA` (`cfg(any(test, feature = "testing"))`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | C05   | The core's e2e suites use the moved server unchanged. A test in another crate starts a scripted model and a `Core`.                                                                                                                                                                                      |

### Milestone P: the protocol and a daemon on the same machine (phase 2)

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Needs    | Done when                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P01 | `rovik-protocol`: the frames, `Request`, `Response` and `Event` (section 4). `AgentEvent` and its payload types (`OutMessage`, `Patch`, `ApprovalRequest`, …) move here from `agent_run/events.rs`, and the core re-exports them. Add `dialog_id: Option<String>` to `FileWritten` and `Notice` (K02), and add `PROTOCOL` and `Fail`, with `TooLarge` (4.1). `Patch.status` gets a double-option (de)serializer, so `null` still means "clear the status" after a round trip (K39).                                                                                                                                                                                                                                                                                                                                                                  | C05      | Round-trip tests of every variant, including `status: null`. JSON fixtures of `AgentEvent` are read by a Rust test and by an `agentRuns.ts` test, so both sides agree on the shape.                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| P02 | `rovik_core::dispatch` for the Agent, Chat and CLM groups. The app's commands for them call it. The ownership refusal of 4.2 comes with M01's column.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | P01      | The e2e suites pass through `dispatch`, with a unit test per request.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| P03 | `rovik-agentd`. CLI: `serve [--socket P] [--data DIR] [--same-machine]`, `attach --stdio [--socket P]`, `version`, `doctor`. Start-up as in 5.2, with the token hashes read once. Axum on the socket (0600, peer uid equal to its own) with `/health`, `/rpc` and `/call`, and frames of at most 1 MB (4.1). `HostInfo`. `attach --stdio` is a byte bridge. Logs go to `~/.rovik/logs/agentd.log`. `--same-machine` runs a chat in the `root` it is given, for development until T04.                                                                                                                                                                                                                                                                                                                                                                | P02      | Daemon test: `Hello` then `Welcome`; a wrong token is refused; a connection from another uid is refused (skipped when the test isn't root); a WebSocket session works through `attach --stdio`; a 3 MB call through `POST /call` gets the reply it would get on the WebSocket, and a reply over 1 MB comes back as `TooLarge`.                                                                                                                                                                                                                                                                                                                    |
| P04 | The daemon's `EventSink`: `seq` per process, a ring buffer (2,000 events or 8 MB), coalescing of `MessagePatched` (4.3), events of at most 1 MB with `Resync` in place of a bigger one, and no `FileWritten` (4.1), a broadcast to the connections of the chat's computer, `Subscribe` with `since`, and `Subscribed { complete }`. A connection that lags behind is dropped and resubscribes.                                                                                                                                                                                                                                                                                                                                                                                                                                                       | P03      | Tests: three clients of one computer get the same events in order; a client of another computer gets none of them; a 20,000-character reply streamed in 2 seconds sends at most 4 patches a second, the last one complete; a `MessagesAdded` with a 2 MB row and an edit approval of a 2 MB file each send `Resync`; a client that resubscribes with `since` gets the gap; once the buffer has wrapped, `complete` is false.                                                                                                                                                                                                                      |
| P05 | The daemon's store (6.2, D11, D16). `keys.json` (0600, atomic writes). `state.json` with each chat's computer, start settings without keys or message text, `running` and `restarts`. One `AgentConfig` per computer, without keys. `AgentConfigure` without keys is filled from `keys.json` and replaces only its computer's configuration. A key is used only with the base URL it came with. In the core, the runs read `AgentState::config_for(dialog)` in place of `config()`; it takes a resolver that the daemon sets and the desktop doesn't.                                                                                                                                                                                                                                                                                                | P03      | Unit tests: keys appear nowhere but in `keys.json` (the other files and the log are scanned); a rotated key replaces the old one; a key stored for another base URL isn't used; the file and folder modes are 0600 and 0700. Daemon test: two computers' configurations with different approval rules each apply to their own chats' runs.                                                                                                                                                                                                                                                                                                        |
| P06 | Restart interrupted runs at start-up (5.4), with every row of its table, the two-restart limit and its notice.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | P05, C06 | Daemon tests: a `kill -9` during a scripted reply that has a checkpoint makes the restarted daemon continue it (`ContinueReply`); one during a tool call makes it `Resume`, which asks for approval; one during the end-of-run checks gives a result with outcome `interrupted`; a `drain` pause continues; a chat whose run crashed twice isn't restarted.                                                                                                                                                                                                                                                                                       |
| P07 | `rovik-client`: a connection over any `AsyncRead + AsyncWrite` (a Unix socket now, SSH channels in H), `Hello` with the computer's id, calls with ids and timeouts, calls over 1 MB through `POST /call` and `TooLarge` replies repeated there (4.1), `Subscribe` with `since`, reconnection with backoff, and typed wrappers. `Hosts` keeps each host's state: `connecting`, `ready`, `offline` or `version-mismatch`.                                                                                                                                                                                                                                                                                                                                                                                                                              | P04      | Daemon test: calls and events go through the real binary. Restarting the daemon under the client makes the client reconnect and resync (`complete: false`). A read whose reply is over 1 MB arrives through `POST /call` while events keep arriving on the WebSocket.                                                                                                                                                                                                                                                                                                                                                                             |
| P08 | Routing in the app (6.1) for the Agent, Chat and CLM groups. `Hosts` as managed state. `route`, including the lookup by message id. `agent_start`'s remote preparation: `chat_may_use`, the K08 path rewrite, project settings, and the host's policy. Remote events re-emitted under 6.1's rules, with `MessagesAdded` rows written to the mirror first, and `Resync` handled as after `complete: false`. A remote send over 64 MB is refused with the reason (4.1). `current_user_content` is left out of a remote start other than `Send` when the daemon already has it. `agent_status` merged. `agent_configure`, `agent_set_effort` and `workload_focus` sent to every host. Project deletions and merges refused while remote chats exist. Until M01 exists, the developer setting `ROVIK_REMOTE_CHATS=<dialog>=<socket>` marks chats remote. | P07      | Rust tests: the event rules (`FileWritten` dropped, `Notice` titled, an edit approval marked `remote`, `MessagesAdded` rows readable from the mirror before the event is passed on); `chat_update_message` is routed by its message's chat; the path rewrite; after a `Resync`, the frontend has the long message's row and the big file's approval; a Continue after a message with text files doesn't send their text again; a project merge is refused while one of its chats is remote. Manual: a chat pointed at a daemon running with `--same-machine` runs from start to end in the app. `npm test` passes unchanged.                      |
| P09 | Attachments to and from the daemon (6.11). `GET` and `PUT /blobs/<sha256>`: a `PUT` is checked against its hash and stored through `attachments::store_attached`, and the route's body limit is raised to its 20 MB (Axum's default is 2 MB). `BlobsMissing`, which counts a row without a file as missing and renews the grace period of the files the daemon has. The client's `ensure_blobs(ids)`, with progress on `cloud-event`, which `agent_start` calls before a remote `Send` with attachments, after renewing the local files' grace period; Stop, or moving the chat back, cancels it. The `MessagesAdded` fast path fetches the files its rows name first (6.1).                                                                                                                                                                         | P08      | Daemon tests: a remote send with an image and a PDF uploads both, and the scripted model's request has an `image_url` part and a `file` part; sending the same files again uploads nothing; a file the daemon's cleanup removed is uploaded again, and so is one whose row outlived its file; Stop during a slow upload starts no run on the daemon, and the input gets the message back; a screenshot taken on the server shows as soon as its event arrives; a `PUT` whose body doesn't match its name, a Word document and a 21 MB PDF are refused; an image pasted into a remote chat's input sends nothing to the daemon before the message. |

### Milestone T: the project on the server (P1)

The design's phase 5, moved before the move itself: a moved chat needs its
project, and packs through the tunnel (D6) can be tested without SSH.

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Needs         | Done when                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T01 | Shadow repositories and snapshots (`rovik-client::shadow`, 6.4): the index seeded from `HEAD`; subfolder projects; line endings by the project's settings; the guards, including the per-project "without history" choice; the parent fallback when `HEAD`'s objects can't be read; packing loose objects.                                                                                                                                                                                                     | C05           | Tests on fixture projects. Untracked files that aren't ignored are in the snapshot, and ignored ones (the project's `.gitignore`, `info/exclude`, global excludes) are out, but a tracked file that matches an ignore rule stays in. An executable keeps its mode. After one edit, the next snapshot hashes one file (counted through a test hook). The project's `.git` is byte for byte the same before and after, and the project folder's files are untouched (contents and modification times). A subfolder project's snapshot honours the ignore rules above it. With `core.autocrlf` false, CRLF bytes are kept. A non-git project works. A symlink stays a symlink on Unix. |
| T02 | Packs: `GET /packs/<project>/refs`, `POST /packs/<project>` (compare-and-swap, completeness checked before the ref moves) and `POST /packs/<project>/fetch`, with the client's negotiation and progress on `cloud-event`.                                                                                                                                                                                                                                                                                      | T01, P03      | Daemon test: the first push of a three-commit fixture carries the history; after one edit, the next push carries one blob, the changed trees and one commit (counted in the pack); a truncated pack is refused and the ref doesn't move; a fetch gets only new objects.                                                                                                                                                                                                                                                                                                                                                                                                             |
| T03 | Projects and worktrees on the daemon (6.4): `ProjectEnsure` with the client's exclude patterns, the worktree in `~/.local/share/rovik/work` and its branch per chat, `.rovik/project.json` from the project row, and the `project_paths` row. Deleting a chat or collecting garbage removes the worktree, the branch and the refs.                                                                                                                                                                             | T02           | Daemon test: the chat's worktree has the snapshot's files and `.rovik/project.json` with the project's id; `chat_may_use` accepts the chat's root; a file matching only the client's global excludes, created on the server, stays out of the result; after a delete, all of it is gone.                                                                                                                                                                                                                                                                                                                                                                                            |
| T04 | Worktree mode in the run (D8). The daemon's `AgentStart` sets `root` and `isolate`. Add `command_problem` for worktrees: git commands that change the repository are refused, reusing `git_changes_repo`. A command naming `.rovik` in any spelling needs the user's approval whatever the settings say, and the AI reviewer may not grant it. Parallel implementers (`task{kind:"workers"}`) are refused for the same reason as in an unisolated local chat.                                                  | T03, P05, C06 | e2e against the daemon. A scripted run writes a file in the worktree. `git checkout x` is refused. With "allow any command" and the AI reviewer on, `cat ~/.rovik/keys.json`, `cat $HOME/.rovik/keys.json` and `cat ../../../.rovik/keys.json` wait for the user. `cat $PWD/src/a.rs` and a command using a path copied from compiler output don't ask. `workers` is refused.                                                                                                                                                                                                                                                                                                       |
| T05 | Results (6.5): the commit through the daemon's own index, seeded from the landing base; `ResultReady`, `WorkResult` and `WorkResults`; and the outcome-to-reason function taken out of `landing.rs` and used on both sides.                                                                                                                                                                                                                                                                                    | T04           | Tests: the result's tree is the worktree minus ignored files; a file in the base that an ignore rule matches isn't reported as deleted; the agent's `git status` in the worktree is the same before and after; outcomes map to the same reasons as local landing (one table test shared with `landing.rs`).                                                                                                                                                                                                                                                                                                                                                                         |
| T06 | `overlay::import` (core, 6.5): build a chat's overlay store from a base tree and a result tree, written through a checkout into a temporary folder, with no mount, and with `markers.json` when the daemon has marked files. The client imports on `ResultReady`, and on every connection whatever `WorkResults` lists that it hasn't imported. It lands by itself under `landing.rs`'s rules and sends `WorkLanded`. An import over an existing store replaces it, unless conflicts are being resolved there. | T05, P08      | Client and daemon e2e. A remote run's change lands in the local project. A conflicting local change shows in the bar's conflicts, where "Land anyway" and "Resolve" work. Undo puts the project back, and after the next sync the next result doesn't bring the change back. A result that ended while the client was disconnected is imported when it reconnects. With `core.autocrlf` true, a text file comes back with CRLF.                                                                                                                                                                                                                                                     |
| T07 | Sync before each message (6.7, D14): the snapshot and push before each `Send`, `WorkSync` with the exclude patterns, `SyncState`, the marker text builder taken out of `mark_one`, the marked files kept by the daemon, and `markers_left` for worktree files.                                                                                                                                                                                                                                                 | T05, P08      | e2e. A local edit to a file the chat didn't touch is in the worktree before the next turn. With "chat resolves conflicts" on, a conflicting text edit puts the local kind of markers in the file and sends the chat a note, and landing waits until the markers are gone, both by itself and with a manual Land. With it off, the sync waits, the chat goes on with the older copy, and the bar says why. A binary conflict waits whatever the setting.                                                                                                                                                                                                                             |
| T08 | Merges checked on the server (6.6, D13): `WorkRecheck`, and the core start kind `CheckMerged { files }`, which sets `merged_from_project` and runs `verify_at_end`.                                                                                                                                                                                                                                                                                                                                            | T06, T07      | e2e: the local project changed a file the remote chat also changed, on other lines → the daemon rechecks → the checks pass → the merge lands; when a check fails, the chat fixes it and lands afterwards; after two rounds of "changed again", the result waits.                                                                                                                                                                                                                                                                                                                                                                                                                    |
| T09 | The overlay moves with the chat (6.4): the landing base `S0'` from the overlay's base copies, and `C0` with its upper files.                                                                                                                                                                                                                                                                                                                                                                                   | T01           | Test: an overlay with an edited, a new, a deleted and a moved file gives the expected `S0'` and `C0` trees. After a remote run and the import, landing merges exactly as the local overlay would have, including a file the user edited after the chat did.                                                                                                                                                                                                                                                                                                                                                                                                                         |
| T10 | The isolation bar's remote half (6.1): `WorkStatus`, `WorkFile`, `WorkDiscard` and `WorkConflictsToChat` on the daemon; the `overlay_*` routes and refusals in the app; and the routed `chat_read_plan` command.                                                                                                                                                                                                                                                                                               | T06, P08      | Rust tests of each route for a remote chat. Daemon tests: `WorkStatus` lists what the chat changed, and stops listing it after `WorkLanded`; `WorkDiscard` resets the worktree to the landing base; a plan under `.rovik/plans/` reads through `chat_read_plan`.                                                                                                                                                                                                                                                                                                                                                                                                                    |

### Milestone M: moving chats, and the mirror (phase 3)

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Needs                        | Done when                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M01 | `SCHEMA_V10` (6.3): `dialogs.location` (NULL means here; `moving:<host>`, `host:<id>`, and `released` on the daemon), `sync_clock`, `rev` columns and their triggers, an index on `(dialog_id, rev)`, and the local tables `cloud_moves` and `mirror_state`. The client keeps in `mirror_state` the result each chat imported last, with its `seq`, and a landing its host wasn't told of yet (6.5), in place of T06's shadow ref and in-memory list. Also the ownership refusal in `dispatch` (4.2).                                                                                            | P02                          | A migration test from version 9. A test runs every write function of `chat_db/store.rs`, `clm/store.rs` and `chat_db/projects.rs` and checks that `rev` rose on the rows each touched. `EXPLAIN QUERY PLAN` shows the changes query using the index. `dispatch` refuses a chat in `moving:` or `host:`. After a restart of the app, an imported result isn't imported again, and a landing not yet reported is reported after the next connection.                                                                                                                                           |
| M02 | `chat_db::sync`: `ChangeSet`, `changes_since` in pages, and `apply` (6.3), with the CLM revision deletions, the local `project_id` and `location` kept, and the location check in the same transaction.                                                                                                                                                                                                                                                                                                                                                                                          | M01                          | Tests: applying the changes in any batches and pages gives the same database as one full export; applying twice changes nothing; a finalized body stored in `message_bodies` arrives whole; a child chat arrives with its parent's location; a CLM revision the owner deleted is deleted; a chat that moved back isn't written to.                                                                                                                                                                                                                                                           |
| M03 | The safe point. `AgentPause` in the core: a flag on `RunControl`, handed to the run with its `Controls` and checked where a checkpoint pause happens in `run.rs` and between verification rounds. The run pauses with `PauseInfo { kind: "move", host }`, a new field, and no `reason`, so there is no summary request (K29). `Drain` uses the same flag with kind `drain`. Running `task` children finish first, and the call returns when the run has ended. `MessageList.tsx` shows a `move` pause as "Moved to <host>" and a `drain` pause as "Paused for an upgrade", not as a stuck agent. | P02                          | e2e: a pause during a tool call stores the call's result and makes no further request, with outcome `paused-move`; `ContinueRun` then continues the count; a pause during a scout lets the scout finish first; Stop still stops at once. `npm test`: both pause kinds render with their own text.                                                                                                                                                                                                                                                                                            |
| M04 | Import and ownership on the daemon: `POST /chats/import` (one transaction, idempotent per `move_id`, the chat's computer recorded, the worktree created before the commit, a released chat cleared first, a row without a file for an attachment the daemon doesn't have), `ChatOwner` and `ChatRelease`.                                                                                                                                                                                                                                                                                        | M02, T03, T09                | Daemon tests: two imports with one `move_id` make one chat; another computer's import of a chat it doesn't own is refused; after a release, writes are refused and reading changes still works; a chat released earlier can be imported again; a bundle that references a file the client couldn't upload imports, and requests say the file is no longer shown; a bundle of 50 MB imports.                                                                                                                                                                                                  |
| M05 | The mirror's writer (`rovik-client::mirror`, 6.3): paged pulls, attachments (stored before the rows that reference them, and a row without a file for one the owner can't serve, 6.11), `mirror_state`, and the events after each pull. "Re-sync this chat" in the chat menu pulls `changes_since(0)` and replaces the mirror's rows. It never deletes the `dialogs` row: the input's draft, with its files, cascades from it (K45).                                                                                                                                                             | M02, P07, P09                | Tests: after a run with many streaming checkpoints, the mirror equals the daemon's rows; the app offline during a run catches up when it reconnects; a chat opened between an event and its pull ends with every stored message; a remote chat read offline shows everything stored before the disconnection; a user message with ten 1 MB text files reaches the mirror whole; a screenshot taken on the server shows in the app; a file the daemon has lost doesn't stop the pull; "Re-sync this chat" keeps the input's draft and its images.                                             |
| M06 | Moving to a host (6.8): `cloud_move_preflight` (with the attachments to upload and their size) and `cloud_move_chat`, the lock, progress on `cloud-event`, "Cancel move", and recovery from `cloud_moves` at start-up.                                                                                                                                                                                                                                                                                                                                                                           | M03, M04, M05, T07, P08, P09 | Client and daemon e2e with the scripted model. A move during a run lets the run continue on the daemon, its events reach the app, and the mirror ends with the same rows as the daemon. A Continue or a new message during the move is refused. Killing the app between the import and the flip → the next start finishes the flip. Killing it before the import → the move rolls back. "Cancel move" with the host offline → the chat is local, and the queued release reaches the host later. A chat whose history has an image and a PDF sends both to the scripted model after the move. |
| M07 | Moving back (6.8), in the order given there, with its `cloud_moves` row, and `ChatForget` after the flip, which the daemon refuses for a chat it hasn't released.                                                                                                                                                                                                                                                                                                                                                                                                                                | M06, T06                     | e2e: a chat moved there and back has the rows the daemon had; a write sent to the daemon after the release is refused; its changes wait as the chat's pending changes; the run continues locally; the daemon keeps the chat's rows until `ChatForget` and deletes them then; killing the app after `ChatRelease` → the next start finishes the move back.                                                                                                                                                                                                                                    |
| M08 | Writes to and deletion of remote chats (6.1): rename, model and checklist dismissal go to the owner; deletion removes the local rows at once and queues a tombstone for a host that is offline.                                                                                                                                                                                                                                                                                                                                                                                                  | M06                          | e2e: a rename in the app shows on the daemon; a chat deleted while its host is offline is deleted there on reconnection.                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |

### Milestone H: hosts, SSH and deploy (phase 4)

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Needs    | Done when                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| H01 | Host settings: `cloud-hosts.json` in the app's config folder (id, name, host, port, user, auth kind, key path, pinned host key, approval policy, deploy state, and this computer's id). Secrets go in the OS keychain (`keyring`, with each platform's native store): password, key passphrase, this computer's token. Where no Secret Service exists, a file encrypted with argon2id and XChaCha20-Poly1305 takes their place, with a passphrase asked once per session, and the interface says so.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | –        | Unit tests: no secret ever appears in the JSON; the fallback round-trips; a missing keychain entry gives a clear error.                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| H02 | `rovik-client::ssh` with russh. Authentication: key file with passphrase, ssh-agent (`SSH_AUTH_SOCK`; on Windows the OpenSSH agent pipe or Pageant), password, keyboard-interactive. The host key is shown as SHA-256 on first use and pinned; a mismatch is a hard error. Keepalives every 15 s. One SSH connection per host, with a `direct-streamlocal` channel per WebSocket or HTTP connection, and exec channels for setup.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | H01      | Tests against an in-process russh server (`russh::server`): each authentication method works; a changed host key is refused; a WebSocket session and an HTTP upload run at once on separate channels.                                                                                                                                                                                                                                                                                                                                                                              |
| H03 | The tunnel's fallbacks. The socket's path comes from `$HOME`, asked for once. When streamlocal is refused, the client uses `exec /opt/rovik/bin/rovik-agentd attach --stdio`. The "Use my system ssh" option runs `ssh -N -L <local socket or port>:<remote socket> <host>` under the app's control, so jump hosts, FIDO keys and certificates come from the user's own SSH configuration.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | H02, P03 | Test: a server that refuses streamlocal still connects through `attach`. Manual: the system-ssh option through a jump host.                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| H04 | The connection test (design §7), one step at a time with a result for each: DNS and TCP; handshake and fingerprint; authentication; a probe script in one exec (`uname -sm`, free disk in `$HOME`, `git --version`, Chromium and whether it starts headless, user namespaces, `/dev/fuse`, systemd as PID 1, whether `sudo` works and asks for a password, whether Rovik is installed and at which version, with which administrators, other users' daemons); whether the providers' API hosts can be reached from the server; and the daemon's health and version once it is deployed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | H03      | Unit tests of the probe parser on fixtures (Ubuntu, Debian, Alpine, a container without systemd). Manual run against a VPS.                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| H05 | Deploy, upgrade and removal (5.3). The release binary (B02), or `ROVIK_AGENTD_BINARY`, is uploaded to a temporary name once its signature is checked. On a server without Rovik, `install --admin <user>` through `sudo`, or the command for someone with root when there's no sudo; on a server with Rovik, `user start`. A root login is refused. A new token for this computer is added to the user's `config.toml`: its hash on the server, the token in the keychain. Then `/health`, `Welcome`. On every connection the client checks over SSH that the binary's hash and the unit's `ExecStart` are the release's, and stops with a warning if not (D5). An upgrade is the administrator's (`admin upgrade`, R02); another user whose app needs a newer daemon is told whom to ask. Removal stops and disables the user's instance, then deletes `~/.rovik` and `~/.local/share/rovik` after a confirmation that names what goes; removing Rovik from the server is an admin command. Before removing a host that still has chats, the app offers to move them back or delete them. | H04, B02 | Test against a container image with sshd and systemd: a first deploy installs Rovik and makes its user the administrator; a second user's deploy starts their own instance, and that user can't start or stop the first one's; a deploy without sudo stops with the command and goes on once it has run; a root login is refused; a binary changed behind the app's back is reported. Manual: an upgrade by the administrator while two users' runs are going (both pause, then continue); a second computer added to the same host, whose chats the first doesn't see; a removal. |
| H06 | The desktop's API keys move into the keychain. `ai-settings.json` keeps a reference per provider, migrated at the first start. `load_ai_settings` returns the keys filled in, so the frontend doesn't change.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | H01      | Tests: migration of a version 2 settings file, with no key left in the file afterwards; a missing keychain falls back as in H01.                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| H07 | Team hosts (5.5, D10, D17): the installer and its `user` and `admin` commands, the system unit with its limits drop-in and `rovik.slice`, the sudoers rule, the limits in `/etc/rovik/agentd.toml`, the daemon's ownership and mode checks on both folders, the children's OOM score, slots defaulting to 1 while others' daemons run, and `docs/cloud-hosts.md` for admins (the install, the administrator, limits, what admins can read, shared ports).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | P03, H04 | Tests: a group-readable `~/.rovik` stops the daemon with a message; the admin file's `max_slots` wins over the user's; a scripted command that takes too much memory is killed at its instance's cap while the daemon and its other runs go on; nothing the daemon or a scripted run writes lands in the host's `/tmp`; `admin` commands are refused to a user outside `rovik-admin`, and `user` commands act only on the caller's instance; the sudoers file passes `visudo -c`. The document is reviewed.                                                                        |
| H08 | Chrome over a pipe (D18). Fork `eoka` 0.5.14 (MIT) into `src-tauri/crates/eoka`, used through `[patch.crates-io]`, with its license kept, and add a pipe transport: Chrome started with `--remote-debugging-pipe` and file descriptors 3 and 4 (Unix), JSON messages ended by a NUL byte, readiness from the first `Browser.getVersion` reply, and the browser connection's flat sessions unchanged. The port stays for attaching to a Chrome that's already running, and on Windows. `browser_tools.rs` uses the pipe on Linux and macOS. Offer the change upstream, and drop the fork once a release has it.                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | C05      | eoka's transport tests run over both transports. An e2e check finds no TCP socket listening in Chrome's processes, `navigator.webdriver` is false, and eoka's stealth tests pass, all in pipe mode. `browse_url` and `browser_screenshot` work on Linux and macOS, with snap Chromium too.                                                                                                                                                                                                                                                                                         |

### Milestone U: the interface

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Needs         | Done when                                                                                                                                                                                                                                                                                                                                           |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| U01 | Settings → Cloud hosts, a new `AiSettingsPage` wired the way Subagents was: the list; add and edit (the fields of design §7, without the agent port); Test connection with each step's result and the fingerprint to confirm; deploy, upgrade and remove; the approval policy (6.9); server storage; the providers with a key on the server, with "Remove keys from this server"; Rovik on the server, with its version and administrators, and for an administrator, the limits and "Upgrade for everyone".                                                                                                                                                                                                                                                                                                                                                                                                                                                   | H05, P05      | `npm test` covers the page's states (not tested, testing, a failed step, needs someone with root, deployed, upgrade available, an administrator's view) and checks that the key list never shows a key. Manual check.                                                                                                                               |
| U02 | Location and moving. The chat header shows a location chip (this computer, the host's name, moving, or offline). The chat menu has "Move to <host>…", "Move back" and "Re-sync this chat". The move dialog shows what moves, what will wait for you (from the permissions and the host's policy), CLM becoming trimming, the keys notice and the preflight's other notes, then the progress and errors from `cloud-event`, with "Cancel move" while the host is unreachable. A chat that is moving is read-only.                                                                                                                                                                                                                                                                                                                                                                                                                                               | M06, U01      | `npm test` for the dialog's text under different settings, and for a moving chat being read-only. Manual: a move and a move back.                                                                                                                                                                                                                   |
| U03 | Chat list badges: a cloud badge on remote chats, a "waiting for you" badge on any chat with a pending approval or question (local or remote, from the approval and question events and `agent_status`), and an offline host on its chats.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | P08           | `npm test`: the badges follow a scripted sequence of events.                                                                                                                                                                                                                                                                                        |
| U04 | Guards for remote chats in the frontend. The auto-resume effect skips remote chats, which the daemon resumes (K07). Draft recovery at start-up skips remote chats until their host has connected and the mirror has caught up (K23). Edit approvals marked `remote` show in a compare view built from `current` and `proposed`, never on the local file's tab (K09). Plans are read with `chat_read_plan` in place of `readFileContent`, and a remote chat's plan opens in a read-only tab (K09). Without `realPath`, the CLM context file of a remote chat opens read-only from `clm_read_revision`. While a remote send uploads its files, the composer shows the progress from `cloud-event` (6.11). For a remote chat, the input's saved draft (`composer_drafts`) is cleared only once `agent_start` has succeeded: the `localStorage` copy that brings a failed message back can't hold a few megabytes of text files, and its error is swallowed (K49). | P08, T10, P09 | `npm test`: a remote chat with unanswered calls starts no `resume`; draft recovery leaves a remote chat's message alone; a remote edit approval doesn't open the local file; a remote plan opens from the owner; a remote send shows its upload's progress; a remote send that fails after the upload gives the input back its ten 1 MB text files. |
| U05 | The isolation bar for remote chats: the files changed so far, from `WorkStatus`, while a run goes; the result's pending reason and conflicts, from the imported store; "a newer result is ready" while conflicts are being resolved; the sync state, with "Hand to the chat" and "Keep the older copy"; and "Sync now".                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | T10, T07      | `npm test` for the bar's states. Manual check.                                                                                                                                                                                                                                                                                                      |
| U06 | Notices: an approval or a question from a remote chat notifies as a local question does today. On reconnection, one notice sums up what waits.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | U03           | `npm test`.                                                                                                                                                                                                                                                                                                                                         |

### Milestone B: building and releasing the daemon

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                | Needs    | Done when                                                                                                                 |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------- |
| B01 | Static builds: `cargo zigbuild --profile release-daemon -p rovik-agentd --target {x86_64,aarch64}-unknown-linux-musl`, run by `scripts/build-agentd.sh`. If `aws-lc-sys` won't build for musl, the daemon's rustls uses the `ring` provider (K20).                                                                                                                                                  | C05, P03 | Both binaries build in a clean container, `file` reports them statically linked, and each starts on Alpine and on Ubuntu. |
| B02 | Release workflow `.github/workflows/agentd.yml`, on `v*` tags: build both binaries, sign them (minisign, with a key kept in the repository's secrets), and attach `rovik-agentd-<version>-<arch>` with its `.minisig` to the release. The app compiles in the public key and checks the signature before uploading, and the installed daemon checks an upgrade's signature with the same key (5.3). | B01      | A unit test with a test key refuses a tampered binary. The workflow publishes on a test tag.                              |
| B03 | The development override: `ROVIK_AGENTD_BINARY`, and a "local build" field in a host's advanced settings. On a server, a local build goes through `admin upgrade`, which takes an unsigned build only where `agentd.toml` says `allow_unsigned = true`. The daemon reports its version and git hash.                                                                                                | B01      | Manual: deploy a locally built daemon.                                                                                    |

### Milestone R: running it for real

| ID  | Task                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | Needs         | Done when                                                                                                                                                                                                                                                                                          |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R01 | Storage and garbage collection. `Storage` reports the sizes of `projects/`, `work/`, `data/` and `tmp/`. Worktrees of released and deleted chats are removed after 7 days. A released chat's rows go when its computer sends `ChatForget` (6.8), which frees their attachments (K43). Bare repositories get `git gc --auto` when git is installed (libgit2 has no gc). The attachment cleanup runs daily too (the desktop runs it only at start-up and after deletions), and the hosts page has "Clean up now". | M07           | Daemon test: a deleted chat's worktree and refs are gone after collection, and the sizes are reported; after `ChatForget`, a released chat's rows and its PDF are gone too; a file uploaded for a message that never came is gone after the grace period, and one renewed by `BlobsMissing` stays. |
| R02 | Version skew (design §10.6): the protocol check on connection; the client refuses to move a chat to an older daemon, offers an administrator to upgrade it for everyone, and tells other users whom to ask; a newer compatible daemon is fine; an upgrade drains every instance first, pausing runs with kind `drain`, and 5.4 continues them on the new binary.                                                                                                                                                | H05, M03, P06 | Daemon test: an upgrade while a run is going gives `paused-drain`, and the run continues on the new binary.                                                                                                                                                                                        |
| R03 | `rovik-agentd doctor` prints what the connection test needs (versions, mounts, the install and its limits, slots, storage), and H04 uses it once the daemon is deployed. Log rotation.                                                                                                                                                                                                                                                                                                                          | P03           | A snapshot test of the output on a fixture.                                                                                                                                                                                                                                                        |
| R04 | Windows and macOS clients. Local paths, such as drive letters in `projects.path`, never reach the daemon: it gets project ids and relative paths. ssh-agent works through the named pipe. Importing on a case-insensitive file system handles two paths that differ only in case. Line endings as in T01 and T06, checked on a real Windows checkout.                                                                                                                                                           | T09, T06, H02 | Tests on Windows and macOS where they are available. Manual check on one Windows laptop.                                                                                                                                                                                                           |

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
- Uploading a remote chat's attachments as soon as they are attached,
  not when the message is sent.
- Listing and adopting another computer's chats on the same host.
- Choosing which users of a server may start a daemon.
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
   - `npx prettier --check <the files you added>`, and on files you
     changed that were formatted before (some docs aren't)
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
C:  C01 ─► C02 ─► C03 ─► C04 ─► C05 ─► C06                    no visible change
P:  P01 ─► P02 ─► P03 ─► P04 ─► P05 ─► P06 (needs C06)
                          └─► P07 ─► P08 ─► P09               a chat on a same-machine daemon
T:  T01 (after C05) ─► T02 ─► T03 ─► T04 ─► T05 ─► T06 ─► T07 ─► T08 ─► T10;  T09 after T01
    (T04 also needs P05 and C06; T06 and T07 need P08)
M:  M01 (after P02) ─► M02 ─► M03 ─► M04 ─► M05 ─► M06 ─► M07 ─► M08
    (M05 and M06 also need P09)
B:  B01 (after P03) ─► B02 ─► B03
H:  H01 ─► H02 ─► H03 ─► H04 ─► H05 (needs B02) ─► H07;  H06 any time after H01;  H08 any time after C05
U:  U03 after P08;  U04 after T10 and P09;  U05 after T10;  U01 after H05;  U02 after M06 and U01;  U06 after U03
R:  R03 after P03;  R01 after M07;  R02 after H05 and P06;  R04 after T06, T09 and H02
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
- **Core e2e:** the scripted provider of `agent_run/e2e_tests.rs`, moved
  by C06 into `rovik-core`'s `testing` feature, so the daemon's tests can
  start one too.
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
  during a tool call, during the end-of-run checks, during an import and
  during a pack upload; the app killed at each step of a move; the SSH
  connection cut in the middle of an upload; a socket closed without a
  FIN, which is what a sleeping laptop looks like; a server clock an hour
  off; a file removed by hand from the daemon's `attachments/`. Ordering uses `seq` and `rev`, never time.
- **Manual matrix:**
  - Servers: an Ubuntu 24.04 x86_64 VPS; Debian on aarch64; an Alpine
    container without systemd, FUSE or user namespaces; a team host with
    two accounts working at once; one account used from two computers; a
    server whose first user has no sudo.
  - Clients: Linux; macOS without macFUSE; Windows without WinFsp.
  - Attachments: a remote chat sent an image and a 20 MB PDF in one
    message, and ten 1 MB text files in another, over a slow uplink.

---

## 10. Gaps found in review

A check of the design against the code found K01–K21. A review of this
plan found K22–K40. K41–K51 came from checking the plan against the chat
input's attachments, which landed after it was written, and K52 from a
question about how users are separated. Each is now part
of a decision or a task.

| ID  | Gap                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Resolved in                                                                                                |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| K01 | A mirror keyed by `seq` can't follow patches: `MessagePatched` changes rows by id, the text streamed between checkpoints is never stored, and a ring buffer can't cover a night's sleep.                                                                                                                                                                                                                                                                                                    | D9, M01, M02, M05                                                                                          |
| K02 | `FileWritten`, `ReasoningLearned`, `ApiRouteLearned` and `Notice` carry no dialog id, so events can't all be told apart by chat.                                                                                                                                                                                                                                                                                                                                                            | P01 (a dialog id on `FileWritten` and `Notice`; the two "learned" events apply to settings, not to a chat) |
| K03 | Every run reads the project's settings from the app's config folder, keyed by the local path. A daemon wouldn't have them, and `Core` had no config folder.                                                                                                                                                                                                                                                                                                                                 | D2, 6.2, M04, P05                                                                                          |
| K04 | `tauri::async_runtime` can spawn from any thread, but `tokio::spawn` panics outside a runtime.                                                                                                                                                                                                                                                                                                                                                                                              | D2, C02                                                                                                    |
| K05 | The design moves `git_sync` and three helpers from `git.rs` into the core, but the core needs only `ConflictFile`.                                                                                                                                                                                                                                                                                                                                                                          | C01                                                                                                        |
| K06 | The webview's CSP allows connections only to IPC, so a frontend `backend` module couldn't reach the tunnel.                                                                                                                                                                                                                                                                                                                                                                                 | D4                                                                                                         |
| K07 | ChatPanel resumes by itself any chat with unanswered tool calls. It would resume a remote chat that the daemon is running, or that it resumes itself.                                                                                                                                                                                                                                                                                                                                       | U04                                                                                                        |
| K08 | The editor context in a message names the active file's absolute local path, which means nothing on the server.                                                                                                                                                                                                                                                                                                                                                                             | 6.1, P08                                                                                                   |
| K09 | `meta.planFile` and the `absPath` of edit approvals are local paths read from the local disk.                                                                                                                                                                                                                                                                                                                                                                                               | 6.1, T10, U04                                                                                              |
| K10 | Local auto-landing merges the project's newer changes and checks them before landing. Landing a remote result needs the same.                                                                                                                                                                                                                                                                                                                                                               | D13, T08                                                                                                   |
| K11 | Isolated chats refuse git commands that change the repository. Without the same rule in worktrees, a branch switch would become the chat's result.                                                                                                                                                                                                                                                                                                                                          | D8, T04                                                                                                    |
| K12 | A systemd service starts with a bare `PATH`, and without systemd `$SHELL` can be missing, so the agent wouldn't find `cargo` or `npm`.                                                                                                                                                                                                                                                                                                                                                      | 5.2                                                                                                        |
| K13 | `/tmp` is shared on team hosts: `api_log`'s default file, the agent's temporary files and Chrome's files would be visible to others, or open to symlink tricks.                                                                                                                                                                                                                                                                                                                             | D10, 5.2, 5.3, H07                                                                                         |
| K14 | The design builds the daemon "in CI", but there is no CI.                                                                                                                                                                                                                                                                                                                                                                                                                                   | D15, B01, B02                                                                                              |
| K15 | CLM needs the virtual-files mount, which most servers don't have.                                                                                                                                                                                                                                                                                                                                                                                                                           | 5.2 (mounted when FUSE works), U02 (the dialog says trimming will be used)                                 |
| K16 | The design relies on a "waiting for you" badge in the chat list, but none exists.                                                                                                                                                                                                                                                                                                                                                                                                           | U03                                                                                                        |
| K17 | Moving a chat whose overlay has changes, with the project tree as the base, would lose the user's later edits to the chat's files at landing.                                                                                                                                                                                                                                                                                                                                               | 6.4, T09                                                                                                   |
| K18 | Process-wide statics stop two `Core`s with different data folders from sharing one test process.                                                                                                                                                                                                                                                                                                                                                                                            | 9 (the daemon in its own process)                                                                          |
| K19 | The release profile's `panic = "abort"` would let one run's panic end every run on the daemon.                                                                                                                                                                                                                                                                                                                                                                                              | D15, C05                                                                                                   |
| K20 | reqwest's rustls uses `aws-lc-rs`, which needs CMake and a C toolchain to build for musl.                                                                                                                                                                                                                                                                                                                                                                                                   | B01                                                                                                        |
| K21 | Nothing stops local code from writing to a chat whose owner is elsewhere.                                                                                                                                                                                                                                                                                                                                                                                                                   | 4.2, 6.3, M01                                                                                              |
| K22 | Re-emitted events reached the frontend before their rows reached the mirror. `chatStore` reuses a loaded chat, so one opened in between missed rows for the rest of the session.                                                                                                                                                                                                                                                                                                            | D9, 6.1, 6.3, P08, M05                                                                                     |
| K23 | Draft recovery at start-up would mark a remote chat's answered message as failed before its host connected, and failed messages are never sent to the model again.                                                                                                                                                                                                                                                                                                                          | 6.10, U04                                                                                                  |
| K24 | A result that ended while the client was away (its `ResultReady` lost with the ring) was never imported.                                                                                                                                                                                                                                                                                                                                                                                    | 4.3, 6.5, T05, T06                                                                                         |
| K25 | Nothing locked a chat during its move: a Continue between the pause and the flip would have run locally and been lost.                                                                                                                                                                                                                                                                                                                                                                      | 6.8, M01, M06                                                                                              |
| K26 | With the worktrees under `~/.rovik`, the "names `~/.rovik`" rule would have asked about almost every command, while `$HOME/.rovik` slipped past a literal match.                                                                                                                                                                                                                                                                                                                            | D8, 5.1, T04                                                                                               |
| K27 | A second computer on the same daemon would have seen the first's events, and its configuration would have replaced the first's approval rules.                                                                                                                                                                                                                                                                                                                                              | D16, 4.3, P04, P05                                                                                         |
| K28 | Streaming patches (every 50 ms, with the whole text) would have flooded the tunnel and filled the ring in seconds.                                                                                                                                                                                                                                                                                                                                                                          | 4.3, P04                                                                                                   |
| K29 | A pause with a `reason` makes a summary request (`summarize_before_pause`), and the frontend shows it as a stuck agent.                                                                                                                                                                                                                                                                                                                                                                     | 6.8, M03                                                                                                   |
| K30 | Empty shadow and result indexes would have dropped tracked files that match an ignore rule, and lost file modes. A drop on one side only would have landed as a deletion.                                                                                                                                                                                                                                                                                                                   | D7, 6.4, 6.5, T01, T05                                                                                     |
| K31 | Projects that are a subfolder of a repository missed the ignore rules above them, and their history didn't match their tree.                                                                                                                                                                                                                                                                                                                                                                | 6.4, T01                                                                                                   |
| K32 | Ignore rules that exist only on the client (`info/exclude`, global excludes) would have let server-made files come back in results.                                                                                                                                                                                                                                                                                                                                                         | 6.4, T03, T07                                                                                              |
| K33 | `merge_bytes` makes no marker text, and `markers_left` knows only the local markers' labels. Binary and delete conflicts stay with the user locally, and the marked list has to travel with the result.                                                                                                                                                                                                                                                                                     | D14, 6.5, 6.7, T06, T07                                                                                    |
| K34 | The daemon never learned that a result had landed, so an Undo of the landing would have been undone by the next result.                                                                                                                                                                                                                                                                                                                                                                     | D12, 6.5, T06, T10                                                                                         |
| K35 | `changes_since(0)` in one frame would have exceeded 16 MB for long chats.                                                                                                                                                                                                                                                                                                                                                                                                                   | 4.1, 4.2, 4.4, M02, M04                                                                                    |
| K36 | Moving a chat back to a host that released it less than 7 days earlier would have collided with its leftovers.                                                                                                                                                                                                                                                                                                                                                                              | 6.8, M04                                                                                                   |
| K37 | Runs drained for an upgrade weren't continued, and a run killed during its end-of-run checks stayed "running" with no result.                                                                                                                                                                                                                                                                                                                                                               | 5.4, P06, R02                                                                                              |
| K38 | Merging or deleting a project would have rewritten or orphaned its remote chats.                                                                                                                                                                                                                                                                                                                                                                                                            | 6.1, 6.3, P08                                                                                              |
| K39 | `Patch.status` uses `null` to clear the status, which a plain `Option` loses in a round trip.                                                                                                                                                                                                                                                                                                                                                                                               | P01                                                                                                        |
| K40 | Line endings: copying bytes would have put CRLF scripts on a Linux server, and normalising without restoring would have given Windows checkouts LF files.                                                                                                                                                                                                                                                                                                                                   | 6.4, 6.5, T01, T06, R04                                                                                    |
| K41 | Attached text files go inside the message, up to ten of 1 MB, and the start carries that text two or three times (`content`, `currentUserContent`, and often `meta.sentContent`). One `AgentStart` could have been 30 MB and one stored row 20 MB, over the 16 MB frame; Axum takes bodies of 2 MB by default, and long pastes have no limit at all. Edit approvals and `FileWritten` carry whole files. A frame that big would have held up events and pings for minutes on a slow uplink. | 4.1, 4.2, 4.4, 6.1, P01, P03, P04, P07, P08, M05                                                           |
| K42 | Images and PDFs are stored on the computer where they were attached, before their message exists, and the daemon's `agent_start` refuses a message whose files it hasn't stored. Nothing sent them at send time, and Axum's default body limit of 2 MB would have refused most PDFs.                                                                                                                                                                                                        | 6.1, 6.11, P09                                                                                             |
| K43 | The daemon has no drafts: only the hour's grace keeps a file between its upload and its message, and a file it already had could have been cleaned up between the check and the start. On this computer, sending clears the draft that kept the files. Released chats' rows, kept until a re-import, would have kept their files forever.                                                                                                                                                   | 6.8, 6.11, P09, R01                                                                                        |
| K44 | A file a message references but this side doesn't have would have failed, on the foreign key from `message_attachments`, the rows of a `MessagesAdded` (a screenshot taken on the server), a move's import, or a pull, for good. Writing the rows without their attachments wouldn't do: no patch can add them later.                                                                                                                                                                       | 6.1, 6.3, 6.8, 6.11, P09, M04, M05                                                                         |
| K45 | "Re-sync this chat" replacing the mirror's rows could have deleted the `dialogs` row, and with it the input's draft and the files it holds (`composer_drafts` cascades).                                                                                                                                                                                                                                                                                                                    | M05                                                                                                        |
| K46 | 4.5 let every new optional field through to an older daemon, but serde drops fields it doesn't know. A field that changes what a request means, as attachments did for messages, would be lost without a word.                                                                                                                                                                                                                                                                              | 4.5                                                                                                        |
| K47 | `attachments::missing` checks rows, not files. A `BlobsMissing` built on it would never send again a file whose row outlived it (K44's rows without files, a file removed by hand), and every later message with it would say it's no longer shown.                                                                                                                                                                                                                                         | 6.3, 6.11, P09                                                                                             |
| K48 | Stop is offered while a remote send uploads, but went to a daemon with no run yet: the run started after the user had stopped it.                                                                                                                                                                                                                                                                                                                                                           | 6.11, P09                                                                                                  |
| K49 | A failed remote send could lose a message with big text files: sending clears the input's saved draft, and the `localStorage` copy meant to bring it back can't hold a few megabytes.                                                                                                                                                                                                                                                                                                       | U04                                                                                                        |
| K50 | Continue, Retry and the checks' runs send the last message's wire text again, so ten text files would have gone up with every Continue, and into `state.json`.                                                                                                                                                                                                                                                                                                                              | 5.1, 6.1, 6.2, P05, P08                                                                                    |
| K51 | A move back cut short after `ChatRelease` had no recovery: `cloud_moves` covered only moves to a host, and a released chat refuses writes. Deleting its rows after 7 days would then have lost the rest.                                                                                                                                                                                                                                                                                    | 6.8, M07, R01                                                                                              |
| K52 | eoka starts Chrome with DevTools on a `127.0.0.1` port that has no password. On a shared server any user could drive a browser that runs as you, and read your files through it, the API keys among them.                                                                                                                                                                                                                                                                                   | D18, 5.5, H08                                                                                              |

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
- **The shadow's borrowed objects.** If the user's reflog expires or they
  prune, old snapshot parents can disappear from the project's object
  store. Snapshots then fall back to a parent the shadow still has, or to
  none (6.4).
- **Same-user tampering.** The agent's commands run as the user, so a
  command set on it can still change the daemon's files. D8's rule and
  H05's checks on connection make it visible; they don't make it
  impossible.
- **Disk on team hosts.** Each person's bare repository holds the
  project's whole history. Collection (R01) and the storage line help in
  v1; a shared object cache is a later option.
- **The mirror drifting from the owner.** M02's and M05's tests are the
  guard, and "Re-sync this chat" is the way to recover.
- **Big attachments wait for their upload.** A remote send uploads its
  images and PDFs first: a 20 MB PDF takes about 16 seconds on a
  10 Mbit/s uplink. The composer shows the progress, and a file the host
  already has isn't sent again.
- **The eoka fork.** Until upstream takes the pipe transport, Rovik
  carries a fork, and updating eoka means bringing the change along. It
  touches only how Chrome is launched and the transport.
- **Known limits of v1:**
  - Chats without a project don't move.
  - Submodules and nested repositories aren't carried.
  - Parallel implementers and fine-grained undo aren't available on
    servers.
  - "Redo sequentially" for a parallel group doesn't survive a move.
  - A computer sees only the chats it moved to a host.
  - Ports the agent's commands open are shared by everyone on a server
    (5.5).

---

## 12. Changes to the design document

This plan changes the design in these places. `cloud-agent-plan.md`
points here from each one.

| Design                                                                                     | Change                                                                                                                                                                                     | Here                        |
| ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------- |
| §2.3, a frontend `backend` module                                                          | Routing in the Rust command layer, and no streaming call goes remote                                                                                                                       | D4, 6.1                     |
| §3.1, worktrees in `~/.rovik/work/`                                                        | Worktrees and temporary files in `~/.local/share/rovik/`, so the `.rovik` rule asks only about the daemon's own files                                                                      | D8, 5.1                     |
| §3.2 and §6, "pushes it with libgit2 over SSH", two SSH stacks                             | Packs through the tunnel, one SSH stack                                                                                                                                                    | D6, 6.4                     |
| §3.2, snapshots in the project's repository, a shadow repository only for non-git projects | A shadow repository for every project                                                                                                                                                      | D7                          |
| §4, a localhost port or a Unix socket                                                      | A Unix socket, with `attach --stdio` as a byte bridge                                                                                                                                      | D5                          |
| §5, a mirror written from events keyed by `seq`                                            | A mirror that follows stored rows by `rev`                                                                                                                                                 | D9, 6.3                     |
| §5, screenshots are the attachments, and they travel when a chat moves                     | The chat input's images and PDFs too, uploaded with each message that carries them as well as with a move                                                                                  | 6.11                        |
| §7, deploy: the binary in `~/.rovik/bin`, a systemd user unit and lingering                | Installed once per server by its first user, with `sudo`: a root-owned signed binary, a system unit per user, limits set by the administrator                                              | D17, 5.3                    |
| §9, the phases                                                                             | Project transport and the move before SSH                                                                                                                                                  | 8                           |
| Open questions                                                                             | Team servers with one daemon per person, serving each of their computers separately; containers and separate users later; the non-git shadow's disk cost accepted; v1 undo is landing undo | D10, D16, 7 (Later), D7, 11 |

## Notes from implementation

Tasks add notes here when reality differs from the plan.

- **C02, the runtime without a host's.** `rt` doesn't fall back to
  `Handle::current()`: it makes a multi-thread runtime on first use, as
  `tauri::async_runtime` did. On a `#[tokio::test]`'s single thread a run
  moves only while the test waits, and an isolated chat's e2e test
  (`a_running_chat_resolves_its_own_conflict`) failed two runs in three
  that way. The app takes that runtime from `rt::handle()`, after
  `adopt_login_shell_path`, and hands it to Tauri, so there is one runtime
  and no `rt::init`.
- **C03, two ways to build a `Core`.** `Core::new` sets up what the
  process keeps once for its data folder (`workload::init`,
  `overlay::chats::init`), so a process builds one. `Core::open` opens the
  database and builds the parts without them: the e2e harness and the
  headless runner build cores with it, since one test process holds many
  (K18). The agent log's folder is a third path, `Paths::log_dir`: the data
  folder, except for the headless runner, which keeps the log with its
  report.
- **C04, two tests besides the harness.** The test that stored an
  attachment through the mock app's IPC is two tests now: the app's
  `chat_store_attachment` reads the bytes from either body (a pure
  function), and the core stores them (`attachments::store_attached`).
  The headless test that checked the agent log's process-wide folder was
  reset keeps its other checks: with the folder in each `Core`, there is
  no such folder left. The core functions behind the commands take the
  part of the `Core` they need (`&DbState`, `&AgentState`), or the `Core`
  when they need more or send events.
- **C05, the runner's code waits a task.** `rovik-headless` takes the
  runner's binary, but headless.rs stays in `rovik-core` until C06: its
  e2e tests need the scripted model server, which only the `testing`
  feature lets another crate use. C06 moves the runner and those tests to
  `rovik-headless`. The workspace's shared dependencies are declared once,
  in `[workspace.dependencies]`.
- **P01, the types that travel.** Besides `AgentEvent` and `StartRequest`,
  what they and the requests carry moved too: the settings (`AgentConfig`,
  `Permissions`, `SubagentConfig`), the CLM, checklist and question types,
  `ToolCall`, `MessagePatch`, `ChatModel` and `Hunk`. The core re-exports
  each where it was. `SubagentConfig`'s methods need the core's kinds, so
  they are a trait there (`SubagentSettings`), and its test-only
  `extra_tools` is behind the protocol's `testing` feature, which the
  core's tests turn on. `Request` has today's operations of the Agent,
  Chat and CLM groups; the requests and events of later milestones
  (`AgentPause`, `ChatChangesSince`, `BlobsMissing`, the Work and Host
  groups, `ChatChanged`, `ResultReady`, `SyncState`) come with their
  tasks, as additive changes. `Response` has one variant per kind of
  value, which a caller takes with `TryFrom` (`bool::try_from(reply)`).
  A token travels as a `Secret`, whose `Debug` doesn't show it.
- **P02, async commands.** The commands that now go through `dispatch`
  are async, and return a `Result` as Tauri requires of async commands
  that take the app's state: `agent_stop`, `agent_status` and the others
  that were synchronous answer the frontend the same values. An
  operation's own error travels as `FailCode::Internal` with its text.
  The e2e suites call `dispatch` through stand-ins with the operations'
  names in the harness, so the suites' only change is an `.await` on
  those calls.
- **P03, the daemon's edges.**
  - `serve --data DIR` keeps both of the daemon's folders in DIR.
    `serve --no-virtual-files` skips the mount: the daemon tests use it, so
    a test that kills the daemon leaves no dead mount, and an
    administrator can turn FUSE off.
  - Every request but `/health` carries its token (4.4), the WebSocket's
    upgrade too, and its `Hello` must name the same computer with the same
    token. A refused upgrade or `/call` gets a 401 with a `Fail`; a refused
    `Hello` closes the session with code 1008.
  - Without `--same-machine`, `AgentStart` is refused. With it, a chat
    with no row on the daemon gets one, until M04 imports chats, and runs
    in its start's folder without an overlay (D8).
  - Events go nowhere until P04. The core's own messages still go to
    stderr, which is the journal under systemd; `agentd.log` has the
    daemon's.
  - The test of a reply over 1 MB found two slow spots in the core. A
    reply's stream was searched for an event's end from the start of the
    buffer at each chunk, which is quadratic for a tool call sent as one
    event. The diff behind an edit approval sized its search by the files
    instead of by its 2,000-line limit, so rewriting a big file copied
    gigabytes. Both now cost what the input does. The editor's diff
    (lineDiff.ts), which must give the same hunks, copied the same way, and
    applying a hunk of 200,000 lines overflowed its stack; it's fixed too.
- **P04, which computer an event goes to.** A chat's computer is the one
  that started it on the daemon, which P05 keeps in `state.json`, and a
  subagent's chat goes with its lead's (`SubagentStarted` names both).
  The two events about a model's settings name no chat, so they go to
  every computer of the daemon: all of them are one person's (D16).
  `Subscribed` comes before the events it replays. A connection that
  falls behind is closed with code 1013, and its client subscribes again
  from its `since`.
- **P05, a computer's configuration.** On the daemon, `AgentConfigure` goes
  to the store and replaces only its computer's configuration; it isn't
  dispatched. A start's configuration becomes its computer's the same way,
  and the start goes on without it, so the core's one configuration isn't
  touched and every run reads its computer's through `config_for`. The
  store is also where the events find a chat's computer. A start of
  another computer's chat is refused with `NotOwner` already; the other
  requests are refused with M01. `keys.json` keeps one key per provider,
  so of two keys of one provider in a start, the later stays.
- **P06, what an interrupted run comes back as.**
  - The core reads a chat's last rows (`agent_run::recovery::after_crash`),
    so what a row means stays in the core. The daemon applies the limit,
    and starts the run the way a client's start goes.
  - A cut reply is settled by the code that stores a failed turn
    (`settle_failed_turn`, which `store_failed_turn` now calls), with a
    new failure kind, `restarted`, which the chat words "The server
    restarted while the reply was being written." The approval of a
    resumed call no longer says the app was closed.
  - `running` goes from the run's `RunStarted` to its `RunEnded`, as the
    daemon's sink sees them, so a start that made no run leaves nothing
    to restart. `state.json` also keeps the last run's id and outcome; T05
    adds the result beside them.
  - Until T05 commits results, the `interrupted` row ends the run with a
    `RunEnded` of outcome `interrupted`, sent through the core's sink as
    a run's end is, so T05's commit and `ResultReady` hang off that one
    place.
  - Start-up's events (the restarted runs', the notice, those `RunEnded`)
    go out before the socket is bound, so they reach a client that
    subscribes from the start of the boot, `(boot, 0)`, after it catches
    up from the database. P07's client subscribes that way on a new boot.
  - Restarts follow `--same-machine` as starts do: without it, a cut run
    ends `interrupted`. Restarts count by time alone; a start from a
    client doesn't reset them.
  - No run pauses for a drain before R02, so the daemon test makes one in
    the database from a checkpoint's pause, with the kind R02 writes
    (`recovery::DRAIN`). The daemon tests read the rows a killed daemon
    left with `rusqlite`, now one of the workspace's shared dependencies.
- **P07, the client.**
  - Its tests against the real binary are in `rovik-agentd`
    (`tests/client.rs`), since only a binary's own package can run it from
    its tests. Its unit tests talk to a daemon of their own over an
    in-memory stream, which can ask for a newer app than the real one.
  - What a client hears goes to the sink it's given, in order: its state,
    each `Subscribed`, the events. The core's events go to an `EventSink`
    the same way.
  - Back on a daemon of another boot, the client subscribes from that
    boot's first event (P06) and says `complete: false`: its chats catch
    up from the database, then get what the daemon did before anyone
    connected.
  - 6.10's other half is here too: the client connects again at once
    when the wall clock runs 30 s ahead of the monotonic one, which stops
    while the computer sleeps. It sends no pings: H02's keepalives find
    a dead SSH link.
  - A call fails as `Unreachable` while there's no link, and after 60 s
    without a reply. A reply that came back `TooLarge` is asked again
    through `POST /call`, whatever the call: only reads have replies that
    big.
  - The HTTP and WebSocket crates the client shares with the daemon, and
    `log`, are workspace dependencies now.
- **P08, the app's remote chats.**
  - The routing, the remote start and the event rules are in
    `rovik-client`'s `Remote`, which takes the app's `Core` for the
    mirror's database and the sink. They're tested without Tauri, and each
    command is one line over it.
  - Until M05 pulls rows, the mirror also takes each `MessagePatched`, and
    the writes the owner sends no event for (rename, model, a message's
    patch) once the owner has taken them. Deleting a remote chat stays
    local until M08.
  - A remote `Send`'s message is kept until its host announces it. When a
    `Resync` stands in for that, the message goes into the mirror and to
    the frontend, with what waits in the chat from the host's status. Rows
    only the host has wait for M05. Rows passed on carry the mirror's
    `seq`, which the frontend pages by.
  - The approval policy "the AI reviewer, then me" sends what waits for
    the user to the AI reviewer when there is one. Each host's is "wait
    for me" until the hosts page sets it (U01).
  - The developer setting also takes `ROVIK_REMOTE_TOKEN`, the token whose
    hash the daemon's `config.toml` has, and `ROVIK_COMPUTER` (default
    "desktop").
  - `StartRequest.project_settings` and `ApprovalRequest.remote` are new
    optional fields. No daemon without them has shipped, so they need no
    feature (4.5).
  - A run that ended while its host was away is told as `runEnded` with
    outcome `unknown`, until results come (T05).
- **P09, a message's files.**
  - The upload before a remote `Send` (6.11's `ensure_blobs`) is part of
    `Remote`'s start, and its progress goes to the sink the app gives it,
    which emits `cloud-event`.
  - A daemon names `attachments` among its `Welcome`'s features, and the
    client sends a message's files only to one that does (4.5, K46).
  - `PUT /blobs` stores through `store_attached`, which prepares an image
    again. An image the app prepared comes out byte for byte the same; one
    that doesn't is refused. The 20 MB limit is the body read to it, as
    `/call` reads its own, not Axum's layer.
  - The core finds no browser in this environment, so the screenshot test
    uses a file only the daemon has, which another app of the same
    computer sent. Its path is a screenshot's: the files a row names come
    before the row.
  - After Stop during an upload, the input gets its message back the way
    it does for any send that fails (`ChatPanel.tsx`). The test checks
    that the send fails and that the daemon stored and started nothing.
    Cancelling the upload when the chat moves back comes with M07.
- **T01, shadow repositories.**
  - A snapshot reads the work tree with one diff from the shadow's index,
    which also keeps the stat data of the files that didn't change, and
    applies the diff's changes itself, deletions included. `add_all`
    would leave the stat data of an entry seeded from `HEAD` empty, and
    every snapshot would hash every tracked file again; `update_all` isn't
    needed.
  - The index is brought in line with `HEAD` before every snapshot, not
    only when the shadow is created. `HEAD` moves: a file committed later
    though an ignore rule matches it would have stayed out. Entries `HEAD`
    doesn't have that an ignore rule matches now come out, as `git add -A`
    on a fresh index wouldn't take them. While the project's `HEAD` can't
    be read, the index stays as it is.
  - Besides `core.autocrlf` and `info/exclude`, the shadow takes the
    project's `core.eol`, `core.excludesFile`, `core.attributesFile` and
    `info/attributes`, which also decide what `git add` stores or a
    checkout writes (T06). `core.safecrlf` is off: libgit2 refuses where
    git only warns.
  - Nested repositories and submodules stay out of the snapshot, not even
    as gitlinks. The guards list both: the nested repositories the
    snapshot left out, and `HEAD`'s gitlinks.
  - The guards are one call, `Shadow::guards`, for M06's preflight to
    show. The history is estimated as the size of the project's object
    store. The CRLF scripts are files with an executable mode or a shell
    extension.
  - Choosing to send a project without its history deletes its snapshot
    refs, so a snapshot taken before the choice doesn't bring the history
    in through the chain.
  - `HEAD`'s history can be read when the repository isn't shallow and the
    shadow reads `HEAD`'s parents. When the project's object folder
    changes, the shadow's index is cleared, since it may name objects only
    the old store had.
  - A shadow inside the folder it snapshots (a project at the home folder,
    say) leaves itself out of its snapshots.
  - libgit2 runs no filter drivers, so a Git LFS file is snapshotted with
    its content, not its pointer, and shows as changed from `HEAD`.
  - The tests are a test binary of their own, which sets libgit2's search
    paths: the global excludes are a fixture's, and the machine's git
    settings don't count. The hashed files are `Snapshot::hashed`, and a
    test checks that the index keeps every file's stat data.
- **T02, packs.**
  - Building a pack, taking one in and checking that a commit is complete
    are `rovik_core::packs`, which the client and the daemon share. A
    complete commit is one whose whole tree and parents are there; the
    history behind the parents isn't walked again.
  - The daemon creates `projects/<id>.git` on a project's first push.
    T03's `ProjectEnsure` adds the project's row and its exclude patterns.
  - A client may push only refs under `refs/rovik/`, written in letters,
    digits, `-`, `_`, `.` and `/`, so the push's query string needs no
    escaping and no new dependency. A chat's snapshot goes to
    `refs/rovik/chats/<chat>/snapshot` (`packs::snapshot_ref`).
  - A fetch may want only the commits of the project's refs.
  - A push against a ref that moved is refused with `409` and `Busy`. The
    client reads the refs before each push, so pushing again gets there.
  - The daemon reads a pushed pack with a 60-second idle timeout. The
    client has none of its own yet: H02's keepalives find a dead link.
  - The daemon names `projects` among its `Welcome`'s features (4.5), and
    the client checks for it before a push or a fetch.
  - A push's progress is `CloudEvent::Pushing`, from
    `Remote::push_snapshot`, in bytes sent: a pack's size isn't known
    while it's built. A fetch tells none yet; T06 brings the imports that
    would show it.
  - The client's HTTP requests no longer declare a length of 0 for a body
    whose length isn't known: such a body goes chunked.
  - P09's attachments test now picks the `Uploading` events out of what
    the app was told: `CloudEvent` has a second variant, which its
    patterns didn't allow for.
- **T03, projects and worktrees.**
  - The plan names no request that makes a chat's worktree before M04's
    import, so the first `WorkSync` of a chat makes it, at the snapshot.
    A later `WorkSync` changes nothing until T07 brings the merge.
    `WorkSync` also carries `dialogId` and `project`, which section 4.2
    leaves out, and answers `Synced { root }`, which T07 can grow.
  - `ProjectEnsure` creates the bare repository and the project's row,
    whose folder is the repository, and keeps the project's name, folder
    in its repository and settings in `state.json`. A worktree made later
    gets the settings under its root.
  - The worktree's ID file is written alone (`projects::write_id_file`),
    and the repository's `info/exclude` names `**/.rovik/project.json`
    after the client's patterns: the ID file stays out of results even
    when the snapshot has no `.rovik/.gitignore`. The repository's own
    config has `core.autocrlf=false`, whatever the account's says.
  - The client's patterns are `Shadow::excludes()`: the lines of the
    project's `info/exclude` and of the global excludes file
    (`core.excludesFile`, or git's XDG default), without comments and
    blank lines.
  - The test checks that the worktree's git ignores a file only the
    client's global excludes name. Results, which take the same rules,
    are T05's.
  - Deleting a chat removes its worktree, its branch `rovik/<chat>` and
    its refs under `refs/rovik/chats/<chat>/` in every project's
    repository, its root's `project_paths` row and its `state.json`
    entry. Garbage is collected at start-up: the worktrees of chats whose
    rows are gone, and folders in `work/` no chat has. R01's `Gc` can
    call the same.
  - A chat's id names a folder, a worktree and a branch, so a sync with
    one that isn't letters, digits, `-` and `_` is refused.
  - The daemon gives a chat it makes a row for the worktree's project.
- **T04, worktree mode.**
  - The daemon tells its core that its runs work in a server's worktrees
    (`AgentState::set_on_server`, at start-up): every daemon run is one
    (D8). The rules are in `agent_run/server.rs`.
  - The daemon runs a chat in its worktree's root, whatever folder the
    start names. With `--same-machine`, a chat without a worktree still
    runs in the start's folder, for development and milestone P's tests;
    without it, such a start is refused until the project syncs.
  - "Names `.rovik` in any spelling" is `.rovik` as a folder after a
    slash, with quotes and backslashes taken out. The project's own
    `.rovik/plans/…`, named from the worktree's root, doesn't match; named
    through `$PWD` or an absolute path, it does, and asks.
  - Such a command goes to the user with a note, the way commands the AI
    reviewer may never approve do, and never runs alongside the reads
    that need no approval.
  - Parallel workers are refused by the existing rule for chats without an
    overlay, which the test checks on the daemon.
  - The refusal of a start without a worktree still names `--same-machine`,
    as P03's test expects. P05's test that scans the daemon's files for
    keys now waits for the run's end to reach `state.json` first: under
    the whole suite's load it once listed `state.json`'s temporary file
    just before the rename took it away.
- **T05, results.**
  - Staging a work tree into an index against a base, which T01 wrote for
    the shadow, is `rovik_core::staging` now: the shadow stages the
    project against its `HEAD`, the daemon a chat's worktree against its
    landing base, through `results/<chat>.index` set on the worktree's
    repository. The same rules hold on both sides (K30).
  - The outcome-to-reason mapping is `agent_run::pending_reason`, which
    `try_land` and the daemon both call; its table test is in
    `landing.rs`.
  - `ChatResult` (`dialogId`, `commit`, `outcome`, `pending`) is what
    `WorkResult` answers, what `WorkResults` lists and what `ResultReady`
    carries. `WorkResult` takes the chat's last outcome, or `unknown` for
    a chat that never ran here, whose result waits as a run that didn't
    finish.
  - A result whose tree and base are the previous one's keeps its commit.
  - Every run's end of a chat with a worktree commits its result,
    including the end restart gives a run it won't pick up again.
  - `ResultReady` goes to the chat's computer after the run's end. The
    client passes it over until T06 imports results.
  - The daemon tests share `tests/common/project.rs`: a project carried
    to the daemon, and the calls that give a chat its worktree.
- **T06, results back on the desktop.**
  - `overlay::import` builds the chat's store from the result's commit and
    its parent, the landing base, both in the shadow. Renames aren't
    detected, so a moved file is a deletion and a new file, and a change
    between a file and a link is a deletion and a write. Build output and
    dependencies stay out, as `tiers.rs` keeps them out locally. Folders
    the project lacks get entries of their own, so the store replays. A
    deleted file is kept by its base content's hash, which landing checks.
  - The checkouts go through the shadow, whose attributes are read from the
    project's work tree, and leave the shadow's index as it is.
  - The store keeps what it came from in `import.json`: the result's
    commit, the paths it brought from the tree's root, which `WorkLanded`
    reports, and those outside the project's folder, which never land.
  - Which result a chat imported last is kept in the shadow, at
    `refs/rovik/imported/<chat>`, until M01 brings
    `mirror_state.imported_result`.
  - A new store is built in `overlays/.import-<chat>` and swapped in under
    the chat's gate. A result that changes nothing here leaves no store.
  - `Remote` (`remote/results.rs`) imports and lands remote chats' results
    through one queue, one job at a time. The jobs are `ResultReady`,
    `WorkResults` after every connection (complete or not, where 4.3 names
    only `complete: false`), the bar's Land (`Remote::land`), and the open
    project changing.
  - The open project is the folder the app opened last: `chat_open_project`
    tells `Remote`. A result whose project isn't open waits as "Ready to
    land", and lands by itself once it is. The app has no way to close a
    folder; since the review below, the open project also follows what
    the user says the open folder is.
  - A result lands by itself under `landing.rs`'s rules but never merged:
    one that would land merged with the project's newer changes waits,
    with the reason, until T08 checks it on the server. The bar's Land
    lands it merged, as it does locally.
  - `WorkLanded` also carries `dialogId`. The daemon commits the result's
    entries for the paths on the landing base, moves the chat's branch to
    that commit and resets the worktree's index (mixed). A newer result,
    made on the old base, is committed again on the new one: otherwise it
    would bring back what landed. The test of Resolve and Land anyway
    covers this with a newer result that waited while the conflicts were
    settled.
  - When the host can't be reached, a landing is reported after its next
    connection. The report is kept in memory: if the app restarts first,
    the next result carries the landed paths again, which land as no-ops
    unless the user undid that landing meanwhile.
  - The app's `overlay_*` commands aren't routed yet (T10). For a remote
    chat, T10's `overlay_land` calls `Remote::land`. The tests call it and
    the resolver directly.
  - `ChatResult` has no list of marked files yet. T07 adds the daemon's
    marks; `overlay::import` already takes them and writes `markers.json`.
  - The isolation bar saw an import the next time it looked (a run's start
    or end, or the chat opened), and notices say what happened. Since the
    review below, it looks again when a result comes in or lands.
  - "After the next sync" needs T07, so the test checks that the next
    result doesn't bring the undone change back.
  - The tests share `tests/common/app.rs`: the app's core, its `Remote` and
    its shadow, with the overlays' data folder held for the test.
- **T07, syncing before each message.**
  - `Remote::start` syncs a remote chat before each `Send`
    (`remote/sync.rs`): a snapshot, `ProjectEnsure` once a session per host
    and project, the push, and `WorkSync`. A snapshot the chat's last sync
    took isn't synced again, but a landing makes the next message sync,
    since it moved the landing base. A chat without a project, or on a
    daemon without `projects`, isn't synced. A sync that fails says so in a
    notice, and the message goes to the chat as it was.
  - The daemon merges in `sync.rs`, under the same lock as results and
    landings. It takes "chat resolves conflicts" from the chat's computer's
    configuration, which each start brings. A sync that applies moves the
    chat's branch to the snapshot and resets the worktree's index (mixed).
  - A link or a folder on either side is unmergeable, like a binary file. A
    file both sides added is merged against nothing, so different text
    conflicts.
  - `Synced` grows `state` and `files`, and `Event::SyncState` carries the
    same, as `ChatSync` with the chat's id. `SyncState` has four states:
    `synced` (applied; the files merged with the chat's own), `marked`,
    `conflicts` (waits; the chat could take them) and `unmergeable` (waits;
    binary, or deleted on one side and changed on the other).
  - The client keeps each chat's last sync (`Remote::sync_state`) for T10's
    `overlay_status` and U05's bar. Until U05, a sync that waits says why
    in a notice.
  - The marker text builder is `overlay::markers` now. `mark_one`,
    `markers_left` and the daemon's sync use it, with the same labels.
  - The daemon keeps the marked files in `state.json`. The next message's
    start tells the model of them once, with the note a local chat gets
    (`chats::markers_note`), added to the text the model gets for the
    message (`current_user_content`, else `agent_run::wire_content`). Each
    result lists the marked files that still have their markers
    (`ChatResult.marked`), which the import writes to `markers.json`, so
    landing waits until they're gone.
  - A daemon run wasn't reminded of markers left when it ended
    (`remind_of_markers`); since the review below, it is.
  - T06's tests now go through the sync. The undo test checks that the
    next message's sync took the undo to the worktree. The conflict test's
    chat doesn't resolve conflicts, so its second message's sync waits, and
    the local edits come after its first result, which waits for the
    project to open: before, the sync would have brought them to the chat.
- **T08, merges checked on the server.**
  - `WorkRecheck` carries what `WorkSync` does (`dialogId`, `project`,
    `snapshot`, `excludes`) and answers `Synced`. The daemon merges as a
    sync does. A merge that applies with merged files starts the chat with
    `CheckMerged { files }`, from its last start's settings as a restart
    does; one that merged nothing commits a result at once. A merge that
    doesn't apply starts nothing, and the result waits with the reason.
  - `CheckMerged` sets `merged_from_project` without the diffs, which only
    the server's merge knows, so the fix round names the merged files
    without showing what the project changed in them.
  - The client sent every result that would land merged back to the
    server, checks on or off: with the checks off, the server's run ended at
    once and its result landed merged. Since the review below, a result
    without checks to run lands merged here.
  - Rounds are counted per chat, at most two; a landing or a new message
    starts the count again. After two, the result waits with
    `LandError::Unchecked`'s reason, "changed again".
  - `overlay::import::land_by_itself` no longer sets a reason of its own:
    the client decides between a recheck and waiting.
  - An import replaces a store by moving the old one aside and the new one
    in, one rename after the other, then deleting the old one: before, the
    chat had no store while the old one was deleted, and a test that read
    the project in that gap saw the old file.
- **T09, the overlay moves with the chat.**
  - `Shadow::fold` takes a fresh snapshot and the chat's overlay changes
    (`chats::changes`), and builds `S0'`, a commit on the snapshot, and
    `C0`, a commit on `S0'`.
  - A file the chat created, and a move's destination, aren't in `S0'`,
    unless the chat replaced a project file there. A deleted file is in it
    as the project has it now: a deletion whose file changed here since is
    a conflict, and folding refuses it until it's settled, as 6.8's
    preflight asks. A move without a change of content takes the project's
    file. Folders aren't entries of their own.
  - Base copies and upper files are stored through the shadow's filters
    for their paths (`Repository::blob_writer` with the path as its hint),
    so a CRLF file is stored as `git add` would store it.
  - `WorkSync` grows `changes`: a commit on `snapshot` that a new worktree
    checks out as changes, its index left on the snapshot, so the agent's
    `git status` shows them as the chat's own. A chat that has a worktree
    already ignores it. M04's import can make worktrees the same way.
  - Moving itself (pushing `C0`, the flip, removing the local store) is
    M06's: the test does those steps by hand.
- **T10, the isolation bar's remote half.**
  - The requests are `WorkStatus { dialogId }` → `Changes` (each path
    `added`, `changed` or `deleted`), `WorkFile { dialogId, path }` →
    `WorkFile` (`base`, `chat`), `WorkDiscard { dialogId }` → `Done`, and
    `WorkConflictsToChat { dialogId, snapshot, files }` → `Handed`. Paths
    are from the project's folder, as a local overlay's are, and changes
    outside it aren't listed.
  - `WorkStatus` is what the chat's result would change: the worktree
    staged through the chat's results index against its landing base. It
    follows the results' rules, and stops listing a path once `WorkLanded`
    has moved the base.
  - `WorkConflictsToChat` takes the project's versions from a snapshot the
    client pushes first, so they're stored as `git add` would store them,
    and moves those files' landing base to them. That is the remote form of
    what `rebase_file` does locally: without it, the chat's resolution
    would conflict again when it lands. The client drops its import, whose
    pending conflicts would hold back the chat's next result. The request
    it returns is the message that tells the chat, so the daemon adds no
    note of its own.
  - `WorkDiscard` resets the worktree hard to the landing base and removes
    the files the chat made. libgit2's hard reset keeps untracked files, so
    a checkout that removes them follows. Ignored files stay. `WorkDiscard`
    and `WorkConflictsToChat` are refused while the chat's run is going.
  - The routes are `Remote` methods (`remote/bar.rs`), which the app's
    commands call for a remote chat. `overlay_status` is the imported
    result's status with the worktree's files and the last sync, in a new
    `sync` field of `OverlayStatus`. `check_idle` asks the host whether the
    run is going, before anything changes the chat's files. `overlay_land`
    goes through `Remote::land`; Undo and the resolver stay local. Redo,
    Undo the merge and Unmark are refused (`LOCAL_ONLY`).
  - `chat_read_plan` reads a remote chat's plan from its worktree, by the
    part of the path from `.rovik/plans/` on, and a local chat's from the
    path. The frontend moves to it with U04.
  - The tests drive the routes through `Remote` against the real daemon.
    The app's commands only choose between the remote and the local route.
- **After milestone T, a review of its limits.** Each limit the notes above
  name was settled with the plan's owner; the fixes are follow-ups to the
  tasks they change.
  - Which result a chat imported last, and a landing its host wasn't told
    of yet, go into M01's `mirror_state` (6.3, 6.5 and M01's task say so
    now). Until M01, T06's shadow ref and in-memory list stay.
  - Results are numbered (`ChatResult.seq`, additive): the daemon counts
    each chat's results in `state.json`, and the same result again keeps
    its number. The client passes over a result older than the newest one
    that came in this session, or that one again. A `ResultReady` replayed
    after a connection comes behind the result `WorkResults` brought: when
    it was older, its commit was at no ref any more, so its fetch failed
    and the chat said its changes didn't come back.
  - A result with the imported one's commit, from a run that changed
    nothing more, now decides again whether the changes land while they
    wait here: a run stopped and then finished without changes left them
    waiting with the stop's reason. After the app starts, the client
    doesn't know which number it imported, so such a result changes
    nothing until M01 keeps the number. `Remote::result_ready` is public
    for the test that replays an older result.
  - A result says whether checks would run on it merged with the project's
    newer changes (`ChatResult.unchecked`, additive). The daemon works it
    out as it commits the result, from the checks of the chat's root and its
    computer's permissions, with `agent_run::checks_would_run`, which local
    landing now calls too. The import keeps it in `import.json`, and
    `land_by_itself` lands such a merge as a local chat does. Before, it
    went back to the server for a run with nothing to check, which added a
    "no checks are set up" record to the chat when verification was on.
    `overlay::import::import` and `Shadow::import` take the `ChatResult`.
  - The open project follows the user's answers about the open folder:
    "Separate history" for a copy gives it a project of its own, and "Use
    its chats" for a project that moved makes it that project's.
    `chat_answer_copy` and `chat_answer_moved` tell `Remote`
    (`folder_answered`), and results that waited for the new project land.
    Before, the first project's results landed in the copy's folder.
  - A daemon run that ends with conflict markers left is reminded of them,
    twice at most, as an overlay chat's is: the daemon gives its core the
    files it marked that still have them (`set_markers_resolver`), and
    `remind_of_markers` asks it. The T07 test's chat now leaves its
    markers after two reminders. The daemon keeps every mark from the
    worktree's root: those handed to the chat (`WorkConflictsToChat`) were
    kept from the project's folder, so in a project in a subfolder their
    markers weren't found and the result didn't wait for them. The note
    and the reminder name the files from the chat's root.
  - The app is told when a remote chat's changes may look different: its
    result came in or landed, or its project synced (`CloudEvent::Changed`
    on `cloud-event`). The chat panel listens (`lib/cloud.ts`), and its
    isolation bar looks again. Before, the bar looked only while a run went
    and once it ended, so an import or a landing after that showed the next
    time the chat was opened. Showing the sync state stays U05's, and
    reading plans with `chat_read_plan` U04's.
