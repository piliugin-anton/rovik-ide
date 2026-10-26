# Workspaces: several folders in one window

Today a window has one project, and a project is one folder. A **workspace**
is a named list of folders opened together in one window. Each folder is a
row at the top of the file tree, search covers all of them, and the Git
panel lists every repository they're in. A commit puts each file into the
repository it belongs to, on the branch that repository is on.

This plan says what to build, in which files, in which order, and how each
step is tested. Section 1 lists the decisions; section 2 what the code does
today; sections 3 and 4 how workspaces work and the edge cases; sections 5
to 7 the milestones, the tasks and the tests; section 8 the risks; section 9
the questions asked and the ones decided while planning, for the user to
check; section 10 what a later plan would add (chats that change several
folders at once); section 11 the problems in today's code found on the way.

The user's requirements:

- In the project menu (the switcher above the sidebar tabs), above **New
  window**: **Create a workspace…**, and, in a workspace, **Add folder to
  workspace…**.
- A workspace holds several folders and shows them in the file tree.
- A folder may be a repository of its own. A commit puts each file into the
  repository it belongs to, on the branch that repository is on.
- The other places a second folder changes are found, handled and tested.

The idea in one sentence: **a workspace groups projects; it doesn't replace
them.** Each folder stays the project it is when opened alone, with its
chats, its settings, its plans and its ID file. So everything built per
project (runs, isolation, landing, cloud hosts, WSL, checks) works as it
does, folder by folder. What's new is a window with several folders at once:
the tree, the watcher, search, tabs, terminals, the editor's services, the
Git panel and the chat list learn to take a list of folders, and a project
is the list of one.

Contents:

