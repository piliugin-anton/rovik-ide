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
    lib.rs          Tauri builder, plugin + command registration
  capabilities/
    default.json    permission grants (dialog plugin only — our own fs
                     commands are custom app commands, not plugin commands,
                     so they aren't gated by the permission system)
```

## Running it (Linux)

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

If you're on macOS/Windows instead, skip the scripts and just run:

```bash
npm install
npm run tauri dev      # dev mode, hot reload
npm run tauri build    # production bundle
```

See https://v2.tauri.app/start/prerequisites/ for the non-Linux prerequisites
(Xcode CLT on macOS, MSVC Build Tools on Windows).

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
