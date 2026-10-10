# Rovik IDE

A lightweight desktop code editor with a built-in AI coding agent.

Open a project folder, edit files, run commands in the terminal, use Git,
and chat with an AI agent that can read, change and test your code. It
runs on Linux, macOS and Windows and uses much less memory than
Electron-based editors.

- [Features](#features)
- [Installing and running](#installing-and-running)
- [Getting started](#getting-started)
- [For developers](#for-developers)
- [How some parts work](#how-some-parts-work)

---

## Features

**Editor**

- Monaco editor (the editor used in VS Code) with syntax highlighting,
  themes and tabs.
- IntelliSense for JavaScript/TypeScript that understands the whole
  project, not only open files. Ctrl+click jumps to a definition in another
  file. Rust gets help from rust-analyzer when it's installed.
- A file tree that stays fast on very large projects and refreshes when
  files change on disk.
- Project-wide text search powered by ripgrep's engine.
- A built-in terminal that uses your own shell.
- A Git panel: commit, branches, stash, history, pull and push. Merge
  conflicts are resolved in a side-by-side conflict resolver.

**AI agent**

- Works with any OpenAI-compatible provider: OpenAI, Anthropic, Google,
  OpenRouter, Fireworks, or a local model in Ollama or LM Studio. You add
  your own API key.
- Two modes:
  - **Agent** does the work: reads code, edits files, runs commands and
    checks the result.
  - **Plan** only looks around, asks you questions and writes a plan. When
    you approve it, Agent mode carries it out.
- **You stay in control.** You choose which commands the agent may run and
  which actions need your approval. Proposed file edits show as a diff in
  the editor, and you can accept or reject each change separately. You can
  also let a second "reviewer" AI model approve routine actions.
- **Automatic checks.** When the agent finishes, the IDE runs the project's
  type check, lint and tests. If anything fails, the agent is sent back to
  fix it.
- **Several chats at once, safely.** Each chat works on its own private
  copy of your changes, so two chats (or a chat and you) never overwrite
  each other. When a chat finishes, its changes are merged into the
  project. Conflicts are shown to you, and a landing can be undone.
- **Long tasks stay on track.** Older history is trimmed automatically, or
  the agent can manage its own context (see
  [Context management](#context-management)).
- **Subagents (optional, off by default).** The agent can send scouts:
  read-only helpers that explore one question about the code in a fresh
  context and hand back a short report, so long explorations don't fill
  the agent's own context. Turn them on, and pick their models, in
  Settings → Subagents.
- The agent follows your project's own instructions in `AGENTS.md`,
  `CLAUDE.md` and similar files.
- Chats are saved and searchable, and they keep running in the background
  when you switch to another chat.

---

## Installing and running

There are no prebuilt downloads yet, so you build the app from source.

### Linux

Run these from the project folder:

```bash
./setup.sh   # once: installs system packages, Rust and npm dependencies
./dev.sh     # starts the app
./build.sh   # builds an installable package (AppImage, .deb, …)
```

`setup.sh` supports apt, dnf and pacman. It asks for `sudo` to install
system packages, installs Rust through rustup if yours is missing or too
old, and runs `npm install`. You can run it again safely.

Built packages end up in `src-tauri/target/release/bundle/`.

### macOS

1. Install the Xcode Command Line Tools: `xcode-select --install`
2. Install [Rust](https://rustup.rs) and [Node.js](https://nodejs.org) 18
   or newer.
3. From the project folder:

   ```bash
   npm install
   npm run tauri dev      # start the app
   npm run tauri build    # build an installable package
   ```

### Windows

1. Install
   [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
   with the **Desktop development with C++** workload.
2. Install [Rust](https://rustup.rs) with the default
   `x86_64-pc-windows-msvc` toolchain (MinGW isn't supported).
3. Install [Node.js](https://nodejs.org) 18 or newer.
4. Install LLVM: `winget install LLVM.LLVM`
5. From the project folder, run the same `npm` commands as on macOS.

WebView2 is already part of Windows 10 and 11.

More details on prerequisites:
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

### Optional: virtual files driver

Some features work best with a virtual-file driver: FUSE on Linux, macFUSE
on macOS, WinFsp on Windows. The app runs without one. When it's missing,
the app offers to install it for you; you can also do it later in
**Settings → Virtual files**. The driver is required for the agent's
self-managed context (CLM mode), and it makes the agent's shell commands
faster in isolated chats.

---

## Getting started

1. **Open a project:** choose a folder. It appears in the file tree.
2. **Add a model:** open **Settings → Model providers**, paste the
   provider's base URL and your API key, and choose the models you want.
3. **Review permissions:** in **Settings → Permissions & Approval**, choose
   which commands the agent may run and what needs your approval. By
   default the agent may only run a short list of safe commands (`ls`,
   `cat`, `cargo clippy`, …), and it edits files without asking. Turn
   approvals on here if you want to review its edits and commands first.
4. **Chat:** open the AI Chat tab, pick a model and a mode (Agent or Plan),
   and describe what you want.
5. **Check the result:** files the chat changed are listed above the
   message box. Open one to compare it with the project version, then
   **Land** the changes or **Discard** them.

Other settings pages:

| Page            | What it's for                                                      |
| --------------- | ------------------------------------------------------------------ |
| Editor          | Theme and editor options                                           |
| Project checks  | Fix the type-check / lint / test commands the IDE detected         |
| Builds & checks | How many heavy builds may run at once, so the machine stays usable |
| Virtual files   | Driver status, installation and troubleshooting                    |

When the agent runs commands from the IDE, they get the same `PATH` as your
login shell, so tools like `npm`, `cargo` and Homebrew packages are found
even when you start the app from a desktop icon.

---

## For developers

### Tech stack

| Part           | Choice                         | Why                                                                                                                |
| -------------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| App shell      | [Tauri 2](https://tauri.app)   | Uses the OS's own web view instead of bundling Chromium, so it uses far less memory than Electron.                 |
| UI             | [SolidJS](https://solidjs.com) | No virtual DOM: an update touches only the DOM nodes that depend on it, so the UI stays fast as it grows.          |
| Editor         | Monaco                         | VS Code's editor component.                                                                                        |
| Long lists     | `@tanstack/solid-virtual`      | Only visible rows are rendered, so a tree with 50,000 files costs about the same as one with 50.                   |
| Backend        | Rust                           | Files, Git (libgit2), search (ripgrep crates), terminal, language servers, the AI agent, chat storage (SQLite).    |
| Code structure | tree-sitter                    | Outlines of source files for the agent: find a symbol, build a "repo map" of the most relevant code for a request. |

The AI agent runs **in the Rust backend**, not in the web view. A run keeps
going if you switch chats or reload the UI; the frontend only starts it,
stops it, answers its approvals and shows its progress.

### Commands

| Command                                       | What it does                                                            |
| --------------------------------------------- | ----------------------------------------------------------------------- |
| `npm run tauri dev`                           | Run the app with hot reload (same as `./dev.sh`)                        |
| `npm run tauri build`                         | Build a release package (same as `./build.sh`)                          |
| `npm test`                                    | Frontend unit tests (`src/**/*.test.ts`)                                |
| `cd src-tauri && cargo test --lib`            | Backend tests                                                           |
| `cd src-tauri && cargo test vfs -- --ignored` | Mounts a real virtual volume (Linux) and runs shell tools and Git on it |
| `npm run format`                              | Format with Prettier (`format:check` to verify)                         |

To debug model requests, set `ROVIK_API_LOG=1`. Every request and response
is then logged to `<temp dir>/rovik-api-log.jsonl`; set it to a file path
to log there instead. API keys are not written to the log.

### Project layout

```
src/                     frontend (SolidJS + TypeScript)
  App.tsx                main window: wires the panels together
  components/            UI: Editor, FileTree, ChatPanel, GitPanel,
                         TerminalPanel, SearchPanel, settings pages, …
  lib/                   state and logic, no UI
    agentRuns.ts         starts, stops and follows agent runs
    approvals.ts         approvals waiting for the user
    permissions.ts       permission settings (enforced in the backend)
    intellisense.ts      project-wide TypeScript IntelliSense
    lsp/                 language server client (rust-analyzer)
    commands.ts          typed wrappers around backend commands

src-tauri/src/           backend (Rust)
  lib.rs                 app setup: plugins and command registration
  filesystem.rs          directory listing and the file watcher
  git.rs, git_sync.rs    Git panel: status, commit, pull, push
  merge.rs               three-way merge (no git program needed)
  terminal.rs            the terminal panel
  code_search.rs         project search
  lsp/                   language servers, bridged to the editor
  agent_run/             the AI agent: model turns, tool calls, approvals,
                         AI reviewer, verification checks
  chat_request.rs        sending requests to model providers
  chat_db/               chat storage (SQLite)
  clm/                   agent-managed context (CLM mode)
  overlay/               each chat's private view of the project
  vfs/                   virtual files (FUSE / macFUSE / WinFsp)
  workload/              queue for heavy commands (builds, tests)
  file_guard.rs          stops a write from overwriting changes it hasn't seen
  outline/, repo_map/    tree-sitter outlines and the repo map
  project_profile.rs     detects the stack, checks and agent instruction files
  browser_tools.rs       web search / browsing for the agent

docs/design/             design documents (chat isolation)
scripts/test.mjs         frontend test runner
```

---

## How some parts work

### Chat isolation

Every agent run works in an **overlay**: the project as it is on disk, with
that chat's changes on top. Only the changed files are stored. When a run
ends, its changes are **landed**: merged with whatever changed in the
project meanwhile, checked, and written. A clean merge with passing checks
lands on its own. Anything else (conflicts, failing checks) is shown in the
bar above the chat's message box, where you can resolve conflicts, land
anyway, discard, or undo a landing.

Limits: 512 MiB of changed files per chat (warning at 80%). Build output and
dependency folders don't count toward it. Heavy commands such as builds and
tests share a small number of CPU "slots" (one by default) so that several
chats building at once don't freeze the machine. A heavy command in your
own terminal takes a slot too, and is never slowed down.

Design and reasoning: [`docs/design/chat-isolation-plan.md`](docs/design/chat-isolation-plan.md).

### Virtual files

`src-tauri/src/vfs/` provides files that live in the app's memory but look
like real files to every other program. Commands the agent runs can `cat`,
`grep`, `sed -i` or execute them, and the app sees what they changed.

| OS      | Driver  | Installed from                                      | Notes                                                                                                                                                                                              |
| ------- | ------- | --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Linux   | FUSE 3  | the distro's `fuse3` package, via polkit (`pkexec`) | Mounted with `fusermount3`; libfuse isn't needed to build or run.                                                                                                                                  |
| macOS   | macFUSE | the official `.dmg` (signature checked)             | On macOS 15.4+ with macFUSE 5.1+ it uses FSKit (no kernel extension; enable it once in System Settings), mounted at `/Volumes/RovikVirtualFiles`. Older systems need the kernel extension allowed. |
| Windows | WinFsp  | the official `.msi` (signature checked)             | `winfsp.dll` is loaded on first use. Building needs LLVM (`libclang`) for the bindings.                                                                                                            |

The app starts without the driver; only mounting needs it. At startup it
checks in the background and either mounts the volume or offers once to
install the driver (`src/lib/virtualFilesStartup.ts`). If mounting fails,
**Settings → Virtual files** explains why and what to do, and keeps
retrying (`problem.rs`).

Using it from Rust:

```rust
let vfs = app.state::<vfs::VfsState>();
let path = vfs.create_file("plans/step-1.md", b"...", false)?; // a real path

// Be told when a program finishes writing a file:
let id = vfs.subscribe(EventMask::CLOSED, |e| {
    if let Op::Closed { modified: true } = e.op { /* e.path was changed */ }
});

// Every file keeps its revisions:
let revisions = vfs.revisions("notes.md")?;
let old = vfs.read_revision("notes.md", 2)?;
vfs.restore_revision("notes.md", 2)?; // creates a new revision
```

From the frontend, `src/lib/virtualFiles.ts` offers the same calls
(`writeVirtualFile`, `subscribeVirtualFiles`, `listRevisions`, …).

Details:

- Events: created, opened, read, written, truncated, closed (with whether it
  was modified), removed, renamed. `subscribe_batched` delivers them in
  batches on a background thread. With no subscribers, the cost is a single
  atomic check. See `vfs/events.rs` for what a callback may do.
- History: a revision is saved when a program closes a file it modified, or
  when the app writes it. History follows renames and "safe saves" (a
  temporary file renamed over the original) and survives deletion. Up to 64
  revisions per file and 256 MiB in total are kept, oldest dropped first;
  files over 32 MiB aren't versioned (`set_history_limits`).
- Files: `store.rs` (the in-memory filesystem with POSIX semantics),
  `fuse.rs` / `macfuse.rs` / `winfsp_fs.rs` (OS drivers), `problem.rs`
  (mount errors), `events.rs`, `history.rs`, `driver.rs` (finding and
  installing the driver).

### Context management

The row under the model picker in the AI Chat tab sets, per chat, how the
agent's context is kept within the model's limit (`src/lib/contextMode.ts`):

- **Trimming** (default): the app shortens older history. Outdated content
  becomes a short note, old tool results are cut, and the oldest turns are
  dropped (`src-tauri/src/chat_db/history.rs`).
- **CLMs**
  ([Context Language Models](https://github.com/facebookresearch/context-language-models)):
  the agent manages its own context by editing it as a file
  (`src-tauri/src/clm/`). Requires the virtual files driver; without it, the
  chat falls back to Trimming.

How CLM mode works:

- Before each request, the context is written to
  `context/<chat>/LIVE_CONTEXT.md` on the virtual volume, one
  `[[CTX_TURN n role=… id=…]]` block per message. The agent edits it with
  its normal tools, and whatever the file says when the next request goes
  out becomes its context.
- Edits are read back tolerantly (`clm/document.rs`). An edit that grows the
  context past the budget, or empties it, is rejected and the agent is told
  why. Stored messages are never changed.
- **Your messages are protected.** If an edit removes or rewrites one of
  your messages, the app puts it back. (A very long one, such as a pasted
  log, may be shortened, with a pointer to the full text.)
- The agent keeps its own notes in a task card, `@context/TASK.md` (goal,
  scope, decisions, progress, next step). It's saved in the chat database
  and survives restarts.
- Every state of the file is a revision the agent can read
  (`@context/revisions/<n>.md`) or restore (`restore_context`). The context
  row in the UI shows the current revision and size, and lets you view or
  restore revisions.
- The agent gets size reminders at 50% and 75% of the budget, and on every
  request from 90%. Past the budget, old tool output is withheld first, then
  the request is trimmed as in Trimming mode.
- The CLM part of the system prompt is identical on every request of a
  chat, so providers can cache it.
