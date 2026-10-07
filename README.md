# tauri-ide

A minimal, low-memory IDE shell: open a project folder, browse it, edit files.
No syntax highlighting — that's intentionally left for you to add.

## Stack & why

- **Tauri 2** for the shell — the WebView is OS-native (no bundled Chromium),
  so idle memory is a fraction of an Electron app.
- **SolidJS** for the frontend, not React. Solid has no virtual DOM: each
  signal updates only the exact DOM nodes that depend on it, so editor/tree
  updates don't get more expensive as the app grows — no re-render cost to
  amortize as component count increases. This is the "keeps its performance
  as the project grows" property you asked for.
- **`@tanstack/solid-virtual`** for the file tree. Only visible rows are
  mounted, so a project with 50,000 files costs the same to render as one
  with 50.
- **Lazy directory reads.** The Rust side never walks the project
  recursively — `read_dir_entries` lists one directory at a time, called
  again each time you expand a folder. Memory and I/O cost track what's
  expanded, not total project size.
- **Plain `<textarea>` editor.** No tokenizer, no virtual line rendering.
  This is the piece you'll replace — see the note in `src/components/Editor.tsx`
  about keeping whatever you add re-tokenizing incrementally rather than
  re-scanning the whole buffer per keystroke, or you'll undo the memory/perf
  work done elsewhere.

## Project layout

```
src/
  components/
    FileTree.tsx   virtualized, lazy-expanding folder tree
    Tabs.tsx        open-file tabs
    Editor.tsx       the textarea (replace/extend for highlighting)
  lib/
    commands.ts     typed wrappers around the Rust commands + dialog plugin
    fileTree.ts     tree state: expand/collapse, per-directory cache, flatten
  App.tsx           wires it all together, owns open-files state
  types.ts

src-tauri/
  src/
    commands.rs     read_dir_entries / read_file_content / write_file_content
    agent_run/      the AI agent's runs: model turns, tool calls, approvals,
                     verification. They run here, not in the webview, so a
                     run goes on whatever the UI does (src/lib/agentRuns.ts
                     starts, stops and follows them)
    vfs/            virtual files mounted via FUSE / macFUSE / WinFsp
    lib.rs          Tauri builder, plugin + command registration
  capabilities/
    default.json    permission grants (dialog plugin only — our own fs
                     commands are custom app commands, not plugin commands,
                     so they aren't gated by the permission system)
```

## Running it

### Linux

Three scripts, run from the project root:

```bash
./setup.sh   # one-time: system packages + Rust via rustup + npm install
./dev.sh     # start the app, hot-reloading
./build.sh   # produce a release bundle (AppImage/.deb/etc.)
```

`setup.sh` handles apt/dnf/pacman (asks for sudo to install system packages),
installs Rust via `rustup` if your distro's `rustc` is too old or missing —
Tauri needs a recent-ish toolchain and distro packages lag — and runs
`npm install`. It's safe to re-run; already-satisfied steps are skipped.

### macOS and Windows

The scripts are Linux-only. Install the prerequisites once, then use npm:

```bash
npm install
npm run tauri dev      # dev mode, hot reload
npm run tauri build    # production bundle
```