1. [Decisions](#1-decisions)
2. [What the code does today](#2-what-the-code-does-today)
3. [How workspaces work](#3-how-workspaces-work)
4. [Edge cases](#4-edge-cases)
5. [Milestones](#5-milestones)
6. [Implementation order for an AI agent](#6-implementation-order-for-an-ai-agent)
7. [Testing](#7-testing)
8. [Risks and known limits](#8-risks-and-known-limits)
9. [Questions](#9-questions)
10. [Later: chats across folders](#10-later-chats-across-folders)
11. [Problems in today's code found while planning](#11-problems-in-todays-code-found-while-planning)

---

## 1. Decisions

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Why                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | **A workspace groups projects.** It is a name and an ordered list of folders. Each folder stays a project of its own: its chats, project settings, plans, ID file (`.rovik/project.json`) and window claim are the ones it has when opened alone, and opening a workspace opens each folder as opening it alone does.                                                                                                                                                                                                                                                                                                              | Every per-project mechanism keys on one project with one folder: a chat's run and its overlay (`agent_run/run.rs:149-152`; `overlay/chats.rs:405-480` binds a chat to one folder), landing, the sync to WSL and hosts (one shadow repository per project, `rovik-client/src/shadow.rs`), the settings (`project-settings.json`, by folder). Reused folder by folder, none of them changes, and a folder's chats are the same whether it's open alone or in a workspace. |
| D2  | **A chat works in one folder.** A new chat starts in the active folder (the active tab's, else the first); a chip in the composer picks another until the first message is sent. A workspace lists the chats of all its folders, each marked with its folder. Chats that change several folders are a later plan (section 10).                                                                                                                                                                                                                                                                                                     | On Windows every chat of a project runs in WSL from a snapshot of its one folder (`Remote::go_home`, `rovik-client/src/remote.rs:753-756`); overlays, the sandbox's redirect, landing and the hosts' protocol are one-folder designs. A chat per folder keeps all of them as they are. A chat across folders needs all of them redesigned.                                                                                                                              |
| D3  | **The menu:** **Create a workspace…** always, and **Add folder to workspace…** when a workspace is open, both above **New window**.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | The user's wording (section 9).                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| D4  | **Create** opens a dialog: the workspace's name and its folders, the open folder already listed. **Create** opens the workspace in this window and keeps the open folder's tabs. **Add folder to workspace…** opens the folder picker, which takes several folders at once.                                                                                                                                                                                                                                                                                                                                                        | Making one from the open project is the common case. Opening it in this window follows **Open project…**; **New window** is there for the other case.                                                                                                                                                                                                                                                                                                                   |
| D5  | **Folder rules.** A workspace has at least one folder. A folder can't be in it twice (the same folder however it's spelled: case where the file system ignores it, a trailing separator, a link), nor inside another of its folders, nor contain one. A folder may be in several workspaces.                                                                                                                                                                                                                                                                                                                                       | Nested folders would list, watch, search, claim and index the same files twice, and give one file two tree rows with one id (rows are keyed by file id, `filesystem/mod.rs:29-63`).                                                                                                                                                                                                                                                                                     |
| D6  | **Stored in the chats database**, schema version 11: tables `workspaces` and `workspace_folders`. The workspace writes nothing into its folders. Not in `project_paths`, and no workspace file.                                                                                                                                                                                                                                                                                                                                                                                                                                    | The project list is there, with its locking across windows and its migrations. `project_paths` means one project's other locations (copies, moves): workspace folders there would raise false "copy" suggestions and wrong "missing" flags. A file to share, like VS Code's `.code-workspace`, wasn't asked for.                                                                                                                                                        |
| D7  | **One window per folder.** A workspace is claimed as a project is (`instances.rs`): `workspace:<id>` and each folder's own keys (`folder:` and `project:`), all or none. A folder is open in one window at a time, alone or in a workspace; opening it where it's taken brings that window to the front.                                                                                                                                                                                                                                                                                                                           | A chat's run lives in the process of the window that has its project (README, "Several windows"). With D1, a workspace has its folders' chats.                                                                                                                                                                                                                                                                                                                          |
| D8  | **The tree** of a workspace has a row per folder at the top, named after the folder (with its parent folder added when two have the same name). A folder's row offers **Remove from workspace** instead of Rename, Delete, Cut and Copy, and **Locate…** while the folder is missing. A project's tree stays as it is.                                                                                                                                                                                                                                                                                                             | Deleting a folder's row would trash the whole folder: `delete_path` has no guard (`filesystem/entries.rs:437-455`).                                                                                                                                                                                                                                                                                                                                                     |
| D9  | **Git, per repository.** The repositories are found from the folders: the one each folder is in (upward, as today), the ones inside it two levels down, and their submodules. There is one entry per repository, by its git folder (two folders in one repository share one entry; a linked worktree has its own), listing only the changes in the workspace's folders. With one repository the panel is as today. With several, each has a section (its branch with ahead and behind, Pull, History, stash and hooks in its header), and one message box commits every included repository's changes, each on the branch it's on. | The user's requirement. Hooks, merges and branches are already per repository (`Repository::discover`, `git/mod.rs:41-43`). One message box matches "commit, and each file goes to its repository"; a different message per repository is one commit per repository, the others unticked.                                                                                                                                                                               |
| D10 | **A commit across repositories runs every checking hook first:** pre-commit in each repository, then the message hooks in each, and only then the commits. A hook that fails stops the commit everywhere, before any is written. A commit that fails after others were written is reported by repository, and the message stays.                                                                                                                                                                                                                                                                                                   | All or nothing as far as git can do it: commits in several repositories can't be one transaction. Each repository's hooks still run in git's order.                                                                                                                                                                                                                                                                                                                     |
| D11 | **A commit takes only its folders' changes** (`git commit --only` semantics): what's staged elsewhere in the repository stays staged and out of the commit. An untracked folder with a `.git` of its own is never staged by the repository around it.                                                                                                                                                                                                                                                                                                                                                                              | Today a folder of a repository commits whatever a terminal staged beside it, and a repository inside it makes Commit fail (section 11). In a workspace, folders inside repositories are the common case.                                                                                                                                                                                                                                                                |
| D12 | **Search covers every folder**, its results grouped by folder. A chat's `@` lists its folder's files.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | A chat can't read the other folders (D2).                                                                                                                                                                                                                                                                                                                                                                                                                               |
| D13 | **The editor's services take the window's folders.** IntelliSense loads every folder into its one program, as it loads a monorepo's packages today; rust-analyzer gets every folder as a workspace folder. The formatter, the Markdown preview and the diagnostics the agent reads take the file's own folder.                                                                                                                                                                                                                                                                                                                     | One TypeScript program and one rust-analyzer per window are what the app has now (`intellisense/index.ts:49-62`, `lsp/rustAnalyzer.ts:76-107`). A root per file would restart them at every tab switch.                                                                                                                                                                                                                                                                 |
| D14 | **Terminals are the window's.** A workspace's terminals are its own, as a project's are. In a workspace, **+** asks which folder, as it already asks which host once one is connected; a new terminal opened otherwise starts in the active folder.                                                                                                                                                                                                                                                                                                                                                                                | No new kind of menu, and the terminal panel already keeps each project's terminals apart (`TerminalPanel.tsx:88-104`).                                                                                                                                                                                                                                                                                                                                                  |
| D15 | **Settings stay per folder.** Project checks and Restricted files are kept per folder, as now; in a workspace their pages have a folder picker.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Checks run in the chat's folder, and a chat is one folder's (D2).                                                                                                                                                                                                                                                                                                                                                                                                       |
| D16 | **What's saved:** the window's workspace in `last-project.json`; its tabs, open folders in the tree and terminal panel in `project-sessions` under `workspace:<id>`. An update's restart reopens it (`--workspace <id>`).                                                                                                                                                                                                                                                                                                                                                                                                          | As for a project.                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| D17 | **Names.** "Workspace" is the user's word, and the name of the code that stores and opens one. Code that takes several folders (tree, watcher, search, git) says folders and repositories, and treats a project as a list of one. Project checks' "Workspaces (N packages)" heading becomes "Packages (N)".                                                                                                                                                                                                                                                                                                                        | "Workspace" already means package-manager workspaces (`project_profile/workspaces.rs`, the Project checks page), Codex's `workspace-write`, the LSP's workspace folders and Cargo's workspace.                                                                                                                                                                                                                                                                          |
| D18 | **Removing a folder** closes its tabs (asking about unsaved ones). Its chats at work keep running, and its claim is kept until they finish, as when a window leaves a project for another. Its terminals keep running.                                                                                                                                                                                                                                                                                                                                                                                                             | The window does this already when it switches projects (`instances.rs`, `AtWork`).                                                                                                                                                                                                                                                                                                                                                                                      |
| D19 | **A missing folder** (deleted, or a drive or share that isn't mounted) shows as missing, with **Locate…** and **Remove from workspace**; the rest of the workspace opens.                                                                                                                                                                                                                                                                                                                                                                                                                                                          | One folder gone mustn't close the others.                                                                                                                                                                                                                                                                                                                                                                                                                               |
| D20 | **No feature flag.** The menu items come last (milestone F). Every task before them keeps a project exactly as it is, which its tests show.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | AGENTS.md: no speculative options. Nothing reaches a user half built.                                                                                                                                                                                                                                                                                                                                                                                                   |

---

## 2. What the code does today

### One folder, everywhere

- `rootPath` (`src/App.tsx:159`) is the window's one folder. It feeds the
  tree's controller (`:304`), the watcher (`:1979-1987`), search (`:2175`),
  the chat panel (`:2233-2240`; every run's `root`, `ChatPanel.tsx:3057,
3097`), the Git panel (`projectPath`, `:2249`), the editor (`projectDir`,
  `:2327`), the terminals (`:2346`), the Settings dialog (`:2388`) and what's
  saved (`:1929-1977`). It's only ever set to a folder (`:199`, `:520`).
- Switching projects (`openProjectAt`, `:485-506`) checks the claim, settles
  unsaved tabs, saves the session, claims, then resets the tabs, the git
  marks, the tree's selection and Monaco's models. The tree, the watcher,
  rust-analyzer and IntelliSense follow `rootPath`.
- What's saved is keyed by the folder's path: `last-project.json` (one path
  for the app, `filesystem/mod.rs:443-505`), `project-sessions` (tabs, open
  folders, terminal panel; `lib/projectSessions.ts`), `project-settings.json`
  (checks and restricted files; `project_profile.rs:682-715`). Terminals and
  commit drafts are kept in memory by folder.

### Projects and windows

- A project is one folder's identity: an ID in `.rovik/project.json`
  (`chat_db/projects.rs:29`), a row in `projects` (one name, one path, one git
  fingerprint), its other locations (copies and moves) in `project_paths`.
  Chats point at it by `dialogs.project_id`; every chat query filters on one
  project (`chat_db/store.rs:226, 240, 273`; `search.rs:191`). The schema is
  at version 10 (`chat_db/schema.rs:354`).
- A chat runs only in one of its project's folders (`check_project`,
  `agent_run/mod.rs:1301-1333`; `chat_may_use`, `projects.rs:348-364`).
- A window claims its folder with OS locks in `<data>/open-projects`
  (`src-tauri/src/instances.rs`): `folder:<key>` and `project:<id>`
  (`claim_keys`, `:353-362`), all or none (`claim`, `:140-200`). A project
  left with chats at work stays claimed until they finish (`AtWork`). After
  each open, `reclaim` claims the one folder again (`commands/chat.rs:63-71`).
- The cloud client keeps the window's one open project (`State.open`,
  `rovik-client/src/remote.rs:240`): automatic landing of a host's results,
  `handled_elsewhere`, `project_of`, the steering message's paths and the
  delivery test use it (`remote.rs:336-345, 486-496, 810-825`;
  `remote/results.rs:74-86, 395-410, 574-600`).

### The tree, the watcher, search, tabs

- The tree's controller has one root, not shown as a row: its children are
  at depth 0 (`lib/fileTree.ts:14, 244-284`). Rows are keyed by file id
  (`u:dev:ino`), open folders by absolute path. The context menu has New
  file, New folder, Rename, Copy, Cut, Paste and Delete
  (`FileContextMenu.tsx:58-148`); one drop target moves a file to "the
  project root" (`FileTree.tsx:22, 486-494`). Creating, renaming, pasting and
  deleting rely on the watcher to refresh the tree.
- One watcher per process, for one folder (`filesystem/watch.rs:363-396`). It
  sends `fs-changes` with absolute paths, an `unknown` change with the root
  when more than 300 come at once, and `git-changed` with no payload. It
  tells the folder's isolated chats what changed (`project_changed`,
  `project_busy`). A folder inside a repository never sees that
  repository's `.git`.
- The tree's gitignore matcher is one slot (`filesystem/mod.rs:71`).
- Search walks one root and returns paths relative to it
  (`code_search.rs:307-318`); a result opens against the one root
  (`App.tsx:895-901`). A chat's `@` lists one root's files
  (`project_files.rs`).
- Tabs hold absolute paths. Their titles are the file name alone
  (`Tabs.tsx:69-72`): two `package.json` look the same.
- There are three copies of `isAtOrUnder` and two of a path's parent
  (`parentOf`, `parentPathOf`), comparing paths as text, case-sensitively
  (`FileTree.tsx:79-83`, `fileTree.ts:169-173, 197-202`,
  `App.tsx:1046-1050, 1631-1636`).

### Git

- Each of the 35 commands takes the project's folder as `path`. The
  repository is found upward from it (`open_repo`, `git/mod.rs:41-43`), and
  file paths are relative to the repository's working folder. A project in a
  repository's subfolder is scoped to it (`project_scope`, `:107-125`): its
  list of changes, Commit's staging and Discard all.
- `git_commit` stages the scope, runs the hooks, and commits the whole
  index (`git/changes.rs:14-83`).
- GitPanel is one component for one repository: 2794 lines, 34 uses of
  `props.projectPath`. Its commit button commits, then pushes when the
  branch is ahead, or asks to publish a branch with no remote branch yet
  (`commitAndMaybePush`, `GitPanel.tsx:819-861`; `lib/gitPublish.ts`).
- One `GIT_LOG_LATEST` generation stops older history scans for the whole
  process (`git/history.rs:100-109`). ConflictResolver keys its state by the
  project's path (`ConflictResolver.tsx:79-91`).
- A new chat's branch offer asks `git_head` of the window's folder
  (`lib/chatBranch.ts:63-85`).

### The agent

- A run has one `root` (where it works: the overlay's mount in an isolated
  chat) and one `project_root` (`agent_run/run.rs:149-152`). Its tools
  resolve paths within that root (`resolve_within_root`,
  `agent_tools.rs:110-133`), and the model never sees the absolute path.
- A chat's overlay is bound to one folder and mounted as the run's root
  (`overlay/chats.rs:405-480`); the sandbox redirects one view to one folder
  (`overlay/confine.rs:92-105`); landing writes one folder
  (`overlay/land.rs:565-625`).
- The project's context is one folder's: its instructions file, stack,
  settings and restricted files (`load_for`, `agent_run/project.rs:168-198`).
- On Windows a project's chats run in WSL, and on a host, from snapshots of
  the folder (`shadow.rs`).

### The editor's services

- The TypeScript worker holds one project (`intellisense/index.ts:49-62`).
  rust-analyzer gets one workspace folder and restarts when the root changes
  (`lsp/client.ts:117-142`, `lsp/rustAnalyzer.ts:90-107`). The diagnostics
  the agent reads are kept for one root (`lib/diagnostics.ts:15`).
- The formatter looks for its config between the file and the root
  (`formatter.rs:408-430`). `.editorconfig` doesn't depend on the root.

---

## 3. How workspaces work

### 3.1 What's stored

Schema version 11 (`chat_db/schema.rs`):

```sql
CREATE TABLE workspaces (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_opened_at INTEGER NOT NULL
) STRICT;
CREATE TABLE workspace_folders (
  workspace_id TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  -- As the folder is opened: shown, and what its settings are kept by.
  path TEXT NOT NULL,
  -- projects::path_key: links resolved, lowercased where the file
  -- system ignores case. Compares folders.
  key TEXT NOT NULL,
  PRIMARY KEY (workspace_id, key)
) STRICT;
CREATE INDEX workspace_folders_by_key ON workspace_folders (key);
```

`chat_db/workspaces.rs` (new, beside `projects.rs`):

- `create(name, folders)`, `add(id, folders)`, `remove(id, key)` (refused
  for the last folder), `relink(id, key, folder)` (Locate…), `delete(id)`,
  `get(id)`, `list()` (most recently opened first, each folder with
  `missing`), `opened(id)` (`last_opened_at`).
- `check(folders, folder) -> Result<(), Refused>`: `Duplicate`,
  `Inside(other)`, `Contains(other)`, `NotAFolder(error)`, by `path_key`
  (`projects.rs:85-92`). Every way a folder joins (create, add, relink) goes
  through it.
- A name is trimmed, not empty, at most 80 characters. Names needn't be
  unique: the list shows each one's folders.
- A folder's label isn't stored: it's the folder's name, with its parent
  folder's added when two have the same name (`openFolders.ts`, 3.2).
- No folder's project ID is stored. A folder's ID file is its identity, so
  copies and moved folders are recognised as they are when it's opened
  alone.
- `delete` removes the workspace's rows only: its folders, their projects
  and their chats stay. It's refused while another window has the
  workspace open.

Commands (`src-tauri/src/commands/workspace.rs`, new): `workspace_list`,
`workspace_create`, `workspace_add_folders`, `workspace_remove_folder`,
`workspace_relink_folder`, `workspace_delete`. `src/lib/chatApi.ts` wraps
them, beside the project calls.

### 3.2 The window's folders

`src/lib/openFolders.ts` (new) says what the window has open:

```ts
/** What a window has open: a folder (a project), or a workspace. */
export type WindowTarget =
  | { kind: 'folder'; path: string }
  | { kind: 'workspace'; id: string; name: string; folders: string[] };

/** One folder the window has open. */
export interface OpenFolder {
  path: string;
  /** Its name, with its parent's when two folders have the same name. */
  label: string;
  /** The project it is, once resolved (chat_open_project). */
  project?: ProjectInfo;
  missing: boolean;
}
```

- `target()` replaces App.tsx's `rootPath` signal. `openFolders()`,
  `folderOf(path)` (the folder a path is in), `activeFolder()` (the active
  tab's folder, else the tree's selected row's, else the first) and
  `sessionKey(target)` (the folder's path, or `workspace:<id>`: the key of
  project-sessions, the terminals and the commit draft) are derived from it.
- `isAtOrUnder`, `parentOf` and `samePath` move into `lib/fileTools.ts`,
  beside `toRelativePath`: one copy each, with the platform's case rule, and
  drive roots right (`D:\x`'s parent is `D:\`).
- Code that works on folders takes `openFolders()` (a project is a list of
  one) or `folderOf(path)`. Only the menu, the dialog, the folder rows and
  Remove and Locate check `target().kind`.

### 3.3 Opening, switching, restoring

Opening a workspace takes the steps opening a folder takes, in one function
for both (`switchTarget`, from `openProjectAt`):

1. `project_check(target)`: if another window has the workspace or one of its
   folders, that window comes to the front and the notice names the folder.
2. Unsaved tabs are settled (`settleUnsavedTabs`); the session being left is
   saved.
3. `project_claim(target)`: every key, or none.
4. The window shows the target with its saved tabs; the tree, the watcher,
   the Git panel and the chat list follow `openFolders()`.
5. Each folder's project is resolved (`chat_open_project`, all at once). A
   folder that looks like a copy, or like a project that moved, is asked
   about as when it's opened alone. `chat_open_project`'s `reclaim` claims
   the window's current target again (now with each folder's project ID),
   not the folder it was asked about, and gives the cloud client the
   folder's project. So an answer that comes late, for a folder the window
   has already left, claims nothing back (today it would claim that folder
   and let go of the new one until the new one's answer came).
6. `workspaces.opened(id)`.

Creating a workspace from the open project: the folder's keys are part of
the new target's, so they're never let go, and its tabs stay (D4).

Adding folders: for each folder picked, `workspace_add_folders` checks it
(D5), then `project_claim` takes the target with it. A folder another window
has is refused and that window comes to the front; the others are added.
Each added folder is then resolved and shown.

Removing a folder: its tabs are settled, `workspace_remove_folder`, then
`project_claim` with the target without it. That folder's keys are kept while
its chats are at work here (D18).

Claims (`instances.rs`):

- `claim_keys(target)`: `no-project` with nothing open; a folder's
  `folder:<key>` and `project:<id>`; a workspace's `workspace:<id>` and each
  folder's two. The target carries its folders, so claiming never reads the
  database.
- `Claims.path` becomes a path per key: each key's holder file names its own
  folder, and says the workspace's name (`workspace`, with a serde default),
  so a notice reads "api is open in another window, in Shop".
- `opened` takes the window's project IDs; `AtWork` is kept per folder;
  `at_work_in` (`lib.rs:538-547`) looks at each folder being let go.
- `project_claim` and `project_check` take the target (`{ folder }`,
  `{ workspace: { id, name, folders } }` or none); `lib/instances.ts` wraps
  them, and `openElsewhereNotice` words a workspace.

Starting and restarting:

- `last-project.json` gains `workspace` (serde default); the restore claims
  it with the retries a project gets (`App.tsx:371-388`).
- `rovik-ide <folder>` works as today. `--workspace <id>` opens a workspace:
  an update's restart passes it (`restart_args`, `updates.rs:404-417`). Its
  `workspace:<id>` key is claimed before the window exists, as a command
  line's folder is (`lib.rs:68-73`); its folders are claimed as the window
  opens it.

### 3.4 The menu and the dialog

The project switcher's menu (`ProjectSwitcher.tsx`):

```
Open project…              Ctrl+O
New project…
Create a workspace…
Add folder to workspace…   (in a workspace)
New window
```

- In a workspace, the trigger shows its name, with an icon of stacked
  folders; its tooltip lists the folders.
- The list (`ProjectList.tsx`) shows workspaces among the projects, by when
  each was last opened: the name, "Workspace · 3 folders", the folders, and
  "1 folder missing" when one is. A workspace opens with a click. **Delete**
  (not on the open one, as for projects) asks "Delete the workspace Shop?
  Its folders and their chats stay."
- The filter matches workspaces' names and their folders' paths.

`NewWorkspaceDialog.tsx` (new, built as `NewProjectDialog.tsx` is):

- **Name**: the folders' labels joined ("api + web") until the user types
  one.
- **Folders**: a list, each with ✕, the open folder first; **Add folder…**
  opens the picker, several at once.
- Each folder is checked as it's added (`check`, and `project_check` for
  another window's): the reason shows on its row, and **Create** stays off
  while one does.
- **Create**: `workspace_create`, then `switchTarget`.

### 3.5 The tree, files, the watcher

The tree (`lib/fileTree.ts`, `FileTree.tsx`, `App.tsx`):

- The controller takes the roots and whether they're rows. Rows stay keyed
  by file id, and open folders by absolute path: both stay unique, as
  folders don't nest (D5). Each folder's listing uses that folder as its
  root.
- A folder's row: folder icon, label, the path as its tooltip. A missing
  folder's row is greyed, says "Folder not found", and offers **Locate…** and
  **Remove from workspace**.
- A folder's row's menu: New file, New folder, Paste, Remove from
  workspace. No Rename, Delete, Cut or Copy.
- Each folder's row is a drop target. The "move to the project root" target
  is a project's only.
- New file, New folder and Paste with nothing selected go into the active
  folder. Pasting with a file selected goes into the top of that file's
  folder, as it goes into the project's top today (`App.tsx:1511-1540`).
- Moving a file to another folder is a move like any other. Across drives,
  it's copied then deleted (`entries.rs:584-616`). In git, it's a deletion in
  one repository and a new file in the other.
- `rootId` (`App.tsx:2002-2014`) becomes an id per folder.

The watcher (`filesystem/watch.rs`, `filesystem/mod.rs`):

- `start_file_watch(paths)` watches each folder (one `notify` watcher, a
  watch per folder, each with the per-folder fallback it has now), each with
  that folder's ignore rules.
- `unknown` names its folder. `git-changed` names the repository's working
  folder (`{ workdir }`).
- A repository's git folder above a folder is watched too, without
  recursion (`HEAD`, `index`, `ORIG_HEAD`, `FETCH_HEAD`). A commit or a
  checkout from a terminal then refreshes the Git panel, and holds the
  isolated chats of every open folder in that repository (`project_busy`).
- The tree's gitignore matcher becomes a map by folder.

### 3.6 Search, `@`, tabs and the editor's services

- **Search** (`SearchPanel.tsx`): `search_in_files` runs for each folder at
  once. With several folders, results come under a header per folder; with
  one, as today. Collapsed state is kept by folder and path, and a result
  opens from its folder. Include and exclude globs are relative to each
  folder. The result limit is for the whole search.
- **`@`** in a chat lists the chat's folder's files
  (`list_project_files(chatRoot)`).
- **Tabs** (`Tabs.tsx`): when names collide, each title gets the shortest end
  of its parent path that tells them apart; in a workspace that starts with
  the folder's label ("package.json · api", "package.json · web"). A project
  benefits too (two `index.ts`).
- **IntelliSense** loads every open folder into its one program: definitions
  from each folder (`load_project_definitions`), `owns` true in any folder,
  compiler options merged across the folders' tsconfigs as they're merged
  across a monorepo's today (`lib/tsconfig.ts:14-19`: the first wins), within
  today's limits, shared by the folders.
- **rust-analyzer**: `initialize` lists every folder in `workspaceFolders`;
  the client answers the `workspace/workspaceFolders` request (it answers
  -32601 now, `lsp/client.ts:261-282`). When the folders change the server
  restarts, so `didChangeWorkspaceFolders` isn't needed.
- **Formatter**: `format_file(path, language, folderOf(path))`, so a folder
  that isn't a repository still finds its `.prettierrc` above the file.
- **Markdown preview**: a link starting with `/` resolves from the
  document's folder.
- **Diagnostics for the agent**: each marker is reported for its file's folder
  (`lib/diagnostics.ts`), where that folder's chats look them up.

### 3.7 Git

**Finding the repositories** (`src-tauri/src/git/repos.rs`, new; command
`git_repos(folders)`):

```rust
/// A repository the open folders are in, or hold.
pub struct Repo {
    /// Its canonical git folder (`Repository::path()`): two folders in one
    /// repository share it; a linked worktree has its own.
    pub key: String,
    pub workdir: String,
    /// The working folder's name, with its parent's when two clash.
    pub name: String,
    /// The open folders it lists the changes of, relative to `workdir`
    /// (a folder at or above `workdir`: all of it).
    pub scopes: Vec<String>,
    /// Repositories inside it, which it never lists or stages.
    pub nested: Vec<String>,
}
```

- For each folder, the repository it's in (`discover`, upward, as today);
  then `.git` two levels down, skipping hidden folders and the denylisted
  ones (`node_modules`, `target`, …), and each repository's submodules. At
  most 32 repositories.
- A folder with no repository in it or above it comes back as such. Its
  section offers **Initialize repository**, as the panel does today with no
  repository.
- Found again when the folders change, and when `git-changed` names a
  working folder the list doesn't have (a `git init`, a clone).

**Scopes.** `git_status`, `git_commit` and `git_discard_all` take `scopes`
(by default the folder in `path`, as today) instead of computing one
`project_scope`. The line counts take the same scopes (today they diff the
whole repository, `git/mod.rs:262-309`). Every command that rewrites the
working tree (`rewriting`, `util.rs:47-53`) holds the isolated chats of every
open folder in the repository, not only those of `path`.

**What a commit takes (D11).** `stage_all` stages the scopes, as today, and
pre-commit runs on the real index, as today. The tree that's committed is
then HEAD's with the scopes' part taken from the index. An in-memory index
(`git2::Index::new`) reads HEAD's tree (none on an unborn branch). Its
entries in the scopes are removed, and the real index's entries in the scopes
are added, with their modes (executable bits, links, submodules' commits).
`write_tree_to` then writes the tree. So:

- what's staged elsewhere in the repository stays staged and isn't
  committed;
- what pre-commit stages inside the scopes is committed, as today, and what
  it stages outside them stays staged;
- when the scope is the whole repository, as it usually is, the tree is the
  index's, as today.

Status leaves out an untracked folder with a `.git` inside (a repository of
its own), and a submodule whose only change is inside its own working tree;
revert refuses both.

**The panel.**

- `GitPanel.tsx` keeps what's the window's: the list of repositories, the
  message box and the commit button. A new `GitRepoSection.tsx` takes what's
  one repository's: its status, header, banner, error, changes, history,
  stash and hooks. It's keyed by `Repo.key` and given the repository, not
  `projectPath`.
- One repository: no section header; the panel looks and works as today.
- A section's header: the name; the branch button (↑ahead ↓behind) with its
  menu; Fetch, Pull, History and ⋯. It also has an "include in commit" box:
  on when the repository has changes, and off, with why, during a merge, a
  rebase or conflicts ("Finish the merge in api first").
- `commitAction(statuses, message)` (`gitPublish.ts`) looks at the included
  repositories: Push when one is ahead, Commit when one has changes and
  there's a message, else Publish when one can be published.
- Commit (`git_commit_many` in the backend; `git_commit` is the same for
  one repository):
  1. Every included repository is checked: nothing in progress, no
     conflicts. Otherwise nothing happens, and the problem is named.
  2. Each one's scopes are staged, and its pre-commit hook runs.
  3. prepare-commit-msg and commit-msg run in each. A hook that changes the
     message changes that repository's only.
  4. The commits, each on its repository's HEAD: the branch it's on; on a
     detached HEAD as git would; an unborn branch is born.
  5. post-commit in each.

  A failure in 2 or 3 stops it before any commit, with the hook's output and
  **Commit without hooks**. A failure in 4 says which repositories committed
  and which didn't ("Committed in api and web; infra: …"), and the message
  stays in the box until every one has committed.

- Then each repository that's ahead is pushed, as today. Branches with no
  remote branch get one question for all ("Publish api's feature-x and
  web's feature-x?"). A push that fails shows in its section; the others go
  on.
- `git-changed { workdir }` refreshes that repository's section, at most
  every 2 seconds each.
- Commit drafts are kept by `sessionKey`, publish answers by repository and
  branch, `GIT_LOG_LATEST` by repository, and ConflictResolver's `opKey` with
  the repository.

**Change marks in the editor** (fixes section 11, item 1). `loadGitBase`,
`refreshGitBases` and the stash and commit previews find the file's
repository among `git_repos`' (the deepest working folder that holds it), and
send the path relative to its working folder. Tabs that aren't files on
disk (`disk:`, `chat:`) are left out of the refresh (section 11, item 8).

### 3.8 Chats and the agent

The chat list:

- `chat_db`'s `list_dialogs`, `search` and `unsettled_sends` take the
  window's project IDs (`project_id IN (…)`). `chatStore.switchProject`
  takes them; the unread dots are those of the window's projects.
- With several folders, each row shows its folder's label.

A chat's folder:

- A chat's project is one folder's. `chatRoot(dialog)` is that folder's path
  in the window. ChatPanel's eleven uses of `props.rootPath()` go through it:
  the run's `root` (`ChatPanel.tsx:3057, 3097`), the context estimate (`:724,
768`), `@` (`:922, 937`), the editor's selection reference (`:870`), the
  connector's commands (`:608`), the branch offer (`:3113`) and **Implement**
  on a plan (`:3150`).
- A new chat shows a chip in the composer beside its location
  (`ChatLocation.tsx`), "in api ▾", when the window has several folders. It
  starts on `activeFolder()`, and is fixed once the first message is sent
  (`createDialog` stores its `projectId`).
- The editor context, `@`, and **Add to chat** from another folder aren't
  sent. The composer says why: "web/src/x.ts is in web; this chat works in
  api."
- **Implement** on a plan in another folder starts a new chat in that
  folder.
- The isolation bar and the comparison tabs (`chat:<dialog>:<rel>`) resolve
  paths from the chat's folder.
- Runs don't change: `root` is the chat's folder, which `check_project`
  accepts. The branch offer asks about the chat's folder's repository. Plans
  go to its `.rovik/plans`. Project checks and restricted files are its
  folder's.

Cloud hosts and WSL:

- `State.open` becomes the window's open projects (`set_open_projects`).
  `handled_elsewhere`, `land_waiting`, `project_of`, `is_open` and
  `folder_answered` look among them. A steering message's paths are made
  relative to its chat's own folder. The delivery test uses the first
  folder with an `origin` remote.
- Moving a chat, WSL and the snapshots don't change: each still carries one
  folder.

### 3.9 Terminals

- `TerminalPanel` groups terminals by `sessionKey(target)` instead of by
  `rootPath`. A terminal keeps the folder it started in (`cwd`).
- In a workspace, **+** opens its menu with the folders first, then the hosts.
  A terminal opened otherwise (the panel opening empty) starts in the active
  folder. A tab's tooltip names its folder.

### 3.10 Settings

- `SettingsDialog` gets the open folders and the active one. With several,
  the two "This project" pages (Project checks, Restricted files) show a
  folder select (the active folder first) and work on the folder selected,
  as they work on the root today.
- "Workspaces (N packages)" becomes "Packages (N)"
  (`ProjectChecksSettings.tsx:262-330`). The page's `.workspace-*` CSS
  classes stay; new classes are named after their components
  (`new-workspace-dialog…`, `tree-root…`, `git-repo-section…`).

### 3.11 What's saved

- `last-project.json`: `{ path, workspace, openedFiles, expandedPaths,
terminalCollapsed }`, with `path` empty for a workspace.
- `project-sessions`: entries keyed by `sessionKey`. A tab whose folder has
  left the workspace isn't reopened, as a file that's gone isn't today
  (`App.tsx:391-406`).

---

## 4. Edge cases

| Case                                                                                                       | What happens                                                                                                                                       | Tested in                                              |
| ---------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| **Folders**                                                                                                |                                                                                                                                                    |                                                        |
| The same folder twice, spelled differently (case on Windows and macOS, a trailing separator, a link to it) | Refused: "api is already in the workspace".                                                                                                        | `workspaces_tests.rs`; `NewWorkspaceDialog.test.tsx`   |
| A folder inside another folder of the workspace, or containing one                                         | Refused: "web is inside api, which is in the workspace".                                                                                           | `workspaces_tests.rs`; `NewWorkspaceDialog.test.tsx`   |
| Two folders with the same name                                                                             | Labels "api · server" and "api · client" in the tree, tabs, search, Git sections and chat rows.                                                    | `openFolders.test.ts`                                  |
| A file, a folder that's gone, or one that can't be read                                                    | Refused, with the system's reason.                                                                                                                 | `workspaces_tests.rs`                                  |
| A folder in several workspaces                                                                             | Allowed. Only one window can have it open at a time (D7).                                                                                          | `instances.rs` tests                                   |
| Adding a folder another window has open (alone or in a workspace)                                          | Refused; that window comes to the front. In the Create dialog, the reason shows on the folder's row.                                               | `instances.rs` tests; `NewWorkspaceDialog.test.tsx`    |
| Opening a workspace when another window has one of its folders                                             | Nothing opens; that window comes to the front, and the notice names the folder.                                                                    | `instances.rs` tests; `instances.test.ts`              |
| Opening a folder alone while a workspace in another window has it                                          | That window comes to the front: "api is open in another window, in Shop".                                                                          | `instances.rs` tests; `instances.test.ts`              |
| Opening a workspace another window has                                                                     | That window comes to the front.                                                                                                                    | `instances.rs` tests                                   |
| Switching from a workspace to one of its folders, or back                                                  | The shared folder's keys are held throughout; no other window can take it meanwhile.                                                               | `instances.rs` tests                                   |
| Removing a folder whose chats are at work                                                                  | They go on; the folder's keys are kept until they finish, then let go. Its chats leave the list (they're its folder's: open it alone to see them). | `instances.rs` tests; `chatStore.test.ts`              |
| Removing the last folder                                                                                   | Refused: "A workspace keeps at least one folder. Delete it instead."                                                                               | `workspaces_tests.rs`                                  |
| A folder missing when the workspace opens (deleted, a drive or share not mounted)                          | Its row shows it missing, with Locate… and Remove from workspace; the others open. Its chats aren't listed until it's found.                       | `workspaces_tests.rs` (`missing`); `FileTree.test.tsx` |
| A folder deleted or renamed while open                                                                     | Its row turns missing; its tabs show [DELETED], as tabs do now.                                                                                    | `watch.rs` tests; `FileTree.test.tsx`                  |
| Locate… a folder at its new place                                                                          | Checked as an added folder is (D5), then shown again.                                                                                              | `workspaces_tests.rs`                                  |
| Deleting a workspace another window has open                                                               | Refused. Deleting never touches the folders or their chats.                                                                                        | `workspaces_tests.rs`; `commands/workspace.rs` test    |
| A drive root (`D:\`) as a folder                                                                           | Works; a change at its top refreshes it.                                                                                                           | `fileTools.test.ts`; `fileTree.test.ts`                |
| A folder in WSL (`\\wsl.localhost\…`) beside a Windows folder                                              | Each is a project as it would be alone: its chats run in WSL from its snapshot.                                                                    | By hand (Windows)                                      |
| **Tree and files**                                                                                         |                                                                                                                                                    |                                                        |
| A file dragged from one folder to another                                                                  | Moved (across drives: copied, then deleted); its tab follows.                                                                                      | `FileTree.test.tsx`; `entries.rs` tests                |
| Rename, Delete, Cut or Copy on a folder's row                                                              | Not offered.                                                                                                                                       | `FileTree.test.tsx`                                    |
| New file with nothing selected                                                                             | In the active folder.                                                                                                                              | `openFolders.test.ts`                                  |
| **Search, `@`, tabs**                                                                                      |                                                                                                                                                    |                                                        |
| The same relative path in two folders                                                                      | Two groups; each result opens its own file.                                                                                                        | `searchPanel.test.tsx`                                 |
| Two tabs with the same file name                                                                           | Told apart by their folders.                                                                                                                       | `Tabs.test.tsx`                                        |
| A saved tab from a folder that left the workspace                                                          | Not reopened.                                                                                                                                      | `projectSessions.test.ts`                              |
| An import from one folder into another                                                                     | IntelliSense resolves it (both are loaded).                                                                                                        | `projectWorker.test.ts`                                |
| Two folders' tsconfigs with different `jsx` or `baseUrl`                                                   | The first folder's wins, as across a monorepo's packages today.                                                                                    | Known limit (section 8)                                |
| **Git**                                                                                                    |                                                                                                                                                    |                                                        |
| Two folders in one repository                                                                              | One section listing both folders' changes.                                                                                                         | `git/tests.rs`; `GitPanel.test.tsx`                    |
| A folder inside a repository                                                                               | Its changes only, as today; the editor's marks compare with the right file (fixed).                                                                | `git/tests.rs`; `gitMarks.test.ts`                     |
| A folder holding several repositories, not one itself                                                      | A section for each.                                                                                                                                | `git/tests.rs`                                         |
| A repository inside a folder's repository (untracked or ignored)                                           | A section of its own. The outer one never lists or stages it, and its row's revert no longer trashes it (fixed).                                   | `git/tests.rs`                                         |
| A submodule                                                                                                | A section of its own. The outer one lists it only when its commit moved; no empty commits (fixed).                                                 | `git/tests.rs`                                         |
| A linked worktree beside its main folder                                                                   | Two sections.                                                                                                                                      | `git/tests.rs`                                         |
| A detached HEAD                                                                                            | Its header says so; the commit goes on it, as git's would.                                                                                         | `git/tests.rs`                                         |
| A new repository with no commit                                                                            | The commit makes its branch.                                                                                                                       | `git/tests.rs`                                         |
| A merge, a rebase or conflicts in one repository                                                           | Its box is off, saying why; the others commit.                                                                                                     | `GitPanel.test.tsx`                                    |
| pre-commit fails in one repository                                                                         | Nothing is committed anywhere; its output and **Commit without hooks** show.                                                                       | `git/tests.rs`                                         |
| commit-msg rewrites the message in one repository                                                          | Only that repository's commit has the change.                                                                                                      | `git/tests.rs`                                         |
| A commit fails after others were written (a terminal holds `index.lock`)                                   | "Committed in api and web; infra: …"; the message stays.                                                                                           | `git/tests.rs`; `GitPanel.test.tsx`                    |
| Changes staged from a terminal outside the workspace's folders                                             | Stay staged, and aren't committed (fixed).                                                                                                         | `git/tests.rs`                                         |
| A branch switched from a terminal in a folder's repository                                                 | The section updates; that repository's folders' isolated chats wait for the checkout.                                                              | `watch.rs` tests                                       |
| One push rejected                                                                                          | That section says so; the others are pushed.                                                                                                       | `GitPanel.test.tsx`                                    |
| A folder without a repository                                                                              | Its section offers **Initialize repository**.                                                                                                      | `GitPanel.test.tsx`                                    |
| **Chats**                                                                                                  |                                                                                                                                                    |                                                        |
| A new chat in a workspace                                                                                  | In the active folder; the chip changes it until the first message.                                                                                 | `chatInput.test.tsx`                                   |
| Another folder's file in the editor context, `@` or **Add to chat**                                        | Not sent; the composer says why.                                                                                                                   | `chatInput.test.tsx`                                   |
| A new chat on a folder's main branch                                                                       | The branch offer asks about that folder's repository.                                                                                              | `chatBranchOffer.test.tsx`                             |
| A chat on a host or in WSL                                                                                 | As when its folder is open alone; its results land while the workspace is open.                                                                    | `rovik-client` `remote/tests.rs`                       |
| A folder that looks like a copy, or a moved project                                                        | Asked about for that folder, as when it's opened alone.                                                                                            | `projects_tests.rs` (as now)                           |
| **Saved state**                                                                                            |                                                                                                                                                    |                                                        |
| Quitting and starting again                                                                                | The workspace and its tabs come back.                                                                                                              | e2e: restart                                           |
| An update's restart                                                                                        | The workspace opens again.                                                                                                                         | `updates.rs` tests                                     |
| The last workspace open in another window at start                                                         | This window starts with nothing open, as with a project.                                                                                           | `switchTarget.test.ts`                                 |
| **Terminals**                                                                                              |                                                                                                                                                    |                                                        |
| A new terminal in a workspace                                                                              | **+** asks which folder; the terminal starts there.                                                                                                | `terminalPanel.test.tsx`                               |
| A folder removed with terminals in it                                                                      | They keep running.                                                                                                                                 | `terminalPanel.test.tsx`                               |

---

## 5. Milestones

| Milestone                            | What it delivers                                                                                                                                                                                                                                                    | Done when                                                                                                                                              |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **A. A project as a list of one**    | One copy of the path helpers; `openFolders` with one folder; the watcher, the tree's controller, git's scopes and the editor's services take folders; git's repository list, commit with only its scopes, nested repositories left alone, the editor's marks fixed. | Every existing test passes unchanged, the new ones pass with two folders, and the e2e run is unchanged. Nothing changes for a user but the fixed bugs. |
| **B. Workspaces stored and opened**  | Schema 11, `chat_db/workspaces.rs`, the commands, the claims, `switchTarget`, what's saved, `--workspace`.                                                                                                                                                          | Their tests pass; a workspace opens in a test harness (`switchTarget.test.ts`). No menu yet.                                                           |
| **C. A window with several folders** | Folder rows, Remove and Locate, search, tabs, terminals, the settings pages, IntelliSense and rust-analyzer.                                                                                                                                                        | The component tests in tasks C01–C07 pass.                                                                                                             |
| **D. Git across repositories**       | Sections, the commit across repositories, refreshing by repository.                                                                                                                                                                                                 | The git tests of D10 and D11 pass, and GitPanel's tests with one repository pass unchanged.                                                            |
| **E. Chats in a workspace**          | The chat list of several projects, a chat's folder, what's sent, the isolation bar, the cloud client.                                                                                                                                                               | The chat and client tests in E01–E04 pass.                                                                                                             |
| **F. The menu**                      | The menu items, the dialog, workspaces in the list, the README, the e2e steps.                                                                                                                                                                                      | The e2e steps in section 7 pass on Linux, macOS and Windows.                                                                                           |

A–E keep the menu items out: no user can open a workspace until F, so each
of them can be merged on its own. They ship together, in the release F is
in. Sizes: A M, B M, C M, D L, E M, F S.

---

## 6. Implementation order for an AI agent

Rules for every task (AGENTS.md):

- Read the files a task names, their callers and their tests before
  changing them; follow the comments' style.
- Every task ships with tests that would fail without it, named as sentences.
- A project (one folder) behaves as it does today after every task: its
  existing tests pass unchanged.
- Before each commit: `npm test`, `npm run typecheck`, `npm run
format:check`; from `src-tauri/`, `cargo nextest run -p <each crate
changed>` (the app crate is `rovik-ide`) and `cargo clippy -p <crate>
--tests` with no new warnings; `rustfmt --edition 2021` on the Rust files
  changed.
- Write each task's notes under "Notes from implementation" at the end of
  this file.

| Task | Files                                                                                                                                                                                   | What                                                                                                                                                                                                                                                                     | Tests                                                                                                                                                                                                                                                                                                                                                                           |
| ---- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A01  | `src/lib/fileTools.ts`, `src/lib/fileTree.ts`, `src/components/FileTree.tsx`, `src/App.tsx`                                                                                             | One `isAtOrUnder`, `parentOf` and `samePath` in `fileTools.ts`, with the platform's case rule, drive roots and UNC roots, in place of the five copies.                                                                                                                   | `fileTools.test.ts`: POSIX and Windows paths, `D:\x`'s parent is `D:\`, `\\server\share`, case ignored on Windows and macOS only. `fileTree.test.ts`: a change at the top of `D:\` refreshes it.                                                                                                                                                                                |
| A02  | `src/lib/openFolders.ts` (new), `src/App.tsx`                                                                                                                                           | `WindowTarget`, `target()` in place of `rootPath`, `openFolders`, `folderOf`, `activeFolder`, labels, `sessionKey`. App.tsx's 28 uses of `rootPath()` go through them (a project is a list of one).                                                                      | `openFolders.test.ts`: `folderOf` with two folders and with none; labels when names clash; `activeFolder`'s order; `sessionKey`.                                                                                                                                                                                                                                                |
| A03  | `src-tauri/src/filesystem/watch.rs`, `src-tauri/src/filesystem/mod.rs`, `src/lib/commands.ts`, `src/App.tsx`                                                                            | `start_file_watch(paths)`: a watch and ignore rules per folder; `unknown` names its folder; `git-changed { workdir }`; a repository's git folder above a folder watched without recursion, and `project_busy` for every open folder in it; the tree's matcher by folder. | `watch.rs`: changes in two folders are both reported; an overflow names its folder; a commit in a repository above a folder sends `git-changed` with its working folder; a new set of folders stops the old watches.                                                                                                                                                            |
| A04  | `src/lib/fileTree.ts`, `src/components/FileTree.tsx`, `src/App.tsx`                                                                                                                     | The controller takes the roots and whether they're rows; each folder's listing uses its own root; `rootId` per folder. With one root and no rows, the rows are today's.                                                                                                  | `fileTree.test.ts`: two roots as rows, each expanded, refreshed and moved across; one root lists as before.                                                                                                                                                                                                                                                                     |
| A05  | `src-tauri/src/git/repos.rs` (new), `git/mod.rs`, `git/changes.rs`, `src-tauri/src/util.rs`, `src-tauri/src/lib.rs`                                                                     | `git_repos(folders)`; `scopes` on `git_status`, `git_commit` and `git_discard_all` (by default today's one); line counts scoped; `rewriting` holds every open folder in the repository.                                                                                  | `git/tests.rs`: two subfolders of one repository are one repository with two scopes; a linked worktree is its own; a nested repository and a submodule are found; a folder holding repositories lists each; a folder with none says so; a status with two scopes lists both and nothing beside; line counts stay in scope.                                                      |
| A06  | `git/changes.rs`, `git/mod.rs`                                                                                                                                                          | A commit takes only its scopes (an in-memory index from HEAD); status leaves out nested repositories and submodules changed only inside; revert refuses both.                                                                                                            | `git/tests.rs`: a change staged outside the scope stays staged and isn't committed; a nested repository doesn't stop a commit and isn't listed; its revert is refused; a submodule changed only inside isn't listed, and no empty commit is made.                                                                                                                               |
| A07  | `src/App.tsx`, `src/lib/gitMarks.ts`, `src/components/GitPanel.tsx`                                                                                                                     | The editor's marks ask with the path relative to the file's repository; GitPanel's `onOpenFile` keeps the file it's given; tabs not on disk aren't refreshed.                                                                                                            | `gitMarks.test.ts`: in a project at `repo/app`, `package.json` is asked as `app/package.json`; `disk:` and `chat:` tabs keep their bases on a refresh.                                                                                                                                                                                                                          |
| A08  | `src/components/Editor.tsx`, `src/lib/fileFormatters.ts`, `src/lib/markdownPreview.ts`, `src/lib/diagnostics.ts`                                                                        | The formatter's root, the Markdown preview's `/` and the diagnostics' root are the file's folder.                                                                                                                                                                        | Each with two folders: a file of the second folder is formatted, linked and reported against the second.                                                                                                                                                                                                                                                                        |
| B01  | `rovik-core/src/chat_db/schema.rs`, `chat_db/workspaces.rs` (new), `chat_db/workspaces_tests.rs` (new), `chat_db/mod.rs`                                                                | Schema 11; `create`, `add`, `remove`, `relink`, `delete`, `get`, `list`, `opened`; `check` (D5).                                                                                                                                                                         | `upgrades_a_version_10_database`; `workspaces_tests.rs`: each rule of D5 (duplicates by key, inside, containing, not a folder); the last folder stays; deleting leaves the folders' projects and chats; list order and `missing`.                                                                                                                                               |
| B02  | `chat_db/api.rs`, `src-tauri/src/commands/workspace.rs` (new), `commands/mod.rs`, `src-tauri/src/lib.rs`, `src/lib/chatApi.ts`, `src/types.ts`                                          | The commands; delete refused while open elsewhere (`not_open_elsewhere` with `workspace:<id>`).                                                                                                                                                                          | `api` tests as the projects' are; deleting a workspace open elsewhere is refused.                                                                                                                                                                                                                                                                                               |
| B03  | `src-tauri/src/instances.rs`, `src-tauri/src/lib.rs` (`project_claim`, `project_check`, `at_work_in`), `src-tauri/src/commands/chat.rs` (`reclaim`), `src/lib/instances.ts`             | Claim targets (D7): the keys, a holder per key with the workspace's name, `opened` with several IDs, `AtWork` per folder, `reclaim` of the whole target, the notices.                                                                                                    | `instances.rs`: a workspace takes every key or none; a folder another process has stops it and is named; adding and removing a folder keep the others' keys; a folder removed with chats at work keeps its keys; switching to a member keeps its keys. `instances.test.ts`: the notices.                                                                                        |
| B04  | `src/lib/switchTarget.ts` (new), `src/lib/projects.ts`, `src/App.tsx`                                                                                                                   | `switchTarget(target)` for a folder or a workspace, with its steps (3.3) as calls it's given; each folder resolved with its suggestions.                                                                                                                                 | `switchTarget.test.ts`: the steps' order; a refused claim changes nothing; a cancelled unsaved tab stops the switch; every folder is resolved; a target taken elsewhere at start leaves the window empty.                                                                                                                                                                       |
| B05  | `src-tauri/src/filesystem/mod.rs`, `src/lib/projectHistory.ts`, `src/lib/projectSessions.ts`, `src-tauri/src/lib.rs` (`--workspace`), `src-tauri/src/updates.rs`, `src/lib/updates.ts`  | What's saved and the restart (D16).                                                                                                                                                                                                                                      | `updates.rs`: `restart_args` with a workspace; `projectSessions.test.ts`: a workspace's entry, and a tab of a folder that left isn't restored; a `last-project.json` without `workspace` reads as before.                                                                                                                                                                       |
| C01  | `src/components/FileTree.tsx`, `src/components/FileContextMenu.tsx`, `src/App.tsx`, `src/styles.css`                                                                                    | Folder rows (label, missing), their menu (Remove from workspace, Locate…), no Rename, Delete, Cut or Copy on them; dropping on a folder's row; the project-root target for a project only; Remove and Locate (tabs settled, command, claim).                             | `FileTree.test.tsx`: a folder row's menu; a drop on it moves into that folder; a missing row's actions.                                                                                                                                                                                                                                                                         |
| C02  | `src/components/SearchPanel.tsx`, `src/components/searchPanel.test.tsx` (new)                                                                                                           | A search per folder, grouped by folder, one limit, results opened from their folder.                                                                                                                                                                                     | Two folders' results under their headers; the same relative path opens two files; one folder looks as today.                                                                                                                                                                                                                                                                    |
| C03  | `src/components/Tabs.tsx`, `src/lib/editorTabs.ts`                                                                                                                                      | Colliding titles told apart.                                                                                                                                                                                                                                             | `Tabs.test.tsx`: two `package.json` show their folders; a unique name shows alone.                                                                                                                                                                                                                                                                                              |
| C04  | `src/components/TerminalPanel.tsx`                                                                                                                                                      | Grouped by `sessionKey`; **+** asks for the folder in a workspace; the tooltip.                                                                                                                                                                                          | `terminalPanel.test.tsx`: the menu lists the folders, and the terminal starts in the one picked; a removed folder's terminal keeps running.                                                                                                                                                                                                                                     |
| C05  | `src/components/SettingsDialog.tsx`, `ProjectChecksSettings.tsx`, `RestrictedFilesSettings.tsx`, `SettingsMenu.tsx`                                                                     | The folder select in a workspace; "Packages (N)".                                                                                                                                                                                                                        | `SettingsDialog.test.tsx`: the select, and the page following it; `ProjectChecksSettings.test.tsx`: the heading; `RestrictedFilesSettings.test.tsx`: the selected folder's list.                                                                                                                                                                                                |
| C06  | `src/lib/intellisense/index.ts`, `src/lib/intellisense/project.ts`, `src/lib/intellisense/compilerOptions.ts`, `src-tauri/src/typings.rs`                                               | The worker loads every folder; `owns` any; options merged; the limits shared.                                                                                                                                                                                            | `projectWorker.test.ts`: an import from one folder into another resolves; adding a folder loads it.                                                                                                                                                                                                                                                                             |
| C07  | `src/lib/lsp/client.ts`, `src/lib/lsp/rustAnalyzer.ts`, `src/components/Editor.tsx`, `src/App.tsx`                                                                                      | rust-analyzer with every folder; the `workspace/workspaceFolders` answer; a restart when the folders change.                                                                                                                                                             | `lsp/client.test.ts` (new): `initialize` lists both folders; the request is answered with them.                                                                                                                                                                                                                                                                                 |
| D01  | `src/components/GitPanel.tsx`, `src/components/GitRepoSection.tsx` (new), `GitHooksView.tsx`, `ConflictResolver.tsx`, `src-tauri/src/git/history.rs`                                    | GitPanel split into the window's part and a section per repository; one repository looks as today; drafts, publish answers and `GIT_LOG_LATEST` by repository; `opKey` with the repository; folders without one.                                                         | `GitPanel.test.tsx`: one repository renders as before; two show two sections with their branches; `gitBranchMenu`, `gitRebase` and `conflictResolver` tests pass unchanged; `history.rs`: two repositories' logs don't stop each other.                                                                                                                                         |
| D02  | `git/changes.rs` (`git_commit_many`), `git/hooks.rs`, `src/lib/gitPublish.ts`, `GitPanel.tsx`, `GitRepoSection.tsx`                                                                     | The commit across repositories (D10), the include boxes, `commitAction(statuses)`, the pushes and one publish question after.                                                                                                                                            | `git/tests.rs`: a pre-commit failing in the second repository leaves the first uncommitted; a commit-msg hook changes its repository's message only; each commit goes on its own branch; a lock in the third reports the first two. `gitPublish.test.ts`: actions over several. `GitPanel.test.tsx`: a partial failure keeps the message; a rejected push shows in its section. |
| D03  | `GitPanel.tsx`, `src/App.tsx`                                                                                                                                                           | A section refreshed by its `git-changed`; the list found again for a new repository.                                                                                                                                                                                     | `GitPanel.test.tsx`: an event refreshes its section only; a new working folder adds a section.                                                                                                                                                                                                                                                                                  |
| E01  | `rovik-core/src/chat_db/store.rs`, `chat_db/search.rs`, `chat_db/api.rs`, `src-tauri/src/commands/chat.rs`, `src/lib/chatApi.ts`, `src/lib/chatStore.ts`, `src/components/ChatList.tsx` | The chats of several projects; the folder on each row.                                                                                                                                                                                                                   | `chat_db/tests.rs`: two projects' chats listed together by recency, and searched together; `chatStore.test.ts`: switching to two projects keeps the running chats.                                                                                                                                                                                                              |
| E02  | `src/components/ChatPanel.tsx`, `src/lib/fileReferences.ts`, `src/lib/chatBranch.ts`, `src/lib/plans.ts`                                                                                | `chatRoot`; the folder chip; context and `@` kept to the chat's folder, with the note; **Implement** on another folder's plan.                                                                                                                                           | `chatInput.test.tsx`: the chip's default, and fixed after the first message; another folder's file isn't sent; `chatBranchOffer.test.tsx`: the chat's folder's repository.                                                                                                                                                                                                      |
| E03  | `src/components/IsolationBar.tsx`, `src/lib/overlay.ts`, `src/App.tsx` (`chat:` tabs)                                                                                                   | Paths from the chat's folder.                                                                                                                                                                                                                                            | `isolationBar.test.tsx`: a file of the second folder's chat opens from that folder.                                                                                                                                                                                                                                                                                             |
| E04  | `rovik-client/src/remote.rs`, `rovik-client/src/remote/results.rs`, `src-tauri/src/commands/chat.rs`                                                                                    | The window's open projects; steering paths relative to the chat's own folder; the delivery test's folder.                                                                                                                                                                | `remote/tests.rs`: a host's result for the second folder's chat lands while the workspace is open; a steering message's paths are relative to its chat's folder.                                                                                                                                                                                                                |
| F01  | `src/components/ProjectSwitcher.tsx`, `ProjectList.tsx`, `NewWorkspaceDialog.tsx` (new), `src/App.tsx`, `src/styles.css`                                                                | The menu items, the dialog, Add folder, workspaces in the list, the trigger.                                                                                                                                                                                             | `ProjectSwitcher.test.tsx` (new): the items' order, and Add folder only in a workspace; `NewWorkspaceDialog.test.tsx` (new): each refusal shows on its row and keeps Create off, the open folder is listed first; the list's workspace row, its missing count and its delete question.                                                                                          |
| F02  | `README.md`                                                                                                                                                                             | Features, Getting started, Several windows and the Git panel, for workspaces.                                                                                                                                                                                            | `npm run format:check`.                                                                                                                                                                                                                                                                                                                                                         |
| F03  | `scripts/e2e/run.mjs`, `scripts/e2e/README.md`                                                                                                                                          | The e2e steps (section 7).                                                                                                                                                                                                                                               | The steps themselves.                                                                                                                                                                                                                                                                                                                                                           |

---

## 7. Testing

**Rust** (`cargo nextest run` per crate, as CI runs them on Linux, macOS and
Windows):

- `rovik-core`: `chat_db/workspaces_tests.rs` (every rule of D5 and every
  operation), the schema's `upgrades_a_version_10_database`, the chat list
  and search across several projects (`chat_db/tests.rs`).
- The app crate: `instances.rs` (claims, section 4's window rows),
  `filesystem/watch.rs` (several folders, a repository above a folder),
  `git/tests.rs` (repositories, scopes, commit with only its scopes, nested
  repositories and submodules, the commit across repositories with its hooks
  and its partial failure), `updates.rs` (`restart_args`).
- `rovik-client`: `remote/tests.rs` (open projects, landing, steering paths).

Tests that change process-wide state (the watcher's generation,
`GIT_LOG_LATEST`) take that state's lock, so they pass under nextest and
`cargo test` alike.

**Frontend** (`npm test`): the tests named in section 6, and these new files:
`openFolders.test.ts`, `switchTarget.test.ts`, `searchPanel.test.tsx`,
`ProjectSwitcher.test.tsx`, `NewWorkspaceDialog.test.tsx`,
`lsp/client.test.ts`.

**End to end** (`scripts/e2e/run.mjs`, with a second demo folder that is a
repository of its own):

1. `workspace: create one from the open project, add a second folder; each is a row in the tree`
2. `workspace: the same file name in each folder opens two tabs, told apart`
3. `workspace: search finds a match in each folder`
4. `workspace: git lists both repositories with their branches; one commit lands in each`
5. `workspace: a new chat in the second folder changes a file there`
6. `workspace: a folder removed closes its tabs; the other folder stays`
7. `restart: the workspace and its tabs come back`
8. `instances: the workspace's folder opened in another window brings this one to the front`

**By hand**, once per system, for what CI can't reach:

- Windows: a folder in WSL beside a Windows folder (a chat in each); a drive
  root; the same folder added with two spellings of case.
- macOS: a folder under `/tmp` (FSEvents reports `/private/tmp`); a folder on
  a case-insensitive volume added twice in two cases.
- Linux: four large folders (the inotify limit); a chat in each of two
  folders at once, each in bubblewrap with its own folder writable.

---

## 8. Risks and known limits

- **A chat changes one folder.** "Add a field to the API and use it in the
  client" takes a chat per folder, or opening the folders' parent as a
  project. This is the largest limit, and the reason for section 10.
- **A folder is open in one window at a time**, alone or in one workspace
  (D7). Wanting it in two windows means a copy of it.
- **IntelliSense merges compiler options** across folders, the first winning
  a conflict, as it does across a monorepo's packages today.
- **Commits across repositories aren't one transaction.** The hooks all run
  first (D10); a failure while writing the commits leaves the earlier ones,
  says which, and keeps the message.
- **Hooks run interleaved:** every pre-commit before any commit. A hook that
  looks at another repository sees it before its own commit.
- **Repositories deeper than two levels inside a folder aren't listed.** The
  repository around them still never stages them (D11).
- **inotify's limit is the system's**, shared by the folders. A folder that
  can't be watched whole falls back as today, and its tree refreshes when
  its rows are expanded again.
- **Two spellings of one WSL folder** (`\\wsl$\…` and `\\wsl.localhost\…`)
  aren't recognised as one folder, as for projects today: `path_key` keeps
  the host name as written.
- **Opening a workspace counts as opening each folder**: their own rows
  move up the list too (`record_path`, `projects.rs:240-260`).
- **Schema 11:** after the upgrade an older Rovik can't open the chats
  database, as after every schema step (`schema.rs:368-376`).
- **Cost:** a refresh runs a status per repository, and a search per folder.
  Finding repositories reads two levels of each folder, when the folders
  change or a new repository appears, not on every refresh.

---

## 9. Questions

Asked of the user while planning:

| Question                | Answer                                                                                    |
| ----------------------- | ----------------------------------------------------------------------------------------- |
| The menu items' wording | **Create a workspace…** and **Add folder to workspace…**, both above **New window** (D3). |

Decided while planning, for the user to check:

- **A workspace groups projects, and a chat works in one folder** (D1, D2).
  This is the decision with the most weight. It keeps the agent's
  isolation, landing, sandbox, WSL and hosts as they are, and ships the
  user's requirements without rebuilding them. Chats across folders would
  come next (section 10).
- **Create opens the workspace in this window** and keeps the open folder's
  tabs (D4), rather than in a new window.
- **Nested folders are refused** (D5), rather than listed twice.
- **One window per folder** (D7): a folder can't be open alone in one window
  and in a workspace in another.
- **One message box, a commit per repository**, with every checking hook
  run before any commit (D9, D10).
- **A commit takes only its folders' changes** (D11). This changes today's
  behaviour for a folder inside a repository: what a terminal staged beside
  it isn't committed anymore.
- **Repositories two levels inside a folder** are listed, and submodules
  (D9). Deeper ones aren't.
- **IntelliSense loads every folder** into one program (D13), rather than
  following the active file's folder (which would reload on every switch
  between folders).
- **No workspace file and no renaming** in this plan: a workspace is kept in
  the app, and making one again is quick.
- **Workspaces' names needn't be unique.**

---

## 10. Later: chats across folders

Not in this plan's milestones; what a plan for it would need, from the code
as it is:

- **Whose chat.** A chat of the workspace itself: a project row for the
  workspace (`dialogs.project_id` references `projects`), or a
  `dialogs.workspace_id`. `check_project` accepts the workspace's folders.
- **One view of every folder.** The run's root would be a folder whose
  entries are the workspace's folders by label (`api/src/x.ts`): a
  `vfs::backend::Backend` that hands each top-level name to that folder's
  `OverlayFs`, behind one mount (`vfs::platform_mount`). The tools and the
  paths the model sees (`resolve_within_root`, `paths::norm_rel`, write
  scopes, plans' `Files:`) stay as they are; child overlays mount over the
  view as they do now (`overlay/chats.rs:670-697`).
- **Without the driver** there is no such folder. A run that would work
  directly in the project (no virtual-files driver; Plan mode without an
  overlay, `run.rs:760-786`) would be refused, or kept to one folder.
- **The sandbox:** a redirect per folder (`--bind <view>/<label> <folder>`,
  `confine.rs:896-912`). Seatbelt and Landlock can't redirect, so commands
  naming any folder's path are refused by text, as one folder's are today
  (`overlay/chats.rs:80-99`).
- **Landing:** per folder, under one journal, all or none across folders
  (`land.rs` locks per folder already).
- **Context and checks:** each folder's instructions file, labelled; checks
  per folder, run as `cd <label> && …`, as V09 runs checks one folder down
  (`agent_run/project.rs:200-227`).
- **Windows and hosts:** `ProjectEnsure`, `WorkSync` and `ChatBundle` carry a
  snapshot per folder, and the daemon builds `work/<chat>/<label>/`; a
  feature flag (`rovik-protocol/src/lib.rs:66-68`) keeps older hosts on
  one-folder chats.
- **Connectors:** Claude Code's `--add-dir` and Codex's writable roots for
  the other folders (neither is passed today).

Size: L, in a plan of its own once workspaces are in use.

---

## 11. Problems in today's code found while planning

On this plan's path, so it fixes them:

1. **The editor's git marks are wrong in a folder of a repository.**
   `loadGitBase` and `refreshGitBases` send a path relative to the project
   (`App.tsx:1110-1121, 1246-1259`), and `git_base_content` reads it as
   relative to the repository (`git/changes.rs:183-198`). In a project at
   `repo/app`, `package.json` is compared with the repository's own. A07.
2. **A commit takes what was staged outside the project's folder**
   (`git/changes.rs:48-74`), though the README says such a project commits
   only its own folder (`README.md:77-80`). A06.
3. **A repository inside the project makes Commit fail**, as `add_all`
   refuses its folder, and its row's revert moves the whole repository to
   the trash (`git/changes.rs:270-284`). A06.
4. **A submodule changed only inside makes an empty commit**, and its
   revert fails. A06.
5. **A folder inside a repository doesn't see the repository's `.git`:** a
   commit or checkout from a terminal doesn't refresh the Git panel, or
   hold the isolated chats (`filesystem/watch.rs:132-148, 554-566`). A03.
6. **The line counts diff the whole repository** on every refresh
   (`git/mod.rs:262-309`). A05.
7. **`parentOf("D:\\x")` is `"D:"`**, so a change at the top of a drive root
   doesn't refresh it (`fileTree.ts:197-202`). A01.
8. **A git refresh overwrites the base of a comparison tab** (`disk:` and
   `chat:` tabs, whose `file` is only a name: `App.tsx:770, 1031`) with the
   version of a file by that name at the repository's top
   (`App.tsx:1246-1259`). A07.

Left as they are, for issues of their own:

- Stash takes the whole repository, while its menu item follows the
  project folder's changes (`GitPanel.tsx:1796-1806`).
- Pull and Rebase refuse because of changes outside the project's folder
  (`ensure_clean`, `git_sync.rs:233-246`), which the panel doesn't list.
- The Markdown preview drops a UNC path's leading backslash
  (`markdownPreview.ts:357-385`).
- The tree, the watcher, search and `@` each read ignore files their own way
  (`filesystem/mod.rs:111-127`, `watch.rs:78-130`, `code_search.rs:283-292`,
  `project_files.rs`).
- `last-project.json` is written at every keystroke (`App.tsx:1929-1939`).
- An isolated run may look up the editor's diagnostics by its view's root,
  and so never see them (`agent_tools.rs:563-571`; not confirmed).

---

## Notes from implementation
