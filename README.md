# Rovik IDE

A lightweight desktop code editor with a built-in AI coding agent.

Open a project folder, edit files, run commands in the terminal, use Git,
and chat with an AI agent that can read, change and test your code. It
runs on Linux, macOS and Windows and uses much less memory than
Electron-based editors.

Website and downloads: [rovik-ide.dev](https://rovik-ide.dev).

- [Features](#features)
- [Installing and running](#installing-and-running)
- [Getting started](#getting-started)
- [Cloud hosts](#cloud-hosts)
- [For developers](#for-developers)
- [How some parts work](#how-some-parts-work)

---

## Features

**Editor**

- Monaco editor (the editor used in VS Code) with syntax highlighting,
  themes and tabs.
- Markdown files have a rendered preview, as GitHub shows them: beside the
  source (Ctrl+K V), instead of it (Ctrl+Shift+V), or switched with the
  buttons at the end of the tab strip. It scrolls with the editor, opens
  links (project files in a tab), shows the project's images, colors code
  like the editor, and its task boxes can be ticked. While typing, lists
  continue on Enter, and Ctrl+B / Ctrl+I make text bold or italic.
- IntelliSense for JavaScript/TypeScript that understands the whole
  project, not only open files. Ctrl+click jumps to a definition in another
  file. Rust gets help from rust-analyzer when it's installed.
- A file tree that stays fast on very large projects and refreshes when
  files change on disk.
- Project-wide text search powered by ripgrep's engine.
- Several windows, each with a project of its own: **New window** in the
  project menu opens another, and `rovik-ide <folder>` opens a folder in a
  window of its own. A project is open in one window at a time (a copy
  sharing its chats too): opening it again brings that window to the front
  (see [Several windows](#several-windows)).
- A built-in terminal that uses your own shell, on this computer or on a
  connected [cloud host](#a-terminal-on-a-cloud-host).
- A Git panel: commit, branches, stash, history, pull and push, and a
  rebase onto any local or remote branch, found by searching. Merge
  conflicts are resolved in a side-by-side conflict resolver: the
  conflicts git wrote in the file, each settled with one side, both,
  lines ticked from either side, or an edit by hand.

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
  fix it. It's told when a fix made no progress, a fix that breaks
  something that passed before is rolled back, and tests weakened to pass
  (skipped, deleted, fewer assertions) are flagged for you instead of
  counting as verified.
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
  the agent's own context. A change reviewer can read each finished
  change with fresh eyes before the checks run; the agent fixes or
  answers what it finds. When a check fails with a long output, or fails
  again, a triage helper reads all of it and tells the agent the root
  cause, instead of the agent getting pages of compiler output. In Plan
  mode, a plan reviewer checks each plan against the code before it's
  saved. When the agent implements a long plan, step workers can carry it
  out one step at a time, each writing only its step's files; in an
  isolated chat, steps the plan groups can run side by side, each in its
  own copy, then merged and checked once. If those checks fail, each
  failure's cause goes back to the worker whose files it's in, the agent
  fixes the rest, and the checks run once more; if it still fails, you
  can redo the group one step at a time. A test writer can write tests
  from the request and the plan without reading the new code. Turn them
  on, and pick their models, in Settings → Subagents.
- **Autopilot (optional).** A chat in Autopilot mode carries out a whole
  task from one message without asking: it plans, reviews, implements,
  checks and delivers, and writes down what it couldn't decide. How
  thorough it is, when a task stops, and the commands it may never run are
  set in Settings → Autopilot.
- The agent follows your project's own instructions in `AGENTS.md`,
  `CLAUDE.md` and similar files.
- Attach images, PDFs and text files to a message: paste them (Ctrl+V, or
  Paste in the message box's right-click menu) or pick them with the
  paperclip button. Only formats models can read are taken (see
  [Attachments](#attachments)), and only those the chosen model is set up
  for: images need Vision, PDFs need PDF, in Settings → Model providers.
  Images are sent as PNGs at most 768 pixels a side. A message copied from
  the chat or the message box pastes back with its attachments.
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
3. **Review permissions:** in **Settings → Permissions & approval**, choose
   which commands the agent may run and what needs your approval. By
   default the agent may only run a short list of safe commands (`ls`,
   `cat`, `cargo clippy`, …), and it edits files without asking. Turn
   approvals on here if you want to review its edits and commands first.
4. **Chat:** open the AI Chat tab, pick a model and a mode (Agent or Plan),
   and describe what you want.
5. **Check the result:** files the chat changed are listed above the
   message box. Open one to compare it with the project version, then
   **Land** the changes or **Discard** them.

Every setting applies as soon as you change it, and the Settings menu
shows each page's state: what's set, and whether it has an effect on this
computer right now. The pages:

| Page                   | What it affects                                                                                                       |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Editor                 | How code looks and is formatted in the editor: theme, font, indentation, wrapping, formatters                         |
| Model providers        | The models chats can use, with your API keys; the AI reviewer and the subagents pick theirs here too                  |
| Permissions & approval | What the agent may run and change in every chat, and what waits for your approval, or an AI reviewer's, first         |
| Agent runs             | While the agent works and when it finishes: pauses, its checklist, the checks on its changes, conflicts between chats |
| Subagents              | The helpers the agent can send, their models and limits                                                               |
| Autopilot              | Chats that carry out a whole task without asking: effort, budget, the commands they never run                         |
| Project checks         | The type-check / lint / test / build commands of the open project, as detected or as you fix them                     |
| Heavy commands         | How many builds and tests run at once on this computer, so it stays responsive                                        |
| Virtual files          | The driver that isolates chats and lets the agent manage its own context: status, installation, troubleshooting       |
| Cloud hosts            | Servers your chats can run on (see [Cloud hosts](#cloud-hosts))                                                       |

The model, the mode (Agent, Plan or Autopilot), the reasoning effort and
the context mode are chosen per chat, above its message box.

When the agent runs commands from the IDE, they get the same `PATH` as your
login shell, so tools like `npm`, `cargo` and Homebrew packages are found
even when you start the app from a desktop icon. A tool installed after the
app (or a cloud host's daemon) started, by a host's setup script for one, is
found too: when an Autopilot install needs a program that isn't on that
`PATH`, the login shell's is read again first.

---

## Cloud hosts

A cloud host is a Linux server that your chats can move to and run on. You
add one in **Settings → Cloud hosts**, and Rovik logs in to it over SSH as
your own account there. If you run a server that several people share, see
[Rovik on a team server](docs/cloud-hosts.md).

### An SSH key for a cloud host

Make the key on the computer that runs Rovik, put its public half in your
own account on the server, then point Rovik at the private half.

1. **Make the key.** On macOS or Linux:

   ```bash
   ssh-keygen -t ed25519 -f ~/.ssh/rovik_ed25519 -C "rovik on my-laptop"
   ```

   On Windows, in PowerShell (Windows 10 and 11 include the OpenSSH client):

   ```powershell
   ssh-keygen -t ed25519 -f $env:USERPROFILE\.ssh\rovik_ed25519 -C "rovik on my-laptop"
   ```

   A passphrase is optional. If you set one, Rovik keeps it in the system
   keychain. You get two files: `rovik_ed25519`, the private key, which never
   leaves your computer, and `rovik_ed25519.pub`, the public key, which goes
   on the server and is safe to share.

2. **Put the public key in your account on the server.** Use a regular
   account: Rovik's daemon never runs as root, so it won't deploy over a root
   login, and on a shared server each person has their own account. If you
   can already log in with a password, on macOS or Linux:

   ```bash
   ssh-copy-id -i ~/.ssh/rovik_ed25519.pub you@server
   ```

   Windows has no `ssh-copy-id`. In PowerShell:

   ```powershell
   type $env:USERPROFILE\.ssh\rovik_ed25519.pub | ssh you@server "umask 077; mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys"
   ```

   If you can't log in yet, send the `.pub` file to the server's
   administrator, who adds it to your account (here `ann`):

   ```bash
   sudo adduser ann   # only if the account doesn't exist yet
   sudo mkdir -p /home/ann/.ssh
   sudo tee -a /home/ann/.ssh/authorized_keys < rovik_ed25519.pub > /dev/null
   sudo chown -R ann: /home/ann/.ssh
   sudo chmod 700 /home/ann/.ssh && sudo chmod 600 /home/ann/.ssh/authorized_keys
   ```

   On a new cloud server, you can instead paste the contents of the `.pub`
   file into the provider's SSH keys field when you create the server. If
   the provider only sets up root, create a regular account as above first.

3. **Check it from a terminal:**

   ```bash
   ssh -i ~/.ssh/rovik_ed25519 you@server
   ```

   It should log you in without asking for a password. It asks only for the
   key's passphrase, if you set one.

4. **Add the host in Rovik.** In **Settings → Cloud hosts**, click
   **+ Add host** and fill in **Host**, **Port** and **User** (your account).
   Set **Log in with** to **A key file**. In **Key file**, enter the full path
   to the private key, not the `.pub` file: `/home/you/.ssh/rovik_ed25519` on
   Linux, `/Users/you/.ssh/rovik_ed25519` on macOS, or
   `C:\Users\you\.ssh\rovik_ed25519` on Windows. Enter the **Key passphrase**
   only if the key has one. Click **Save**, then **Test connection**.

5. **Trust the server's key.** The first test shows the server's key
   fingerprint. Click **Trust this key** once it matches one of the lines
   this prints on the server:

   ```bash
   for key in /etc/ssh/ssh_host_*_key.pub; do ssh-keygen -lf "$key"; done
   ```

Instead of a key file, you can load the key into your SSH agent
(`ssh-add ~/.ssh/rovik_ed25519`) and choose **My SSH agent**. Rovik then
never reads the key file. On Windows, Rovik uses the OpenSSH Authentication
Agent service, which is off until you start it, or Pageant. To start the
service, in PowerShell run as administrator:

```powershell
Get-Service ssh-agent | Set-Service -StartupType Automatic -PassThru | Start-Service
```

Ed25519 keys are the best choice. RSA and ECDSA keys work too, and so do
PuTTYgen `.ppk` files, so an existing PuTTY key works as it is.

On a server several people share, whoever deploys Rovik there first becomes
its administrator, and that account needs `sudo`. Everyone else only needs
their own account with their key in it.

### A terminal on a cloud host

Once a cloud host is connected, the **+** of the terminal panel asks where
the new terminal runs: **Local**, on this computer, or **_name_ (remote)**,
your login shell on that host, in your home folder there. `` Ctrl+Shift+` ``
still opens a local one at once. While a host is connected, each tab says
where it runs: `bash (local)`, `bash (Build server (remote))`.

A remote terminal logs in as the host's settings say, on a connection of its
own. On a host set to **My system ssh**, it runs your own `ssh` to the host,
with your configuration.

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
| `scripts/build-agentd.sh`                     | Static builds of the daemon for Linux servers (x86_64 and aarch64)      |

`scripts/build-agentd.sh` needs zig and cargo-zigbuild:
`pip install cargo-zigbuild` brings both. Each build is a single file that
runs on any Linux, in `src-tauri/target/agentd/`. To deploy one, start the
app with `ROVIK_AGENTD_BINARY` set to its path.

### Releases

Every merge to master is released. The **Release** workflow
(`.github/workflows/release.yml`) runs on each one:

1. It runs the checks first, the CI workflow's: the tests on Linux, macOS
   and Windows. A merge that fails them isn't released.
2. It chooses the version: the one in the sources while no release has it
   (the first release, or one a pull request set with
   `scripts/set-version.sh 1.3.0`), otherwise the sources' version with
   its last number raised, `1.2.3` to `1.2.4`, which it commits to master
   as `Release 1.2.4 [skip ci]`.
3. It builds the app for Linux (AppImage, .deb, .rpm), macOS (a disk
   image each for Apple silicon and Intel) and Windows (installer and MSI
   package), and the daemon for x86_64 and aarch64 Linux servers.
4. It signs the builds and publishes them on the version's GitHub
   release, tagged `v1.2.4`. The download buttons on
   [rovik-ide.dev](https://rovik-ide.dev) show it at once: they ask GitHub
   for the latest release.

Nothing is published until every build is on the release, so a run that
fails leaves the latest release as it was. The next merge publishes the
version it couldn't; so do **Re-run failed jobs** on its run and **Run
workflow** (in the **Actions** tab, under **Release**) with the version
left empty. **Run workflow** with a version, such as `2.0.0`, releases
that version from the branch you pick.

The app downloads the daemon for a server from its own version's release,
and checks it against the release key before it goes to the server. The
daemon on a server checks an upgrade against the same key.

Before the first release, make the key once:

1. Run `npx tauri signer generate -w ~/.tauri/rovik.key` and choose a
   password. Keep the file and the password safe: without them there are
   no more signed releases.
2. In the repository's **Settings → Secrets and variables → Actions**, add
   `TAURI_SIGNING_PRIVATE_KEY`, with the contents of `~/.tauri/rovik.key`,
   and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, with the password.
3. Paste the public key, the one line in `~/.tauri/rovik.key.pub`, below
   the comments in `src-tauri/release-key.pub`, and commit it. The app's
   updater takes the same key (`plugins.updater.pubkey` in
   `tauri.conf.json`); once `bundle.createUpdaterArtifacts` is `true`
   there too, each release has the updater's `latest.json`.

Versions have numbers only, such as `1.2.3`, or `1.2.3-4` for a
pre-release, which GitHub publishes as one: a Windows installer refuses
anything else. The version is written in `src-tauri/Cargo.toml`
(`[workspace.package]`) and `package.json`; to change it by hand, run
`scripts/set-version.sh 1.2.3`. After a pre-release such as `1.2.3-4`,
merges release `1.2.3-5`, `1.2.3-6`… until a pull request sets the next
version, or **Run workflow** releases one.

#### Protecting master

GitHub can refuse to merge a pull request until its checks pass. In the
repository's **Settings → Rules → Rulesets**, choose **New ruleset → New
branch ruleset**:

1. Name it (`master`) and set **Enforcement status** to **Active**.
2. Under **Bypass list**, add **Repository admin**, set to **Always
   allow**: the release's commit of the version is pushed past the rules
   with your token (below). You can still merge past them on purpose.
3. Under **Target branches**, choose **Add target → Include default
   branch**.
4. Tick **Require a pull request before merging** and **Require status
   checks to pass**. Under **Add checks**, add the four checks (GitHub
   suggests a check once it has run in the past week):
   - `Rust tests (Linux)`
   - `Rust tests (macOS)`
   - `Rust tests (Windows)`
   - `Frontend tests, types and formatting`

   Leave **Require branches to be up to date before merging** off: the
   release's commit of the version after each merge would put every open
   pull request behind. The release runs the checks on each merge anyway.

5. Choose **Create**.

Then give the release a token that may push past the rules. Make a
fine-grained token (**your GitHub settings → Developer settings → Personal
access tokens → Fine-grained tokens**) for this repository only, with
**Contents: Read and write**, and add it to the repository's **Settings →
Secrets and variables → Actions** as `RELEASE_TOKEN`. Without it, a
protected master refuses the release's commit of the version, and the
release stops there. The token expires on the date you choose: choose the
latest it offers, and when it runs out, replace the secret with a new one
(a release with an expired token stops at its checkout).

To debug model requests, set `ROVIK_API_LOG=1`. Every request and response
is then logged to `<temp dir>/rovik-api-log.jsonl`; set it to a file path
to log there instead. API keys are not written to the log.

The app's own log goes to stderr: what its hosts' connections, mirrors and
moves report, at level info. `ROVIK_LOG=debug` (or `trace`, `warn`,
`error`) sets the level.

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
    intellisense/        project-wide TypeScript IntelliSense (and its worker)
    lsp/                 language server client (rust-analyzer)
    commands.ts          typed wrappers around backend commands

src-tauri/               backend (Rust), a Cargo workspace
  src/                   the app
    lib.rs               app setup: plugins and command registration
    commands/            the commands of the agent and its chats, over the core
    filesystem.rs        directory listing and the file watcher
    git.rs, git_sync.rs  Git panel: status, commit, pull, push
    terminal.rs          the terminal panel
    lsp/                 language servers, bridged to the editor
  crates/rovik-core/src/ the agent and what it uses, without Tauri
    core.rs              what the agent needs from its host (`Core`)
    agent_run/           the AI agent: model turns, tool calls, approvals,
                         AI reviewer, verification checks
    chat_request.rs      sending requests to model providers
    chat_db/             chat storage (SQLite)
    clm/                 agent-managed context (CLM mode)
    overlay/             each chat's private view of the project
    vfs/                 virtual files (FUSE / macFUSE / WinFsp)
    workload/            queue for heavy commands (builds, tests)
    merge.rs             three-way merge (no git program needed)
    code_search.rs       project search
    file_guard.rs        stops a write from overwriting changes it hasn't seen
    outline/, repo_map/  tree-sitter outlines and the repo map
    project_profile.rs   detects the stack, checks and agent instruction files
    browser_tools.rs     web search / browsing for the agent
  crates/rovik-headless/ the agent without a window, for evaluations

docs/design/             design documents (chat isolation)
scripts/test.mjs         frontend test runner
website/                 the site at rovik-ide.dev, and the icon's drawing
                         (website/README.md)
```

---

## How some parts work

### Several windows

Each window is a Rovik process of its own, with a project of its own. All
of them share one chats database, settings and data folder, but a chat's
run, its Autopilot task and the files it has open live in the memory of the
process that has its project: two windows on one project would change its
chats unaware of each other. So a project is claimed by the window that
opens it, with an OS lock on a file in `<data>/open-projects`, named after
its folder and its project ID (copies sharing the chats share the ID). The
lock goes with the window, a crash included. Opening a project another
window has asks that window to come to the front instead, over a localhost
connection that only takes the token it wrote next to its lock. A window
with no folder open claims the chats without a project, so a second such
window shows a note instead of them. A window that switches to another
project while chats of the first are still at work (a run, an Autopilot
task, a stage waiting out a rate limit) keeps the first claimed until they
finish.

What one process must not do to another's work is guarded the same way:

- A chat at work holds a lock in `<data>/chats-at-work`. No other window
  starts a run in it, resumes it, or marks its unanswered message failed.
- An open overlay holds a lock in its store. Another window's startup
  cleanup leaves it alone, and no other window opens it, imports results
  into it or deletes it.
- A landing being written holds a lock in its journal's folder, so another
  window starting meanwhile doesn't "finish" it as interrupted.
- Each database write takes the write lock as it begins (`BEGIN IMMEDIATE`)
  and waits for the other window's commit, rather than failing halfway.
  The marker that says the database is open is shared: the integrity check
  and attachment cleanup run only in a window that starts alone.
- What only one process can have gets a slot number per window (1, 2, …):
  the virtual files' folder (`vfs`, `vfs-2`), the system ssh's forwarding
  sockets, and the headless browser's profile under snap Chromium.
- AI settings are saved onto the file as it is now: a window only changes
  what it changed itself, so an older copy in another window never drops a
  provider or a key saved meanwhile.
- The heavy-build slots (Settings → Heavy commands) are the machine's: a
  slot one window's build holds is taken in every window, by a lock in
  `<data>/heavy-slots`, and a busy terminal counts in all of them too. The
  number of slots set in one window applies in the others.
- A window that crashed mid-reply leaves its chat's message unanswered, and
  its own drafts are its own (each window's browser storage is): the window
  that opens that project next marks the message failed and puts its text
  back in the input, as the crashed window would have at its restart.
- Every window opens where the last one was left; a window started while
  others run steps down from there, so it doesn't hide the one it came from.
- Every window connects to the cloud hosts, and a chat on a host is its
  project's window's: that window imports and lands its results, pulls its
  rows and shows its approvals and questions. Land, Undo and Discard of
  such a chat are refused in another window. The chats of a project open
  in no window are every window's, and a mirrored chat is pulled by one
  window at a time.
- Each window follows the hosts' settings file: a host another window
  added, deployed again (a new token), changed or removed is connected
  again or let go of here within seconds, and before a window sends a host
  its configuration it reads the host's approval policy again, so an older
  window doesn't send back the one the user just changed. The file is
  changed by one window at a time.
- A window open while a newer Rovik updated the chat database (an update
  installed while it ran) stops writing to it: close it and open Rovik
  again. rust-analyzer's old versions are cleaned up only while no other
  window runs, as one may still run them.
- What a window used to keep in its own browser storage, and so wrote back
  whole over what other windows had saved, is shared in `<data>/shared`: a
  file each for the projects' tabs, the editor settings, the reasoning
  effort per model, rust-analyzer's choices and the chats' unread marks. A
  save changes only the entries it changed, one window at a time, and a
  window reads them again as it comes to the front. So a project closed
  with its window opens with its tabs in another, and a setting changed in
  one window isn't undone by another's. A window's unread dots are for its
  own project's chats.
- A lock is unlocked as it's let go, not only closed: a program another
  thread starts at that moment shares the open file until its exec, which
  kept a lock let go held meanwhile.

### Chat isolation

Every agent run works in an **overlay**: the project as it is on disk, with
that chat's changes on top. Only the changed files are stored. When a run
ends, its changes are **landed**: merged with whatever changed in the
project meanwhile, checked, and written. A clean merge with passing checks
lands on its own. Anything else (conflicts, failing checks) is shown in the
bar above the chat's message box, where you can resolve conflicts, land
anyway, discard, or undo a landing.

On Linux, the commands a chat runs see its overlay at the project's own
path too, so one that names the project by its full path still stays in
the chat. They run under bubblewrap (`bwrap`, which the .deb and .rpm
packages recommend), or, without it, in a namespace the app makes itself.
Where neither is allowed, and on macOS and Windows, a command that names
the project's full path is refused instead.

Limits: 512 MiB of changed files per chat (warning at 80%). Build output and
dependency folders don't count toward it. Heavy commands such as builds and
tests share a small number of CPU "slots" (one by default) so that several
chats building at once don't freeze the machine. A heavy command in your
own terminal takes a slot too, and is never slowed down.

Design and reasoning: [`docs/design/chat-isolation-plan.md`](docs/design/chat-isolation-plan.md).

### Virtual files

`src-tauri/crates/rovik-core/src/vfs/` provides files that live in the app's memory but look
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
  dropped (`src-tauri/crates/rovik-core/src/chat_db/history.rs`).
- **CLMs**
  ([Context Language Models](https://github.com/facebookresearch/context-language-models)):
  the agent manages its own context by editing it as a file
  (`src-tauri/crates/rovik-core/src/clm/`). Requires the virtual files driver; without it, the
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
- The task card, the checklist and the reasoning sent back with the run's
  steps go with every request but aren't in the file: their room comes out
  of the budget, so a context at its budget still leaves the reply its room.
- The CLM part of the system prompt is identical on every request of a
  chat, so providers can cache it.

Prompt caching (`src-tauri/crates/rovik-core/src/prompt_cache.rs`): providers serve the
start of a request from their cache when it's byte-identical to an
earlier request's, so every request is built in two zones. The system
prompt, the tool schemas and the history only grow from one step to the
next: trimming replaces and drops in batches, and once over the budget it
cuts down to 85% of it and keeps that cut while the requests fit, so the
cut moves once in several steps; a user message is sent the same way on
every turn. The repository map goes on the run's request, before its text:
it changes only when the run rebuilds it, at most every two minutes, and
moves on to the next request with the next run. What changes between steps
(the focus note, context notes) rides on the last message, after everything
cacheable. Nothing long goes there: the end of the request is where the
model looks for what to do next, and a map there read as the start of the
task. On top of that, OpenAI gets a `prompt_cache_key` per chat and
OpenRouter gets `cache_control` breakpoints (Anthropic models need them):
on the system prompt and on the latest message the next step sends
unchanged, never on the last one, whose notes the next step drops.
The run log's reply lines carry `cachedTokens`, and the headless report
the cache hit rate, so the effect of a change to the prompt can be seen.

### Attachments

What can be attached is what models take through the OpenAI-compatible APIs
the app talks to (`src/lib/attachmentFormats.ts`):

| Kind   | Formats                                        | Sent as                                                | Models        |
| ------ | ---------------------------------------------- | ------------------------------------------------------ | ------------- |
| Images | PNG, JPEG, WebP, GIF                           | `image_url` parts, as PNGs (below)                     | marked Vision |
| PDF    | PDF                                            | `file` parts (`input_file` on the Responses API)       | marked PDF    |
| Text   | UTF-8 text: code, Markdown, CSV, JSON, logs, … | in the message, in an `<attached_file name="…">` block | all           |

- OpenAI's models (GPT-4o and later) take PDFs, and OpenRouter takes them
  for every model, reading out the text where a model can't. Anthropic's
  and Gemini's OpenAI-compatible endpoints leave PDFs out, so PDF is off
  until it's turned on for a model, as Vision is.
- An attached image (up to 50 MB) is stored as a PNG at most 768×768
  pixels, its shape kept and never enlarged (`src-tauri/crates/rovik-core/src/chat_db/images.rs`):
  read with the `image` crate, scaled with `fast_image_resize` (Lanczos3
  convolution, transparency premultiplied), turned by its EXIF orientation.
  An animation keeps its first frame (Gemini takes no GIF).
- Word, Excel, PowerPoint, archives, audio and video aren't attached:
  Chat Completions takes no such files. A Word document can be saved as PDF.
- A model not set up for a kind of file doesn't get it attached, and isn't
  sent one the user attached for another model; earlier messages' files
  become a note in the history when the model can't see them.
- Images and PDFs are stored once each (SHA-256) under `attachments/` in
  the app's data folder, sent before the message's text, and counted in
  the context estimate (a PDF by its pages). The current request's files go
  with every step of its run; earlier requests' files are dropped first
  when the history is over budget.
