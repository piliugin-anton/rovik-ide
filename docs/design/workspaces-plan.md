# Workspaces: several folders in one window

Today a window has one project, and a project is one folder. A **workspace**
is a named list of folders opened together in one window. Each folder is a
row at the top of the file tree, search covers all of them, and the Git
panel lists every repository they're in. A commit puts each file into the
repository it belongs to, on the branch that repository is on. A chat works
in one folder, or, as an **All folders** chat, in every folder at once.

This plan says what to build, in which files, in which order, and how each
step is tested. Section 1 lists the decisions; section 2 what the code does
today; section 3 how workspaces work; section 4 how All folders chats work;
section 5 the edge cases; sections 6 to 8 the milestones, the tasks and the
tests; section 9 the risks; section 10 the questions asked and the ones
decided while planning, for the user to check; section 11 what this plan
leaves out; section 12 the problems in today's code found on the way.

The user's requirements:

- In the project menu (the switcher above the sidebar tabs), above **New
  window**: **Create a workspace…**, and, in a workspace, **Add folder to
  workspace…**.
- A workspace holds several folders and shows them in the file tree.
- A folder may be a repository of its own. A commit puts each file into the
  repository it belongs to, on the branch that repository is on.
- **New chat** shows which folder the chat is for: a dropdown of the
  workspace's folders.
- Chats that work across the folders, with the context of all of them, in
  the first release.
- The other places a second folder changes are found, handled and tested.

The idea in one sentence: **a workspace groups projects, and has chats of
its own.** Each folder stays the project it is when opened alone, with its
chats, its settings, its plans and its ID file: a chat started in a folder
is that folder's, and everything built per project (runs, isolation,
landing, WSL, cloud hosts, checks) works for it as it does. The workspace
is a project too, with no folder of its own: its chats are the All folders
chats, which see every folder as a top-level folder of one view
(`api/src/x.ts`, `web/src/y.ts`). Around them, the tree, the watcher,
search, tabs, terminals, the editor's services, the Git panel and the chat
list learn to take a list of folders, and a project is the list of one.

The plan was reviewed against the code twice before it was committed;
section 10 ends with what the reviews changed.

Contents:

