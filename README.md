# Rovik IDE

A lightweight desktop code editor with a built-in AI coding agent.

Open a project folder, edit files, run commands in the terminal, use Git,
and chat with an AI agent that can read, change and test your code. It
runs on Linux, macOS and Windows and uses much less memory than
Electron-based editors.

Website and downloads: [rovik-ide.dev](https://rovik-ide.dev).

![Rovik IDE, with the file tree, the editor and an AI chat open](screenshot.png)

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
  file. Files the agent, git or another program creates or changes are
  picked up as they change, without reopening anything. Rust gets help from
  rust-analyzer when it's installed.
- A file tree that stays fast on very large projects and refreshes when
  files change on disk.
- Project-wide text search powered by ripgrep's engine.
- Several windows, each with a project of its own: **New window** in the
  project menu opens another, and `rovik-ide <folder>` opens a folder in a
  window of its own. A project is open in one window at a time (a copy
  sharing its chats too): opening it again brings that window to the front
  (see [Several windows](#several-windows)).
- A built-in terminal that uses your own shell, on this computer or on a
  connected [cloud host](#a-terminal-on-a-cloud-host). Each project has
  its own terminals: switching to another project, its terminals are shown,
  and the ones left keep running until you come back. Bash up to 5.3, and
  5.4 in development, garbles a command line taller than the terminal;
  with bash 5.1 to 5.4, on this computer, a cloud host or in WSL, a paste
  that would make one stays on one row that scrolls sideways, and wraps
  again once you've deleted enough for it to fit.
  `set horizontal-scroll-mode on` in `~/.inputrc` keeps every long line on
  one row.
- A Git panel: commit, branches, stash, history, pull and push, and a
  rebase onto any local or remote branch, found by searching. Merge
  conflicts are resolved in a side-by-side conflict resolver: the
  conflicts git wrote in the file, each settled with one side, both,
  lines ticked from either side, or an edit by hand. The repository's
  [Git hooks](#git-hooks) run as they do with git: a failing pre-commit
  or pre-push stops the commit or push and shows what it printed.

**AI agent**

- Works with any OpenAI-compatible provider: OpenAI, Anthropic, Google,
  OpenRouter, Fireworks, or a local model in Ollama or LM Studio. You add
  your own API key. Anthropic's API is spoken natively, so Claude models
  get their reasoning effort, PDFs and prompt caching (see
  [Anthropic's API](#anthropics-api)).
- Two modes:
  - **Agent** does the work: reads code, edits files, runs commands and
    checks the result.
  - **Plan** only looks around, asks you questions and writes a plan. When
    you approve it, Agent mode carries it out.
- **Steer it while it works.** In Agent mode, a message you send while the
  agent works is read at its next step, as part of the same request: "use
  the existing HTTP client", "the tests are in `tests/unit`". Nothing it's
  doing is cut short. Until it's read, the message waits at the end of the
  chat, saying what it waits for (an approval, subagents, the checks), and
  ✕ takes it back into the input. A message the agent stopped before
  reading comes back into the input. See [Steering](#steering).
- **You stay in control.** You choose which commands the agent may run and
  which actions need your approval. Proposed file edits show as a diff in
  the editor, and you can accept or reject each change separately. You can
  also let a second "reviewer" AI model approve routine actions.
- **Automatic checks.** When the agent finishes, the IDE runs the project
  checks you turned on: its type check, lint, tests or build. They're found
  in the project's files, and each is off until you tick it in Settings →
  Project checks. If anything fails, the agent is sent back to fix it.
  It's told when a fix made no progress, a fix that breaks
  something that passed before is rolled back, and tests weakened to pass
  (skipped, deleted, fewer assertions) are flagged for you instead of
  counting as verified. A check that fails runs once more on the code
  before the change (in an isolated chat): what fails there too, such as
  the type errors many React Native apps have, isn't the agent's to fix and
  doesn't hold its change back. A test script that runs Jest in watch
  mode is run once.
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
- The agent can search the web and read pages, in a headless Chrome for
  Testing that Rovik downloads into its data folder when it first starts.
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

### Downloads

Download the latest release from [rovik-ide.dev](https://rovik-ide.dev) or
the [GitHub releases](https://github.com/piliugin-anton/rovik-ide/releases):

| System  | Packages                                                                                |
| ------- | --------------------------------------------------------------------------------------- |
| Linux   | AppImage (any distribution), `.deb` (Debian, Ubuntu), `.rpm` (Fedora, openSUSE), 64-bit |
| macOS   | `.dmg` for Apple silicon and for Intel                                                  |
| Windows | Installer (`.exe`) or MSI package (`.msi`)                                              |

Opening it for the first time:

- **macOS:** the app isn't notarized yet. If macOS says it's damaged or
  can't be opened, move it to Applications and run
  `xattr -dr com.apple.quarantine "/Applications/Rovik IDE.app"`.
- **Windows:** SmartScreen may warn that the publisher is unknown: choose
  **More info**, then **Run anyway**.
- **Linux:** make the AppImage executable, then run it:
  `chmod +x Rovik.IDE_*.AppImage`.

Once installed, Rovik [updates itself](#updates).

### Building from source

You need [Rust](https://rustup.rs) and [Node.js](https://nodejs.org) 22
(or 20.19 and newer), plus your system's build tools, as below.

#### Linux

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

#### macOS

1. Install the Xcode Command Line Tools: `xcode-select --install`
2. Install [Rust](https://rustup.rs) and [Node.js](https://nodejs.org) 22
   (or 20.19 and newer).
3. From the project folder:

   ```bash
   npm install
   npm run tauri dev      # start the app
   npm run tauri build    # build an installable package
   ```

#### Windows

1. Install
   [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
   with the **Desktop development with C++** workload.
2. Install [Rust](https://rustup.rs) with the default
   `x86_64-pc-windows-msvc` toolchain (MinGW isn't supported).
3. Install [Node.js](https://nodejs.org) 22 (or 20.19 and newer).
4. From the project folder, run the same `npm` commands as on macOS.

WebView2 is already part of Windows 10 and 11.

More details on prerequisites:
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

### Optional: virtual files driver

Some features work best with a virtual-file driver: FUSE on Linux, macFUSE
on macOS. On Windows there's none to install: the agent works in WSL
(Settings → This computer → WSL), which has FUSE. The app runs without one. When it's missing,
the app offers to install it for you; you can also do it later in
**Settings → Virtual files**. The driver is required for the agent's
self-managed context (CLM mode), and it makes the agent's shell commands
faster in isolated chats.

### Updates

Rovik installed from a release (the AppImage, `.deb` or `.rpm`, the macOS
disk image, the Windows installer or MSI package) updates itself. Every 30
minutes it looks at the latest release on GitHub, and when there's a newer
version, a notification offers it. **Update** downloads it, checks it
against Rovik's release key, installs it and restarts Rovik, which opens
your project again. Before the restart, Rovik asks about chats still at
work (they stop, and you can continue them afterwards) and about tabs with
unsaved changes. A `.deb` or `.rpm` update asks for an administrator's
password. On Windows the installer closes every Rovik window and starts
Rovik again once it's done; on Linux and macOS, Rovik's other windows keep
the version they started with and offer a restart.

**Settings → Updates** shows the version, has **Check now**, and turns the
looks off (they're on by default). They go the way **Settings → Network**
sends downloads. A build of your own isn't replaced: a new release is
offered for download instead, and a development build (`npm run tauri
dev`) looks only when you click **Check now**.

Cloud hosts run a Rovik of their own, which the app's update leaves as it
was. Once Rovik runs a new version, it offers once to bring your hosts to
it: **Update hosts** upgrades the hosts where you're one of Rovik's
administrators, for everyone on each (their runs pause while Rovik restarts
there, then go on), and for the others it says whom to ask. WSL on Windows
is brought to the app's version by itself.

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
4. **Chat:** open the AI Chat tab, pick a model and a mode (Agent, Plan or
   Autopilot), and describe what you want. In Agent mode you can keep
   sending it messages while it works.
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
| Project checks         | The type-check / lint / test / build commands of the open project, as detected (each off until you tick it) or fixed  |
| Heavy commands         | How many builds and tests run at once on this computer, so it stays responsive                                        |
| Virtual files          | Linux and macOS: the driver that isolates chats and lets the agent manage its own context, and its troubleshooting    |
| WSL                    | Windows: the WSL distribution the agent's commands run in, sandboxed (Rovik's Linux side on this computer)            |
| Network                | How Rovik connects to model providers, download servers and cloud hosts: connection timeouts and the proxy            |
| Cloud hosts            | Servers your chats can run on (see [Cloud hosts](#cloud-hosts))                                                       |
| Updates                | Rovik's version, the looks for a new one every 30 minutes, and installing it (see [Updates](#updates))                |

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
their own account with their key in it. Deploying where Rovik is installed
already, the app trusts the installed build when it's one of Rovik's
releases. Any other build, such as an administrator's own, shows in
**Test connection** with its SHA-256, and the app connects once you click
**Trust this build**.

### A terminal on a cloud host

Once a cloud host is connected, the **+** of the terminal panel asks where
the new terminal runs: **Local**, on this computer, or **_name_ (remote)**,
your login shell on that host, in your home folder there. `` Ctrl+Shift+` ``
still opens a local one at once. While a host is connected, each tab says
where it runs: `bash (local)`, `bash (Build server (remote))`.

A remote terminal logs in as the host's settings say, on a connection of its
own. On a host set to **My system ssh**, it runs your own `ssh` to the host,
with your configuration.

Where your login shell there is bash 4.4 to 5.4, Rovik starts it as it does
on this computer: with a file of Rovik's, saved in `~/.rovik/terminal`, that
reads the same login files and keeps a long pasted line from being garbled
(see the terminal above). Started this way, the host shows no login banner
(the message of the day, "Last login"). Through **My system ssh** the shell
starts as your ssh starts it.

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

| Command                                       | What it does                                                                        |
| --------------------------------------------- | ----------------------------------------------------------------------------------- |
| `npm run tauri dev`                           | Run the app with hot reload (same as `./dev.sh`)                                    |
| `npm run tauri build`                         | Build a release package (same as `./build.sh`)                                      |
| `npm test`                                    | Frontend unit tests (`src/**/*.test.ts`)                                            |
| `npm run typecheck`                           | TypeScript type check of the app, its tests and its build config                    |
| `npm run test:scripts`, `npm run test:eval`   | Tests of the release scripts and of the evaluation harness                          |
| `cd src-tauri && cargo test --workspace`      | Backend tests, every crate (as CI runs them)                                        |
| `cd src-tauri && cargo test vfs -- --ignored` | Mounts a real virtual volume (Linux) and runs shell tools and Git on it             |
| `npm run format`                              | Format with Prettier (`format:check` to verify)                                     |
| `scripts/build-agentd.sh`                     | Static builds of the daemon for Linux servers (x86_64 and aarch64)                  |
| `node scripts/e2e/run.mjs`                    | End-to-end runs of a built app ([scripts/e2e](scripts/e2e/README.md))               |
| `node scripts/eval/eval.mjs`                  | The agent's evaluation on a fixed task set ([scripts/eval](scripts/eval/README.md)) |

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
   release, tagged `v1.2.4`, with `latest.json`, what the installed apps'
   updates read (see [Updates](#updates)): each kind of app's bundle on
   the release and its signature, written by `scripts/updater-manifest.mjs`
   once every platform's are there. The download buttons on
   [rovik-ide.dev](https://rovik-ide.dev) show it at once: they ask GitHub
   for the latest release, and so do the apps.

Nothing is published until every build is on the release, so a run that
fails leaves the latest release as it was. The next merge publishes the
version it couldn't; so do **Re-run failed jobs** on its run and **Run
workflow** (in the **Actions** tab, under **Release**) with the version
left empty. **Run workflow** with a version, such as `2.0.0`, releases
that version from the branch you pick.

The app downloads the daemon for a server from its own version's release,
and checks it against the release key before it goes to the server. The
daemon on a server checks an upgrade against the same key, and so does the
app's own update (tauri-plugin-updater, `src-tauri/src/updates.rs`), whose
signatures also say the version they were made for: a `latest.json` that
pairs a new version with an older release's bundle is refused.

Only the release builds the updater's bundles and their signatures
(`--config src-tauri/tauri.updater.conf.json`, which sets
`bundle.createUpdaterArtifacts`): `tauri build` refuses to make them
without the signing key, so a developer's build and the end-to-end runs
make none. A pre-release isn't GitHub's latest release, so installed apps
aren't offered it.

Before the first release, make the key once:

1. Run `npx tauri signer generate -w ~/.tauri/rovik.key` and choose a
   password. Keep the file and the password safe: without them there are
   no more signed releases.
2. In the repository's **Settings → Secrets and variables → Actions**, add
   `TAURI_SIGNING_PRIVATE_KEY`, with the contents of `~/.tauri/rovik.key`,
   and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, with the password.
3. Paste the public key, the one line in `~/.tauri/rovik.key.pub`, below
   the comments in `src-tauri/release-key.pub`, and in
   `plugins.updater.pubkey` in `src-tauri/tauri.conf.json` (the updater
   takes it from `release-key.pub`, but `tauri build` checks the bundles'
   signatures against this one; a test keeps the two the same), and
   commit them.

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
    filesystem/          directory listing and the file watcher
    git/, git_sync.rs    Git panel: status, branches, history, pull, push, hooks
    terminal.rs          the terminal panel
    lsp/                 language servers, bridged to the editor
    instances.rs         several windows: which window has which project
    shared_state.rs      the state the windows share (tabs, settings, …)
    updates.rs           the app's own updates
  crates/rovik-core/src/ the agent and what it uses, without Tauri
    core.rs              what the agent needs from its host (`Core`)
    agent_run/           the AI agent: model turns, tool calls, approvals,
                         AI reviewer, verification checks, subagents,
                         Autopilot, steering
    chat_request.rs      sending requests to model providers
    anthropic/           Anthropic's API, spoken natively (Messages API)
    chat_db/             chat storage (SQLite)
    clm/                 agent-managed context (CLM mode)
    overlay/             each chat's private view of the project
    vfs/                 virtual files (FUSE / macFUSE)
    workload/            queue for heavy commands (builds, tests)
    merge.rs             three-way merge (no git program needed)
    code_search.rs       project search
    file_guard.rs        stops a write from overwriting changes it hasn't seen
    outline/, repo_map/  tree-sitter outlines and the repo map
    project_profile.rs   detects the stack, checks and agent instruction files
    browser_tools.rs     web search / browsing for the agent
    pinned_browser.rs    the Chrome for Testing build the browser tools use
  crates/rovik-protocol/ what the app and the cloud daemon send each other,
                         and how releases are signed
  crates/rovik-client/   the app's side of that: connecting to hosts over SSH,
                         deploying the daemon, WSL
  crates/rovik-agentd/   the daemon: the agent on a cloud host or in WSL
  crates/rovik-headless/ the agent without a window, for evaluations
  crates/eoka/           headless Chrome automation (for the browser tools)

docs/                    team servers (cloud-hosts.md), the WSL checklist,
                         the backend review
docs/design/             design documents: chat isolation, subagents,
                         Autopilot, cloud hosts, steering, Evolve
scripts/test.mjs         frontend test runner
scripts/e2e/             end-to-end runs of a built app
scripts/eval/            the agent's evaluation (tasks, grading)
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

### Git hooks

The Git panel works through libgit2, which runs no hooks, so the panel
runs them itself (`src-tauri/src/git/hooks.rs`), where git would and with
git's arguments and input:

| In the Git panel               | Hooks that run                                               |
| ------------------------------ | ------------------------------------------------------------ |
| Commit                         | pre-commit, prepare-commit-msg, commit-msg, post-commit      |
| Push                           | pre-push                                                     |
| Switching branch               | post-checkout                                                |
| Pull that fast-forwards        | post-merge                                                   |
| Pull that merges               | pre-merge-commit, prepare-commit-msg, commit-msg, post-merge |
| Continuing a merge (conflicts) | pre-commit, prepare-commit-msg, commit-msg, post-commit      |
| Pull that rebases, or Rebase…  | pre-rebase, post-rewrite                                     |

A hook that runs before (pre-…, prepare-commit-msg, commit-msg) and fails
stops the action, and the panel shows what the hook printed. A merge it
stops stays in progress, to continue or abort. What a pre-commit hook
stages is committed, and the message is what the message hooks leave in
`.git/COMMIT_EDITMSG`. Hooks that run after can't stop anything; their
failure is only logged, as git ignores it.

When a hook that checks stops a commit, a push or a merge's commit, the
error has a button to try once more without it: **Commit without hooks**,
**Push without hooks** or **Commit merge without hooks**, as git's
`--no-verify`. That skips pre-commit, commit-msg, pre-merge-commit and
pre-push for that one try. prepare-commit-msg and the hooks that come
after still run, as with git, and a push after such a commit still runs
pre-push. The panel tells the hook from the error (`src/lib/gitHooks.ts`).

The file tree doesn't show `.git`, so the Git panel's ⋯ menu has
**Git hooks…** (`src/components/GitHooksView.tsx`,
`src-tauri/src/git/hook_files.rs`). It lists each of the hooks above with
its state: on, not executable (skipped), only git's example (`.sample`),
or not there. A hook opens in the editor. **Create** makes one from a
small template, executable, and opens it. **Enable** makes one executable
(`chmod +x`), and the trash button moves one to the trash. With husky 9,
whose `core.hooksPath` is its generated `.husky/_`, the view shows the
hooks you write in `.husky/` instead.

Hooks are found as git finds them: in `core.hooksPath` when it's set (a
relative path is from the working folder), else in `.git/hooks`. So
husky, lefthook, pre-commit and simple-git-hooks work as they are. Each
runs in the working folder, with the PATH of the user's login shell (for
`npx`, `node`, …), and without a terminal, so a hook can't wait on input.

- **macOS, Linux:** the hook file must be executable (`chmod +x
.git/hooks/pre-commit`), as git requires. One that isn't is skipped, as
  git skips it. A script without a `#!` line runs with `/bin/sh`.
- **Windows:** a hook script runs with the `sh` of
  [Git for Windows](https://git-scm.com/download/win), which reads its
  `#!` line (`#!/bin/sh`, `#!/usr/bin/env node`, …) as git does, with Git
  for Windows' tools (grep, sed, …) on the PATH. A `<hook>.exe` runs as it
  is. Without Git for Windows a script hook can't run: the action stops
  and says so, rather than going on as if the hook had passed.

### Project checks

When the agent finishes a change, the IDE runs the project's checks (type
check, lint, tests) and sends what fails back to it. Detection only
suggests them: each check found is off until the user ticks it in
Settings → Project checks, which saves the project's list
(`agent_run/project.rs`, `effective_checks`). An Autopilot task with no
check on runs its plan's validation commands instead. What the checks know
lives in tables (paths under `src-tauri/crates/rovik-core/src/`), so
teaching them a new tool is usually one row:

| To teach the checks…                                     | Add it to                                                                                                 |
| -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| another package.json script name for a kind of check     | `CHECK_SCRIPTS` in `project_profile/node.rs` (and its copy in the frontend's `src/lib/projectProfile.ts`) |
| a framework to name in the stack the agent is told about | `STACKS` in `project_profile/node.rs`                                                                     |
| a test runner's watch mode, which never ends             | `WATCHERS` in `project_profile/node.rs`                                                                   |
| a tool's way of printing its failures                    | a rule in `RULES`, `agent_run/command_output.rs`                                                          |
| a cache or home folder a tool must write to              | `TOOL_DIRS` or `TOOL_HOMES` in `overlay/confine.rs`                                                       |

A check that fails runs once more on the code before the change
(`agent_run/before.rs`): in an isolated chat, the project as it is, in a
throwaway overlay. Failures are compared by key (an error's file, code and
message, or a test's name, with paths relative to where each run was), and
those that fail there too are set apart: a check that fails only as it did
before is `preexisting` in its record and doesn't hold the change back.
React Native apps, whose bundler never type-checks, often have such
failures.

### Steering

In Agent mode, Enter (or the Send arrow beside Stop) sends what's typed to
the running agent instead of waiting for it to finish
(`agent_run/steer.rs`, `src/lib/steering.ts`; design in
[`docs/design/steering-plan.md`](docs/design/steering-plan.md)). Plan
mode and Autopilot tasks take no messages while they work: the text stays
in the input.

- A message waits in the run's queue (up to 10 at once) and is read once
  every call of the agent's current reply has its result, before its next
  request to the model. No reply or command is cut short.
- It's read as the user's words, after a short note telling the agent it
  belongs to the same request, and stored where it was read, marked
  `meta.steer`. It doesn't start a new request: the focus note, the
  reviewers and the subagents' briefs see it as part of the one under way.
- An approval waiting is answered first: after Allow, the reply's other
  calls run, then the message is read; after Deny, it's read at once.
  Subagents the agent started report back first.
- When the agent says it's done, a message waiting is read instead of
  starting the end-of-run checks, and the checks run again at its next end.
  Nothing lands before the agent has read it.
- A message the run didn't read (Stop, an error) goes back into the input,
  with a notice. One sent just as the run ended is sent as a new message
  once it has. A crash puts queued messages back into the input too;
  nothing is sent on its own after a restart.
- From the third steering message in one request, a line under the input
  suggests a fresh start with one message that says it all.
- It works the same for a chat on a cloud host. The run log has a `steer`
  line for each step of a message's life, and the headless runner can send
  scripted messages (`--steer "TEXT@N"`).

### Chat isolation

Every agent run works in an **overlay**: the project as it is on disk, with
that chat's changes on top. Only the changed files are stored. When a run
ends, its changes are **landed**: merged with whatever changed in the
project meanwhile, checked, and written. A clean merge with passing checks
lands on its own. Anything else (conflicts, failing checks) is shown in the
bar above the chat's message box, where you can resolve conflicts, land
anyway, discard, or undo a landing.

On Linux, every command the agent runs is sandboxed with bubblewrap
(`bwrap`): the system is read-only to it but for the chat's own folders
(its view or worktree, its build folder, `/tmp` and the toolchains' caches
under your home), the credential stores (`~/.rovik`, `~/.ssh`, `~/.aws`,
`~/.gnupg`, the cloud CLIs' folders, Expo's login in `~/.expo`) are empty
to it, it sees no other process, and it has no capabilities, so it can't
undo any of that. Expo's CLI, which writes its state on every run, gets a
folder of its own under `~/.cache/rovik-ide` instead of `~/.expo`. An
isolated chat's commands also see its overlay at the project's own path,
so one that names the project by its full path still stays in the chat.
The .deb and .rpm packages depend on bubblewrap, and the app offers to
install it at start where it's missing (the AppImage can't carry it).
Where bubblewrap can't run (not installed, or user namespaces forbidden,
as in many containers), the commands run under the kernel's own sandbox,
Landlock (Linux 6.2 and later, nothing to install), with the same policy
but lighter: it doesn't hide the other processes or end what a command
leaves running, and like Seatbelt below it can't redirect a path, and
listing your home folder itself fails; the app then sets bubblewrap up by
itself once, and offers it again if that fails. With neither, the agent's
commands are refused. Ubuntu 23.10 and later
restrict unprivileged user namespaces through AppArmor, which stops
bubblewrap from making its sandbox: the packages add a small profile that
lets `/usr/bin/bwrap` make one (`rovik-bwrap`, removed with Rovik), and the
app offers to add it at start where it's missing. Your own terminal isn't
sandboxed, and `ROVIK_CONFINE_COMMANDS=0` turns the sandbox off for
troubleshooting. On Windows, Rovik's Linux side runs in a WSL2
distribution (Settings → This computer → WSL): the daemon runs inside it
with the same sandbox, every chat of a project moves there by itself when
it first runs, the app reaches it through `wsl.exe`, and Rovik there
follows the app's updates; a chat without a project, or a computer
without WSL, is refused its commands. On macOS every command runs under
Seatbelt (`sandbox-exec`, part of macOS) with the same policy: the system
read-only but for the chat's own folders, `/tmp` and the caches (including
`~/Library/Caches`), and the credential stores (with `~/Library/Keychains`)
out of reach. Seatbelt can't redirect a path, so an isolated chat's commands
see the project read-only rather than the chat's view at its path, and a
command naming the project's full path is refused by text there. Unlike
bubblewrap it doesn't hide the other processes.

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
| Windows | none    | —                                                   | The agent works in WSL, whose daemon mounts them with FUSE. A chat of a project runs there; without WSL it doesn't start.                                                                          |

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
  `fuse.rs` / `macfuse.rs` (OS drivers), `problem.rs`
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
task. On top of that, OpenAI gets a `prompt_cache_key` per chat, and
OpenRouter and Anthropic's API get `cache_control` breakpoints (Anthropic
models need them): on the system prompt and on the latest message the next
step sends unchanged, never on the last one, whose notes the next step
drops.
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
  for every model, reading out the text where a model can't. Claude models
  on Anthropic's API take them as documents; Anthropic's model list says
  which models take images and PDFs, and fetching it sets Vision and PDF
  for them. Gemini's OpenAI-compatible endpoint leaves PDFs out, so PDF is
  off until it's turned on for a model, as Vision is.
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

### Anthropic's API

A provider whose base URL is on `api.anthropic.com` (for example
`https://api.anthropic.com/v1`) is spoken to in Anthropic's own Messages
API, not its OpenAI-compatible endpoint, which ignores the reasoning
effort, never returns the model's reasoning, drops PDFs and caches nothing
(`src-tauri/crates/rovik-core/src/anthropic/`):

- Every request there carries the headers Anthropic requires: the key as
  `x-api-key` and `anthropic-version`, plus the `anthropic-beta` a
  request's features need. The model list, asked for all models at once,
  gets them too.
- The request the app builds for every provider is translated on the way
  out (system prompt, images, PDFs as documents, tool calls and results,
  `cache_control` breakpoints) and the reply on the way in
  (`chat_request/messages.rs`), so chats, subagents and the reviewer work
  as with any provider.
- What each model takes comes from Anthropic's Models API, once per model:
  its thinking modes, effort levels and output limit. `max_tokens`, which
  the API requires, is the model's most when streaming (it doesn't count
  against the rate limits), at most 16,000 otherwise.
- The effort picked in the chat goes in the model's own form: adaptive
  thinking with `output_config.effort`, or a thinking budget on models from
  before adaptive thinking. "No thinking" turns thinking off as far as the
  model allows, and models that can't turn it off get their lowest effort.
  The reasoning is shown, summarized, and its signed thinking blocks go
  back with the run's next steps where they were. Anthropic binds those
  blocks to the exact conversation, which the app's history doesn't keep
  byte for byte (notes move on, old results shrink), so a request asks the
  API to leave out a block whose conversation changed rather than refuse
  the request.
- A refusal naming something a model doesn't take (a thinking mode, an
  effort level, a sampling parameter, a forced tool, more output than it
  writes) gets the request sent again without it, and the model is
  remembered that way while the app runs.
- When a model's safety classifiers decline a request
  (`stop_reason: "refusal"`), the chat shows why, and the request isn't
  retried as it is.

### OpenAI's Responses API

A chat with a model on an OpenAI-compatible provider goes to the
provider's `/responses` (OpenAI's Responses API) first, and moves to its
`/chat/completions` when the provider says the model is there
(`src-tauri/crates/rovik-core/src/chat_request/responses.rs`). A Responses
API that can't read the tool calls sent back without an `id` on each, which
OpenAI's doesn't need (SGLang's router reads a run's first request but not
the next ones), gets them with ids, and the model stays there; one that
can't read the request even then moves to `/chat/completions`. Nothing is
stored with the provider (`store: false`): the whole history goes with
every request, so the model's own output goes back with it:

- Every request asks for the model's reasoning, encrypted
  (`include: ["reasoning.encrypted_content"]`), whatever the effort picked,
  except "No thinking": at the provider's default effort a model reasons
  too.
- A step's reasoning and commentary (the notes a model writes between its
  tool calls, shown with its reasoning) go back with the run's next steps
  in the order they came, each right before the call it preceded, as
  OpenAI asks of a tool-calling loop.
- A model that refuses the request for its reasoning, or the reasoning sent
  back, gets the request again without it, and isn't sent it again while
  the app runs. The other models on the same provider still are.