- **macOS:** the Xcode Command Line Tools (`xcode-select --install`), Rust
  via [rustup](https://rustup.rs) and Node.js 18+. macFUSE isn't needed to
  build; the app offers to install it when virtual files are first used.
- **Windows:** Visual Studio Build Tools with "Desktop development with
  C++", Rust via [rustup](https://rustup.rs) (the default `x86_64-pc-windows-msvc`
  toolchain; the MinGW one isn't supported), Node.js 18+, and **LLVM**
  (`winget install LLVM.LLVM`), whose `libclang` generates the WinFsp
  bindings. WebView2 ships with Windows 10/11. WinFsp isn't needed to build.

Details: https://v2.tauri.app/start/prerequisites/.

### Platform behavior

- The AI agent's `run_command` runs through `$SHELL -c` on macOS/Linux and
  `cmd.exe` on Windows; the terminal panel starts your login shell (or
  `%COMSPEC%`). Started from Finder, the Dock or a desktop entry, the app
  takes `PATH` from your login shell at startup, so commands and language
  servers find `npm`, `cargo`, Homebrew tools and the like
  (`src-tauri/src/process.rs`).

### Tests

From `src-tauri/`:

```bash
cargo test --lib
```

## Virtual files

`src-tauri/src/vfs/` gives the app files that live in its memory but are
real files to every other program — so commands the AI agent runs can
`cat`, `grep`, `sed -i`, `chmod +x` and execute them, or create and change
files there that the app reads back. The volume is a user-space filesystem
mounted through the OS's driver, which is optional software the app installs
on request (Settings → Virtual files):

| OS      | Driver  | Installed from                                       | Notes                                                                                                                                                                                                              |
| ------- | ------- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Linux   | FUSE 3  | the distro's package (`fuse3`) via polkit (`pkexec`) | Mounted with `fusermount3`; no libfuse needed to build or run.                                                                                                                                                     |
| macOS   | macFUSE | the official release `.dmg` (signature checked)      | macOS 15.4+ with macFUSE 5.1+ uses FSKit (no kernel extension; switch macFUSE on once in System Settings), mounted at `/Volumes/RovikVirtualFiles`; older systems use the kernel extension, which must be allowed. |
| Windows | WinFsp  | the official release `.msi` (signature checked)      | winfsp.dll is delay-loaded. Building needs LLVM (`libclang`) for the WinFsp bindings.                                                                                                                              |

The app starts on every OS without the driver; only mounting needs it. At
start it checks in the background: with the driver installed it mounts the
volume; without it, one notification offers to install it (then mounts).
Either can be silenced with "Don't ask again" (`src/lib/virtualFilesStartup.ts`).
When a mount fails, Settings → Virtual files shows why and what to do (on
macOS, with a button to the right System Settings page) and keeps retrying
while the fix is up to the user; after installing, it mounts right away so
those steps show up at once (`problem.rs`).

```rust
let vfs = app.state::<vfs::VfsState>();
let path = vfs.create_file("plans/step-1.md", b"...", false)?; // a real path
```

From the frontend, `src/lib/virtualFiles.ts` wraps the same calls
(`writeVirtualFile` returns the `realPath` to hand to a command).

Callbacks report what programs (and the app) do to the files — created,
opened, read, written, truncated, closed (with whether it was modified),
removed, renamed — with the path used:

```rust
let id = vfs.subscribe(EventMask::CLOSED, |e| {
    if let Op::Closed { modified: true } = e.op { /* a program finished writing e.path */ }
});
```

`subscribe_batched` (and `subscribeVirtualFiles` in the frontend) deliver
batches on a background thread instead. Without subscribers the file
operations pay one atomic check; paths are found by walking up from the
file, so the cost doesn't grow with the number of files. See
`src-tauri/src/vfs/events.rs` for what a callback may do.

Files are versioned: a revision is saved when a program closes a file it
modified, or when the app writes one. History follows a file through
renames and "safe saves" (temporary file renamed over the original, as
`sed -i` and editors do), and outlives deleting it:

```rust
let revisions = vfs.revisions("notes.md")?;      // number, size, time, source
let old = vfs.read_revision("notes.md", 2)?;
vfs.restore_revision("notes.md", 2)?;            // itself a new revision
```

(`listRevisions` / `readRevision` / `restoreRevision` in the frontend.)
At most 64 revisions per file and 256 MiB in all are kept, oldest dropped
first; files over 32 MiB aren't versioned (`set_history_limits`).

- `store.rs` — the in-memory filesystem (POSIX semantics: hard links,
  symlinks, xattrs, atomic rename, unlinked-but-open files), all unit tested.
- `fuse.rs` / `macfuse.rs` — Linux and macOS driver (macOS through macFUSE's
  library, loaded at run time); `winfsp_fs.rs` — Windows.
- `problem.rs` — why a mount failed and what to do; the macOS backend choice.
- `events.rs` — callbacks for what happens to the files.
- `history.rs` — revisions of files.
- `driver.rs` — detecting and installing the driver.

`cargo test vfs -- --ignored` mounts a real volume on Linux and runs shell
tools and git against it.

## Context management

The row under the model and effort in the AI Chat tab chooses, per chat, how
the agent's context is managed (`src/lib/contextMode.ts`,
`src/components/ContextControls.tsx`):

- **Trimming** — the app trims older history to fit the model's window:
  out-of-date content becomes a short note, old tool results are cut to
  their start, the oldest turns are dropped (`src-tauri/src/chat_db/history.rs`).
- **CLMs** — [Context Language Models](https://github.com/facebookresearch/context-language-models):
  the agent manages its own context by editing it as a file
  (`src-tauri/src/clm/`). Before every request the context is written to
  `context/<chat>/LIVE_CONTEXT.md` on the virtual volume, one
  `[[CTX_TURN n role=… id=…]]` block per message. The agent edits it with its
  ordinary tools — `read_file` / `edit_file` / `write_file` on
  `@context/LIVE_CONTEXT.md`, or shell tools on its real path — and whatever
  the file says when the next request goes out is its context from then on.
  Edits are read back tolerantly (`clm/document.rs`): untouched blocks stay
  the original messages, rewritten ones become text, a tool call keeps its
  structure only with all its results. An edit that grows the context past
  the budget, or leaves it with no messages, is rejected and the agent is
  told why; one that shrinks it is always taken. Stored messages are never
  changed. Restoring an older revision keeps everything stored after it.
  The agent has these tools on `@context/…` in Plan mode and without a
  project too (offered for the context folder only), `list_dir @context`
  for its folder, and `restore_context` to restore a revision or go back to
  the stored chat itself, as the context row does.

  Every state of the file is a revision (the volume's file history), also
  saved in the chat database so it survives a restart. The agent reads them
  at `@context/revisions` and `@context/revisions/<n>.md[#block]`. Its
  system prompt tells it to check for a revision before redoing work it
  compacted away. When it repeats a call whose earlier result it removed,
  the result names the revision and block that still hold it. The context
  row shows the revision and size, lists revisions (view, restore) and can
  reset to the stored chat; a run updates it as each request goes out, not
  only when it ends. Requests carry a size readout and reminders at
  25/50/75% of the budget, then every request from 90%. Past the budget,
  older tool outputs are withheld; if it's still over, that request is trimmed
  like Trimming.

  CLMs need virtual files. Without the driver, the row explains how to
  install it (Settings → Virtual files) and the chat uses Trimming.

## Notes / things left simple on purpose

- No file-system watcher — the tree doesn't auto-refresh if files change on
  disk outside the app. Add `notify` on the Rust side + an event emit if you
  want that.
- No unsaved-changes diffing beyond a single dirty flag per file.
- No multi-window, no split panes, no command palette.
- `.git`, `node_modules`, and `target` are filtered out of directory
  listings by default (see `commands.rs`) since they're rarely useful to
  browse and can be huge — remove that filter if you need it.
- The placeholder app icons in `src-tauri/icons/` are generated flat
  squares — swap them for real artwork before shipping.