1. [Decisions](#1-decisions)
2. [What the code does today](#2-what-the-code-does-today)
3. [How workspaces work](#3-how-workspaces-work)
4. [All folders chats](#4-all-folders-chats)
5. [Edge cases](#5-edge-cases)
6. [Milestones](#6-milestones)
7. [Implementation order for an AI agent](#7-implementation-order-for-an-ai-agent)
8. [Testing](#8-testing)
9. [Risks and known limits](#9-risks-and-known-limits)
10. [Questions](#10-questions)
11. [Not in this plan](#11-not-in-this-plan)
12. [Problems in today's code found while planning](#12-problems-in-todays-code-found-while-planning)

---

## 1. Decisions

### The workspace

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Why                                                                                                                                                                                                                                                                                                                                                                                |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | **A workspace groups projects, and has chats of its own.** It is a name and an ordered list of folders. Each folder stays a project of its own: its chats, project settings, plans, ID file (`.rovik/project.json`) and window claim are the ones it has when opened alone. The workspace is itself a project, with no folder: its chats are the All folders chats (D22).                                                                                                                                                                                             | Every per-project mechanism keys on one project: a chat's run and its overlay (`agent_run/run.rs:149-152`; `overlay/chats.rs:405-480`), landing, the sync to WSL and hosts (`rovik-client/src/shadow.rs`), the settings (by folder). A folder's chats stay the same whether it's open alone or in a workspace, and the workspace's own chats need an owner (`dialogs.project_id`). |
| D2  | **New chat asks which folder.** With several folders, both **New chat** buttons open a dropdown of the workspace's folders, then **All folders**; the chat is made in the one clicked. A folder chat works in its folder, as a project's chat does. The message box shows the chat's folder; it doesn't change once the chat has a message.                                                                                                                                                                                                                           | The user's choice. The folder decides what the chat can read and change, so the user picks it rather than the app guessing.                                                                                                                                                                                                                                                        |
| D3  | **The menu:** **Create a workspace…** always, and **Add folder to workspace…** when a workspace is open, both above **New window**.                                                                                                                                                                                                                                                                                                                                                                                                                                   | The user's wording.                                                                                                                                                                                                                                                                                                                                                                |
| D4  | **Create** opens a dialog: the workspace's name and its folders, the open folder already listed. **Create** opens the workspace in this window; the open folder's tabs, terminals and commit message come along. **Add folder to workspace…** opens the folder picker, which takes several folders at once.                                                                                                                                                                                                                                                           | Making one from the open project is the common case. Opening it in this window follows **Open project…**; **New window** is there for the other case.                                                                                                                                                                                                                              |
| D5  | **Folder rules.** A workspace has at least one folder. A folder can't be in it twice (the same folder however it's spelled: case where the file system ignores it, a trailing separator, a link), nor inside another of its folders, nor contain one, nor share another member's project ID (a copy made with `cp -r`, or one sharing its chats). A folder may be in several workspaces.                                                                                                                                                                              | Nested folders would list, watch, search, claim and index the same files twice. Two folders with one project ID would need one claim key twice, which an OS lock refuses (`os_lock.rs:1-7`).                                                                                                                                                                                       |
| D6  | **Stored in the chats database**, schema version 11: `projects.kind` (`folder` or `workspace`) and a `workspace_folders` table. A workspace is a `projects` row of kind `workspace` with no path; its folders are rows of their own. Only `projects::list` and `summary` learn the kind; everything that asks which chats, may this chat run here, or is it open elsewhere works by ID as it does.                                                                                                                                                                    | The All folders chats need an owner row: `dialogs.project_id` references `projects`. In the same database, a workspace and its folders change in one transaction, across windows. The schema step also makes an older Rovik refuse the database rather than show a workspace as a missing project with **Relink** (`schema.rs:368-376`).                                           |
| D7  | **One window per folder.** A workspace is claimed as a project is (`instances.rs`): its own `project:<id>` and each folder's keys (`folder:` and `project:`), all or none. A folder is open in one window at a time, alone or in a workspace; opening it where it's taken brings that window to the front.                                                                                                                                                                                                                                                            | A chat's run lives in the process of the window that has its project (README, "Several windows"). With `project:<id>`, the checks made today by project ID (`open_elsewhere`, `handled_elsewhere`) guard the workspace's chats unchanged.                                                                                                                                          |
| D8  | **Each folder has a stored name**, a valid path segment on every system, unique in the workspace whatever its case, given when the folder joins and never changed: its folder's name, else `<parent>-<name>`, then `-2`, `-3`. A removed folder's name stays taken, and comes back if the folder does. The name is what the tree, tabs, search, the Git panel and the chats show.                                                                                                                                                                                     | All folders chats write `api/src/x.ts` into their history and their plans: a name that changed, or went to another folder, would point old messages at the wrong files.                                                                                                                                                                                                            |
| D9  | **The tree** of a workspace has a row per folder at the top, with its name. A folder's row can't be renamed, deleted, cut, copied or dragged, from the menu, a shortcut or the mouse, and the backend refuses those on a window's folder; its menu has **Remove from workspace**, and **Locate…** and **Try again** while the folder is missing. A project's tree stays as it is.                                                                                                                                                                                     | Deleting a folder's row would trash the whole folder (`delete_path` has no guard, `filesystem/entries.rs:437-455`), and Mod+Backspace deletes the selected rows (`App.tsx:1762-1765`). Moving one into another would nest them (D5).                                                                                                                                               |
| D10 | **Git, per repository.** Each folder's repository is the one it's in, found upward as today; a folder without one says so. There is one entry per repository, by its git folder (two folders in one repository share it; a linked worktree has its own), listing only the changes in the window's folders. With one repository the panel is as today. With several, each has a section (its branch with ahead and behind, Pull, History, stash and hooks in its header), and one message box commits every included repository's changes, each on the branch it's on. | The user's requirement. Hooks, merges and branches are already per repository (`git/mod.rs:41-43`). A different message per repository is one commit per repository, the others unticked. Repositories inside a folder aren't searched for: to commit in several repositories, add each as a folder.                                                                               |
| D11 | **A commit across repositories runs every checking hook first:** pre-commit in each repository, then the message hooks in each, and only then the commits. A hook that fails stops the commit everywhere, before any is written. A commit that fails after others were written is reported by repository, and the message stays.                                                                                                                                                                                                                                      | All or nothing as far as git can do it. Each repository's hooks still run in git's order.                                                                                                                                                                                                                                                                                          |
| D12 | **A commit never takes changes outside its folders.** If a repository has changes staged outside the window's folders (a terminal's `git add` beside them), its commit is refused before anything is staged or run, naming them. An untracked folder with a `.git` of its own is never staged by the repository around it.                                                                                                                                                                                                                                            | Today a folder of a repository commits whatever a terminal staged beside it, and a repository inside the folder makes Commit fail (section 12). Refusing keeps the hooks seeing what's committed, as today.                                                                                                                                                                        |
| D13 | **Search covers every folder**, its results grouped by folder. A folder chat's `@` lists its folder's files; an All folders chat's lists every folder's, under their names.                                                                                                                                                                                                                                                                                                                                                                                           | A folder chat can't read the other folders.                                                                                                                                                                                                                                                                                                                                        |
| D14 | **The editor's services take the window's folders.** IntelliSense loads every folder, and the active file's folder decides the compiler options, as the active file's config already decides them (`intellisense/activeConfig.ts:1-21`). rust-analyzer gets every folder as a workspace folder. The formatter, the Markdown preview and the diagnostics the agent reads take the file's own folder.                                                                                                                                                                   | One program and one rust-analyzer per window are what the app has. One folder's options for every file would give the others false errors, which reach the agent's diagnostics (`lib/diagnostics.ts:84-93`).                                                                                                                                                                       |
| D15 | **Terminals are the window's.** In a workspace, **+** asks which folder (the same folder list as New chat), as it already asks which host once one is connected; a new terminal opened otherwise starts in the active folder.                                                                                                                                                                                                                                                                                                                                         | No new kind of menu, and the terminal panel already keeps each project's terminals apart (`TerminalPanel.tsx:88-104`).                                                                                                                                                                                                                                                             |
| D16 | **Settings stay per folder.** Project checks and Restricted files are kept per folder, as now; in a workspace their pages have a folder picker. An All folders chat follows each folder's, for that folder's files.                                                                                                                                                                                                                                                                                                                                                   | Checks and secrets are a folder's: the same file is restricted whichever chat touches it.                                                                                                                                                                                                                                                                                          |
| D17 | **What's saved:** the window's workspace in `last-project.json`; its tabs, open folders in the tree and terminal panel in `project-sessions` under `workspace:<id>`. An update's restart reopens it (`--workspace <id>`).                                                                                                                                                                                                                                                                                                                                             | As for a project.                                                                                                                                                                                                                                                                                                                                                                  |
| D18 | **Names in the code.** "Workspace" is the user's word, and the name of the code that stores and opens one. Code that takes several folders (tree, watcher, search, git, the overlay's `Folders`) says folders and repositories, and treats a project as a list of one. Project checks' "Workspaces (N packages)" heading becomes "Packages (N)".                                                                                                                                                                                                                      | "Workspace" already means package-manager workspaces (`project_profile/workspaces.rs`, the Project checks page), Codex's `workspace-write`, the LSP's workspace folders and Cargo's workspace.                                                                                                                                                                                     |
| D19 | **Removing or locating a folder** is refused while an All folders chat is at work or has changes in that folder that aren't landed, naming the chats. Otherwise it closes the folder's tabs (asking about unsaved ones); its own chats at work keep running, its claim kept until they finish, as when a window leaves a project for another. Its terminals keep running.                                                                                                                                                                                             | An All folders chat's changes in a folder that left would have nowhere to land. A folder chat's changes land in its own folder whatever the window has open.                                                                                                                                                                                                                       |
| D20 | **A missing folder** (deleted, or a drive or share that isn't mounted) shows as missing, with **Locate…**, **Try again** and **Remove from workspace**; the rest of the workspace opens. An All folders chat doesn't start a run while one is missing, and says which.                                                                                                                                                                                                                                                                                                | One folder gone mustn't close the others; an All folders chat's view needs every folder.                                                                                                                                                                                                                                                                                           |
| D21 | **No feature flag, and each merge safe to ship.** Every merge to master is released (`release.yml:1`). The menu items come last (milestone H): until then no window has more than one folder, nothing offers All folders, and every task keeps a project as it is, but for the bugs it fixes and two changes a project benefits from too (C03's tab titles, C05's heading).                                                                                                                                                                                           | AGENTS.md: no speculative options. Nothing reaches a user half built.                                                                                                                                                                                                                                                                                                              |

### All folders chats

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Why                                                                                                                                                                                                             |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D22 | **One view of every folder.** An All folders chat's run works in one folder, mounted for the chat, whose top-level folders are the workspace's folders by name, each backed by that folder's own overlay. The view's top level is read-only; moving a file from one folder to another is a copy and a delete (EXDEV), as across file systems. A chat's folders share its one 512 MiB budget.                                                                                | The run keeps one root: every tool, the repository map, write scopes, plans' `Files:` and subagents work as they do. Each folder keeps its own overlay, so its ignore rules, conflicts and landing are its own. |
| D23 | **Always isolated.** An All folders chat opens its view for every run, Plan mode included, and needs the virtual-files driver for it, as the agent-managed context does today. Without the driver, or with isolation turned off, it doesn't start, and the dropdown shows **All folders** greyed with the reason and a way to the driver's settings.                                                                                                                        | There is no single folder to fall back to working in.                                                                                                                                                           |
| D24 | **Lands all or nothing across folders.** One landing writes every folder's changes under one journal; a conflict in any folder lands nothing. **Undo** puts back every folder.                                                                                                                                                                                                                                                                                              | One change across an API and its client is one change.                                                                                                                                                          |
| D25 | **Each folder speaks for itself.** The agent is told it works in a workspace and how paths are made; each folder's instructions (`AGENTS.md`…), stack and checks are given under its name, with one shared limit on the instructions (12,000 characters). Each folder's restricted files apply under its name.                                                                                                                                                              | A folder's rules are for its files; N full instruction files on every request would crowd the context.                                                                                                          |
| D26 | **Commands run at the view's top** (`cd api && npm test`). Under bubblewrap each folder's view also shows at the folder's real path; where commands can't be redirected (Seatbelt, Landlock), a command naming any folder's real path is refused by text, as one folder's is today.                                                                                                                                                                                         | As an isolated chat's commands work now, for every folder.                                                                                                                                                      |
| D27 | **Checks: those of the folders the chat changed**, each run in its folder; a failure that was there before the change is set apart per folder, as today.                                                                                                                                                                                                                                                                                                                    | Checks are a folder's setting. Unchanged folders' slow suites stay out of every round.                                                                                                                          |
| D28 | **Plans go in the workspace's first folder**, in its `.rovik/plans`, as a project's do; **Implement** on a plan whose `Files:` name the workspace's folders starts an All folders chat.                                                                                                                                                                                                                                                                                     | A plan in a folder lands, syncs to WSL and hosts and shows in the tree like any other file, with no special place to keep.                                                                                      |
| D29 | **The branch offer covers every repository** on its main branch: one question names them all, and one branch name goes to each.                                                                                                                                                                                                                                                                                                                                             | One task, one branch name across its repositories.                                                                                                                                                              |
| D30 | **In WSL and on hosts**, an All folders chat is carried as one project: each folder's files under its name, in one snapshot, one push and one worktree. The chat's results come back split by folder and land through each folder's overlay. There, git shows Rovik's snapshots, not the folders' history, and Autopilot keeps its results for the app to land rather than pushing a branch. A Rovik there without the feature refuses the chat, with the upgrade it needs. | On Windows every chat runs in WSL, so All folders chats need this to exist there. One project there keeps the daemon and the protocol as they are, but for a list of folders.                                   |
| D31 | **Connectors work in the view.** Claude Code and Codex run with the view as their folder, so they need no extra directories; the workspace's section of the prompt is given to them, as they'd find no `AGENTS.md` at the view's top.                                                                                                                                                                                                                                       | Every folder is under their working folder; a chat keeps its view's path, so their sessions keep going.                                                                                                         |
| D32 | **An empty chat can change folder**, by New chat's dropdown (which reuses it); a chat with messages never does. To work across folders later, start an All folders chat.                                                                                                                                                                                                                                                                                                    | A chat's history names paths from its folder (`src/x.ts` against `api/src/x.ts`): moving it would make them wrong.                                                                                              |

---

## 2. What the code does today

### One folder, everywhere

- `rootPath` (`src/App.tsx:159`) is the window's one folder. It feeds the
  tree's controller (`:304`), the watcher (`:1979-1987`), search (`:2175`),
  the chat panel (`:2233-2240`), the Git panel (`:2249`), the editor
  (`:2327`), the terminals (`:2346`), the Settings dialog (`:2388`) and
  what's saved (`:1929-1977`). It's only ever set to a folder (`:199`,
  `:520`). Every run's `root` is it (`ChatPanel.tsx:3057`, `:3097`).
- Switching projects (`openProjectAt`, `App.tsx:485-506`) checks the claim, settles
  unsaved tabs, saves the session, claims, then resets the tabs, the git
  marks, the tree's selection and Monaco's models. The tree, the watcher,
  rust-analyzer and IntelliSense follow `rootPath`.
- What's saved is keyed by the folder's path: `last-project.json` (one path
  for the app, `filesystem/mod.rs:443-505`), `project-sessions` (tabs, open
  folders, terminal panel; `lib/projectSessions.ts`), `project-settings.json`
  (checks and restricted files; `project_profile.rs:682-715`). Terminals and
  commit drafts are kept in memory by folder.
- Every merge to master is released (`release.yml:1`).

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
  each open, `reclaim` claims the folder it was given again
  (`commands/chat.rs:63-71`). The window's one project (`opened`,
  `instances.rs:217-219`) decides whether a switch keeps the old claim
  (`project_claim`, `lib.rs:588-605`).
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
  (`FileContextMenu.tsx:58-148`), the keyboard has the same
  (`App.tsx:1744-1766`), and any row can be dragged (`FileTree.tsx:57-72`).
  Creating, renaming, pasting and deleting rely on the watcher to refresh
  the tree.
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
- Paths are compared as text, case-sensitively, in three copies of
  `isAtOrUnder` (`FileTree.tsx:79-83`, `fileTree.ts:169-173`,
  `App.tsx:1046-1050`) and three of a path's parent (`fileTree.ts:197-202`,
  `App.tsx:1631-1636`, `NewProjectDialog.tsx:30`).

### Git

- Each of the 35 commands takes the project's folder as `path`. The
  repository is found upward from it (`open_repo`, `git/mod.rs:41-43`), and
  file paths are relative to the repository's working folder. A project in a
  repository's subfolder is scoped to it (`project_scope`, `:107-125`): its
  list of changes, Commit's staging and Discard all. libgit2 matches such a
  scope at folder boundaries: `app` doesn't take `app2/` (checked with the
  app's git2 0.21).
- `git_commit` stages the scope, runs the hooks, and commits the whole
  index (`git/changes.rs:14-83`).
- GitPanel is one component for one repository: 2794 lines, 34 uses of
  `props.projectPath`. Its commit button commits, then pushes when the
  branch is ahead, or asks to publish a branch with no remote branch yet
  (`commitAndMaybePush`, `GitPanel.tsx:819-861`; `lib/gitPublish.ts`).
- Commands that rewrite the working tree hold the isolated chats of their
  one `path` (`rewriting`, `util.rs:45-53`; `hold_busy`,
  `overlay/chats.rs:1324-1340`).
- One `GIT_LOG_LATEST` generation stops older history scans for the whole
  process (`git/history.rs:100-109`). ConflictResolver keys its state by the
  project's path (`ConflictResolver.tsx:79-91`).
- A new chat's branch offer asks `git_head` of the window's folder
  (`lib/chatBranch.ts:63-85`).

### Chats

- The chat store shows one project's chats (`chatStore.ts:271-293`):
  switching empties the list and clears the open chat, and ChatPanel resets
  its composer and opens the newest chat when its `projectId` changes
  (`ChatPanel.tsx:1015-1082`).
- A chat's project is stored when the chat is made (`chatStore.ts:571-572`).
  Two buttons make one: ＋ in the chat's header and **New chat** in the chat
  list (`ChatPanel.tsx:4438`, `:4450`); either reuses an empty open chat
  (`:1160-1175`). With no chat to show, the panel makes an empty one itself
  (`:1029-1034`, `:1270`). `set_dialog_project` (`chat_db/store.rs:504`)
  can move a chat to another project.
- The editor context sent with a message names the active file by its
  absolute path (`activeContext.ts:71-83`).

### The agent

- A run has one `root` (where it works: the overlay's mount in an isolated
  chat) and one `project_root`, the real folder (`agent_run/run.rs:149-152`),
  used in about 35 places in 11 files: settings, plans, the code before the
  change, the busy waits. `Run::isolated()` is `root != project_root`
  (`repair.rs:843-846`). Tools resolve paths within the root
  (`resolve_within_root`, `agent_tools.rs:110-133`); the model never sees
  the absolute path.
- A failed mount, or Plan mode with no changes, works directly in the
  project (`run.rs:751-806`).
- The project's context is one folder's: its instructions file (the first
  of `AGENTS.md`, `CLAUDE.md`…, 6000 characters at most), stack, checks,
  settings and restricted files (`load_for`, `agent_run/project.rs:168-198`;
  `project_profile.rs:643-670`). Autopilot's V09 already runs checks of
  folders one level down as `cd <folder> && …` (`project.rs:200-227`).
- Checks run in the root (`run_checks`, `verify.rs:1610, 1685-1686`); a check that fails runs
  again on the code before the change, in a scratch overlay of the project
  (`before.rs:186-275`).
- Plans are saved in `project_root`'s `.rovik/plans`, also in an isolated
  chat (`execute.rs:2087-2139`).
- Commands run in the root (`command_tool.rs:632-655`). bubblewrap binds
  the chat's view at the project's path; Seatbelt and Landlock can't, so a
  command naming the project's full path is refused by text
  (`overlay/confine.rs:71-105, 906-911`; `overlay/chats.rs:80-99`).
- Connectors run in the root (`run.rs:2259`), and are given no project
  section in their own-tools mode: they read `CLAUDE.md`/`AGENTS.md` from
  their working folder (`connectors/claude_code.rs:276-278`).

### The overlay

- The view is one `OverlayFs` over one folder (`overlay/fs.rs:430-509`),
  mounted per chat (`overlay/chats.rs:352-392`) through the inode-based
  `vfs::backend::Backend` (`vfs/backend.rs:15-67`). Its store is
  `<data>/overlays/<chat>/`, bound to the folder by `project.txt`
  (`chats.rs:428-440`).
- Landing plans a three-way merge per file, locks the project
  (`PROJECT_LOCKS`, `land.rs:458-468`) and its files, and writes under a
  journal, with `project.txt` per chat (`land.rs:565-625, 884-959`). A
  landing can be undone (`:1156-1222`).
- Each chat's clashes with other chats on the same project are kept by chat
  (`refresh_project_peers`, `chats.rs:1565-1650`).
- Windows has no driver: a project's chat runs in WSL (`vfs/mod.rs:108-121`;
  `rovik-client/src/remote/moving.rs:275-297`).

### WSL and hosts

- A chat there works in a worktree of its project's repository on the
  daemon, `work/<chat>` (`rovik-agentd/src/work.rs:200-234`). The client
  snapshots the folder into the project's shadow repository, pushes it, and
  sends `WorkSync` before each message (`rovik-client/src/remote/sync.rs`);
  the daemon refuses a chat in another project (`work.rs:162-163`).
- Results come back as commits, imported into the chat's overlay
  (`remote/results.rs:250-319`; `overlay/import.rs:103-410`) and landed
  while the project is open. Delivery pushes a branch to the project's
  remote (`rovik-agentd/src/results.rs:164-240`).
- Features are negotiated (`rovik-protocol/src/lib.rs:66-68`); an older
  host is offered an upgrade (`moving.rs:753-784`).

### The editor's services

- The TypeScript worker holds one project (`intellisense/index.ts:49-62`);
  the active file's config decides its options and files, and the worker
  restarts only when the options differ (`intellisense/activeConfig.ts`).
- rust-analyzer gets one workspace folder and restarts when the root changes
  (`lsp/client.ts:117-142`, `lsp/rustAnalyzer.ts:90-107`). The diagnostics
  the agent reads are kept for one root (`lib/diagnostics.ts:15`).
- The formatter looks for its config between the file and the root
  (`formatter.rs:408-430`). `.editorconfig` doesn't depend on the root.

---

## 3. How workspaces work

### 3.1 What's stored

Schema version 11 (`chat_db/schema.rs`):

```sql
ALTER TABLE projects ADD COLUMN kind TEXT NOT NULL DEFAULT 'folder'
  CHECK (kind IN ('folder', 'workspace'));
CREATE TABLE workspace_folders (
  project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
  key        TEXT NOT NULL,   -- path_key: what folders are compared by
  name       TEXT NOT NULL,   -- stable: the chats' `<name>/<path>`
  path       TEXT NOT NULL,   -- as opened: shown, and its settings' key
  position   INTEGER NOT NULL,
  removed_at INTEGER,         -- set: no longer a member; its name stays taken
  PRIMARY KEY (project_id, key)
) STRICT;
CREATE UNIQUE INDEX workspace_folders_by_name
  ON workspace_folders (project_id, name COLLATE NOCASE);
CREATE TRIGGER project_paths_of_folders_only BEFORE INSERT ON project_paths
  WHEN (SELECT kind FROM projects WHERE id = NEW.project_id) = 'workspace'
BEGIN SELECT RAISE(ABORT, 'a workspace has no project folders'); END;
```

- A workspace's row in `projects` holds its name, an empty `path` and no git
  columns. Its All folders chats point at it by `dialogs.project_id`.
- `projects::list` and `summary` take `kind = 'folder'` only, so a
  workspace never shows as a missing project with **Relink**, and never
  reaches `project_of` as a folder.
- `chat_may_use` stays one rule, with no branch on the kind: a folder is
  one of the owner's folders if `project_paths` has it, or
  `workspace_folders` has it as a member (one `UNION ALL`, two key
  lookups).
- The trigger keeps `project_paths` for folder projects: a bug that sent a
  workspace's ID there fails instead of corrupting it.
- `schema.rs`'s two upgrade tests that assert version 10 (`:414`, `:506`)
  use `VERSION` instead, and `upgrades_a_version_10_database` is added.
- Once released, version 11 is permanent: an older Rovik refuses the
  database, and older windows still open stop writing to it until they're
  restarted (`chat_db/mod.rs:216-232`), as after every schema step. Its task
  (B01) merges once this plan is reviewed, so no later column means a
  version 12.

`chat_db/workspaces.rs` (new, beside `projects.rs`):

- `check_folder(members, folder) -> Option<FolderProblem>`: `Duplicate`,
  `Inside`, `Contains`, `SameProject` (its ID file names another member's
  project), `NotAFolder` (D5), each naming the other folder. It compares by
  `path_key` (`projects.rs:85-92`).
- `create(name, folders)`, `add_folders(id, folders)`,
  `remove_folder(id, name)` (refused for the last; refused while an All
  folders chat is at work or has changes there, D19),
  `relocate_folder(id, name, to)` (Locate…, same guard), `opened(id)`,
  `list()` (most recently opened first, each folder with `missing`), and
  `get(id)`. `create` and `add_folders` check and name folders inside their
  write transaction, so D5 and D8 hold across windows.
- A workspace's name is trimmed, not empty, at most 80 characters, and
  needn't be unique.
- Deleting a workspace is `chat_delete_project` with its ID, as for a
  project (`chat_db/api.rs:107-123, 187-231`): its own chats are stopped and
  deleted with their overlays, and its folders' projects and chats stay.

**Folder names** (D8): `folder_name(folder, taken)`, a pure function:

1. The folder's last part as opened (`D:\` gives `D`).
2. Made portable: `<>:"/\|?*` and control characters become `-`; spaces and
   trailing dots trimmed; leading `.` and `@` dropped (the view's top keeps
   `@context` for the agent-managed context); a Windows device name (`CON`,
   `NUL`, `COM1`…) gets `_` in front; at most 64 characters; empty becomes
   `folder`.
3. A name the agent's walkers skip (`build`, `dist`, `target`, `vendor`,
   `node_modules`…, `agent_tools.rs:31-47`) is taken as taken, so it gets
   its parent's name: `app-build`.
4. Unique whatever its case among every name the workspace ever gave,
   removed folders' included: the base, then `<parent>-<base>`, then
   `<base>-2`, `-3`.

The file-name check this needs moves from
`src-tauri/src/filesystem/entries.rs:84-147` into
`rovik-core/src/file_name.rs`, so both sides share it, with the Windows
device names it lacks (`COM0`, `LPT0`, `CONIN$`, `CONOUT$`).

Commands (`src-tauri/src/commands/workspace.rs`, new): `workspace_list`,
`workspace_create`, `workspace_add_folders`, `workspace_remove_folder`,
`workspace_relocate_folder`, `workspace_check_folder`. `src/lib/chatApi.ts`
wraps them, beside the project calls. `DialogSummary` gains the chat's
`projectId` (`chat_db/store.rs:49-59`, serialize only), so the UI knows a
chat's folder.

### 3.2 The window's folders

`src/lib/openFolders.ts` (new) says what the window has open:

```ts
/** What a window has open: a folder (a project), or a workspace. */
export type WindowTarget =
  | { kind: 'folder'; path: string }
  | { kind: 'workspace'; id: string; name: string; folders: WorkspaceFolder[] };

/** One folder the window has open. */
export interface OpenFolder {
  path: string;
  /** Its stored name in the workspace (D8); a project's folder name. */
  name: string;
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
- A chat's paths go through one pair of functions, for both kinds of chat:
  `chatPathToAbsolute(chatRoot, path)` (a folder chat's `src/x.ts` is joined
  to its folder; an All folders chat's `api/src/x.ts` is found in the folder
  named `api`; an unknown name names no file) and `chatPathOf(chatRoot,
abs)` (the reverse, with `/` separators; null outside the chat's folders).
  `chatRootOf(projectId)` gives a chat's root: its folder, every folder for
  the workspace's project, or none when the folder has left the window.
  `runPlace(chatRoot)` gives what a run is sent: `{ root }`, or `{ root:
null, folders }`, refusing while a folder is missing (D20).
- Code that works on folders takes `openFolders()` (a project is a list of
  one) or `folderOf(path)`. Only the menu, the dialog, the folder rows,
  Remove, Locate and Try again, and these chat functions look at the kind.

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
5. Each folder that's there is resolved (`chat_open_project`, all at once).
   A missing folder isn't (it would record a project for a path that isn't
   there, `projects.rs:445-465`); it's tried again when the window comes to
   the front, and by its row's **Try again**.
6. `workspaces.opened(id)`.

A folder that looks like a copy, or like a project that moved, is asked
about as when it's opened alone, in a notification of its own that names
it (each folder has its own notification id, and its own count of runs so
that resolving one folder doesn't drop another's answer, in place of
`projects.ts`'s one of each, `:57-61, 95, 124`).
When the original of a copy is a member, the question offers only
**Separate history** (D5).

Creating a workspace from the open project: the folder's keys are part of
the new target's, so they're never let go. Its tabs stay, and its
terminals and commit message move to the workspace's `sessionKey` (D4).

Adding folders: each folder picked is checked (`workspace_check_folder`),
then `project_claim` takes the target with it. A folder another window has
is refused and that window comes to the front; the others are added (and
named, D8), then resolved and shown.

Removing a folder: refused while an All folders chat is at work or has
changes there (D19). Otherwise its tabs are settled, `project_claim` takes
the target without it (its keys kept while its own chats are at work here),
and `remove_folder` marks it removed.

Claims (`instances.rs`):

- `Instances` keeps the window's target, which only `project_claim` sets,
  and a record per owner: each folder's project ID once resolved, the
  workspace's ID from the start, and whether its chats are at work.
  `opened`, `AtWork` and the cloud client's open owners come from that
  record.
- `claim_keys(target)` runs under the claims' lock, without duplicates:
  `no-project` with nothing open; a folder's `folder:<key>` and
  `project:<id>`; a workspace's `project:<its id>` and each folder's two.
  `project_claim` reads a workspace's folders from the database.
- `reclaim(root, id)` (after `chat_open_project` and a copy or move answer)
  records that folder's project ID, and claims the stored target again only
  if `root` is still in it. So two folders resolved at once both keep their
  keys, and an answer that comes late, for a folder the window has left,
  claims nothing back (today it would claim that folder and let go of the
  new one until the new one's answer came).
- Leaving a workspace whose own chats are at work keeps its key and every
  folder's (an All folders run uses them all); leaving one whose folder
  chats are at work keeps those folders'. `at_work_in` (`lib.rs:538-547`)
  takes the owners.
- Each key's holder file names its own folder, and the workspace's name
  (`workspace`, with a serde default), so a notice reads "api is open in
  another window, in Shop", or "Shop is open in another window".
  `elsewhere()` reports a copy only for a holder with a folder
  (`instances.rs:266`): another window's workspace isn't "a copy sharing its
  chats".
- `project_claim` and `project_check` take the target (`{ folder }`,
  `{ workspace }` or none); `lib/instances.ts` wraps them, and
  `openElsewhereNotice` words a workspace.

Starting and restarting:

- `last-project.json` gains `workspace` (serde default); the restore claims
  it with the retries a project gets (`App.tsx:371-388`).
- `rovik-ide <folder>` works as today. `--workspace <id>` opens a workspace:
  an update's restart passes it (`restart_args`, `updates.rs:404-417`). Its
  `project:<id>` key is claimed before the window exists, as a command
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
  (not on the open one) asks "Delete the workspace Shop? Its 4 chats of all
  folders are deleted for good. Its folders, and their own chats, aren't
  touched."
- The list hides **Delete** for every project the window has open, not only
  for one (`ProjectList.tsx:57`), and `chat_delete_project` refuses a
  project this window has open, as it refuses one another window has.
  **Relink…** on the project of a workspace's missing folder does what
  **Locate…** does.
- The filter matches workspaces' names and their folders' paths.

`NewWorkspaceDialog.tsx` (new, built as `NewProjectDialog.tsx` is):

- **Name**: the folders' names joined ("api + web") until the user types
  one.
- **Folders**: a list, each with ✕, the open folder first. **Add folder…**
  opens the picker, several at once; a field beside it takes a folder's
  path, typed or pasted, as New project's location field does.
- Each folder is checked as it's added (`workspace_check_folder`, and
  `project_check` for another window's): the reason shows on its row, and
  **Create** stays off while one does.
- **Create**: `workspace_create`, then `switchTarget`.

### 3.5 The tree, files, the watcher

The tree (`lib/fileTree.ts`, `FileTree.tsx`, `App.tsx`):

- The controller takes the roots and whether they're rows. Each folder's
  listing uses that folder as its root. Rows are keyed by folder and file
  id, so a link in one folder to another
  (`web/node_modules/@shop/api → ../../api`) gives rows of its own.
- A folder's row: folder icon, name, the path as its tooltip. A missing
  folder's row is greyed, says "Folder not found", and offers **Locate…**,
  **Try again** and **Remove from workspace**.
- A folder's row's menu: New file, New folder, Paste, Remove from
  workspace. The menu, the keyboard (`App.tsx:1744-1766`) and dragging
  (`FileTree.tsx:57-72`) never rename, delete, cut, copy or move a folder's
  row. In the backend, `delete_path`, `rename_path` and `move_path` refuse
  a folder the watcher watches as a window's (it has the list, below).
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
  that folder's ignore rules. A folder that can't be watched doesn't stop
  the others' watches (today one error ends the start, `watch.rs:429`).
- Paths are reported under the folder's own spelling, also where the system
  reports the resolved one (macOS's FSEvents gives `/private/tmp` for a
  folder opened as `/tmp`).
- `unknown` names its folder. `git-changed` names the folder it came from
  (`{ folder }`); the frontend knows which repository that is.
- The git folder of a folder's repository, when it's outside the folder
  (`repo.path()`: a folder inside a repository, a linked worktree), is
  watched too: itself without recursion (`HEAD`, `index`, `ORIG_HEAD`,
  `FETCH_HEAD`, `packed-refs`), and its `refs/` (the common one's, for a
  worktree). A commit, a checkout or a push from a terminal then refreshes
  the Git panel. A HEAD that moves makes the repository's working folder
  busy (`project_busy`), which holds the isolated chats of every folder in
  it (3.7).
- Changes in a folder reach the overlays of every chat on it: the folder's
  own chats, and the All folders chats' parts for that folder (4.2).
- The tree's gitignore matcher becomes a map by folder.

### 3.6 Search, `@`, tabs and the editor's services

- **Search** (`SearchPanel.tsx`): `search_in_files` runs for each folder at
  once. With several folders, results come under a header per folder; with
  one, as today. Collapsed state is kept by folder and path, and a result
  opens from its folder. Include and exclude globs are relative to each
  folder. The result limit is for the whole search.
- **`@`** in a chat: a folder chat lists its folder's files
  (`list_project_files`); an All folders chat lists every folder's, under
  their names (`api/src/x.ts`).
- **Tabs** (`Tabs.tsx`): when names collide, each title gets the shortest end
  of its parent path that tells them apart; in a workspace that starts with
  the folder's name ("package.json · api", "package.json · web"). A project
  benefits too (two `index.ts`).
- **IntelliSense** loads every open folder: definitions from each
  (`load_project_definitions`), `owns` true in any folder, within today's
  limits, shared by the folders. Each folder's tsconfigs are merged as a
  project's are today (`lib/tsconfig.ts:14-19`), and `ActiveConfig` uses the
  active file's folder's options: a TSX file in `web` gets `web`'s `jsx` and
  `lib`. Moving between folders whose options differ restarts the worker, as
  moving between configs does today.
- **rust-analyzer**: `initialize` lists every folder in `workspaceFolders`;
  the client answers the `workspace/workspaceFolders` request (it answers
  -32601 now, `lsp/client.ts:261-282`). When the folders change the server
  restarts, so `didChangeWorkspaceFolders` isn't needed.
- **Formatter**: `format_file(path, language, folderOf(path))`, so a folder
  that isn't a repository still finds its `.prettierrc` above the file.
- **Markdown preview**: a link starting with `/` resolves from the
  document's folder.
- **Diagnostics for the agent**: each marker is reported for its file's
  folder (`lib/diagnostics.ts`), and a run finds them through its folders
  (4.10).

### 3.7 Git

**The repositories** (`src-tauri/src/git/repos.rs`, new; command
`git_repos(folders)`):

```rust
/// The repository one or more of the window's folders are in.
pub struct Repo {
    /// Its canonical git folder (`Repository::path()`): two folders in one
    /// repository share it; a linked worktree has its own.
    pub key: String,
    pub workdir: String,
    /// The working folder's name, with its parent's when two clash.
    pub name: String,
    /// The window's folders in it, relative to `workdir` ("" when a folder
    /// is the working folder: all of it).
    pub scopes: Vec<String>,
}
```

- Each folder's repository is found upward from it (`discover`, as today),
  and the results are put together by `key`. Folders in no repository come
  back as such; each has a section offering **Initialize repository**, as
  the panel does today with no repository.
- Found again when the folders change, and when a folder's `git-changed`
  doesn't match its repository (a `git init` in a folder that had none).

**Scopes.**

- `git_status`, `git_commit` and `git_discard_all` take `scopes` (by
  default the folder in `path`, as today) instead of computing one
  `project_scope`. They're matched as git matches pathspecs (`app` doesn't
  take `app2/`), never as text. The line counts take the same scopes
  (today they diff the whole repository, `git/mod.rs:262-309`).
- A command that rewrites the working tree holds the repository's working
  folder (`rewriting`), and `is_busy(folder)` counts a hold on that folder,
  on a folder above it, or on one below it (today only on the same folder,
  `overlay/chats.rs:1294-1340`). So a checkout holds the isolated chats of
  every folder in the repository, those left at work included, with no list.

**What a commit takes (D12).**

- Before anything is staged, the index is compared with HEAD. An entry that
  differs outside the scopes (`git2::Pathspec`) refuses the commit, naming
  it: "api has changes staged outside this window's folders: app2/x.ts.
  Commit or unstage them in a terminal first." What's staged stays staged.
- Otherwise `stage_all` stages the scopes, the hooks run and the index is
  committed, as today.
- Status leaves out an untracked folder with a `.git` inside (a repository
  of its own), and a submodule whose only change is inside its own working
  tree. Revert refuses both.

**The panel.**

- `GitPanel.tsx` keeps what's the window's: the list of repositories, the
  message box and the commit button. A new `GitRepoSection.tsx` takes what's
  one repository's: its status, header, banner, error, changes, history,
  stash and hooks. It's keyed by `Repo.key` and given the repository, not
  `projectPath`.
- One repository: no section header; the panel looks and works as today.
- A section's header: the name; the branch button (↑ahead ↓behind) with its
  menu; Fetch, Pull, History and ⋯. It also has an "include in commit" box:
  on when the repository has changes or commits to push, and off, with why,
  during a merge, a rebase or conflicts ("Finish the merge in api first").
- `commitAction(statuses, message)` (`gitPublish.ts`): with one repository,
  exactly today's. With several, over the included ones: Commit when one has
  changes and there's a message, else Push when one is ahead, else Publish
  when one can be published.
- Commit (`git_commit_many` in the backend; `git_commit` is the same for
  one repository), holding each included repository's working folder for
  the whole run:
  1. Every included repository is checked: nothing in progress, no
     conflicts, nothing staged outside the scopes. Otherwise nothing
     happens, and every problem is named.
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

- Then the repositories committed in that click are pushed, as today (a
  repository that was only ahead isn't: the button offers Push for it once
  nothing's left to commit). Branches with no remote branch get one
  question for all ("Publish api's feature-x and web's feature-x?"). A push
  that fails shows in its section; the others go on.
- A folder's `git-changed` refreshes its repository's section, at most every
  2 seconds each.
- Commit drafts are kept by `sessionKey`, publish answers by repository and
  branch, `GIT_LOG_LATEST` by repository, and ConflictResolver's `opKey` with
  the repository.

**Change marks in the editor** (fixes section 12, item 1). `loadGitBase`,
`refreshGitBases` and the stash and commit previews find the file's
repository among `git_repos`' (the deepest working folder that holds it), and
send the path relative to its working folder. Tabs that aren't files on
disk (`disk:`, `chat:`) are left out of the refresh (section 12, item 8).

### 3.8 Chats

The chat list:

- `chat_db`'s `list_dialogs`, `search` and `unsettled_sends` take the
  window's owners: its folders' projects, and the workspace's own
  (`project_id IN (…)`). ChatPanel's `projectId` prop becomes `projectIds`
  (`ChatPanel.tsx:273, 1015-1082`).
- `chatStore.setProjects(ids)` keeps the open chat while its owner is still
  in the set: a folder resolving, added, removed or located doesn't move the
  user to another chat or clear the composer. The chat list resets as today
  only when the window's target changes.
- Unanswered sends left by a crash are recovered for each owner as it's
  added to the set (`recoverUnsettledSends`, `ChatPanel.tsx:2780-2788`), not
  only for the first.
- The unread dots are those of the window's owners. With several folders,
  each chat's row shows its folder's name, or "All folders".

New chat (D2):

- With several folders, **New chat** (both buttons) opens a dropdown:
  each folder's name, its path as a tooltip, a missing one listed but not
  clickable; then a separator and **All folders** (4.12). The chat is made
  in the one clicked; Escape or a click outside makes none. The arrow keys
  and Enter work too, starting on the active folder. With one folder
  there's no dropdown, as today.
- An empty open chat is reused, as today, moved to the owner clicked
  (`set_dialog_project`, `chat_db/store.rs:504`, refused once the chat has
  a message), so empty chats don't pile up (D32).
- With no chat to show (no chat yet, or the last one deleted), the panel
  lists the same choices to start a chat in, rather than making an empty
  chat in a folder it would have to guess.
- The message box shows the chat's folder beside its location
  (`ChatLocation.tsx`): "in api", "in all folders". It's a label, not a
  control.
- `FolderMenu.tsx` draws the folders as menu items, for New chat's
  dropdown, the panel's list and the terminal's **+** menu (3.9), with a
  slot for what a caller adds (New chat's **All folders**).

A folder chat:

- `chatRoot(dialog)` comes from `chatRootOf` (3.2). ChatPanel's uses of
  `props.rootPath()` go through it and `runPlace`: the run (`ChatPanel.tsx:3057`,
  `:3097`), the context estimate (`:724`, `:768`), `@` (`:922`, `:937`), the
  editor's selection reference (`:870`), the connector's commands (`:608`),
  the branch offer (`:3113`) and **Implement** (`:3150`). The "Open a
  folder" hint (`:4488`) is for a window with no folder.
- What the user sends names files as the chat does: the editor context, `@`
  and **Add to chat** give `chatPathOf` (today the editor context gives the
  absolute path, which an isolated chat's tools refuse: section 12). A file
  outside the chat's folders isn't sent; the composer says why: "web/src/x.ts
  is in web; this chat works in api."
- **Implement** on a plan in another folder starts a new chat in that
  folder; on a plan whose `Files:` name the workspace's folders, an All
  folders chat (D28).
- The isolation bar and the comparison tabs (`chat:<dialog>:<rel>`) resolve
  paths through `chatPathToAbsolute`.
- Runs don't change: `root` is the chat's folder, which `check_project`
  accepts. The branch offer asks about the chat's folder's repository. Plans
  go to its `.rovik/plans`. Project checks and restricted files are its
  folder's.

Cloud hosts and WSL, for folder chats:

- `State.open` becomes the window's open owners, from the claims' record.
  `handled_elsewhere`, `land_waiting`, `is_open` and `folder_answered` look
  among them; a chat's files come from `files_of(chat)` (4.11), which for a
  folder chat is today's `project_of`. A steering message's paths are made
  relative to its chat's own folder. The delivery test uses the first
  folder with an `origin` remote.
- Moving a folder chat, WSL and the snapshots don't change.

### 3.9 Terminals

- `TerminalPanel` groups terminals by `sessionKey(target)` instead of by
  `rootPath`. A terminal keeps the folder it started in (`cwd`).
- In a workspace, **+** opens its menu with the folders first
  (`FolderMenu`), then the hosts. A terminal opened otherwise (the panel
  opening empty) starts in the active folder. A tab's tooltip names its
  folder.

### 3.10 Settings

- `SettingsDialog` gets the open folders and the active one. With several,
  the two "This project" pages (Project checks, Restricted files) show a
  folder select (the active folder first) and work on the folder selected,
  as they work on the root today. An All folders chat has no settings of
  its own (D16).
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

## 4. All folders chats

### 4.1 What they are

- An All folders chat belongs to the workspace's project (D1). Its run is
  given the workspace's folders by name and path, and works in one view of
  them (D22): `api/src/x.ts`, `web/src/y.ts`. Paths in its tools, its
  history, its plans and its changes start with a folder's name.
- `StartRequest` (`rovik-protocol/src/agent.rs:230-258`) gains
  `folders: Vec<RunFolder>` with a serde default, and then has no `root`:

  ```rust
  /// One folder of an All folders chat's run.
  pub struct RunFolder {
      /// Its stored name (D8): the top-level folder of the run's view.
      pub name: String,
      /// Where it is. Never sent to a daemon, which has its own (4.11).
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub path: Option<String>,
      /// Its project settings, for a daemon (as `project_settings` is).
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub settings: Option<serde_json::Value>,
  }
  ```

  Autopilot's `Settings` (`autopilot/driver.rs:38-77`) and the context
  estimate (`agent.rs`, `chat_context_estimate`) carry `folders` too.

- `check_project` (`agent_run/mod.rs:1301-1333`) takes `root` or `folders`,
  never both. A workspace chat's `folders` must be exactly the workspace's
  members, by name, each passing `chat_may_use`; a missing folder is refused
  by name (D20); a project chat sends none. Thorough candidates keep their
  exception (`:1312-1317`).
- Folder chats are unchanged.

### 4.2 The view

`overlay/folders.rs` (new):

```rust
/// The folders a chat's view shows: one, named "" (a folder chat), or a
/// workspace's, each by its name.
pub struct Folder { pub name: String, pub path: PathBuf }
pub struct Folders(Vec<Folder>);
impl Folders {
    pub fn one(path: PathBuf) -> Self;
    pub fn named(list: Vec<Folder>) -> Result<Self, String>;
    /// `api/src/x.ts` → (api, `src/x.ts`); a folder chat's path as it is.
    pub fn split<'a>(&self, path: &'a str) -> Option<(&Folder, &'a str)>;
    /// The real file a chat path names.
    pub fn abs(&self, path: &str) -> Option<PathBuf>;
    /// The chat path of a real file.
    pub fn chat_path(&self, abs: &Path) -> Option<String>;
}
```

`overlay/composite.rs` (new): `CompositeFs`, a `vfs::backend::Backend`
(inode based, `vfs/backend.rs:15-67`) over the folders' `OverlayFs`s:

- **Inode numbers**: a folder's inode `n` is `(n << 16) | slot`, with a slot
  per folder never reused while mounted; the root stays 1 (`vfs/store.rs:22-23`).
  Every `Attr.ino` and `DirEntry.ino` is renumbered, so numbers stay below
  2^53 (Node reads them as doubles).
- **The root**: a folder of mode 0o555 listing each folder by its name;
  `lookup` takes the exact name, and the case folding `OverlayFs::find`
  does on macOS (`fs.rs:825-862`). Creating, removing or renaming at the
  root, or setting its attributes, is refused by the backend itself (root
  ignores 0o555).
- **Across folders**: `rename` and `link` between two folders answer EXDEV:
  a new `VfsError::CrossDevice` (`vfs/store.rs:39-52`, mapped in
  `fuse.rs:37-52`). `mv` copies and deletes, as across file systems; the
  chat's changes then show an add in one folder and a delete in the other.
- **Notifications**: each folder's listener renumbers what it tells the
  kernel and forwards it (`Listener`, `vfs/store.rs:137`); a new folder is a
  new slot and a new entry at the root, without remounting (the kernel keeps
  an entry a second and no listing, `fuse.rs:34, 498`).
- **`usage`**: the chat's budget, below.

`overlay/fs.rs`:

- **One budget per chat**: `limit`, `used` and `warned` move into an
  `Arc<Budget>` the folders share (about ten places today:
  `fs.rs:918-920, 1004-1024, 1217-1250, 1462-1475, 1536, 1747, 1785,
2256-2300, 2466`), so a chat keeps 512 MiB and one warning at 80%.
  `OverlayFs::open(base, store, limit)` keeps its signature, with a budget
  of its own.
- A `ChatFs` trait (`changes`, `versions`, `rebase_file`, `revert_file`,
  `flush`, `close_handles`, `used`, `by_product_bytes`, `generation`),
  implemented by `OverlayFs` and `CompositeFs`, which routes by the path's
  first part and names its changes from the view's root.

`overlay/chats.rs`:

- `ChatOverlay { fs: Arc<dyn ChatFs>, parts, view, store, … }`: a folder
  chat has one part, mounted directly, with its store and `project.txt` as
  today. An All folders chat's store is `overlays/<chat>/folders.json` and
  `overlays/<chat>/parts/<name>/` per folder; its chat-level files
  (`landing.json`, `markers.json`, …) hold chat paths. An older Rovik finds
  no `manifest.json` or `project.txt` there, and leaves it alone.
- `open_all(chat, &Folders, on_warn)` opens and mounts it; at each run's
  start it adds folders that joined. `folders_of(chat)` replaces
  `project_of`. `changes_in_folder(chat, folder)` answers Remove and Locate
  (D19).
- **Watcher and peers per folder**: `project_changed(folder, …)` reaches
  each open overlay's part for that folder, with the paths it notes and
  marks prefixed with the folder's name. Clashes between chats are kept per
  (chat, folder) and compared by in-folder path, so a folder chat and an All
  folders chat on the same folder see each other's clashes. Refreshing one
  folder no longer replaces the chat's whole row (`chats.rs:1635, 1646`).
- `is_busy` for an All folders chat is any of its folders busy; the busy
  wait before landing loops over them (`agent_run/landing.rs:481-507`).
- Child overlays (subagents' workers, Thorough candidates) stay one
  `OverlayFs` over the parent's view (`chats.rs:670-697`): their paths are
  the parent's chat paths, and their landing goes through the composite
  into the right folder. Their room comes from the shared budget.
- Scratch views (the before-run, the test writer's red check) take
  `&Folders`: a scratch of an All folders chat is a composite too
  (`open_scratch`, `chats.rs:700-712`).
- `project_for(view)` maps `view/<name>/…` to the folder
  (`chats.rs:495-502`, the workload's history).
- `CARGO_TARGET_DIR` is one folder per set of folders (`chats.rs:45-60`).

### 4.3 The run

`agent_run/folders.rs` (new) replaces `Run.project_root`:

- `Folders` (4.2) with `single()` (a folder chat's folder), `file(key)`
  (the real file a chat path names), `each()` and `plans_home()` (D28).
  `isolated()` becomes "the root isn't `single()`", so an All folders chat
  is always isolated.
- Each of today's uses moves to it, one folder giving today's behaviour:
  `run.rs:751-786, 1016-1036, 1166, 1233, 1381`; the code before a change
  (`repair.rs:895, 968, 1157`; `verify.rs:748-760`); landing
  (`landing.rs:134, 410, 456, 481-507`); the before-run and the red check
  (`before.rs:186-275`; `test_writer.rs:378-405`); plans
  (`execute.rs:2087-2139`; `run.rs:1001-1003`; `subagent_run.rs:1646`;
  `change_review.rs:1029`; `test_writer.rs:622`); Thorough candidates
  (`candidates.rs:789, 842, 869, 962, 1005, 1556`, a candidate's own
  `Settings` clearing `folders`).
- `prepare` opens the view for every run of an All folders chat, Plan mode
  included, and refuses without it: "An All folders chat needs the virtual
  files driver (Settings → Virtual files), or start a chat in one folder."
  The same with isolation off (`ROVIK_ISOLATE_CHATS=0`). It never falls back
  to working in a folder (`run.rs:775-805`).

### 4.4 What the agent is told

`agent_run/project.rs` gains `load_folders(config_dir, root, &folders, …)`;
with one folder it's today's `load_for`. For an All folders chat, per
folder:

- `detect(root/<name>)` on the view; its settings from
  `load_project_settings(config_dir, path)`; its checks as
  `in_folder(name, cmd)`; its stack as `"{stack} ({name}/)"`, V09's form
  (`project.rs:183-202`).
- Its instructions file, under its name, the folders sharing 12,000
  characters (D25). `project_context` (`project.rs:85-148`) is split into
  its checks part and its instructions part, so both builders share them.
- Its restricted files with `name/` in front (`restricted_files.rs:348-363`);
  `blocked` and the scan of the view work as they are.

The project section adds:

> This chat works in a workspace: each of its folders is a top-level folder
> of the project root (`api`, `web`), and every path starts with one
> (`api/src/main.rs`); nothing can be made at the root itself. Run a
> folder's commands with `cd api && …`. `api` and `web` are git
> repositories (`git -C web status`); the root isn't. A folder's checks and
> instructions are for its files.

The system prompt's lines and the tools' descriptions stay true: the
project root is the view's.

### 4.5 Commands and the sandbox

- Commands run at the view's top (`command_tool.rs:632-655`), background
  commands too.
- `Confine` (`overlay/confine.rs:71-105`) takes a list of redirects:
  bubblewrap binds each `<view>/<name>` at its folder's path (`:906-911`).
  `writable_dirs(view)` already covers every folder.
- Where commands can't be redirected (Seatbelt, Landlock), `command_problem`
  (`overlay/chats.rs:80-99`) refuses a command naming any folder's real
  path. It's applied to the connectors' commands always: they run
  unconfined (`connectors/harness.rs:272-290`), so a harness command could
  reach a folder's real path even where bubblewrap confines the agent's own.
- Installs after a failing `cd web && …` install that folder's
  dependencies, not every folder's (`environment.rs:423-441`).

### 4.6 Landing

`overlay/land.rs` works on `Folders` (D24):

- **Plan**: `plan(&Folders, changes, keep_chat)` merges each file against
  the folder its path names; a path that names no folder is a conflict. A
  conflict in any folder means nothing is written anywhere.
- **Locks**: each folder's `PROJECT_LOCKS` entry (`land.rs:458-468`) is
  taken in the order of its real path, then every file's
  (`file_guard::lock_all`, sorted). A folder chat takes one, as today, so
  no cycle is possible. Undo takes the same set in the same order.
- **One journal**: `Journal` gains `folders` (serde default), and its
  entries hold chat paths. `apply` (`land.rs:884-959`) writes them, rolling
  back every folder on a failure, and writes no `project.txt` for such a
  landing; recovery and undo read their folders from the journal, else from
  `project.txt` as today. An interrupted landing is finished at start, path
  by path; a folder missing then keeps its paths for the next start.
- After a landing, `project_changed` runs once per folder.
- The bar's payload (`src/lib/overlay.ts:14-81`) keeps its types: paths are
  chat paths (`api/src/x.ts`), and `used`/`limit` the chat's budget.
- Wrappers with today's signature (`Folders::one`) keep child landings and
  today's tests as they are.

### 4.7 Checks

- **Which**: the enabled checks of the folders the chat changed (D27): the
  first part of `touched_keys()` (`repair.rs:1133-1141`), command changes
  and earlier unlanded runs included, filters `enabled_checks()`
  (`verify.rs:601-607`). The prompt lists every folder's checks, so the
  agent can run the others itself.
- **Where**: from the root, as `cd api && cmd`; `run_checks`
  (`verify.rs:1610`) is unchanged. A folder's failed type check skips
  only that folder's tests (`verify.rs:1653-1667`).
- **Before the change**: the before-run uses a scratch view of every folder
  under the same names, so its output compares with the round's.
- **Output paths**: a folder's real path in a check's output is rewritten
  to `<view>/<name>`; a relative path printed under `cd api` resolves
  against that folder first (`command_output.rs:236-259`).
- File-to-command helpers that assume the root is a package
  (`js_runner(root)`, `project.rs:331, 402`: light checks, scoped tests, the
  breaker's runs, the test writer's red check) group files by folder, run
  in the folder, and wrap the result in `in_folder`.

### 4.8 Plans

- `plans_home()` is the workspace's first folder (D28): a plan is saved in
  its `.rovik/plans`, as `api/.rovik/plans/<date>-<stem>.md`, and lands,
  syncs and shows like any file there.
- Plans are shown by their chat path, found from their real path through
  `Folders` (`relative_plan_path`, `plan.rs:713-726`); `Files:` lines are
  checked against the view, so they name folders (`api/src/x.ts`).
- **Implement** (3.8) starts an All folders chat for a plan whose `Files:`
  name the workspace's folders.

### 4.9 Subagents, Thorough, Autopilot, the test writer

- Subagents share the run's root, folders, context, restricted files and
  map (`run.rs:1363-1386`); workers' write scopes are chat paths
  (`worker.rs:98-160`). Nothing else changes.
- Thorough candidates diff and adopt against the main view
  (`candidates.rs:148-175`); their changes are listed in 4.3.
- Autopilot carries `folders` in its `Settings`; its delivery reports what
  landing did, as for a folder chat here (`delivery.rs:63-75`). In WSL and
  on hosts it keeps its results for the app to land (D30).
- The test writer's red check runs a new test in its own folder (4.7); its
  `test_check()` names each changed folder's test check
  (`test_writer.rs:501-506`).

### 4.10 Connectors, diagnostics, the estimate

- Connectors run in the view (`run.rs:2259`); Claude Code's edit paths come
  out as chat paths (`claude_code.rs:1590-1593`), and its session resumes,
  the view's path being the chat's own. In their own-tools mode the
  workspace's section of the prompt is given to them
  (`--append-system-prompt`; Codex's developer instructions), as the view's
  top has no `AGENTS.md` (`claude_code.rs:276-278`; `codex/mod.rs:841`).
- The editor's diagnostics are found through the folders: `name/rest` is
  looked up as (that folder's path, `rest`), and a folder chat's key as
  (its folder, key) (`agent_tools.rs:433-435, 555-571`). This also fixes
  today's isolated chats, which never see them (section 12).
- The context estimate is given `folders`, and caches by the folder list
  (`project.rs:229-251`).

### 4.11 In WSL and on hosts

Carried as one project (D30). The protocol grows by fields, with a feature
to agree on them:

- `features::FOLDERS` (`rovik-protocol/src/lib.rs`), advertised by a daemon
  whose core runs All folders chats (`rovik-agentd/src/session.rs:30-37`).
- `Project.folders: Vec<ProjectFolder { label, settings }>` (serde
  default): a project whose top-level folders are those, with `subpath` "".
  `ProjectEnsure`, `WorkSync`, `WorkRecheck`, `WorkStatus`, `WorkFile`,
  `WorkLanded`, `ChatBundle`, results, moves, deletion and ownership don't
  change: their paths start with a folder's name.

The daemon (small):

- `ProjectState.folders` (`rovik-agentd/src/store.rs:52-69`), set by `ensure_project`, which
  checks each label is one plain name; `folder_roots()` gives each
  folder's root in the worktree, for its settings
  (`work.rs:226-232`; `daemon.rs:404-422`) and the run's `folders`.
- A result is unchecked if any folder has checks
  (`results.rs:298-303`); delivery of such a project fails, saying to land
  the results from the app (`results.rs:164-240`).

The client (`rovik-client`):

- `ChatFiles` (`remote/files.rs`, new) replaces `project_of`: the chat's
  owner, name and folders. A folder chat is one folder named "" and takes
  today's path unchanged: a full snapshot with history and `subpath`.
- **Snapshots**: each folder is snapshotted into its own shadow as today;
  a `Composite` shadow per workspace (`shadow/composite.rs`) commits one
  tree with each folder's tree under its name, borrowing the folder shadows'
  objects. One push and one `WorkSync` per message.
- **Ignore rules**: a folder inside a repository loses the `.gitignore`s
  above it in the composite, so `shadow/rules.rs` rebases them under the
  folder's name (global excludes, `info/exclude`, then each ancestor's
  `.gitignore`), or `node_modules` under `web/` would come back in a
  result.
- **Results**: the composite result is fetched once, then each folder's
  part is grafted into that folder's own shadow and imported into its part
  of the chat's overlay with today's `import` (taking the store it writes),
  so each repository's attributes (line ends) apply. A missing folder fails
  the whole import; it's never partial.
- **Landing**: the chat lands all folders or none (4.6), then reports one
  `WorkLanded` with every folder's paths.
- **Moves**: each folder's store is folded, then composed; the move dialog
  lists the folders and says that their git history stays here and the
  first push carries all their files. A missing folder blocks the move.
- **Start**: the folders' paths are made relative with their names
  (`remote.rs:1605-1649`), each folder's settings go with it, and a host
  without `FOLDERS` refuses the chat: a cloud host through the move's
  preflight (an upgrade for its administrators, `moving.rs:753-784`), WSL
  with "WSL runs an older Rovik, which can't run a chat across folders yet:
  update it from Settings → This computer → WSL."

### 4.12 In the UI

- **New chat**'s **All folders** item (3.8) is greyed, with the reason as
  its second line, when the chat couldn't run: no virtual-files driver here
  ("Needs FUSE: Settings → Virtual files"), on Windows WSL not ready or too
  old, or a folder missing. Clicking it opens that page
  (`ChatPanel.tsx:4842`, as the agent-managed context does), named once for
  both.
- The message box says "in all folders"; the chat's row says "All folders".
- `@`, the editor context, **Add to chat** and pasted references use
  `chatPathOf`: any folder's file goes in as `api/…` or `web/…`.
- The branch offer (D29): folders are grouped by repository (`git_repos`),
  each repository on its main branch is named in one question with one
  name, and a repository on another branch is said to stay there. Yes makes
  the branch in each in turn; one that fails says which have it, and Yes
  again makes it in the rest.
- The isolation bar lists the changed files by chat path, with one **Land**
  and one **Undo**; conflicts open in today's resolver.
- Removing or locating a folder that an All folders chat has changes in is
  refused, naming the chats (D19).

---

## 5. Edge cases

| Case                                                                                                          | What happens                                                                                                                                                                                  | Tested in                                                                 |
| ------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| **Folders**                                                                                                   |                                                                                                                                                                                               |                                                                           |
| The same folder twice, spelled differently (case on Windows and macOS, a trailing separator, a link to it)    | Refused: "api is already in the workspace".                                                                                                                                                   | `workspaces_tests.rs`; `NewWorkspaceDialog.test.tsx`                      |
| A folder inside another folder of the workspace, or containing one                                            | Refused: "web is inside api, which is in the workspace".                                                                                                                                      | `workspaces_tests.rs`; `NewWorkspaceDialog.test.tsx`                      |
| A copy of a member (`cp -r`, the same ID file), or a folder sharing a member's chats                          | Refused: "web-copy shares its chats with web, which is in the workspace". A member's copy question offers only **Separate history** when its original is a member.                            | `workspaces_tests.rs`; `instances.rs`: a target's keys have no duplicates |
| Two folders with the same name                                                                                | Named `api` and `server-api` for good; the tree, tabs, search, Git sections and chats use those.                                                                                              | `workspaces_tests.rs`                                                     |
| A folder named `build`, `CON` or `.config`, or a drive root                                                   | Named `app-build`, `_CON`, `config`, `D`: valid everywhere, and seen by the agent's walkers.                                                                                                  | `workspaces_tests.rs`; `file_name.rs` tests                               |
| A folder removed, then added again                                                                            | It gets its old name back; no other folder ever takes it.                                                                                                                                     | `workspaces_tests.rs`                                                     |
| A file, a folder that's gone, or one that can't be read                                                       | Refused, with the system's reason.                                                                                                                                                            | `workspaces_tests.rs`                                                     |
| A folder in several workspaces                                                                                | Allowed. Only one window can have it open at a time (D7).                                                                                                                                     | `instances.rs` tests                                                      |
| Adding a folder another window has open (alone or in a workspace)                                             | Refused; that window comes to the front. In the Create dialog, the reason shows on the folder's row.                                                                                          | `instances.rs` tests; `NewWorkspaceDialog.test.tsx`                       |
| Opening a workspace when another window has one of its folders                                                | Nothing opens; that window comes to the front, and the notice names the folder.                                                                                                               | `instances.rs` tests; `instances.test.ts`                                 |
| Opening a folder alone while a workspace in another window has it                                             | That window comes to the front: "api is open in another window, in Shop".                                                                                                                     | `instances.rs` tests; `instances.test.ts`                                 |
| Opening a workspace another window has                                                                        | That window comes to the front: "Shop is open in another window", not "a copy sharing its chats".                                                                                             | `instances.rs` tests                                                      |
| Switching from a workspace to one of its folders, or back                                                     | The shared folder's keys are held throughout.                                                                                                                                                 | `instances.rs` tests                                                      |
| Two folders resolved at once                                                                                  | Both keep their `project:` keys.                                                                                                                                                              | `instances.rs`                                                            |
| A folder resolved after the window moved on                                                                   | It claims nothing back.                                                                                                                                                                       | `instances.rs`                                                            |
| Leaving a workspace while its All folders chat is at work                                                     | The workspace's key and every folder's are kept until the chat finishes.                                                                                                                      | `instances.rs`                                                            |
| Leaving a workspace while two of its folders' chats are at work                                               | Both folders' keys are kept until their chats finish.                                                                                                                                         | `instances.rs`                                                            |
| Removing a folder whose own chats are at work                                                                 | They go on; the folder's keys are kept until they finish. Its chats leave the list (they're its folder's).                                                                                    | `instances.rs`; `chatStore.test.ts`                                       |
| Removing or locating a folder an All folders chat has changes in, or is at work in                            | Refused, naming the chat: "Fix the API has changes in api that aren't landed: land or discard them first."                                                                                    | `chat_db/tests.rs`; `overlay` tests (`changes_in_folder`)                 |
| Removing the last folder                                                                                      | Refused: "A workspace keeps at least one folder. Delete it instead."                                                                                                                          | `workspaces_tests.rs`                                                     |
| A folder missing when the workspace opens (deleted, a drive or share not mounted)                             | Its row shows it missing, with Locate…, Try again and Remove from workspace; the others open. It isn't resolved, its chats aren't listed, and All folders chats don't start, until it's back. | `workspaces_tests.rs`; `switchTarget.test.ts`; `openFolders.test.ts`      |
| A missing folder's drive mounted again                                                                        | Found as the window comes to the front, or with **Try again**.                                                                                                                                | `switchTarget.test.ts`                                                    |
| A folder deleted or renamed while open                                                                        | Its row turns missing; its tabs show [DELETED], as tabs do now.                                                                                                                               | `watch.rs` tests; `FileTree.test.tsx`                                     |
| A folder that can't be watched (unreadable, a share without events)                                           | The other folders are watched; its tree refreshes as its rows are expanded.                                                                                                                   | `watch.rs` tests                                                          |
| A folder opened through a link (macOS: `/tmp` is `/private/tmp`)                                              | Its changes are reported under the folder's own path.                                                                                                                                         | `watch.rs` tests                                                          |
| Deleting a workspace                                                                                          | Its own chats are deleted (the question says how many); its folders and their chats aren't touched. Refused while another window has it open.                                                 | `projects_tests.rs`; `ProjectList` test                                   |
| Deleting, or relinking, the project of one of the window's folders from the list                              | Delete isn't offered, and `chat_delete_project` refuses it. Relink on a missing folder's project does what Locate… does.                                                                      | `ProjectList` test; `chat_db` `api` tests                                 |
| A drive root (`D:\`) as a folder                                                                              | Works; a change at its top refreshes it.                                                                                                                                                      | `fileTools.test.ts`; `fileTree.test.ts`                                   |
| A folder in WSL (`\\wsl.localhost\…`) beside a Windows folder                                                 | Each is a project as it would be alone: its chats run in WSL from its snapshot.                                                                                                               | By hand (Windows)                                                         |
| **Tree and files**                                                                                            |                                                                                                                                                                                               |                                                                           |
| A file dragged from one folder to another                                                                     | Moved (across drives: copied, then deleted); its tab follows.                                                                                                                                 | `FileTree.test.tsx`; `entries.rs` tests                                   |
| A folder's row: Rename, Delete, Cut or Copy from the menu, Mod+Backspace, Mod+X, a drag                       | None of them acts on it; the backend refuses deleting, renaming or moving a window's folder.                                                                                                  | `FileTree.test.tsx`; `entries.rs` tests                                   |
| A link in one folder to another (`web/node_modules/@shop/api → ../../api`)                                    | Its rows are told apart from api's own.                                                                                                                                                       | `fileTree.test.ts`                                                        |
| New file with nothing selected                                                                                | In the active folder.                                                                                                                                                                         | `openFolders.test.ts`                                                     |
| **Search, `@`, tabs, IntelliSense**                                                                           |                                                                                                                                                                                               |                                                                           |
| The same relative path in two folders                                                                         | Two groups; each result opens its own file.                                                                                                                                                   | `searchPanel.test.tsx`                                                    |
| Two tabs with the same file name                                                                              | Told apart by their folders.                                                                                                                                                                  | `Tabs.test.tsx`                                                           |
| A saved tab from a folder that left the workspace                                                             | Not reopened.                                                                                                                                                                                 | `projectSessions.test.ts`                                                 |
| A TSX file in the second folder                                                                               | Gets its folder's `jsx` and `lib`, not the first folder's.                                                                                                                                    | `activeConfig.test.ts`                                                    |
| **Git**                                                                                                       |                                                                                                                                                                                               |                                                                           |
| Two folders in one repository                                                                                 | One section listing both folders' changes.                                                                                                                                                    | `git/tests.rs`; `GitPanel.test.tsx`                                       |
| A folder inside a repository                                                                                  | Its changes only, as today; the editor's marks compare with the right file (fixed).                                                                                                           | `git/tests.rs`; `gitMarks.test.ts`                                        |
| Folders `app` and `app2` of one repository, only `app` in the workspace                                       | `app2`'s changes aren't listed or committed.                                                                                                                                                  | `git/tests.rs`                                                            |
| A folder that isn't a repository but holds some                                                               | "Not a repository", with **Initialize repository**. Each repository added as a folder gets its section.                                                                                       | `GitPanel.test.tsx`                                                       |
| A repository inside a folder's repository (untracked or ignored)                                              | The outer one never lists or stages it, so its Commit works and its row's revert can't trash it (fixed).                                                                                      | `git/tests.rs`                                                            |
| A submodule                                                                                                   | The outer repository lists it only when its commit moved; no empty commits (fixed).                                                                                                           | `git/tests.rs`                                                            |
| A linked worktree beside its main folder                                                                      | Two sections.                                                                                                                                                                                 | `git/tests.rs`                                                            |
| A detached HEAD                                                                                               | Its header says so; the commit goes on it, as git's would.                                                                                                                                    | `git/tests.rs`                                                            |
| A new repository with no commit                                                                               | The commit makes its branch.                                                                                                                                                                  | `git/tests.rs`                                                            |
| A merge, a rebase or conflicts in one repository                                                              | Its box is off, saying why; the others commit.                                                                                                                                                | `GitPanel.test.tsx`                                                       |
| Changes staged from a terminal outside the window's folders                                                   | Nothing is committed anywhere; the problem names them; they stay staged (fixed).                                                                                                              | `git/tests.rs`                                                            |
| pre-commit fails in one repository                                                                            | Nothing is committed anywhere; its output and **Commit without hooks** show.                                                                                                                  | `git/tests.rs`                                                            |
| commit-msg rewrites the message in one repository                                                             | Only that repository's commit has the change.                                                                                                                                                 | `git/tests.rs`                                                            |
| A commit fails after others were written (a terminal holds the third repository's `refs/heads/<branch>.lock`) | "Committed in api and web; infra: …"; the message stays.                                                                                                                                      | `git/tests.rs`; `GitPanel.test.tsx`                                       |
| A branch switched, or a push, from a terminal in a folder's repository                                        | The section updates; during a checkout, the isolated chats of that repository's folders wait.                                                                                                 | `watch.rs` tests; `overlay` tests (`is_busy`)                             |
| One push rejected                                                                                             | That section says so; the others are pushed.                                                                                                                                                  | `GitPanel.test.tsx`                                                       |
| A repository that was only ahead when Commit is clicked                                                       | Not pushed by that commit; the button offers Push for it afterwards.                                                                                                                          | `GitPanel.test.tsx`                                                       |
| **Folder chats**                                                                                              |                                                                                                                                                                                               |                                                                           |
| **New chat** with several folders                                                                             | A dropdown of the folders, then All folders; the chat is made in the one clicked, and Escape makes none. With one folder, no dropdown, as today.                                              | `chatInput.test.tsx`                                                      |
| **New chat** while an empty chat is open                                                                      | The empty chat is reused, moved to the choice clicked; its draft stays. A chat with a message is never moved.                                                                                 | `chatInput.test.tsx`; `chat_db/tests.rs`                                  |
| No chat yet, or the last one deleted                                                                          | The chat panel lists the choices to start a chat in; no empty chat is made in a guessed folder.                                                                                               | `chatInput.test.tsx`                                                      |
| A missing folder in New chat's dropdown                                                                       | Listed, but can't be clicked.                                                                                                                                                                 | `chatInput.test.tsx`                                                      |
| A folder resolving, added, removed or located while a chat is open                                            | The open chat and its draft stay (unless its folder left).                                                                                                                                    | `chatStore.test.ts`                                                       |
| A crash with an unanswered message in the second folder's chat, or an All folders chat                        | It comes back into the input when the workspace opens.                                                                                                                                        | `chatStore.test.ts`                                                       |
| Another folder's file in a folder chat's editor context, `@` or **Add to chat**                               | Not sent; the composer says why.                                                                                                                                                              | `chatInput.test.tsx`                                                      |
| A new chat on a folder's main branch                                                                          | The branch offer asks about that folder's repository.                                                                                                                                         | `chatBranchOffer.test.tsx`                                                |
| A folder chat on a host or in WSL                                                                             | As when its folder is open alone; its results land while the workspace is open.                                                                                                               | `rovik-client` `remote/tests.rs`                                          |
| Two folders that look like copies or moved projects                                                           | Each is asked about in a notification of its own, naming it.                                                                                                                                  | `projects.test.ts`                                                        |
| **All folders chats**                                                                                         |                                                                                                                                                                                               |                                                                           |
| An All folders chat reads, edits and runs commands in two folders                                             | Its paths start with the folders' names; `cd api && npm test` runs in api's view.                                                                                                             | `workspace_e2e_tests.rs`                                                  |
| Its changes in two folders                                                                                    | Land in both, under one landing; **Undo** puts both back.                                                                                                                                     | `overlay` land tests; `workspace_e2e_tests.rs`                            |
| A conflict in one folder                                                                                      | Nothing lands in either; the conflict shows under its folder's name.                                                                                                                          | `overlay` land tests                                                      |
| A crash while landing in two folders                                                                          | The landing is finished at the next start, in both.                                                                                                                                           | `overlay` land tests                                                      |
| `mv api/x web/x` in a command                                                                                 | Copied and deleted (EXDEV): web shows an add, api a delete.                                                                                                                                   | `overlay` composite tests (mounted)                                       |
| A file made at the view's top                                                                                 | Refused: the top is read-only.                                                                                                                                                                | `overlay` composite tests                                                 |
| A command naming a folder's real path                                                                         | Under bubblewrap it reaches the view; elsewhere it's refused by text, a connector's too.                                                                                                      | `confine.rs` tests; `workspace_e2e_tests.rs`                              |
| Only api changed                                                                                              | Only api's checks run, in api; a failure that was there before is set apart.                                                                                                                  | `workspace_e2e_tests.rs`                                                  |
| api's type check fails                                                                                        | web's tests still run.                                                                                                                                                                        | `workspace_e2e_tests.rs`                                                  |
| A restricted file in one folder                                                                               | Refused by its path in the view; the other folder's file of that name isn't restricted.                                                                                                       | `workspace_e2e_tests.rs`                                                  |
| Each folder's `AGENTS.md`                                                                                     | Given under its name, within the shared limit.                                                                                                                                                | `workspace_e2e_tests.rs`                                                  |
| A plan from an All folders chat                                                                               | Saved in the first folder's `.rovik/plans`; its `Files:` name folders; **Implement** on it starts an All folders chat.                                                                        | `workspace_e2e_tests.rs`; `plans.test.ts`                                 |
| A subagent's worker in an All folders chat                                                                    | Writes only its step's paths, through the view, into the right folder.                                                                                                                        | `workspace_e2e_tests.rs`                                                  |
| Two chats changing one file (a folder chat and an All folders chat)                                           | Each sees the other's clash; refreshing one folder keeps the other folder's clashes.                                                                                                          | `overlay` chats tests                                                     |
| No virtual-files driver, or isolation off                                                                     | All folders is greyed, saying why; a chat made before is refused at start, naming Settings → Virtual files.                                                                                   | `virtualFiles.test.ts`; `workspace_e2e_tests.rs`                          |
| A folder added while an All folders chat's view is mounted                                                    | It shows at the chat's next run.                                                                                                                                                              | `overlay` composite tests                                                 |
| A request whose folders aren't the workspace's                                                                | Refused.                                                                                                                                                                                      | `agent_run` `check_project` tests                                         |
| The editor's diagnostics                                                                                      | Found by the agent through the view (fixed for isolated folder chats too).                                                                                                                    | `workspace_e2e_tests.rs`                                                  |
| Claude Code in an All folders chat                                                                            | Works in the view, keeps its session, and is told about the folders.                                                                                                                          | `workspace_e2e_tests.rs`                                                  |
| On Windows, an All folders chat                                                                               | Runs in WSL as one project; an older Rovik there refuses it with how to update.                                                                                                               | `rovik-agentd/tests/folders.rs`; `remote/tests.rs`                        |
| Moving an All folders chat to a cloud host                                                                    | The dialog lists its folders and says their git history stays here; an older host is offered its upgrade; a missing folder blocks the move.                                                   | `chatLocation.test.tsx`; `remote/tests.rs`                                |
| A folder inside a repository, on a host                                                                       | Its ancestors' ignore rules still keep `node_modules` out of the chat's results.                                                                                                              | `shadow/rules.rs` tests; `rovik-agentd/tests/folders.rs`                  |
| Autopilot's delivery in an All folders chat on a host                                                         | Its results wait for the app to land them, saying why.                                                                                                                                        | `rovik-agentd/tests/folders.rs`                                           |
| **Saved state**                                                                                               |                                                                                                                                                                                               |                                                                           |
| Quitting and starting again                                                                                   | The workspace and its tabs come back.                                                                                                                                                         | e2e: restart                                                              |
| An update's restart                                                                                           | The workspace opens again.                                                                                                                                                                    | `updates.rs` tests                                                        |
| The last workspace open in another window at start                                                            | This window starts with nothing open, as with a project.                                                                                                                                      | `switchTarget.test.ts`                                                    |
| Creating a workspace from the open project                                                                    | Its tabs, its running terminals and its commit message come along.                                                                                                                            | `terminalPanel.test.tsx`; `GitPanel.test.tsx`                             |
| **Terminals**                                                                                                 |                                                                                                                                                                                               |                                                                           |
| A new terminal in a workspace                                                                                 | **+** asks which folder; the terminal starts there.                                                                                                                                           | `terminalPanel.test.tsx`                                                  |
| A folder removed with terminals in it                                                                         | They keep running.                                                                                                                                                                            | `terminalPanel.test.tsx`                                                  |

---

## 6. Milestones

| Milestone                                    | What it delivers                                                                                                                                                                                                                                                                    | Done when                                                                                                                                              |
| -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **A. A project as a list of one**            | One copy of the path helpers; `openFolders` with one folder; the watcher, the tree's controller, git's scopes and the editor's services take folders; the repository list; commits that never take what's staged outside; nested repositories left alone; the editor's marks fixed. | Every existing test passes unchanged, the new ones pass with two folders, and the e2e run is unchanged. Nothing changes for a user but the fixed bugs. |
| **B. Workspaces stored and opened**          | Schema 11 and `workspaces.rs`, the shared file-name check, the commands, the claims, `switchTarget`, what's saved, `--workspace`, the guards on the window's projects.                                                                                                              | Their tests pass; a workspace opens in `switchTarget.test.ts`. No menu yet, so nothing creates one.                                                    |
| **C. A window with several folders**         | Folder rows and their guards, Remove, Locate and Try again, search, tabs, terminals, the settings pages, IntelliSense and rust-analyzer.                                                                                                                                            | The tests in C01–C07 pass.                                                                                                                             |
| **D. Git across repositories**               | Sections, the commit across repositories, refreshing by repository.                                                                                                                                                                                                                 | The git tests in D02 and D03 pass, and GitPanel's tests with one repository pass unchanged.                                                            |
| **E. Folder chats in a workspace**           | The chat list of several owners, New chat's dropdown, a chat's folder, what's sent, the isolation bar, the cloud client's owners.                                                                                                                                                   | The tests in E01–E04 pass.                                                                                                                             |
| **F. All folders chats on this computer**    | The run's `Folders`, the composite view, landing across folders, context, sandbox, checks, plans, subagents, connectors, and the UI.                                                                                                                                                | The overlay, land and run tests of F01–F14 pass, with FUSE on Linux CI and macFUSE by hand.                                                            |
| **G. All folders chats in WSL and on hosts** | The protocol's folders, the daemon's, the composite snapshot and its ignore rules, results per folder, moves and the dialog.                                                                                                                                                        | `rovik-agentd/tests/folders.rs` and the client's tests pass.                                                                                           |
| **H. The menu**                              | The menu items, the dialog, workspaces in the list, the README, the e2e steps.                                                                                                                                                                                                      | The e2e steps in section 8 pass on Linux, macOS and Windows.                                                                                           |

Every merge to master is released (`release.yml:1`), so each task is safe to
ship alone. A–G keep the menu items out: no window has more than one folder
until H, and nothing offers All folders. What a user of a project sees
change before H: the fixed bugs (section 12), tab titles told apart (C03),
and the "Packages (N)" heading (C05). B01's schema step is permanent once
released (3.1). Sizes: A M, B M, C M, D L, E M, F XL, G L, H S.

---

## 7. Implementation order for an AI agent

Rules for every task (AGENTS.md):

- Read the files a task names, their callers and their tests before
  changing them; follow the comments' style.
- Every task ships with tests that would fail without it, named as sentences.
- A project (one folder) behaves as it does today after every task, but for
  the fixes and changes section 6 names: its existing tests pass unchanged.
- A protocol type's new field has a serde default and a sample in
  `covers()`; an older peer's message still parses.
- Before each commit: `npm test`, `npm run typecheck` and
  `npm run format:check`. From `src-tauri/`: `cargo nextest run -p <crate>`
  for each crate changed (the app crate is `rovik-ide`), and
  `cargo clippy -p <crate> --tests` with no new warnings. Format the Rust
  files changed with `rustfmt --edition 2021`.
- Write each task's notes under "Notes from implementation" at the end of
  this file.

| Task | Files                                                                                                                                                                                                                                                      | What                                                                                                                                                                                                                                                                                                                                                  | Tests                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| ---- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A01  | `src/lib/fileTools.ts`, `src/lib/fileTree.ts`, `src/components/FileTree.tsx`, `src/components/NewProjectDialog.tsx`, `src/App.tsx`                                                                                                                         | One `isAtOrUnder`, `parentOf` and `samePath` in `fileTools.ts`, with the platform's case rule, drive roots and UNC roots, in place of the six copies.                                                                                                                                                                                                 | `fileTools.test.ts`: POSIX and Windows paths; `D:\x`'s parent is `D:\`; `\\server\share`; case ignored on Windows and macOS only. `fileTree.test.ts`: a change at the top of `D:\` refreshes it.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| A02  | `src/lib/openFolders.ts` (new), `src/App.tsx`                                                                                                                                                                                                              | `WindowTarget`, `target()` in place of `rootPath`, `openFolders`, `folderOf`, `activeFolder`, `sessionKey`. App.tsx's 28 uses of `rootPath()` go through them (a project is a list of one).                                                                                                                                                           | `openFolders.test.ts`: `folderOf` with two folders and with none; `activeFolder`'s order; `sessionKey`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| A03  | `src-tauri/src/filesystem/watch.rs`, `src-tauri/src/filesystem/mod.rs`, `src/lib/commands.ts`, `src/App.tsx`                                                                                                                                               | `start_file_watch(paths)`: a watch and ignore rules per folder, one folder's error not stopping the others; paths under each folder's own spelling; `unknown` names its folder; `git-changed { folder }`; a repository's git folder outside the folder, and its `refs/`, watched; `project_busy` on the working folder; the tree's matcher by folder. | `watch.rs`: changes in two folders are both reported; an unwatchable folder leaves the other watched; an overflow names its folder; a commit and a push from a terminal in a repository above a folder send `git-changed` for it; a folder opened through a link reports under its own path; a new set of folders stops the old watches.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| A04  | `src/lib/fileTree.ts`, `src/components/FileTree.tsx`, `src/App.tsx`                                                                                                                                                                                        | The controller takes the roots and whether they're rows; each folder's listing uses its own root; rows keyed by folder and file id; `rootId` per folder. With one root and no rows, the rows are today's.                                                                                                                                             | `fileTree.test.ts`: two roots as rows, each expanded, refreshed and moved across; a link in one folder to the other lists rows of its own; one root lists as before.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| A05  | `src-tauri/src/git/repos.rs` (new), `git/mod.rs`, `git/changes.rs`, `src-tauri/src/util.rs`, `rovik-core/src/overlay/chats.rs`, `src-tauri/src/lib.rs`                                                                                                     | `git_repos(folders)`; `scopes` on `git_status`, `git_commit` and `git_discard_all` (by default today's one), matched as pathspecs; line counts scoped; `rewriting` holds the working folder, and `is_busy` counts holds above and below.                                                                                                              | `git/tests.rs`: two subfolders of one repository are one repository with two scopes; a linked worktree is its own; a folder in none says so; a status with two scopes lists both and nothing beside; `app2` isn't taken with `app`; line counts stay in scope. `overlay` tests: a hold on a working folder makes a folder inside it busy, and one beside it not.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| A06  | `git/changes.rs`, `git/mod.rs`                                                                                                                                                                                                                             | Commit refused while something is staged outside the scopes; status leaves out nested repositories and submodules changed only inside; revert refuses both.                                                                                                                                                                                           | `git/tests.rs`: a change staged outside the folder stops the commit and stays staged; a nested repository doesn't stop a commit and isn't listed; its revert is refused; a submodule changed only inside isn't listed, and no empty commit is made.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| A07  | `src/App.tsx`, `src/lib/gitMarks.ts`, `src/components/GitPanel.tsx`                                                                                                                                                                                        | The editor's marks ask with the path relative to the file's repository; GitPanel's `onOpenFile` keeps the file it's given; tabs not on disk aren't refreshed.                                                                                                                                                                                         | `gitMarks.test.ts`: in a project at `repo/app`, `package.json` is asked as `app/package.json`; `disk:` and `chat:` tabs keep their bases on a refresh.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| A08  | `src/components/Editor.tsx`, `src/lib/fileFormatters.ts`, `src/lib/markdownPreview.ts`, `src/lib/diagnostics.ts`                                                                                                                                           | The formatter's root, the Markdown preview's `/` and the diagnostics' root are the file's folder.                                                                                                                                                                                                                                                     | With two folders, a file of the second is formatted with the second's config, its `/` links resolve in the second, and its markers are reported for the second.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| B01  | `rovik-core/src/chat_db/schema.rs`, `chat_db/workspaces.rs` (new), `chat_db/workspaces_tests.rs` (new), `chat_db/projects.rs`, `chat_db/projects_tests.rs`, `chat_db/store.rs`, `rovik-core/src/file_name.rs` (new), `src-tauri/src/filesystem/entries.rs` | Schema 11 (3.1); `workspaces.rs`; `list`/`summary` by kind; `chat_may_use`'s union; the trigger; `DialogSummary.projectId`; `set_dialog_project` for empty chats only; the file-name check moved, with `folder_name`. Merged once this plan is reviewed.                                                                                              | `upgrades_a_version_10_database`. `workspaces_tests.rs`: each rule of D5 (a symlink, a copy's ID); names (a clash takes its parent's, then a number; case; `CON`, `build`, `@context`, a drive root, 300 characters; a removed name never reused; a re-added folder's name back; locating keeps it); the last folder stays; list order and `missing`. `projects_tests.rs`: a workspace isn't listed or summarised as a project; it never gets project folders; a workspace chat works only in its folders, not in a removed one; a folder chat doesn't work in the other folders; deleting a workspace deletes its own chats and leaves its folders' projects.                                                                                                                                                                                                                                                                                          |
| B02  | `chat_db/api.rs`, `src-tauri/src/commands/workspace.rs` (new), `commands/mod.rs`, `src-tauri/src/lib.rs`, `src/lib/chatApi.ts`, `src/lib/workspaces.ts` (new), `src/types.ts`                                                                              | The commands; the frontend's calls.                                                                                                                                                                                                                                                                                                                   | `chat_db` `api` tests: removing or locating a folder is refused while a workspace chat is at work there or has changes in it; deleting a workspace open elsewhere is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| B03  | `src-tauri/src/instances.rs`, `src-tauri/src/lib.rs` (`project_claim`, `project_check`, `at_work_in`), `src-tauri/src/commands/chat.rs` (`reclaim`), `src/lib/instances.ts`                                                                                | Claim targets (D7): the stored target, the record per owner, keys under the lock and without duplicates, `reclaim(root, id)`, `keep_open` per owner, holders with the workspace's name, `elsewhere()`'s copy only with a folder, the notices.                                                                                                         | `instances.rs`: a workspace takes its key and its folders' or none; a folder another process has stops it and is named; adding and removing a folder keep the others' keys; two folders resolved at once keep their keys; a folder resolved after the window moved on takes nothing back; leaving a workspace with its own chat at work keeps it and its folders; with two folders' chats at work keeps both; a workspace open elsewhere isn't reported as a copy. `instances.test.ts`: the notices.                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| B04  | `src/lib/switchTarget.ts` (new), `src/lib/projects.ts`, `src/App.tsx`                                                                                                                                                                                      | `switchTarget(target)` for a folder or a workspace (3.3), its steps as calls it's given; each folder resolved, with a notification and a counter of its own; missing folders not resolved, and tried again on focus and Try again.                                                                                                                    | `switchTarget.test.ts`: the steps' order; a refused claim changes nothing; a cancelled unsaved tab stops the switch; a missing folder isn't resolved, and is when it's back; a target taken elsewhere at start leaves the window empty. `projects.test.ts` (new): two folders' copy questions are both asked, each naming its folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| B05  | `src-tauri/src/filesystem/mod.rs`, `src/lib/projectHistory.ts`, `src/lib/projectSessions.ts`, `src-tauri/src/lib.rs` (`--workspace`), `src-tauri/src/updates.rs`, `src/lib/updates.ts`                                                                     | What's saved and the restart (D17).                                                                                                                                                                                                                                                                                                                   | `updates.rs`: `restart_args` with a workspace passes `--workspace`. `projectSessions.test.ts`: a workspace's entry, and a tab of a folder that left isn't restored. A `last-project.json` without `workspace` reads as before.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| B06  | `src/components/ProjectList.tsx`, `src/App.tsx`, `rovik-core/src/chat_db/api.rs`, `src-tauri/src/commands/chat.rs`                                                                                                                                         | Delete hidden for every project the window has; `chat_delete_project` refuses one this window has; Relink on a workspace folder's project does what Locate… does.                                                                                                                                                                                     | `ProjectList` test: no Delete on any of the window's projects. `chat_db` `api` tests: deleting a project this window has open is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| C01  | `src/components/FileTree.tsx`, `src/components/FileContextMenu.tsx`, `src/App.tsx`, `src-tauri/src/filesystem/entries.rs`, `src/styles.css`                                                                                                                | Folder rows (name, missing); their menu (Remove from workspace, Locate…, Try again); no rename, delete, cut, copy or drag of them by menu, keyboard or mouse; the backend's refusal; dropping on a folder's row; the project-root target for a project only; Remove and Locate (their guard, tabs settled, claim).                                    | `FileTree.test.tsx`: a folder row's menu; "Mod+Backspace on a folder row deletes nothing"; "a folder row can't be dragged"; a drop on it moves into that folder; a missing row's actions. `entries.rs`: deleting, renaming or moving a watched folder is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| C02  | `src/components/SearchPanel.tsx`, `src/components/searchPanel.test.tsx` (new)                                                                                                                                                                              | A search per folder, grouped by folder, one limit, results opened from their folder.                                                                                                                                                                                                                                                                  | Two folders' results under their headers; the same relative path opens two files; one folder looks as today.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| C03  | `src/components/Tabs.tsx`, `src/lib/editorTabs.ts`                                                                                                                                                                                                         | Colliding titles told apart.                                                                                                                                                                                                                                                                                                                          | `Tabs.test.tsx`: two `package.json` show their folders; a unique name shows alone.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| C04  | `src/components/TerminalPanel.tsx`, `src/components/FolderMenu.tsx` (new)                                                                                                                                                                                  | Grouped by `sessionKey`; **+** asks for the folder in a workspace (`FolderMenu`, with a slot for a caller's own items); the tooltip.                                                                                                                                                                                                                  | `terminalPanel.test.tsx`: the menu lists the folders, and the terminal starts in the one picked; a removed folder's terminal keeps running.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| C05  | `src/components/SettingsDialog.tsx`, `ProjectChecksSettings.tsx`, `RestrictedFilesSettings.tsx`, `SettingsMenu.tsx`                                                                                                                                        | The folder select in a workspace; "Packages (N)".                                                                                                                                                                                                                                                                                                     | `SettingsDialog.test.tsx`: the select, and the page following it. `ProjectChecksSettings.test.tsx`: the heading. `RestrictedFilesSettings.test.tsx`: the selected folder's list.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| C06  | `src/lib/intellisense/index.ts`, `src/lib/intellisense/project.ts`, `src/lib/intellisense/activeConfig.ts`, `src/lib/intellisense/compilerOptions.ts`, `src-tauri/src/typings.rs`                                                                          | The worker loads every folder; `owns` any; each folder's options kept, the active file's folder's applied; the limits shared.                                                                                                                                                                                                                         | `activeConfig.test.ts` (new): a TSX file in the second folder gets its folder's `jsx` and `lib`. `projectWorker.test.ts`: adding a folder loads its sources.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| C07  | `src/lib/lsp/client.ts`, `src/lib/lsp/rustAnalyzer.ts`, `src/components/Editor.tsx`, `src/App.tsx`                                                                                                                                                         | rust-analyzer with every folder; the `workspace/workspaceFolders` answer; a restart when the folders change.                                                                                                                                                                                                                                          | `lsp/client.test.ts` (new): `initialize` lists both folders; the request is answered with them.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| D01  | `src/components/GitPanel.tsx`, `src/components/GitRepoSection.tsx` (new), `GitHooksView.tsx`, `ConflictResolver.tsx`, `src-tauri/src/git/history.rs`                                                                                                       | GitPanel split into the window's part and a section per repository; one repository looks as today; drafts, publish answers and `GIT_LOG_LATEST` by repository; `opKey` with the repository; folders without one.                                                                                                                                      | `GitPanel.test.tsx`: one repository renders as before; two show two sections with their branches. The `gitBranchMenu`, `gitRebase` and `conflictResolver` tests pass unchanged. `history.rs`: two repositories' logs don't stop each other.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| D02  | `git/changes.rs` (`git_commit_many`), `git/hooks.rs`, `src/lib/gitPublish.ts`, `GitPanel.tsx`, `GitRepoSection.tsx`                                                                                                                                        | The commit across repositories (D11), holding each repository; the include boxes; `commitAction(statuses)`; pushing what was committed; one publish question.                                                                                                                                                                                         | `git/tests.rs`: a pre-commit failing in the second repository leaves the first uncommitted; a commit-msg hook changes its repository's message only; each commit goes on its own branch; a held `refs/heads/<branch>.lock` in the third reports the first two. `gitPublish.test.ts`: the actions over several. `GitPanel.test.tsx`: a partial failure keeps the message; a rejected push shows in its section; a repository that was only ahead isn't pushed by a commit.                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| D03  | `GitPanel.tsx`, `src/App.tsx`                                                                                                                                                                                                                              | A folder's `git-changed` refreshes its repository's section; the list found again for a new repository.                                                                                                                                                                                                                                               | `GitPanel.test.tsx`: an event refreshes its section only; `git init` in a folder without a repository adds a section.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| E01  | `rovik-core/src/chat_db/store.rs`, `chat_db/search.rs`, `chat_db/api.rs`, `src-tauri/src/commands/chat.rs`, `src/lib/chatApi.ts`, `src/lib/chatStore.ts`, `src/components/ChatPanel.tsx`, `src/components/ChatList.tsx`                                    | The chats of several owners; `setProjects` keeping the open chat; unanswered sends recovered per owner added; the folder on each row.                                                                                                                                                                                                                 | `chat_db/tests.rs`: two owners' chats listed together by recency, and searched together; a subagent of a workspace chat belongs to the workspace. `chatStore.test.ts`: "adding a folder keeps the open chat and its draft"; "after a crash, an unanswered send in the second folder's chat comes back".                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| E02  | `src/components/ChatPanel.tsx`, `src/components/FolderMenu.tsx`, `src/lib/openFolders.ts`, `src/lib/chatStore.ts`, `src/lib/fileReferences.ts`, `src/lib/activeContext.ts`, `src/lib/chatBranch.ts`, `src/lib/plans.ts`                                    | New chat's dropdown on both buttons, and the panel's list when there's no chat; an empty chat reused for the choice; `chatRootOf`, `chatPathToAbsolute`, `chatPathOf`, `runPlace`; the folder label; what's sent named as the chat names files (the editor context included), another folder's file not sent; Implement on another folder's plan.     | `openFolders.test.ts`: a folder chat's path is joined to its folder; a file is named from its folder; a file outside the chat's folders has no name; a Windows file is named with `/` and found again. `chatInput.test.tsx`: with two folders, New chat lists them and makes the chat in the one clicked; Escape makes none; with one folder there's no dropdown; an empty chat is reused; with no chat, the panel lists the folders; a missing folder can't be clicked; the editor context names the file as the chat does; another folder's file isn't sent. `chatBranchOffer.test.tsx`: the offer asks about the chat's folder's repository.                                                                                                                                                                                                                                                                                                         |
| E03  | `src/components/IsolationBar.tsx`, `src/lib/overlay.ts`, `src/App.tsx` (`chat:` tabs)                                                                                                                                                                      | Paths through `chatPathToAbsolute`.                                                                                                                                                                                                                                                                                                                   | `isolationBar.test.tsx`: a changed file of the second folder's chat opens from the second folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| E04  | `rovik-client/src/remote.rs`, `rovik-client/src/remote/results.rs`, `rovik-client/src/remote/files.rs` (new), `src-tauri/src/commands/chat.rs`                                                                                                             | The window's open owners; `files_of(chat)` (one folder named "" for a folder chat, today's path); steering paths relative to the chat's own folder; the delivery test's folder.                                                                                                                                                                       | `remote/tests.rs`: a host's result for the second folder's chat lands while the workspace is open; a steering message's paths are relative to its chat's folder; a project without folders reads and writes as before.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| F01  | `rovik-core/src/agent_run/folders.rs` (new), `run.rs`, `repair.rs`, `verify.rs`, `landing.rs`, `before.rs`, `test_writer.rs`, `execute.rs`, `subagent_run.rs`, `change_review.rs`, `candidates.rs`, `agent_run/mod.rs`                                     | `Folders` in place of `Run.project_root` (4.3), one folder giving today's behaviour everywhere.                                                                                                                                                                                                                                                       | The existing agent_run suites pass unchanged. Unit: a key maps to its folder's real file; a folder chat is `single()`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| F02  | `rovik-protocol/src/agent.rs`, `src/lib/agentRuns.ts`, `agent_run/mod.rs` (`check_project`), `autopilot/driver.rs`, `agent.rs` (estimate), `src-tauri/src/commands/agent.rs`                                                                               | `StartRequest.folders` (`RunFolder`), `check_project` with folders, Autopilot's `Settings.folders`, the estimate's.                                                                                                                                                                                                                                   | Protocol: a start request without folders still parses; `covers()` samples. `check_project`: a workspace chat's folders must be its members by name; a missing or unknown folder is refused by name; root and folders together are refused; a project chat with folders is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| F03  | `rovik-core/src/vfs/store.rs`, `vfs/fuse.rs`, `vfs/macfuse.rs`, `overlay/fs.rs`                                                                                                                                                                            | `VfsError::CrossDevice` (EXDEV); the shared `Budget`; the `ChatFs` trait. No behaviour change.                                                                                                                                                                                                                                                        | `overlay` tests: folders of one chat share its limit; the warning comes once per chat. `vfs` tests: a cross-device error reaches the kernel as EXDEV.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| F04  | `overlay/folders.rs` (new), `overlay/land.rs`, `overlay/chats.rs`                                                                                                                                                                                          | `Folders` (4.2); landing on `Folders` (4.6): plan, lock order, one journal with its folders, recovery, undo; `folders_of` in place of `project_of`.                                                                                                                                                                                                   | `land` tests: changes in two folders land under one landing; a conflict in one folder lands nothing in any; a failed write in the second folder puts the first back; an interrupted two-folder landing is finished at start; undo puts back every folder; such a landing writes no `project.txt`; two such chats landing at once both finish; a folder chat's journal has no folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| F05  | `overlay/composite.rs` (new)                                                                                                                                                                                                                               | `CompositeFs` (4.2), unit-tested without FUSE.                                                                                                                                                                                                                                                                                                        | `the_root_lists_each_folder_by_its_name`; `inode_numbers_of_two_folders_never_collide`; `a_folders_root_has_the_views_root_as_its_parent`; `nothing_can_be_created_renamed_or_removed_at_the_root`; `a_rename_between_folders_is_a_cross_device_error`; `a_folders_kernel_notifications_carry_the_views_inode_numbers`; `changes_name_paths_from_the_views_root`; `a_folder_added_while_mounted_shows_and_old_inodes_stay_valid`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| F06  | `overlay/chats.rs`, `overlay/fs.rs`                                                                                                                                                                                                                        | `open_all` (store layout, reconciliation), the watcher and peers per (chat, folder), busy per folder, children over the view, scratch views of `&Folders`, `project_for`, `changes_in_folder`, `CARGO_TARGET_DIR` per set.                                                                                                                            | Mounted (FUSE): common tools work across the folders of one view; `mv` between folders lands as an add and a delete; the view's root is read-only to commands. `chats` tests: an All folders chat reopens with each folder's changes; the project changing under one folder notes paths with its name; a folder chat and an All folders chat see each other's clashes; refreshing one folder keeps the other's clashes; a folder with changes can't leave; a worker lands into the right folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| F07  | `agent_run/run.rs`, `agent_run/landing.rs`                                                                                                                                                                                                                 | `prepare` opens the view for every run, Plan mode too, and refuses without the driver or with isolation off; landing through `Folders`; the busy wait over every folder.                                                                                                                                                                              | `workspace_e2e_tests.rs` (new, with a `workspace_harness`): an All folders chat reads and edits files in two folders by their names; its changes land in each folder and nowhere else; Plan mode works in the view and changes nothing; a workspace chat is refused without the driver or with isolation off.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| F08  | `agent_run/project.rs`, `project_profile.rs`, `restricted_files.rs`, `agent.rs`                                                                                                                                                                            | `load_folders` (4.4): per folder detection, settings, checks, stack, instructions under the shared limit; restricted files prefixed; the project section.                                                                                                                                                                                             | `workspace_e2e_tests.rs`: each folder's instructions and checks are in the prompt under its name; a folder's restricted file is refused by its path in the view; one folder's restricted entry doesn't restrict the other.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| F09  | `overlay/confine.rs`, `overlay/confine/landlock.rs`, `overlay/chats.rs` (`command_problem`), `agent_run/execute.rs`, `agent_run/environment.rs`                                                                                                            | Redirects per folder; the text check for every folder's path, connectors' commands always; installs by folder.                                                                                                                                                                                                                                        | `confine.rs`: each folder's path shows its place in the view (bubblewrap); a command naming any folder's path is refused where it can't be redirected. `workspace_e2e_tests.rs`: a command in each folder runs there and sees the chat's changes; a command naming a folder's real path works in the view or is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| F10  | `agent_run/verify.rs`, `agent_run/before.rs`, `agent_run/command_output.rs`, `agent_run/project.rs` (helpers), `agent_run/group.rs`, `agent_run/breaker.rs`, `agent_run/test_writer.rs`                                                                    | Checks of the changed folders, each in its folder; the type-check rule per folder; before-runs on a scratch view; output paths rewritten; the file-to-command helpers per folder.                                                                                                                                                                     | `workspace_e2e_tests.rs`: only the changed folder's checks run, in it; a failed type check in one folder doesn't skip the other's tests; a check failing as before the change is set apart per folder; the red check runs a new test in its own folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| F11  | `agent_run/plan.rs`, `agent_run/execute.rs`, `src/lib/plans.ts`                                                                                                                                                                                            | Plans in the first folder; shown by chat path, the Plan-mode plan's too; `Files:` against the view; Implement's choice.                                                                                                                                                                                                                               | `workspace_e2e_tests.rs`: a workspace chat's plan is saved in the first folder's plans; its files are checked against the folders by name. `plans.test.ts`: a plan naming the workspace's folders is implemented by an All folders chat.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| F12  | `agent_run/candidates.rs`, `autopilot/driver.rs`, `agent_tools.rs`, `agent_run/tools.rs`, `agent_run/project.rs` (estimate)                                                                                                                                | Thorough candidates and Autopilot with folders; diagnostics through the folders; the estimate.                                                                                                                                                                                                                                                        | `workspace_e2e_tests.rs`: a scout reads both folders and a worker writes only its step's folder; a candidate follows its task chat's folders' settings; the diagnostics tool finds the editor's markers through the view (and in an isolated folder chat).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| F13  | `connectors/claude_code.rs`, `connectors/codex/mod.rs`, `connectors/mod.rs`                                                                                                                                                                                | The workspace's section given in own-tools mode.                                                                                                                                                                                                                                                                                                      | `workspace_e2e_tests.rs` (scripted harness): Claude Code works in the view, keeps its session, and is told about the folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| F14  | `src/components/ChatPanel.tsx`, `src/components/FolderMenu.tsx`, `src/lib/virtualFiles.ts`, `src/lib/chatBranch.ts`, `src/components/NewBranchDialog.tsx`, `src/components/ChatList.tsx`                                                                   | New chat's **All folders** item and its availability; labels; every folder's files in `@`, the editor context and Add to chat; the branch offer across repositories.                                                                                                                                                                                  | `openFolders.test.ts`: an All folders chat's path is found in the folder its first part names; an unknown name names no file; an All folders run is sent every folder by name and no root; a missing folder stops the run, naming it. `virtualFiles.test.ts` (new): All folders needs the driver and says how to get it; on Windows it needs Rovik in WSL to take it. `workspaceChat.test.tsx` (new): New chat lists the folders, then All folders, and the chat made there is the workspace's; All folders can't be picked without the driver; `@` lists each folder's files under its name; Add to chat from either folder goes in as `api/…` or `web/…`; the message box says the chat works in all folders. `chatBranch.test.ts`: repositories on their main branch are offered one branch, named once; two folders of one repository are asked about once; a repository on another branch keeps it; a branch that fails in one says which have it. |
| G01  | `rovik-protocol/src/lib.rs`, `rovik-protocol/src/work.rs`, `rovik-protocol/src/agent.rs`                                                                                                                                                                   | `features::FOLDERS`; `Project.folders` (`ProjectFolder`); `RunFolder`'s daemon fields.                                                                                                                                                                                                                                                                | Protocol: an older peer's `Project` reads with no folders; `covers()` samples.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| G02  | `rovik-agentd/src/session.rs`, `store.rs`, `work.rs`, `daemon.rs`, `results.rs`                                                                                                                                                                            | `FOLDERS` advertised; `ProjectState.folders`; labels checked; `folder_roots()` for settings and the run; unchecked per folder; delivery refused for such a project.                                                                                                                                                                                   | `rovik-agentd/tests/folders.rs` (new): a two-folder chat's first sync gives it one worktree with each folder under its name; each folder's settings reach the run; a merge checked on the server runs each folder's checks; a chat across folders isn't delivered and says why.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| G03  | `rovik-client/src/shadow.rs`, `rovik-client/src/shadow/composite.rs` (new)                                                                                                                                                                                 | `folder_tree`, `graft`, `borrow`; the `Composite` shadow and `compose`.                                                                                                                                                                                                                                                                               | Unit: composing two folders' trees puts each under its name; an unchanged compose keeps its commit; grafting a folder's result back gives its repository's tree.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| G04  | `rovik-client/src/shadow/rules.rs` (new)                                                                                                                                                                                                                   | Ignore rules rebased under a folder's name.                                                                                                                                                                                                                                                                                                           | `rebased_rules_ignore_what_git_ignores_in_the_folder` (a table checked against libgit2's `is_path_ignored` in the real layout); `what_an_ancestors_gitignore_ignores_stays_out_of_the_result`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| G05  | `rovik-client/src/remote/files.rs`, `remote/sync.rs`, `remote/results.rs`, `remote/moving.rs`, `remote/back.rs`, `remote/bar.rs`, `rovik-core/src/overlay/import.rs`                                                                                       | Sync, recheck, start, import per folder, landing's report, moves and back, the bar, for a chat of folders; `import` taking its store.                                                                                                                                                                                                                 | `remote/tests.rs` (fake daemon): a chat across folders isn't carried to a daemon without folders, and an upgrade is offered. `rovik-agentd/tests/folders.rs`: a run editing both folders brings one result with both; the result comes back into each folder's changes and lands in both; a landing moves the base for both; a chat across folders moved back waits with its changes in each folder; a folder added reaches the chat's worktree at its next message; a folder inside a repository imports with its repository's attributes.                                                                                                                                                                                                                                                                                                                                                                                                             |
| G06  | `rovik-client/src/remote/moving.rs`, `src/components/MoveChatDialog.tsx`, `src/lib/cloud.ts`                                                                                                                                                               | The preflight's folders and notes; blocked by a missing folder or an old host.                                                                                                                                                                                                                                                                        | `chatLocation.test.tsx`: the dialog lists the folders and the notes; a missing folder blocks the move.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| H01  | `src/components/ProjectSwitcher.tsx`, `ProjectList.tsx`, `NewWorkspaceDialog.tsx` (new), `src/App.tsx`, `src/styles.css`                                                                                                                                   | The menu items; the dialog (picker and typed path); Add folder; workspaces in the list, and their delete question; the trigger; Create bringing the folder's terminals and commit message along.                                                                                                                                                      | `ProjectSwitcher.test.tsx` (new): the items' order, and Add folder only in a workspace. `NewWorkspaceDialog.test.tsx` (new): each refusal shows on its row and keeps Create off; the open folder is listed first; a typed path is added. The list's workspace row, its missing count, and its delete question naming its chats.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| H02  | `README.md`                                                                                                                                                                                                                                                | Features, Getting started, Several windows, the Git panel, and chats across folders.                                                                                                                                                                                                                                                                  | `npm run format:check`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| H03  | `scripts/e2e/run.mjs`, `scripts/e2e/README.md`                                                                                                                                                                                                             | The e2e steps (section 8), after the project's restart step, with a restart of their own at the end.                                                                                                                                                                                                                                                  | The steps themselves.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |

---

## 8. Testing

**Rust** (`cargo nextest run` per crate, as CI runs them on Linux, macOS and
Windows):

- The app crate: `instances.rs` (claims, and the races of section 5),
  `filesystem/watch.rs`, `entries.rs`, `git/tests.rs`, `updates.rs`.
- `rovik-core`: `chat_db/workspaces_tests.rs`, `projects_tests.rs`,
  `chat_db/tests.rs`, the schema's `upgrades_a_version_10_database`,
  `file_name.rs`; the overlay's composite, budget, land and chats tests (the
  mounted ones gated by `vfs::mount_works()`, as today); `confine.rs`; the
  new `agent_run/workspace_e2e_tests.rs`, with a `workspace_harness` beside
  `harness` (`agent_run/e2e_tests.rs:132-198`): two folders, their checks
  on, the chat's folders, isolation, and the request sent with `folders`.
- `rovik-client`: `remote/tests.rs`, `shadow` and `rules` unit tests.
- `rovik-agentd`: the new `tests/folders.rs`, with `common/folders.rs`: `api`
  (a repository with history), `web` (a folder of a repository whose root
  `.gitignore` ignores `node_modules/`), `notes` (no git).

Tests that change process-wide state (the watcher's generation,
`GIT_LOG_LATEST`, the busy holds, the overlays' data folder) take that
state's lock, so they pass under nextest and `cargo test` alike.

**Frontend** (`npm test`): the tests named in section 7, among them these
new files: `openFolders.test.ts`, `workspaces.test.ts`,
`switchTarget.test.ts`, `projects.test.ts`, `searchPanel.test.tsx`,
`activeConfig.test.ts`, `lsp/client.test.ts`, `virtualFiles.test.ts`,
`workspaceChat.test.tsx`, `ProjectSwitcher.test.tsx` and
`NewWorkspaceDialog.test.tsx`.

**End to end** (`scripts/e2e/run.mjs`). A second demo folder, a repository of
its own, is added through the dialog's path field (the native picker can't
be driven). The steps come after `restart: the project and its tabs come
back`, so the project's steps are unchanged:

1. `workspace: create one from the open project, add a second folder; each is a row in the tree`
2. `workspace: the same file name in each folder opens two tabs, told apart`
3. `workspace: search finds a match in each folder`
4. `workspace: git lists both repositories with their branches; one commit lands in each`
5. `workspace: New chat's dropdown starts a chat in the second folder, which changes a file there`
6. `workspace: an All folders chat changes a file in each folder, and both land`
7. `workspace: a folder removed closes its tabs; the other folder stays`
8. `instances: the workspace's folder opened in another window brings this one to the front`
9. `restart: the workspace and its tabs come back`

Step 6 needs FUSE in the e2e run on Linux (its job installs `fuse3`, as
CI's Rust jobs do), macFUSE on macOS, and WSL on Windows; where it isn't
there, the step checks that All folders is greyed with its reason.

**By hand**, once per system, for what CI can't reach:

- Windows: a folder in WSL beside a Windows folder (a chat in each); an All
  folders chat running in WSL, landing in both folders; WSL on an older
  Rovik refusing it; a drive root; the same folder added with two spellings
  of case.
- macOS: an All folders chat with macFUSE (FSKit and the kernel extension);
  a folder under `/tmp` (FSEvents reports `/private/tmp`); a folder on a
  case-insensitive volume added twice in two cases.
- Linux: four large folders (the inotify limit); an All folders chat under
  bubblewrap, and under Landlock alone (`ROVIK_CONFINE_COMMANDS=landlock`);
  a chat in each of two folders at once.
- A cloud host: moving an All folders chat there and back; an Autopilot
  task there waiting for the app to land it.

---

## 9. Risks and known limits

- **The composite view is new code under FUSE**: inode numbers,
  notifications, EXDEV. It's unit-tested without FUSE (F05) and mounted on
  Linux CI; a tool that mishandles EXDEV within one mount is possible
  (Linux's overlayfs does the same, so most handle it).
- **All folders chats need the virtual-files driver** here: macOS without
  macFUSE has none, and the item says why. On Windows they need WSL.
- **Rebasing ignore rules** for WSL and hosts (G04) is the riskiest piece of
  G: a rule rebased wrongly lets by-products into a result, or keeps a file
  out. Its tests compare with libgit2 on the real layout.
- **In WSL and on hosts, an All folders chat has no git history**: git there
  shows Rovik's snapshots, and Autopilot doesn't push branches for it (D30).
- **Only the changed folders' checks run** (D27): a change in `api` that
  breaks an unchanged `web` isn't caught by the checks. The prompt lists
  every folder's checks.
- **Instructions share 12,000 characters**: a workspace of many folders with
  long `AGENTS.md` files has them cut.
- **The same relative path in two folders** can make a check's output
  ambiguous; a check's own folder is looked at first (4.7).
- **`CARGO_TARGET_DIR` is one per set of folders**: two folders building
  binaries of one name overwrite each other's in `target/debug`.
- **The repository map's limits (4 seconds, 20,000 files) are reached
  sooner** over several folders.
- **A chat changes one folder, or all of them**: a folder chat can't start
  working across folders; a new All folders chat can, with the folder
  chat's history left behind (D32).
- **A folder is open in one window at a time**, alone or in one workspace
  (D7). Wanting it in two windows means a copy of it, which can't then be in
  the same workspace (D5).
- **Repositories inside a folder aren't listed** (D10). The repository around
  them never stages them (D12).
- **A commit with changes staged outside its folders is refused** (D12) until
  they're committed or unstaged from a terminal.
- **Commits across repositories aren't one transaction.** The hooks all run
  first (D11); a failure while writing the commits leaves the earlier ones,
  says which, and keeps the message.
- **Schema 11 is permanent once released** (3.1).
- **Two spellings of one WSL folder** (`\\wsl$\…` and `\\wsl.localhost\…`)
  aren't recognised as one folder, as for projects today.
- **Opening a workspace counts as opening each folder**: their own rows
  move up the list too (`record_path`, `projects.rs:240-260`).
- **Folder names don't fold Unicode forms** (macOS's NFD against NFC): two
  names that look alike can both exist.

---

## 10. Questions

Asked of the user while planning:

| Question                                              | Answer                                                                                                  |
| ----------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| The menu items' wording                               | **Create a workspace…** and **Add folder to workspace…**, both above **New window** (D3).               |
| How is a chat's folder chosen?                        | **New chat** opens a dropdown of the workspace's folders, and the chat is made in the one clicked (D2). |
| Should a chat be able to change folders between runs? | No: chats across folders are better served by All folders chats, which see every folder at once (D32).  |
| When do chats across folders ship?                    | In the first release (D22–D31).                                                                         |

Decided while planning, for the user to check:

- **A workspace groups projects and owns its All folders chats** (D1): a
  folder's own chats stay with it.
- **All folders chats need the virtual-files driver** (D23), as the
  agent-managed context does: none on macOS without macFUSE.
- **In WSL and on hosts they carry no git history and Autopilot keeps their
  results** for the app to land (D30): Windows gets All folders chats
  through WSL in the first release.
- **Only the changed folders' checks run** (D27), rather than every
  folder's.
- **Plans go in the workspace's first folder** (D28), rather than in the
  app's data.
- **Instructions share 12,000 characters** (D25).
- **Folder names are given once and kept**, a removed folder's too (D8).
- **Stored in the chats database** (D6), with a schema step.
- **Create opens the workspace in this window**, bringing the open folder's
  tabs, terminals and commit message (D4).
- **Nested folders, and two folders of one project, are refused** (D5).
- **One window per folder** (D7).
- **One message box, a commit per repository**, every checking hook first
  (D10, D11).
- **A commit never takes what's staged outside its folders**, and is refused
  while there is some (D12). This changes today's behaviour for a project
  inside a repository, which commits it now.
- **Only the repository each folder is in** is listed (D10).
- **IntelliSense loads every folder**, the active file's folder deciding the
  compiler options (D14).
- **No workspace file, and no renaming** of workspaces or folders.

The plan was reviewed against the code twice. The first review: the claims
keep the window's target and a record per owner, so folders resolved at
once, late answers and folders left at work keep their claims; two folders
of one project are refused; the chat list keeps the open chat as folders
come and go, and recovers each folder's unanswered messages; folder rows
are guarded from the keyboard and the mouse as well as the menu, and in the
backend; a commit is refused while something is staged outside its
folders; repositories inside a folder aren't searched for; each folder
keeps its own compiler options; the watcher covers a repository's refs, a
linked worktree's git folder, and folders reached through a link; the
project list guards the window's projects; the e2e adds folders by typed
path. The second, for All folders chats: the workspace moved into the chats
database (its chats need an owner row; the schema step also keeps older
Rovik from misreading it); folder names are stored and kept, a removed
one's too; the claim key is the workspace's `project:` key, and another
window's workspace isn't taken for a copy; each folder keeps its overlay
under one composite view rather than one overlay over several folders or a
folder of links (`resolve_within_root` refuses links out of the root, and
walkers don't follow them); one landing journal rather than one per folder;
peers kept per (chat, folder); plans in the first folder, so they sync to
WSL and hosts with nothing special; WSL and hosts carry one composite
project rather than a worktree per folder.

---

## 11. Not in this plan

- **Moving a chat with messages to another folder**, or from a folder to
  All folders (D32).
- **A folder chat reading the other folders.** It would work here, but not
  in WSL or on hosts, where a chat has its own folder's files only.
- **Git history and branch delivery for All folders chats in WSL and on
  hosts**: each folder's snapshot pushed to its own project there, and the
  result grafted per repository. A later plan, once these chats are in use.
- **Renaming** a workspace or a folder's name; a **workspace file** to share
  (VS Code's `.code-workspace`).
- **Repositories found inside a folder**, beyond the folder's own.

---

## 12. Problems in today's code found while planning

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
9. **A late `reclaim` takes back a folder the window has left**: it claims
   the folder it was asked about and lets go of the new one, until the new
   one's own answer comes (`commands/chat.rs:63-71`, `instances.rs:140-200`).
   B03.
10. **An isolated chat never sees the editor's diagnostics**: they're kept
    by the real folder and looked up by the run's root, the view
    (`agent_tools.rs:433-435, 555-571`). F12.
11. **The editor context names the active file by its absolute path**
    (`activeContext.ts:71-83`), which an isolated chat's tools refuse: they
    take paths inside the view only (`agent_tools.rs:110-133`). E02. (Seen
    in the code; not reproduced.)
12. **A connector's commands may reach the real folder**: they run
    unconfined (`connectors/harness.rs:272-290`), and the text check against
    naming the project's path is skipped wherever the agent's own commands
    are confined (`overlay/chats.rs:80-99`). F09. (Seen in the code; not
    reproduced.)
13. **In Plan mode, an isolated chat's current plan is named by its full
    path**: it's made relative to the run's root, the view, while plans are
    saved in the project (`run.rs:1064`; `plan.rs:713-726`). F11.

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
- A command that names a restricted file by its real path may not match the
  file's entry, which is kept by its path in the project
  (`restricted_files.rs:348-363`; not confirmed).

---

## Notes from implementation
