# Workspaces: several folders in one window

Today a window has one project, and a project is one folder. A **workspace**
is a named list of folders opened together in one window. Each folder is a
row at the top of the file tree, search covers all of them, and the Git
panel lists every repository they're in. A commit puts each file into the
repository it belongs to, on the branch that repository is on. A chat works
in one folder, or, as a **workspace chat**, in the folders checked for it,
or all of them.

This plan says what to build, in which files, in which order, and how each
step is tested. Section 1 lists the decisions; section 2 what the code does
today; section 3 how workspaces work; section 4 how workspace chats work;
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
- **New chat** picks the chat's folders: **All folders**, or folders
  checked one by one.
- Chats that work across the folders, with the context of all of them, in
  the first release; their folders can be added and removed between runs.
- The other places a second folder changes are found, handled and tested.

The idea in one sentence: **a workspace groups projects, and has chats of
its own.** Each folder stays the project it is when opened alone, with its
chats, its settings, its plans and its ID file: a chat started in one
folder is that folder's, and everything built per project (runs, isolation,
landing, WSL, cloud hosts, checks) works for it as it does. The workspace
is a project too, with no folder of its own: its chats are the workspace
chats, which see the folders checked for them as top-level folders of one
view (`api/src/x.ts`, `web/src/y.ts`). Around them, the tree, the watcher,
search, tabs, terminals, the editor's services, the Git panel and the chat
list learn to take a list of folders, and a project is the list of one.

Three words used throughout: a **workspace chat** is a chat of the
workspace, working in one or more of its folders, or all of them (**All
folders**); a window's **owners** are the projects whose chats it lists
(each open folder's, and the workspace's own), and those it left whose
chats are still at work; a **part** is one folder's overlay within a
workspace chat's view.

Contents:

1. [Decisions](#1-decisions)
2. [What the code does today](#2-what-the-code-does-today)
3. [How workspaces work](#3-how-workspaces-work)
4. [Workspace chats](#4-workspace-chats)
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

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Why                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | **A workspace groups projects, and has chats of its own.** It is a name and an ordered list of folders. Each folder stays a project of its own: its chats, project settings, plans, ID file (`.rovik/project.json`) and window claim are the ones it has when opened alone. The workspace is itself a project, with no folder: its chats are the workspace chats (D22), which work in one or more of its folders, or all of them.                                                                                                                                                                                                                                                                                                                                                                  | Every per-project mechanism keys on one project: a chat's run and its overlay (`agent_run/run.rs:149-152`; `overlay/chats.rs:405-480`), landing, the sync to WSL and hosts (`rovik-client/src/shadow.rs`), the settings (by folder). A folder's chats stay the same whether it's open alone or in a workspace, and the workspace's own chats need an owner (`dialogs.project_id`).                                                       |
| D2  | **New chat picks the chat's folders.** With several folders, both **New chat** buttons open a menu: **All folders**, then a checkbox per folder (the active folder checked to begin with), and **Start chat**. One folder checked makes a folder chat, which works in that folder as a project's chat does; several make a workspace chat across them; **All folders** makes one across every folder, those added later included. A label by the message box shows the chat's folders.                                                                                                                                                                                                                                                                                                             | The user's choice. The folders decide what the chat can read and change, so the user picks them rather than the app guessing. A chat in one folder stays that folder's, so it's there when the folder is opened alone.                                                                                                                                                                                                                   |
| D3  | **The menu:** **Create a workspace…** always, and **Add folder to workspace…** when a workspace is open, both above **New window**.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | The user's wording.                                                                                                                                                                                                                                                                                                                                                                                                                      |
| D4  | **Create** opens a dialog: the workspace's name and its folders, the open folder already listed. **Create** opens the workspace in this window; the open folder's tabs, terminals and commit message come along. **Add folder to workspace…** opens the folder picker, which takes several folders at once.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | Making one from the open project is the common case. Opening it in this window follows **Open project…**; **New window** is there for the other case.                                                                                                                                                                                                                                                                                    |
| D5  | **Folder rules.** A workspace has at least one folder. A folder can't be in it twice (the same folder however it's spelled: case where the file system ignores it, a trailing separator, a link), nor inside another of its folders, nor contain one, nor share another member's project ID (a copy made with `cp -r`, or one sharing its chats). A folder may be in several workspaces.                                                                                                                                                                                                                                                                                                                                                                                                           | Nested folders would list, watch, search, claim and index the same files twice. Two folders with one project ID would need one claim key twice, which an OS lock refuses (`os_lock.rs:1-7`).                                                                                                                                                                                                                                             |
| D6  | **Stored in the chats database**, schema version 11: `projects.kind` (`folder` or `workspace`), a `workspace_folders` table, and a workspace chat's folders (`dialogs.folders`). A workspace is a `projects` row of kind `workspace` with no path; its folders are rows of their own. `projects::list` and `summary` learn the kind, and `check_project` reads a workspace chat's folders (4.1); listing chats and asking whether a project is open elsewhere work by ID as they do.                                                                                                                                                                                                                                                                                                               | The workspace chats need an owner row: `dialogs.project_id` references `projects`. In the same database, a workspace and its folders change in one transaction, across windows. The schema step makes an older Rovik refuse the database at start (`schema.rs:368-376`) rather than show a workspace as a missing project with **Relink**; one already open stops writing, and shows it so until it restarts (`chat_db/mod.rs:216-232`). |
| D7  | **One window per folder.** A workspace is claimed as a project is (`instances.rs`): its own `project:<id>` and each folder's keys (`folder:` and `project:`), all or none. A folder is open in one window at a time, alone or in a workspace; opening it where it's taken brings that window to the front.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | A chat's run lives in the process of the window that has its project (README, "Several windows"). With `project:<id>`, the checks made today by project ID (`open_elsewhere`, `handled_elsewhere`) guard the workspace's chats unchanged.                                                                                                                                                                                                |
| D8  | **Each folder has a stored name**, a plain name (no separators), unique in the workspace whatever its case, given when the folder joins and never changed: its folder's name, else `<parent>-<name>`, then `-2`, `-3`. A removed folder's name stays taken, and comes back if the folder does. The tree, tabs, search and the chats show it, and so does the Git panel for a repository that is one folder.                                                                                                                                                                                                                                                                                                                                                                                        | Workspace chats write `api/src/x.ts` into their history and their plans: a name that changed, or went to another folder, would point old messages at the wrong files.                                                                                                                                                                                                                                                                    |
| D9  | **The tree** of a workspace has a row per folder at the top, with its name. A folder's row can't be renamed, deleted, cut, copied or dragged, from the menu, a shortcut or the mouse, and the backend refuses those on a window's folder; its menu has **Remove from workspace**, and **Locate…** and **Try again** while the folder is missing. A project's tree stays as it is.                                                                                                                                                                                                                                                                                                                                                                                                                  | Deleting a folder's row would trash the whole folder (`delete_path` has no guard, `filesystem/entries.rs:437-455`), and Mod+Backspace deletes the selected rows (`App.tsx:1762-1765`). Moving one into another would nest them (D5).                                                                                                                                                                                                     |
| D10 | **Git, per repository.** Each folder's repository is the one it's in, found upward as today; a folder without one says so. There is one entry per repository, by its git folder (two folders in one repository share it; a linked worktree has its own), listing only the changes in the window's folders. With one repository the panel is as today. With several, each has a section (its branch with ahead and behind, Pull, History, stash and hooks in its header), and one message box commits every included repository's changes, each on the branch it's on.                                                                                                                                                                                                                              | The user's requirement. Hooks, merges and branches are already per repository (`git/mod.rs:41-43`). A different message per repository is one commit per repository, the others unticked. Repositories inside a folder aren't searched for: to commit in several repositories, add each as a folder.                                                                                                                                     |
| D11 | **A commit across repositories runs every checking hook first:** pre-commit in each repository, then the message hooks in each, and only then the commits. A hook that fails stops the commit everywhere, before any is written; **Commit without hooks** skips them in every repository. A commit that fails after others were written is reported by repository, the message stays, and nothing is pushed until every one has committed.                                                                                                                                                                                                                                                                                                                                                         | All or nothing as far as git can do it. Each repository's hooks still run in git's order. Pushing half of a change that spans repositories would publish it half done.                                                                                                                                                                                                                                                                   |
| D12 | **A commit never takes changes outside its folders.** If a repository has changes staged outside the window's folders (a terminal's `git add` beside them), its commit is refused before anything is staged or run, naming them. An untracked folder with a `.git` of its own is never staged by the repository around it.                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | Today a folder of a repository commits whatever a terminal staged beside it, and a repository inside the folder makes Commit fail (section 12). Refusing keeps the hooks seeing what's committed, as today.                                                                                                                                                                                                                              |
| D13 | **Search covers every folder**, its results grouped by folder. A folder chat's `@` lists its folder's files; a workspace chat's lists its folders' files, under their names.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | A chat can't read the folders it doesn't work in.                                                                                                                                                                                                                                                                                                                                                                                        |
| D14 | **The editor's services take the window's folders.** IntelliSense loads every folder, and the active file's folder decides the compiler options, as the active file's config already decides them (`intellisense/activeConfig.ts:1-21`). rust-analyzer gets every folder as a workspace folder. The formatter, the Markdown preview and the diagnostics the agent reads take the file's own folder.                                                                                                                                                                                                                                                                                                                                                                                                | One program and one rust-analyzer per window are what the app has. One folder's options for every file would give the others false errors, which reach the agent's diagnostics (`lib/diagnostics.ts:84-93`).                                                                                                                                                                                                                             |
| D15 | **Terminals are the window's.** In a workspace, **+** asks which folder (the folders as New chat lists them, one to pick), as it already asks which host once one is connected; a new terminal opened otherwise starts in the active folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | No new kind of menu, and the terminal panel already keeps each project's terminals apart (`TerminalPanel.tsx:88-104`).                                                                                                                                                                                                                                                                                                                   |
| D16 | **Settings stay per folder.** Project checks and Restricted files are kept per folder, as now; in a workspace their pages have a folder picker. A workspace chat follows each of its folders' settings, for that folder's files.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Checks and secrets are a folder's: the same file is restricted whichever chat touches it.                                                                                                                                                                                                                                                                                                                                                |
| D17 | **What's saved:** the window's workspace in `last-project.json`; its tabs, open folders in the tree and terminal panel in `project-sessions` under `workspace:<id>`. An update's restart reopens it (`--workspace <id>`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | As for a project.                                                                                                                                                                                                                                                                                                                                                                                                                        |
| D18 | **Names in the code.** "Workspace" is the user's word, and the name of the code that stores and opens one. Code that takes several folders (tree, watcher, search, git, `Folders`) says folders and repositories, and treats a project as a list of one. Project checks' "Workspaces (N packages)" heading becomes "Packages (N)".                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | "Workspace" already means package-manager workspaces (`project_profile/workspaces.rs`, the Project checks page), Codex's `workspace-write`, the LSP's workspace folders and Cargo's workspace.                                                                                                                                                                                                                                           |
| D19 | **Removing a folder** is refused while a workspace chat that works in it is at work, has background commands running, or has changes in it that aren't landed, here, in WSL or on a host, naming the chats; otherwise such chats let it go, and one left with no folders asks for its folders before its next run. **Locate…** is refused while such a chat is at work; its changes there go with the folder when the folder found is the same project (its ID file), and Locate… to another project is refused while there are some. Removing closes the folder's tabs (asking about unsaved ones); Locate… points them at the new place. A folder's own chats at work keep running, its claim kept until they finish, as when a window leaves a project for another; its terminals keep running. | A workspace chat's changes in a folder that left would have nowhere to land; a folder found elsewhere is the same files, and a chat keeps its changes by their paths in the folder. A folder chat's changes land in its own folder whatever the window has open.                                                                                                                                                                         |
| D20 | **A missing folder** (deleted, or a drive or share that isn't mounted) shows as missing, with **Locate…**, **Try again** and **Remove from workspace**; the rest of the workspace opens. While one of its folders is missing, a workspace chat doesn't start a run, land or undo, and says which folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | One folder gone mustn't close the others; a workspace chat's view, and its landing, need every one of its folders.                                                                                                                                                                                                                                                                                                                       |
| D21 | **No feature flag, and each merge safe to ship.** Every merge to master is released (`release.yml:1`). The menu items come last (milestone H): until then no window has more than one folder, so New chat has nothing to check and nothing offers a workspace chat. Every task keeps a project as it is, except for the fixes in section 12 and three visible changes: tab titles told apart (C03), the "Packages (N)" heading (C05), and a `cd` within the project passing the allowed commands (D26, F09).                                                                                                                                                                                                                                                                                       | AGENTS.md: no speculative options. Nothing reaches a user half built.                                                                                                                                                                                                                                                                                                                                                                    |

### Workspace chats

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Why                                                                                                                                                                                                                                                                                                                                          |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D22 | **One view of the chat's folders.** A workspace chat's run works in one folder, mounted for the chat, whose top-level folders are the chat's folders by name (those checked for it, or all of the workspace's), each backed by that folder's own overlay. The view's top level is read-only; moving a file from one folder to another is a copy and a delete (EXDEV), as across file systems. A chat's folders share its one 512 MiB budget.                                                                                                                                                                                                                                                                                                                                              | The run keeps one root: every tool, the repository map, write scopes, plans' `Files:` and subagents work as they do. Each folder keeps its own overlay, so its ignore rules, conflicts and landing are its own.                                                                                                                              |
| D23 | **Isolated here.** On this computer a workspace chat works in its view for every run, Plan mode included, and needs the virtual-files driver for it, as the agent-managed context does today. Without the driver, New chat can't check a second folder or **All folders**, says why, and links to the driver's settings; a workspace chat made before doesn't start. In WSL and on hosts it works in its worktree there (D30).                                                                                                                                                                                                                                                                                                                                                            | There is no single folder to fall back to working in.                                                                                                                                                                                                                                                                                        |
| D24 | **Lands all or nothing across folders.** One landing writes every folder's changes under one journal; a conflict in any folder lands nothing. **Undo** puts back every folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | One change across an API and its client is one change.                                                                                                                                                                                                                                                                                       |
| D25 | **Each folder speaks for itself.** The agent is told it works in folders of a workspace and how paths are made; each of the chat's folders' instructions (`AGENTS.md`…), stack, services and checks are given under its name, with one shared limit on the instructions: 12,000 characters, each folder an even share, and what one doesn't use going to the others. Each folder's restricted files apply to its files, as when it's open alone.                                                                                                                                                                                                                                                                                                                                          | A folder's rules are for its files; N full instruction files on every request would crowd the context.                                                                                                                                                                                                                                       |
| D26 | **Commands run at the view's top** (`cd api && npm test`), and a `cd` into a folder of the project passes the allowed commands, as it runs nothing; the command's other parts are checked as today. Under bubblewrap each folder's view also shows at the folder's real path; where commands can't be redirected (Seatbelt, Landlock), a command naming any of the chat's folders' real paths is refused by text, as one folder's is today.                                                                                                                                                                                                                                                                                                                                               | Today every part of a command must be on the allowed list (`agent_run/permissions.rs:1495-1519`), so `cd api && npm test` would be refused, and checks written that way skipped, except in Autopilot, which allows any command. A `cd` within the project is safe to allow in every chat.                                                    |
| D27 | **Checks: those of the folders the chat changed**, each run in its folder, and those that name no folder (a plan's own commands); a failure that was there before the change is set apart per folder, as today.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Checks are a folder's setting. Unchanged folders' slow suites stay out of every round.                                                                                                                                                                                                                                                       |
| D28 | **Plans go in the chat's first folder** (in the workspace's order), in its `.rovik/plans`, written there directly as a project's are, with a line under their title naming the chat's folders (`Folders: api, web`). **Implement** sends a plan with that line to a workspace chat whose folders include every name on it, even one folder: the active chat if it does, else a new one across those folders; if this workspace lacks one of them, it says which and starts nothing. A plan without the line goes to a folder chat of its folder, the active chat if it's that one.                                                                                                                                                                                                        | A plan in a folder is a file of that folder: it shows in the tree, and needs no place of its own. The line tells a plan whose paths start with folders' names from a folder chat's plan kept in the same place, and which folders it needs.                                                                                                  |
| D29 | **The branch offer covers every repository** of the chat's folders on its main branch: one question names them all, and one branch name goes to each. A folder that joins a chat later is offered it at the chat's next send, when its repository is on its main branch.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | One task, one branch name across its repositories.                                                                                                                                                                                                                                                                                           |
| D30 | **In WSL and on hosts**, a workspace chat is carried as one project, its workspace: the chat's folders' files under their names, in one snapshot, one push and one worktree, where its runs work directly, as every chat's do there. Its results come back split by folder and land through each folder's overlay. There git shows Rovik's snapshots, not the folders' history, and Autopilot keeps its results for the app to land rather than pushing a branch. Such a chat is never carried to a Rovik without the feature: the move offers the upgrade.                                                                                                                                                                                                                               | On Windows every chat runs in WSL, so workspace chats need this to exist there. One project there keeps the daemon and the protocol as they are, but for a list of folders.                                                                                                                                                                  |
| D31 | **Connectors work in the view.** Claude Code and Codex run with the view as their folder, so they need no extra directories; the chat's section of the prompt is given to them, as they'd find no `AGENTS.md` at the view's top.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | Every folder is under their working folder; a chat keeps its view's path, so their sessions keep going.                                                                                                                                                                                                                                      |
| D32 | **A workspace chat's folders can change between runs**, from the label by its message box: **Apply** puts a folder checked into its view, and takes one unchecked out of it, which is refused while the chat has unlanded changes there. Checking **All folders** makes it follow the workspace again; checking every box doesn't. Nothing changes while the chat is at work (an Autopilot task waiting on a rate limit included) or has background commands running. Apply needs one folder checked; a chat whose folders all left the workspace (D19) asks for them before its next run. A missing folder can't be checked, but can be unchecked when the chat has no changes there. A folder chat keeps its folder; an empty chat can still change, through New chat, which reuses it. | A workspace chat's paths start with a folder's name (`api/src/x.ts`), so its earlier messages stay right as folders come and go, and the agent is told which joined and which left. A folder chat's don't (`src/x.ts`): moving it would make them wrong. A folder that left with unlanded changes would take them out of the chat's landing. |

---

## 2. What the code does today

### One folder, everywhere

- `rootPath` (`src/App.tsx:159`) is the window's one folder. It feeds the
  tree's controller (`:304`), the watcher (`:1979-1987`), search (`:2175`),
  the chat panel (`:2233-2240`), the Git panel (`:2249`), the editor
  (`:2327`), the terminals (`:2346`), the Settings dialog (`:2388`) and
  what's saved (`:1929-1977`). It's only ever set to a folder (`:199`,
  `:520`). Every run's `root` is it (`ChatPanel.tsx:3057`, `:3097`).
- Switching projects (`openProjectAt`, `App.tsx:485-506`) checks the claim,
  settles unsaved tabs, saves the session, claims, then resets the tabs, the
  git marks, the tree's selection and Monaco's models. The tree, the
  watcher, rust-analyzer and IntelliSense follow `rootPath`.
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
  `App.tsx:1631-1636`, `NewProjectDialog.tsx:30`); `tsconfig.ts` has its own
  `samePath` and `startsWithPath`, with a case rule (`tsconfig.ts:286-294`).

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
  (`:1029-1034`, `:1270`). The daemon moves a chat, messages and all, to its
  project with `set_dialog_project` (`chat_db/store.rs:504`;
  `rovik-agentd/src/daemon.rs:427-438`).
- The chip in a chat's header says where it runs (`ChatLocation.tsx`,
  `ChatPanel.tsx:4423-4425`), once a host exists or the chat has a place
  (`ChatLocation.tsx:145`).
- **Implement** sends a plan into the active chat, from the chat or from a
  plan open in the editor (`ChatPanel.tsx:3133-3151, 3177-3181`).
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
- Every part of a command must be on the user's allowed list
  (`agent_run/permissions.rs:1495-1519`); Autopilot allows any command but
  its denylist (`autopilot/config.rs:87`).
- Checks run in the root (`run_checks`, `verify.rs:1610, 1685-1686`); a
  check that fails runs again on the code before the change, in a scratch
  overlay of the project (`before.rs:186-275`).
- Plans are written into `project_root`'s `.rovik/plans` directly, also in an
  isolated chat (`execute.rs:2087-2139`).
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
  landing can be undone (`:1156-1222`), and one a crash interrupted is
  finished at start (`land::recover`, `chats.rs:302`).
- Each chat's clashes with other chats on the same project are kept by chat
  (`refresh_project_peers`, `chats.rs:1565-1650`).
- Windows has no driver: a project's chat runs in WSL (`vfs/mod.rs:108-121`;
  `rovik-client/src/remote/moving.rs:275-297`).

### WSL and hosts

- A chat there works in a worktree of its project's repository on the
  daemon, `work/<chat>` (`rovik-agentd/src/work.rs:200-234`), directly: the
  daemon sets the run's `root` to it and turns isolation off
  (`rovik-agentd/src/daemon.rs:403-406`). The client snapshots the folder
  into the project's shadow repository, pushes it, and sends `WorkSync`
  before each message (`rovik-client/src/remote/sync.rs`); the daemon
  refuses a chat in another project (`rovik-agentd/src/work.rs:162-163`).
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
-- A workspace chat's folders: "all", or the names checked (JSON).
ALTER TABLE dialogs ADD COLUMN folders TEXT;
```

- A workspace's row in `projects` holds its name, an empty `path` and no git
  columns. Its workspace chats point at it by `dialogs.project_id`.
- A workspace chat's folders are `dialogs.folders`: `"all"` for **All
  folders**, which follows the workspace, or the names checked
  (`["api","web"]`), in the workspace's order; NULL for every other chat.
  A workspace chat is made with them: `NewDialog` gains `folders` (serde
  default), written by the same `INSERT` (`chat_db/store.rs:88-98, 314-323`).
  `store::set_dialog_folders` changes them, bumping the chat's `rev` as
  every change to a chat does, and `DialogSummary` carries them, so the UI
  shows a chat's folders.
- `projects::list` and `summary` take `kind = 'folder'` only, so a
  workspace never shows as a missing project with **Relink**, and never
  reaches `project_of` as a folder.
- `chat_may_use` doesn't change: a workspace has no project folders, and
  `check_project` reads a workspace chat's folders from `dialogs.folders`
  and their paths from `workspace_folders` (4.1).
- `schema.rs`'s two upgrade tests that assert version 10 (`:414`, `:506`)
  use `VERSION` instead, and `upgrades_a_version_10_database` is added.
- Once released, version 11 is permanent: an older Rovik refuses the
  database, and older windows still open stop writing to it until they're
  restarted (`chat_db/mod.rs:216-232`), as after every schema step. B01
  merges once this plan is approved, when the schema is settled; a column
  added later needs version 12.

`chat_db/workspaces.rs` (new, beside `projects.rs`):

- `check_folder(members, folder) -> Option<FolderProblem>`: `Duplicate`,
  `Inside`, `Contains`, `SameProject` (its ID file names another member's
  project), `NotAFolder` (D5), each naming the other folder. It compares by
  `path_key` (`projects.rs:85-92`). The file system is read first (the
  folder's canonical path, its ID file); `create` and `add_folders` then
  compare keys, project IDs and names inside their write transaction, so D5
  and D8 hold across windows without holding the database while a slow
  share answers.
- `create(name, folders)`, `add_folders(id, folders)`,
  `remove_folder(id, name)` (refused for the last; the folder leaves the
  folders of the workspace chats that had it), `relocate_folder(id, name,
to)` (Locate…), `opened(id)`, `list()` (most recently opened first, each
  folder with `missing`), `members(id)` and `get(id)`. D19's guard is the
  commands' (3.3), as it asks the overlays and the hosts.
- A workspace's name is trimmed, not empty, at most 80 characters, and
  needn't be unique.
- Deleting a workspace is `chat_delete_project` with its ID, as for a
  project (`chat_db/api.rs:107-123, 187-231`): its own chats are stopped and
  deleted with their overlays, and its folders' projects and chats stay.

**Folder names** (D8): `folder_name(folder, taken)`, a pure function:

1. The folder's last part as opened (`D:\` gives `D`).
2. Made plain: separators, `:`, `$`, `~`, backticks, `*`, `?`, `[`, `]` and
   control characters become `-`; leading `.`, `@` and `-` are dropped (the
   view's top keeps `@context` for the agent-managed context); at most 64
   characters; empty becomes `folder`. So every name passes the `cd` rule
   (4.5).
3. A name the agent's walkers skip (`build`, `dist`, `target`, `vendor`,
   `node_modules`…, `agent_tools.rs:31-47`) is taken as taken, so it gets
   its parent's name: `app-build`.
4. Unique among every name the workspace ever gave, removed folders'
   included, compared in lower case (SQLite's `NOCASE` folds ASCII only):
   the base, then `<parent>-<base>`, then `<base>-2`, `-3`.

A name is only ever a folder of the view (on Linux and macOS, and in WSL)
or an entry of a git tree, so Windows' reserved names don't apply to it.

Commands (`src-tauri/src/commands/workspace.rs`, new): `workspace_list`,
`workspace_create`, `workspace_add_folders`, `workspace_remove_folder`,
`workspace_relocate_folder`, `workspace_check_folder`, and, with workspace
chats, `chat_set_folders` (D32). `src/lib/chatApi.ts` wraps them, beside the
project calls. `DialogSummary` gains the chat's `projectId` and `folders`
(`chat_db/store.rs:49-59`, serialize only), so the UI knows a chat's
folders.

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
  to its folder; a workspace chat's `api/src/x.ts` is found in its folder
  named `api`; an unknown name names no file) and `chatPathOf(chatRoot, abs)`
  (the reverse, with `/` separators; null outside the chat's folders).
  `chatRootOf(dialog)` gives a chat's root: its folder, or the chat's folders
  for a workspace chat (all of the workspace's for **All folders**), or none
  when its folder has left the window. `runPlace(chatRoot)` gives what a run
  is sent: `{ root }` for a folder chat, or `{ root: null }` for a workspace
  chat, whose folders the backend reads itself (4.1).
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
`projects.ts`'s one of each, `:57-61, 95, 124`). When the original of a
copy is a member, the question offers only **Separate history** (D5).

Creating a workspace from the open project: the folder's keys are part of
the new target's, so they're never let go. Its tabs stay, and its
terminals and commit message move to the workspace's `sessionKey` (D4).

Adding folders: each folder picked is checked (`workspace_check_folder`),
then `project_claim` takes the target with it. A folder another window has
is refused and that window comes to the front; the others are added (and
named, D8), then resolved and shown. They join the workspace chats set to **All folders** at
their next runs.

Removing a folder, and Locate…: `workspace_remove_folder` refuses the last
folder, and both commands apply D19's guard, which comes with workspace
chats (F06): for each workspace chat that works in the folder, its changes
there are read here from its part's manifest, and in WSL or on a host from
its status (`Remote::overlay_status`, `remote/bar.rs:33-52`), and whether
it's at work, or has background commands running, from its runs and tasks
here and there (a dev server outlives its run until it's stopped,
`background_e2e_tests.rs:1-5`). While a host with such a chat
can't be asked, the guard refuses (`overlay_status` only logs then,
`remote/bar.rs:49`). Then the folder's tabs are settled (Remove) or pointed
at the new place (Locate…), `project_claim` takes the new target (the old
folder's keys kept while its own chats are at work here), and the folder is
marked removed, leaving the folders of the chats that had it, or moved.
After Locate… or Try again finds a folder, landings a crash interrupted in
it are finished (`land::recover`, 4.6).

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
- The keys held are always the target's and every at-work owner's,
  recomputed at each change (today `claim` drops the old target's keys
  unless it's kept open, `instances.rs:166-193`). So leaving `api`, opened
  alone, keeps api's keys while a workspace chat of a workspace left at
  work still uses it.
- `reclaim(root, id)` (after `chat_open_project` and a copy or move answer)
  records that folder's project ID, and claims the stored target again only
  if `root` is still in it. So two folders resolved at once both keep their
  keys, and an answer that comes late, for a folder the window has left,
  claims nothing back (section 12, item 9).
- Leaving a workspace whose own chats are at work keeps its key and every
  folder's (a workspace chat's run uses some or all of them); leaving one
  whose folder chats are at work keeps those folders'. `at_work_in`
  (`lib.rs:538-547`) takes the owners.
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
  (not on the open one) asks "Delete the workspace Shop? Its 4 workspace
  chats are deleted for good. Its folders, and their own chats, aren't
  touched."
- The list hides **Delete** for every project the window has open, not only
  for one (`ProjectList.tsx:57`), and `chat_delete_project` refuses a
  project this window has open, as it refuses one another window has.
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
- A folder that's deleted or renamed is reported as gone (its watch sees
  its own root go), and the window checks its folders when it comes to the
  front; the folder's row turns missing.
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
  own chats, and the workspace chats' parts for that folder (4.2).
- The tree's gitignore matcher becomes a map by folder.

### 3.6 Search, `@`, tabs and the editor's services

- **Search** (`SearchPanel.tsx`): `search_in_files` runs for each folder at
  once. With several folders, results come under a header per folder; with
  one, as today. Collapsed state is kept by folder and path, and a result
  opens from its folder. Include and exclude globs are relative to each
  folder. The result limit is for the whole search.
- **`@`** in a chat: a folder chat lists its folder's files
  (`list_project_files`); a workspace chat lists its folders' files, under
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
    /// The folder's stored name when the repository is exactly one of the
    /// window's folders; else the working folder's name, with its parent's
    /// when two clash.
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
  The hold is deliberately the working folder and not the folder a command
  was asked from: a checkout rewrites every tracked file in the repository,
  so the folders beside it in it must wait too. The price is that in a home
  folder kept in git (`~/.git`), an operation in a project under `~` that
  isn't a repository of its own holds `~`, and so makes the isolated chats of
  the other projects under it wait for the operation and 2 s (`QUIET`) after
  it; holding the folder alone would leave their files unwatched while a
  checkout rewrites them, which is the failure this rule prevents.

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
  **Commit without hooks**, which skips the hooks in every repository. A
  failure in 4 says which repositories committed and which didn't
  ("Committed in api and web; infra: …"); the message stays in the box, and
  nothing is pushed, until every one has committed.

- Once every included repository has committed this message, in one
  click or after a retry, each of them is pushed, as today; one that was
  only ahead before the message isn't (the button offers Push for it once
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

- `chat_db`'s `list_dialogs` and `search` take the window's owners
  (`project_id IN (…)`). ChatPanel's `projectId` prop becomes `projectIds`
  (`ChatPanel.tsx:273, 1015-1082`).
- `chatStore.setProjects(ids)` keeps the open chat while its owner is still
  in the set: a folder resolving, added, removed or located doesn't move the
  user to another chat or clear the composer. The chat list resets as today
  only when the window's target changes.
- Unanswered sends left by a crash are recovered for each owner as it's
  added to the set (`recoverUnsettledSends`, `ChatPanel.tsx:2780-2788`), so
  `unsettled_sends` keeps its one-project signature.
- The unread dots are those of the window's owners. With several folders,
  each chat's row shows its folders: a folder's name, the names checked
  ("api, web"), or "All folders".

New chat (D2):

- With several folders, **New chat** (both buttons) opens a menu:
  **All folders**, a separator, then each folder with a checkbox (its path
  as a tooltip; a missing one listed but not checkable), and **Start chat**,
  which says how many ("Start chat in 2 folders"). The active folder starts
  checked. Checking **All folders** checks every folder, and unchecking any
  folder unchecks **All folders**; checking every box isn't the same, as
  folders added later join only a chat set to **All folders**. Space toggles
  a box, Enter starts the chat, and Escape or a click outside makes none.
  With one folder there's no menu, as today.
- One folder checked makes a folder chat in it; several, or **All
  folders**, a workspace chat whose `folders` are the names checked, or
  `"all"` (4.12 says when a second box can't be checked).
- An empty open chat is reused, as today, moved to the owner for the
  choice (the folder's project, or the workspace's with the chat's folders,
  which it takes too) by a new `chat_move_empty_dialog` (`move_empty_dialog`
  in `chat_db/store.rs`), which refuses a chat with a message or a subagent's,
  so empty chats don't pile up. `set_dialog_project` stays as it is, for
  the daemon.
- With no chat to show (no chat yet, or the last one deleted), the panel
  shows the same menu to start a chat, rather than making an empty chat in
  folders it would have to guess.
- In a workspace, a label by the message box shows the chat's folders: "in
  api", "in api, web", "in all folders". A folder chat's is only a label,
  and the header's chip still says where a chat runs. A workspace chat's
  label opens the same menu, its folders checked, with **Apply** in place of
  **Start chat**: that's how its folders change between runs (D32, 4.12).
- `FolderMenu.tsx` draws the folders, with checkboxes for chats and as a
  list to pick one from for the terminal's **+** (3.9), with a slot for
  what a caller adds (the chats' **All folders**).

A folder chat:

- `chatRoot(dialog)` comes from `chatRootOf` (3.2). ChatPanel's uses of
  `props.rootPath()` go through it and `runPlace`: the run
  (`ChatPanel.tsx:3057`, `:3097`), the context estimate (`:724`, `:768`),
  `@` (`:922`, `:937`), the editor's selection reference (`:870`), the
  connector's commands (`:608`), the branch offer (`:3113`) and
  **Implement** (`:3150`). The "Open a folder" hint (`:4488`) is for a
  window with no folder.
- What the user sends names files as the chat does: the editor context, `@`
  and **Add to chat** give `chatPathOf` (today the editor context gives the
  absolute path, which an isolated chat's tools refuse: section 12). A file
  outside the chat's folders isn't sent; the composer says why: "web/src/x.ts
  is in web; this chat works in api."
- **Implement** (D28) sends a plan without a `Folders:` line to a folder
  chat of its folder: the active chat if it's that one, else a new one. A
  plan with the line goes to a workspace chat (4.8).
- The isolation bar and the comparison tabs (`chat:<dialog>:<rel>`) resolve
  paths through `chatPathToAbsolute`.
- Runs don't change: `root` is the chat's folder, which `check_project`
  accepts. The branch offer asks about the chat's folder's repository. Plans
  go to its `.rovik/plans`. Project checks and restricted files are its
  folder's.

Cloud hosts and WSL, for folder chats:

- `State.open` becomes the window's open owners, from the claims' record.
  `handled_elsewhere`, `land_waiting`, `is_open` and `folder_answered` look
  among them. `chat_open_project` no longer sets the client's open project
  (`commands/chat.rs:54-56`), so an answer that comes late can't point it
  at a folder the window has left.
- A steering message's paths are made relative to its chat's own folder.
  The delivery test uses the first folder with an `origin` remote.
- Moving a folder chat, WSL and the snapshots don't change.

### 3.9 Terminals

- `TerminalPanel` groups terminals by `sessionKey(target)` instead of by
  `rootPath`. A terminal keeps the folder it started in (`cwd`).
- In a workspace, **+** opens its menu with the folders first
  (`FolderMenu`, one to pick), then the hosts. A terminal opened otherwise
  (the panel opening empty) starts in the active folder. A tab's tooltip
  names its folder.

### 3.10 Settings

- `SettingsDialog` gets the open folders and the active one. With several,
  the two "This project" pages (Project checks, Restricted files) show a
  folder select (the active folder first) and work on the folder selected,
  as they work on the root today. A workspace chat has no settings of its
  own (D16).
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

## 4. Workspace chats

### 4.1 What they are

A workspace chat belongs to the workspace's project (D1). Its folders are
those checked for it, or all of the workspace's (`dialogs.folders`, 3.1).
Its run gets them from the database and works in one view of them (D22):
`api/src/x.ts`, `web/src/y.ts`. Paths in its tools, its history, its plans
and its changes start with a folder's name. Folder chats are unchanged.

`rovik-core/src/folders.rs` (new) is the one type for a chat's folders,
shared by the overlay, the run and the cloud client:

```rust
/// The folders a chat works in: one, named "" (a folder chat), or a
/// workspace chat's, each by its stored name.
pub struct Folder { pub name: String, pub path: PathBuf }
pub struct Folders(Vec<Folder>);
impl Folders {
    pub fn one(path: PathBuf) -> Self;
    pub fn named(list: Vec<Folder>) -> Result<Self, String>;
    /// A folder chat's folder; None for a workspace chat's.
    pub fn single(&self) -> Option<&Path>;
    /// Where plans go (D28): the first folder.
    pub fn first(&self) -> &Folder;
    /// `api/src/x.ts` → (api, `src/x.ts`); a folder chat's path as it is.
    pub fn split<'a>(&self, path: &'a str) -> Option<(&Folder, &'a str)>;
    /// The real file a chat path names.
    pub fn abs(&self, path: &str) -> Option<PathBuf>;
    /// The chat path of a real file.
    pub fn chat_path(&self, abs: &Path) -> Option<String>;
    /// For reading paths in commands' output: each folder's path, its
    /// canonical spelling and its place in the view, with what each maps
    /// to (`name/`; the view's root to "").
    pub fn output_roots(&self, view: &Path) -> Vec<(PathBuf, String)>;
}
```

`check_project` (`agent_run/mod.rs:1301-1333`) gives the run its
`Folders`, read where the chat's folders are kept rather than taken from
the request, so a run always has the chat's folders as they are now:

- **Here**: for a workspace chat (its project is a workspace), the chat's
  folders (`dialogs.folders`; all of the workspace's for **All folders**)
  at the paths `workspace_folders` has for them. Its request has no `root`;
  one with a root is refused, as is one today that would pass with no root
  at all (`agent_run/mod.rs:1306-1308`). A missing folder is refused by
  name (D20), and so is a chat left with no folders (D19). The context
  estimate (`agent.rs`, `chat_context_estimate`) reads them the same way,
  and Autopilot's stages, which keep their start's `Settings`
  (`autopilot/driver.rs:38-77`; a rate-limit retry starts from them,
  `rate_limit.rs:99-167`), get them at each stage's start.
- **In WSL and on hosts**: the client sends the chat's folders in
  `StartRequest.folders` (`rovik-protocol/src/agent.rs:230-258`, serde
  default), and the daemon sets `root` to the chat's worktree: each folder
  is `root/<name>`, and the root is one of the chat's project's folders, as
  today (`chat_may_use`; 4.11).

  ```rust
  /// One folder of a workspace chat's run on a daemon.
  pub struct RunFolder {
      /// Its stored name (D8): a top-level folder of the chat's worktree.
      pub name: String,
      /// Its project settings (as `project_settings` is).
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub settings: Option<serde_json::Value>,
  }
  ```

- A folder chat is unchanged, and Thorough candidates keep their exception
  (`agent_run/mod.rs:1312-1317`).

### 4.2 The view

`overlay/composite.rs` (new): `CompositeFs`, a `vfs::backend::Backend`
(inode based, `vfs/backend.rs:15-67`) over the chat's folders' `OverlayFs`s,
its parts. Every call is routed by inode, and file handles are the mount's
own (`fuse.rs:142-144`), so only these need more than routing:

- **Inode numbers**: a part's inode `n` is `(n << 16) | slot`, with a slot
  per part never reused while mounted; the root stays 1
  (`vfs/store.rs:22-23`) and is checked for before a number is decoded. A
  part's own numbers start at 2 and are never reused (`fs.rs:448, 701-706`),
  which, with replies at generation 0 (`fuse.rs:148`), keeps the kernel's
  caches right. Renumbered, they stay below 2^53 (Node reads them as
  doubles).
- **The root**: a folder of mode 0o555 listing each of the chat's folders
  by its name; `lookup` takes the exact name, and the case folding
  `OverlayFs::find` does on macOS (`fs.rs:825-862`). The backend itself
  refuses creating, linking, removing or renaming at the root, and setting
  its attributes or extended attributes: FSKit's requests come as root
  (`fuse.rs:54-62`), so 0o555 alone doesn't stop them. Opening and
  releasing the root do nothing.
- **Across folders**: `rename` and `link` between two parts answer EXDEV, a
  new `VfsError::CrossDevice` with its message (`vfs/store.rs:39-71`,
  mapped in `fuse.rs:37-52`). `mv` copies and deletes, as across file
  systems; the chat's changes then show an add in one folder and a delete
  in the other.
- **Changes** are named from the view's root: a change's `path` and its
  `origin` both get the folder's name, as `origin != path` means a move
  (`land.rs:148-159, 287-301`; `chats.rs:1492-1494, 1904-1910`).
- **Notifications**: each part's listener renumbers what it tells the
  kernel and forwards it (`Listener`, `vfs/store.rs:137`), a part added
  later's too.
- **Folders that join or leave**: **Apply** (D32) matches the view to the
  chat's new folders at once, under the chat's lock: a folder checked is a
  new slot and a new entry at the root; one unchecked is dropped, the
  kernel told (`Entry { ROOT, name }`), after `changes_in_folder` is asked
  again under the lock, and its part's store is deleted. Each run's start
  matches the view again, for a folder that joined or left the workspace
  for a chat set to **All folders**, and one located elsewhere, which is
  reopened at its new path. None of it remounts: the kernel caches a name
  lookup for a second and never caches the root's listing
  (`fuse.rs:34, 498`).
- **`usage`**: the chat's budget, and the parts' node counts added up.

`overlay/fs.rs`:

- **One budget per chat**: the limit, what's used, the warning at 80% and
  its callback move into a `Budget` the parts share, with a lock of its
  own. Today each check and charge is under the folder's own lock
  (`fs.rs:915, 1004-1026`), so two parts could both pass the limit, and the
  warning is marked given before its callback is looked up. Its other uses:
  the fields and the callback (`:106, 339-344, 512-518`), the counts
  (`:1229-1231`, `:1474-1478`, `:1536`, `:1750`, `:1813`, `:2274-2299`) and
  `usage` (`:2468`). `OverlayFs::open(base, store, limit)` keeps its
  signature, with a budget of its own; `open_in(base, store, budget)` takes
  a shared one. A chat keeps 512 MiB and one warning.
- **`trait ChatFs: Backend`**: `changes`, `versions`, `rebase_file`,
  `revert_file`, `flush`, `close_handles`, `used`, `by_product_bytes` and
  `generation` (`fs.rs:517-552`, `:1354-1373`, `:1504-1648`), implemented
  by `OverlayFs`, and by `CompositeFs`, which routes them by the path's
  first part. It extends `Backend`: a mount takes an `Arc<dyn Backend>`
  (`chats.rs:448, 780`), which an `Arc<dyn ChatFs>` upcasts to, and
  `with_fs`'s callers create and remove files through it (`redo_group`,
  `chats.rs:3296-3342, 3456`).

`overlay/chats.rs`:

- **One `open(chat, &Folders, on_warn)`** for both kinds of chat. A folder
  chat's store and mount are today's: one `OverlayFs`, and `project.txt`.
  A workspace chat's store is `overlays/<chat>/folders.json` (the
  workspace's ID, and its folders' names and paths at its last run) and
  `overlays/<chat>/parts/<name>/` per folder; its chat-level files
  (`landing.json`, `markers.json`, …) hold chat paths. `exists` means
  `manifest.json` or `folders.json` (`chats.rs:512-515`), and opening a
  store as the other kind is refused: today a store without `project.txt`
  is taken as the opener's, which writes one and sweeps the store
  (`chats.rs:438-445`). An older Rovik never reaches such a store: it
  refuses the newer database before the overlays start (`core.rs:72-75`).
- `folders_of(chat)`, from `folders.json` or `project.txt`, replaces
  `project_of` and the other readers of `project.txt` (`saved_project`,
  `with_fs`, `changes` and `file_versions`: `chats.rs:1747-1750`,
  `:1997-2002`, `:2322`, `:2341-2346`), so a closed workspace chat's bar
  still lists its changes. `ChatOverlay.project`, read in 17 places in
  `chats.rs`, `agent_run/landing.rs` and `candidates.rs`, becomes its
  `Folders`; a child's is its parent's view, as one folder.
- `changes_in_folder(chat, folder)` answers Remove, Locate… and unchecking
  a folder (D19, D32) from that folder's part's `manifest.json`
  (`Manifest`, `fs.rs:309-313`), so it answers while the folder is missing
  (`OverlayFs::open` needs its base, `fs.rs:437`). Locate… to the same
  project updates `folders.json`, the chat's and its landings' (4.6).
- **Watcher and peers per folder**: `project_changed(folder, …)` reaches
  each open overlay's part for that folder, with the paths it notes and
  marks prefixed with the folder's name. Clashes between chats are kept per
  (chat, folder) and compared by in-folder path, so a folder chat and a
  workspace chat on the same folder see each other's clashes. Refreshing
  one folder no longer replaces the chat's whole row
  (`chats.rs:1635, 1646`).
- `is_busy` for a workspace chat is any of its folders busy; the busy
  wait before landing loops over them (`agent_run/landing.rs:481-507`).
- Child overlays (subagents' workers, Thorough candidates) stay one
  `OverlayFs` over the parent's view (`chats.rs:670-697`): their paths are
  the parent's chat paths, and their landing goes through the composite
  into the right folder. Their room comes from the shared budget.
- Scratch views (the before-run, the test writer's red check) take
  `&Folders`: a scratch of a workspace chat is a composite too
  (`open_scratch`, `chats.rs:700-712`).
- `project_for` (`chats.rs:495-502`) keys the workload's history. Commands
  register with the run's root (`command_tool.rs:425`), so a workspace
  chat's view maps to its workspace, and a command's history is kept by
  workspace and command (`cd api && npm test`).
- `CARGO_TARGET_DIR` (`chats.rs:45-60`), shared today by a project's
  isolated chats, is shared by a workspace's chats, by its ID: a folder
  joining doesn't start a cold build.

### 4.3 The run

`Run.project_root` becomes `Run.folders` (4.1):

- `isolated()` says the run works in an overlay: always for a workspace
  chat here, never in the daemon's form (4.11), and for a folder chat when
  its root isn't its folder, as today (`repair.rs:843-846`). The code before
  a change (`code_before`, `before.rs:224-229`), the baselines
  (`repair.rs:895-901`) and landing (`landing.rs:134`) follow it.
- Each of today's uses moves to it, one folder giving today's behaviour:
  `run.rs:751-786, 1016-1036, 1166, 1233, 1381`; the code before a change
  (`repair.rs:895, 968, 1157`; `verify.rs:748-760`); P3.2's slips and V05's
  files behind (`verify.rs:996-1004, 1068-1070`); a failure's causes
  (`path_context`, `repair_run.rs:444-466`); landing
  (`landing.rs:134, 410, 456, 481-507`); the before-run and the red check
  (`before.rs:186-275`; `test_writer.rs:378-405`); plans
  (`execute.rs:2087-2139`; `run.rs:1001-1003`; `subagent_run.rs:1646`;
  `change_review.rs:1029`; `test_writer.rs:622`); Thorough candidates
  (`candidates.rs:789, 842, 869, 962, 1005, 1556`).
- `prepare` opens the view for every run of a workspace chat here, Plan
  mode included, and refuses without it: "A chat across folders needs the
  virtual files driver (Settings → Virtual files), or start a chat in one
  folder." The same when isolation is off (`ROVIK_ISOLATE_CHATS=0`, or a
  request's `isolate: false`, `run.rs:756-758`), but for the daemon's form,
  which works in its root (4.11). It never falls back to working in a
  folder (`run.rs:775-805`).

### 4.4 What the agent is told

`agent_run/project.rs` gains `load_folders(config_dir, root, &folders, …)`;
with one folder it's today's `load_for`. For a workspace chat, each of its
folders is read as it is when opened alone:

- `detect(<root>/<name>)` on the view, V09's folders one level down
  included, inside that folder (never at the view's top, where it would
  find the chat's folders); its settings from
  `load_project_settings(config_dir, path)`; its services line
  (`services::prompt_line`, `project.rs:196`); its checks as
  `in_folder(name, cmd)`; its stack as `"{stack} ({name}/)"`, V09's form
  (`project.rs:183-202`).
- Its instructions file, under its name. The folders share 12,000
  characters: each an even share, and what one doesn't use goes to the
  others. The cut moves out of `read_instructions` into `cut(text, max)`,
  which both use (`project_profile.rs:86, 643-670`). `project_context`
  (`project.rs:85-148`) is split into its checks part and its instructions
  part, so both builders share them.
- Restricted files go by a path's first part: a workspace chat's
  `RestrictedFiles` hands `api/rest` to api's own with `rest` (`entry_for`,
  `blocks`, `skips`), and the scan (`denied`) runs in each folder from its
  own root, adding the prefix. A folder's verdicts are those it has opened
  alone: one named `examples` or `Pods` keeps its secrets found, which a
  prefix alone would lose (`restricted_files.rs:118-125, 186, 218-224`,
  `:535-616`).

The project section adds:

> This chat works in folders of a workspace: each is a top-level folder of
> the project root (`api`, `web`), and every path starts with one
> (`api/src/main.rs`); nothing can be made at the root itself. Run a
> folder's commands with `cd api && …`. A folder's checks and instructions
> are for its files.

then a line from each folder's own facts on whether git works there (`api`
is a repository: `git -C api status`; `web`, a folder of one, isn't, in the
view), and, when the chat's folders changed since its last run, which
joined and which left: earlier messages may name a folder that's gone. Each
run records its folders' names in its message's `meta`, which moves with
the chat, so the next run compares with them here, in WSL and on hosts
alike. The
system prompt's lines and the tools' descriptions stay true: the project
root is the view's.

### 4.5 Commands and the sandbox

- Commands run at the view's top (`command_tool.rs:632-655`), background
  commands too.
- A `cd` into the project passes the allowed commands (D26): a `cd` part
  with one argument that's a relative path staying in the project (no `..`
  part; not starting with `-` or `~`; no `$`, backtick or glob character)
  runs nothing, so `check_command` (`permissions.rs:1495-1519`) lets it by,
  and checks the command's other parts as today. Every folder name passes
  it, quoted by `in_folder` when it has a space (`project.rs:202-212`). This
  holds for every chat; the README says so (Getting started, step 3:
  `README.md:308-312`).
- `Confine` (`overlay/confine.rs:71-105`) takes a list of redirects:
  bubblewrap binds each `<view>/<name>` at its folder's path (`:906-911`).
  `writable_dirs(view)` already covers every folder.
- Where commands can't be redirected (Seatbelt, Landlock), `command_problem`
  (`overlay/chats.rs:80-99`) refuses a command naming any of the chat's
  folders' real paths. It's applied to the connectors' commands always:
  they run unconfined (`connectors/harness.rs:272-290`), so a harness
  command could reach a folder's real path even where bubblewrap redirects
  the agent's own (section 12, item 12).
- Installs after a failing `cd web && …` install that folder's
  dependencies, not every folder's (`environment.rs:423-441`).

### 4.6 Landing

`overlay/land.rs` works on `Folders` (D24):

- **Plan**: `plan(&Folders, changes, keep_chat)` resolves each change's
  `path` and `origin` by their first parts, and merges against those
  folders; a path that names no folder is a conflict. A conflict in any
  folder means nothing is written anywhere.
- **Missing folders**: Land and Undo are refused while one of the chat's
  folders is missing, naming it (D20).
- **Locks**: each folder's `PROJECT_LOCKS` entry (`land.rs:458-468`), keyed
  and ordered by `file_guard`'s key for the folder (canonical, and
  case-folded where the system ignores case: `file_guard.rs:57-90`; today
  the path as given), then every file's (`file_guard::lock_all`, sorted).
  Project locks are only ever taken first (`land.rs:582, 1165`), and a
  folder chat takes one, so no cycle is possible. Undo takes the folders'
  locks the same way, then its files one at a time, as today
  (`land.rs:1176-1182`).
- **One journal**: `Journal` gains `folders` (their names, serde default),
  and its entries hold chat paths. `apply` (`land.rs:884-959`) writes them,
  rolling back every folder on a failure. Where a folder chat's landing
  writes `landings/<chat>/project.txt`, for recovery and Undo once the
  overlay is gone (`land.rs:891-896`; a landing discards it,
  `chats.rs:2178`), such a landing writes `landings/<chat>/folders.json`,
  each folder's name and path, which Locate… updates. With no
  `project.txt`, an older Rovik's recovery leaves it alone
  (`land.rs:1038-1040`).
- **Recovery** (`recover`: at start, `chats.rs:302`, and after Try again or
  Locate… finds a folder) and **Undo** (`overlay_undo`, `chats.rs:2286`)
  take a landing's folders from `folders.json`, else its folder from
  `project.txt` as today. Recovery finishes a folder only if the folder is
  there; otherwise none of that folder's entries are written and the
  journal stays "applying". Today a missing folder is recreated with only
  the landing's new files in it, and its other files are given up for good
  (`land.rs:792-794, 1090-1122`). The check holds for every landing; a
  folder opened alone has its landing finished at the next start.
- After a landing, `project_changed` runs once per folder.
- The bar's payload (`src/lib/overlay.ts:14-81`) keeps its types: paths are
  chat paths (`api/src/x.ts`), and `used`/`limit` the chat's budget.
- Wrappers with today's signature (`Folders::one`) keep child landings and
  today's tests as they are.

### 4.7 Checks

- **Which** (D27): the enabled checks of the folders the chat changed, the
  first part of `touched_keys()` (`repair.rs:1133-1141`, command changes
  and earlier unlanded runs included) filtering `enabled_checks()`
  (`verify.rs:601-607`), and the checks that name no folder: a plan's own
  commands (V02, `run.rs:1046-1061`) have no `cd`, and always run.
  `checks_would_run` (`landing.rs:106-112`) uses the same filter. The prompt
  lists every one of the chat's folders' checks, so the agent can run the
  others itself.
- **Where**: from the root, as `cd api && cmd`, which the allowed commands
  now pass (4.5); `run_checks` (`verify.rs:1610`) is unchanged. A folder's
  failed type check skips only that folder's tests, in every chat, not only
  in Autopilot (`verify.rs:1653-1667`).
- **Before the change**: the before-run uses a scratch view of the chat's
  folders under the same names, so its output compares with the round's.
- **Paths in output**: where today's code strips the project's path from a
  command's output, it maps through `Folders::output_roots()`, so a
  folder's real path, or its place in the view, reads as `name/`: P3.2's
  slips (`verify.rs:996-1004`; `printed_is_written`, `repair.rs:251-269`),
  V05's files behind (`verify.rs:1068-1070`), a failure's causes
  (`causes::resolve`, `causes.rs:162-184`) and the before-run's roots
  (`before.rs:232-241`). A relative path printed by `cd api && …` is tried
  as `api/<path>` first.
- File-to-command helpers that assume the root is a package
  (`js_runner(root)`, `project.rs:331, 402`: light checks, scoped tests, the
  breaker's runs, the test writer's red check) group files by folder, run
  in the folder, and wrap the result in `in_folder`.

### 4.8 Plans

- `first()` is the chat's first folder, in the workspace's order (D28): a
  plan is written into its `.rovik/plans` directly, as a project's plans
  are (`execute.rs:2087-2139`; `plan.rs:672-710`), as
  `api/.rovik/plans/<date>-<stem>.md`, with a line under its title naming
  the chat's folders (`Folders: api, web`). When the chat's first folder
  changes (unchecked, or gone from the workspace), new plans go to its new
  first folder; earlier ones stay where they were written.
- A plan is named by its chat path (`relative_plan_path` through `Folders`,
  `plan.rs:713-726`) wherever its path is shown or sent: the Plan-mode
  plan's "Saved in" line (`run.rs:1064`), the implementation request
  (`plan::expand_implementation`, `agent_run/mod.rs:1507`;
  `plan.rs:769-800`) and Autopilot's reports (`autopilot/driver.rs:469`;
  `autopilot/artifacts.rs:224`). Today those use the request's root, which
  a workspace chat doesn't have, and the Plan-mode line the view's
  (section 12, item 14).
- `Files:` lines are checked against the view, so they name folders
  (`api/src/x.ts`).
- **Implement** (D28) on a plan with a `Folders:` line uses the active
  chat if it's a workspace chat of this workspace whose folders include
  every name on the line, and otherwise starts a workspace chat across
  them, even one folder (a folder chat would read `api/src/x.ts` as
  `api/api/src/x.ts`). If this workspace lacks one of them (a folder may be
  in several workspaces, D5), it says which and starts nothing.

### 4.9 Subagents, Thorough, Autopilot, the test writer

- Subagents share the run's root, folders, context, restricted files and
  map (`run.rs:1363-1386`); workers' write scopes are chat paths
  (`worker.rs:98-160`). Nothing else changes.
- Thorough: `copies_for` (`candidates.rs:212-229`) takes the task's folders
  where it needs a root today; the main view's copies need no project once
  it's mounted (`Copies::open`, `candidates.rs:91-138, 490`); and a
  candidate's record keeps its task chat's `Folders` in place of one
  project (`add_candidate`, `candidate_project`: `agent_run/mod.rs:857-893`),
  from which its run loads every folder's context (`run.rs:1016-1024`).
  Candidates diff and adopt against the main view (`candidates.rs:148-175`).
- Autopilot carries `folders` in its `Settings`; its delivery reports what
  landing did, as for a folder chat here (`delivery.rs:63-75`), and finds a
  closed workspace chat's changes through `exists` (4.2). In WSL and on
  hosts it keeps its results for the app to land (D30).
- The test writer's red check runs a new test in its own folder (4.7); its
  `test_check()` names each changed folder's test check
  (`test_writer.rs:501-506`).

### 4.10 Connectors, diagnostics, the estimate

- Connectors run in the view (`run.rs:2259`); Claude Code's edit paths come
  out as chat paths (`claude_code.rs:1590-1593`), and its session resumes,
  the view's path being the chat's own. In their own-tools mode the chat's
  section of the prompt is given to them (`--append-system-prompt`; Codex's
  developer instructions), as the view's top has no `AGENTS.md`
  (`claude_code.rs:276-278`; `codex/mod.rs:841`).
- The editor's diagnostics are found through the folders: `name/rest` is
  looked up as (that folder's path, `rest`), and a folder chat's key as
  (its folder, key) (`agent_tools.rs:433-435, 555-571`). In an isolated
  chat of either kind they're given only for files the chat hasn't
  changed: they describe the project's version, and the age check compares
  them with the view's (`agent_tools.rs:563-600`). This fixes today's
  isolated chats, which never see them (section 12).
- The context estimate is given `folders`, and caches by the folder list
  (`project.rs:229-251`).

### 4.11 In WSL and on hosts

Carried as one project, the workspace (D30). The protocol grows by
`StartRequest.folders` (4.1) and a feature:

- `features::FOLDERS` (`rovik-protocol/src/lib.rs`), advertised by a daemon
  whose core runs workspace chats (`rovik-agentd/src/session.rs:30-37`).
- The project there is a plain project, the workspace's, with `subpath` ""
  (`work::Project`, `rovik-protocol/src/work.rs:9-16`). Each chat's
  snapshot has the chat's folders as its top-level folders, and its
  worktree follows them: a folder checked joins at the chat's next message,
  and one unchecked leaves (it has no unlanded changes there, D32).
  `ProjectEnsure`, `WorkSync`, `WorkRecheck`, `WorkStatus`, `WorkFile`,
  `WorkLanded`, `ChatBundle`, results, moves, deletion and ownership don't
  change: their paths start with a folder's name.

The daemon (small):

- A run there works in the chat's worktree, as every chat's run does there
  (`daemon.rs:403-406`), in the daemon's form (4.1): it adds each folder of
  the start's `folders` as `root/<name>`, saves each folder's settings at
  `<worktree>/<name>`, as `keep_project_settings` does for a project
  (`daemon.rs:415-422`), and keeps the names in the chat's `StartSettings`
  (serde default, `rovik-agentd/src/store.rs:177-185`) for checked merges
  and restarts.
- A result counts as unchecked only when none of the checks 4.7 picks
  would run (the result's `unchecked` is `!checks`:
  `rovik-agentd/src/results.rs:298-303`; `rovik-protocol/src/work.rs:41-45`);
  delivery of such a chat fails, saying to land the results from the app
  (`rovik-agentd/src/results.rs:164-240`).
- Files a run makes at the worktree's top, outside every folder, aren't
  brought back: the result says they stay on the host.
- A folder that leaves a chat has its folder deleted from the chat's
  worktree at the next sync, ignored files and all.
- Thorough's candidates there copy the whole worktree, as today
  (`rovik-agentd/src/candidates.rs:35-47, 89-101`), and run with the task's
  folders.

The client (`rovik-client`):

- `ChatFiles` (`remote/files.rs`, new) holds a chat's owner, name and
  `Folders`, in place of `project_of`: a folder chat is one folder named
  "" and takes today's path unchanged (a full snapshot with history and
  `subpath`). The name sent to the host comes from it, not from
  `projects::summary` (`remote/sync.rs:283`; `moving.rs:549`), which has
  none for a workspace.
- **Snapshots**: each folder is snapshotted into its own shadow as today; a
  `Composite` shadow per workspace (`shadow/composite.rs`) commits, for each
  chat, one tree with each of its folders' trees under its name, reading
  the folder shadows' objects through alternates (absolute, two levels
  deep: the composite, a folder's shadow, the folder's `.git`). One push
  and one `WorkSync` per message.
- **Ignore rules**: a folder inside a repository loses the `.gitignore`s
  above it in the composite, so `shadow/rules.rs` rebases them under the
  folder's name (global excludes, `info/exclude`, then each ancestor's
  `.gitignore`), or `node_modules` under `web/` would come back in a
  result. The rules sent are the workspace's, every folder's rebased, the
  same for all its chats: the daemon keeps one `info/exclude` per project,
  rewritten from each request's (`rovik-agentd/src/packs.rs:103-126`), so
  chats sending their own folders' rules would undo each other's.
- **Results** are fetched into the composite shadow
  (`remote/results.rs:276-299`). Each folder's part is grafted into that
  folder's own shadow by copying its new trees and blobs there (a folder's
  shadow doesn't read the composite's objects, and `configure` rewrites its
  alternates, `shadow.rs:401-407`), committing the grafted base and result
  on the folder's current tree, then imported into its part of the chat's
  overlay with today's `import` (taking the store it writes), so each
  repository's attributes (line ends) apply. A missing folder fails the
  whole import; it's never partial.
- **Landing**: the chat lands all its folders or none (4.6), then reports
  one `WorkLanded` with every folder's paths.
- **Moves**: each folder's store is folded, then composed; the move dialog
  lists the chat's folders and says that their git history stays here and
  the first push carries all their files. A missing folder blocks the move.
- **Never to a host without `FOLDERS`**: an older daemon would drop the
  field and run the chat as one plain project (`rovik-protocol` has no
  `deny_unknown_fields`). `movable` (`moving.rs:703`), which the preflight
  and every move use, checks it, and so do `start` and `sync`; the refusal
  offers the upgrade (`too_old`, `moving.rs:753-784`, with words for this
  case). On Windows a chat's first start moves it to WSL through the same
  `movable` (`go_home`, `moving.rs:275-297`), so WSL running an older Rovik
  says: "WSL runs an older Rovik, which can't run a chat across folders
  yet: update it from Settings → This computer → WSL."
- **Start**: paths in the message under a folder become `<name>/…`
  (`relative_paths`, `remote.rs:1605-1649`), and the chat's folders go as
  `RunFolder`s, each with its name and settings.

### 4.12 In the UI

- **New chat**'s menu (3.8) lets a second box, or **All folders**, be
  checked only when a workspace chat could run: not without a
  virtual-files driver here ("Needs FUSE: Settings → Virtual files"), nor,
  on Windows, while WSL isn't ready or has no `FOLDERS`. Then those boxes
  are off, with the reason on their rows; clicking it opens the page the
  agent-managed context links to (Virtual files, or WSL on Windows:
  `ChatPanel.tsx:4842`). A missing folder can't be checked; in a workspace
  chat's menu it can be unchecked when the chat has no changes there. The UI
  learns
  a host's features from a new `HostSummary.features`
  (`rovik-client/src/hosts_page.rs:147`; `src/lib/cloud.ts:131-152`), sent
  by `hosts_summaries` (`commands/hosts.rs:36`).
- A workspace chat's label opens the menu with its folders checked and
  **Apply** (D32), through `chat_set_folders`. Apply is off while the chat
  is at work or has background commands running (they're named, with a way
  to stop them), and while no folder is checked. Unchecking a folder the chat
  has unlanded changes in is refused, naming them: "Fix the API has changes
  in infra that aren't landed: land or discard them first." Checking
  **All folders** makes the chat follow the workspace again; unchecking a
  folder of a chat set to **All folders** keeps the rest checked, and stops
  it following the workspace.
- The label says "in all folders" or the folders' names; the chat's row
  says the same.
- `@`, the editor context, **Add to chat** and pasted references use
  `chatPathOf`: a file of any of the chat's folders goes in as `api/…` or
  `web/…`.
- The branch offer (D29): the chat's folders are grouped by repository
  (`git_repos`), each repository on its main branch is named in one question
  with one name, and a repository on another branch is said to stay there.
  Yes makes the branch in each in turn; one that fails says which have it,
  and Yes again makes it in the rest. A folder that joins the chat is
  offered it at the next send, when its repository is on its main branch:
  the chat's branch if it made one, else a new name.
- The isolation bar lists the changed files by chat path, with one **Land**
  and one **Undo**; conflicts open in today's resolver. While one of the
  chat's folders is missing, Land and Undo say which (D20).
- Removing a folder that a workspace chat has changes in, or locating it as
  another project, is refused, naming the chats (D19).

---

## 5. Edge cases

| Case                                                                                                          | What happens                                                                                                                                                                                                                   | Tested in                                                                  |
| ------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------- |
| **Folders**                                                                                                   |                                                                                                                                                                                                                                |                                                                            |
| The same folder twice, spelled differently (case on Windows and macOS, a trailing separator, a link to it)    | Refused: "api is already in the workspace".                                                                                                                                                                                    | `workspaces_tests.rs`; `NewWorkspaceDialog.test.tsx`                       |
| A folder inside another folder of the workspace, or containing one                                            | Refused: "web is inside api, which is in the workspace".                                                                                                                                                                       | `workspaces_tests.rs`; `NewWorkspaceDialog.test.tsx`                       |
| A copy of a member (`cp -r`, the same ID file), or a folder sharing a member's chats                          | Refused: "web-copy shares its chats with web, which is in the workspace". A member's copy question offers only **Separate history** when its original is a member.                                                             | `workspaces_tests.rs`; `projects.test.ts`                                  |
| Two folders with the same name                                                                                | Named `api` and `server-api` for good; the tree, tabs, search and chats use those, and the Git panel for a repository that's one folder.                                                                                       | `workspaces_tests.rs`                                                      |
| A folder named `build` or `.config`, or a drive root                                                          | Named `app-build`, `config`, `D`: seen by the agent's walkers.                                                                                                                                                                 | `workspaces_tests.rs`                                                      |
| A folder named `-x`, `a$b` or `~tmp`                                                                          | Named `x`, `a-b`, `-tmp` made `tmp`: `cd <name>` passes the allowed commands.                                                                                                                                                  | `workspaces_tests.rs`; `permissions` tests                                 |
| A folder removed, then added again                                                                            | It gets its old name back; no other folder ever takes it.                                                                                                                                                                      | `workspaces_tests.rs`                                                      |
| A file, a folder that's gone, or one that can't be read                                                       | Refused, with the system's reason.                                                                                                                                                                                             | `workspaces_tests.rs`                                                      |
| A folder in several workspaces                                                                                | Allowed. Only one window can have it open at a time (D7).                                                                                                                                                                      | `instances.rs` tests                                                       |
| Adding a folder another window has open (alone or in a workspace)                                             | Refused; that window comes to the front. In the Create dialog, the reason shows on the folder's row.                                                                                                                           | `instances.rs` tests; `NewWorkspaceDialog.test.tsx`                        |
| Opening a workspace when another window has one of its folders                                                | Nothing opens; that window comes to the front, and the notice names the folder.                                                                                                                                                | `instances.rs` tests; `instances.test.ts`                                  |
| Opening a folder alone while a workspace in another window has it                                             | That window comes to the front: "api is open in another window, in Shop".                                                                                                                                                      | `instances.rs` tests; `instances.test.ts`                                  |
| Opening a workspace another window has                                                                        | That window comes to the front: "Shop is open in another window", not "a copy sharing its chats".                                                                                                                              | `instances.rs` tests                                                       |
| Switching from a workspace to one of its folders, or back                                                     | The shared folder's keys are held throughout.                                                                                                                                                                                  | `instances.rs` tests                                                       |
| Two folders resolved at once                                                                                  | Both keep their `project:` keys.                                                                                                                                                                                               | `instances.rs`                                                             |
| A folder resolved after the window moved on                                                                   | It claims nothing back, and the cloud client isn't pointed at it.                                                                                                                                                              | `instances.rs`; app crate                                                  |
| Leaving a workspace while its workspace chat is at work                                                       | The workspace's key and every folder's are kept until the chat finishes.                                                                                                                                                       | `instances.rs`                                                             |
| Leaving a workspace while two of its folders' chats are at work                                               | Both folders' keys are kept until their chats finish.                                                                                                                                                                          | `instances.rs`                                                             |
| Leaving a folder opened alone while a workspace chat that uses it is at work                                  | Its keys stay held until that chat finishes.                                                                                                                                                                                   | `instances.rs`                                                             |
| Removing a folder whose own chats are at work                                                                 | They go on; the folder's keys are kept until they finish. Its chats leave the list (they're its folder's).                                                                                                                     | `instances.rs`; `chatStore.test.ts`                                        |
| Removing a folder a workspace chat has changes in or is at work in, or locating it while that chat is at work | Refused, naming the chat: "Fix the API has changes in api that aren't landed: land or discard them first."                                                                                                                     | `overlay` tests (`changes_in_folder`); app crate (`commands/workspace.rs`) |
| Removing a folder the workspace chats that work in it have no changes in                                      | It leaves their folders; a chat left with none asks for its folders before its next run.                                                                                                                                       | app crate (`commands/workspace.rs`); `workspaceChat.test.tsx`              |
| Locating a folder a workspace chat has changes in                                                             | The same project found elsewhere: the changes go with it, and its tabs point at the new place. Another project: refused while there are changes.                                                                               | app crate (`commands/workspace.rs`); `chats` tests; `FileTree.test.tsx`    |
| Removing a folder while a host with a workspace chat can't be reached                                         | Refused until the host can be asked.                                                                                                                                                                                           | app crate (`commands/workspace.rs`)                                        |
| Removing the last folder                                                                                      | Refused: "A workspace keeps at least one folder. Delete it instead."                                                                                                                                                           | `workspaces_tests.rs`                                                      |
| A folder missing when the workspace opens (deleted, a drive or share not mounted)                             | Its row shows it missing, with Locate…, Try again and Remove from workspace; the others open. It isn't resolved, its chats aren't listed, and the workspace chats that work in it don't start, land or undo, until it's back.  | `workspaces_tests.rs`; `switchTarget.test.ts`; `openFolders.test.ts`       |
| A missing folder's drive mounted again                                                                        | Found as the window comes to the front, or with **Try again**.                                                                                                                                                                 | `switchTarget.test.ts`                                                     |
| A folder deleted or renamed while open                                                                        | Its row turns missing; its tabs show [DELETED], as tabs do now.                                                                                                                                                                | `watch.rs` tests; `FileTree.test.tsx`                                      |
| A folder that can't be watched (unreadable, a share without events)                                           | The other folders are watched; its tree refreshes as its rows are expanded.                                                                                                                                                    | `watch.rs` tests                                                           |
| A folder opened through a link (macOS: `/tmp` is `/private/tmp`)                                              | Its changes are reported under the folder's own path.                                                                                                                                                                          | `watch.rs` tests                                                           |
| Deleting a workspace                                                                                          | Its own chats are deleted (the question says how many); its folders and their chats aren't touched. Refused while another window has it open.                                                                                  | `projects_tests.rs`; `instances.rs`; `ProjectList.test.tsx`                |
| Deleting the project of one of the window's folders from the list                                             | Not offered, and `chat_delete_project` refuses it.                                                                                                                                                                             | `ProjectList.test.tsx`; `chat_db` `api` tests                              |
| A drive root (`D:\`) as a folder                                                                              | Works; a change at its top refreshes it.                                                                                                                                                                                       | `fileTools.test.ts`; `fileTree.test.ts`                                    |
| A folder in WSL (`\\wsl.localhost\…`) beside a Windows folder                                                 | Each is a project as it would be alone: its chats run in WSL from its snapshot.                                                                                                                                                | By hand (Windows)                                                          |
| **Tree and files**                                                                                            |                                                                                                                                                                                                                                |                                                                            |
| A file dragged from one folder to another                                                                     | Moved (across drives: copied, then deleted); its tab follows.                                                                                                                                                                  | `FileTree.test.tsx`; `entries.rs` tests                                    |
| A folder's row: Rename, Delete, Cut or Copy from the menu, Mod+Backspace, Mod+X, a drag                       | None of them acts on it; the backend refuses deleting, renaming or moving a window's folder.                                                                                                                                   | `FileTree.test.tsx`; `entries.rs` tests                                    |
| A link in one folder to another (`web/node_modules/@shop/api → ../../api`)                                    | Its rows are told apart from api's own.                                                                                                                                                                                        | `fileTree.test.ts`                                                         |
| New file with nothing selected                                                                                | In the active folder.                                                                                                                                                                                                          | `openFolders.test.ts`                                                      |
| **Search, `@`, tabs, IntelliSense**                                                                           |                                                                                                                                                                                                                                |                                                                            |
| The same relative path in two folders                                                                         | Two groups; each result opens its own file.                                                                                                                                                                                    | `searchPanel.test.tsx`                                                     |
| Two tabs with the same file name                                                                              | Told apart by their folders.                                                                                                                                                                                                   | `Tabs.test.tsx`                                                            |
| A saved tab from a folder that left the workspace                                                             | Not reopened.                                                                                                                                                                                                                  | `projectSessions.test.ts`                                                  |
| A TSX file in the second folder                                                                               | Gets its folder's `jsx` and `lib`, not the first folder's.                                                                                                                                                                     | `activeConfig.test.ts`                                                     |
| **Git**                                                                                                       |                                                                                                                                                                                                                                |                                                                            |
| Two folders in one repository                                                                                 | One section listing both folders' changes, named after the repository's folder.                                                                                                                                                | `git/tests.rs`; `GitPanel.test.tsx`                                        |
| A folder inside a repository                                                                                  | Its changes only, as today; the editor's marks compare with the right file (fixed).                                                                                                                                            | `git/tests.rs`; `gitMarks.test.ts`                                         |
| Folders `app` and `app2` of one repository, only `app` in the workspace                                       | `app2`'s changes aren't listed or committed.                                                                                                                                                                                   | `git/tests.rs`                                                             |
| A folder that isn't a repository but holds some                                                               | "Not a repository", with **Initialize repository**, as the panel says today. Each repository added as a folder gets its section.                                                                                               | `GitPanel.test.tsx`                                                        |
| A repository inside a folder's repository (untracked or ignored)                                              | The outer one never lists or stages it, so its Commit works and its row's revert can't trash it (fixed).                                                                                                                       | `git/tests.rs`                                                             |
| A submodule                                                                                                   | The outer repository lists it only when its commit moved; no empty commits (fixed).                                                                                                                                            | `git/tests.rs`                                                             |
| A linked worktree beside its main folder                                                                      | Two sections.                                                                                                                                                                                                                  | `git/tests.rs`                                                             |
| A detached HEAD                                                                                               | Its header says so; the commit goes on it, as git's would.                                                                                                                                                                     | `git/tests.rs`                                                             |
| A new repository with no commit                                                                               | The commit makes its branch.                                                                                                                                                                                                   | `git/tests.rs`                                                             |
| A merge, a rebase or conflicts in one repository                                                              | Its box is off, saying why; the others commit.                                                                                                                                                                                 | `GitPanel.test.tsx`                                                        |
| Changes staged from a terminal outside the window's folders                                                   | Nothing is committed anywhere; the problem names them; they stay staged (fixed).                                                                                                                                               | `git/tests.rs`                                                             |
| pre-commit fails in one repository                                                                            | Nothing is committed anywhere; its output and **Commit without hooks** show, which skips every repository's hooks.                                                                                                             | `git/tests.rs`                                                             |
| commit-msg rewrites the message in one repository                                                             | Only that repository's commit has the change.                                                                                                                                                                                  | `git/tests.rs`                                                             |
| A commit fails after others were written (a terminal holds the third repository's `refs/heads/<branch>.lock`) | "Committed in api and web; infra: …"; the message stays, nothing is pushed until infra commits too, and then all three are.                                                                                                    | `git/tests.rs`; `GitPanel.test.tsx`                                        |
| A branch switched, or a push, from a terminal in a folder's repository                                        | The section updates; during a checkout, the isolated chats of that repository's folders wait.                                                                                                                                  | `watch.rs` tests; `overlay` tests (`is_busy`)                              |
| One push rejected                                                                                             | That section says so; the others are pushed.                                                                                                                                                                                   | `GitPanel.test.tsx`                                                        |
| A repository that was only ahead when Commit is clicked                                                       | Not pushed by that commit; the button offers Push for it afterwards.                                                                                                                                                           | `GitPanel.test.tsx`                                                        |
| **New chat**                                                                                                  |                                                                                                                                                                                                                                |                                                                            |
| **New chat** with several folders                                                                             | A menu: All folders, then a box per folder, the active one checked; Start chat makes the chat, and Escape makes none. With one folder, no menu, as today.                                                                      | `workspaceChat.test.tsx`                                                   |
| One folder checked                                                                                            | A folder chat in it, as when the folder is opened alone.                                                                                                                                                                       | `workspaceChat.test.tsx`                                                   |
| api and web checked                                                                                           | A workspace chat across them; infra isn't in its view.                                                                                                                                                                         | `workspaceChat.test.tsx`; `workspace_e2e_tests.rs`                         |
| All folders checked, then a folder added to the workspace                                                     | It joins the chat at its next run.                                                                                                                                                                                             | `overlay` composite tests; `workspace_e2e_tests.rs`                        |
| **New chat** while an empty chat is open                                                                      | The empty chat is reused, moved to the choice; its draft stays. A chat with a message is never moved.                                                                                                                          | `workspaceChat.test.tsx`; `chat_db/tests.rs`                               |
| No chat yet, or the last one deleted                                                                          | The chat panel shows the menu to start a chat; no empty chat is made in guessed folders.                                                                                                                                       | `workspaceChat.test.tsx`                                                   |
| A missing folder in the menu                                                                                  | Listed, but can't be checked.                                                                                                                                                                                                  | `workspaceChat.test.tsx`                                                   |
| No virtual-files driver                                                                                       | A second box and All folders can't be checked, saying why; a workspace chat made before is refused at start, naming Settings → Virtual files.                                                                                  | `workspaceChat.test.tsx`; `workspace_e2e_tests.rs`                         |
| **Folder chats**                                                                                              |                                                                                                                                                                                                                                |                                                                            |
| A folder resolving, added, removed or located while a chat is open                                            | The open chat and its draft stay (unless its folder left).                                                                                                                                                                     | `chatStore.test.ts`                                                        |
| A crash with an unanswered message in the second folder's chat, or a workspace chat                           | It comes back into the input when the workspace opens.                                                                                                                                                                         | `workspaceChat.test.tsx`                                                   |
| Another folder's file in a folder chat's editor context, `@` or **Add to chat**                               | Not sent; the composer says why.                                                                                                                                                                                               | `workspaceChat.test.tsx`                                                   |
| A file of a folder a workspace chat doesn't work in, in its editor context, `@` or **Add to chat**            | Not sent; the composer says why.                                                                                                                                                                                               | `workspaceChat.test.tsx`                                                   |
| A folder chat's label                                                                                         | Shows its folder; it isn't a control, and its folder can't change.                                                                                                                                                             | `workspaceChat.test.tsx`                                                   |
| **Implement** on another folder's plan while a folder chat is open                                            | A new chat in the plan's folder implements it.                                                                                                                                                                                 | `workspaceChat.test.tsx`                                                   |
| A new chat on a folder's main branch                                                                          | The branch offer asks about that folder's repository.                                                                                                                                                                          |
| A folder checked for a workspace chat whose repository is on its main branch                                  | At the next send, the chat's branch is offered for it.                                                                                                                                                                         | `chatBranchOffer.test.tsx`                                                 |
| A folder chat on a host or in WSL                                                                             | As when its folder is open alone; its results land while the workspace is open.                                                                                                                                                | `rovik-client` `remote/tests.rs`                                           |
| Two folders that look like copies or moved projects                                                           | Each is asked about in a notification of its own, naming it.                                                                                                                                                                   | `projects.test.ts`                                                         |
| **Workspace chats**                                                                                           |                                                                                                                                                                                                                                |                                                                            |
| A workspace chat reads, edits and runs commands in two folders                                                | Its paths start with the folders' names; `cd api && npm test` runs in api's view.                                                                                                                                              | `workspace_e2e_tests.rs`                                                   |
| `cd api && npm test` with the default allowed commands                                                        | Runs; `cd .. && npm test`, `cd /tmp && ls` and `cd api && rm x` don't. The same in a folder chat (`cd src && npm test`).                                                                                                       | `permissions` tests                                                        |
| A folder checked for a workspace chat between runs                                                            | It joins the view at the next run; earlier messages keep their paths, and the agent is told it joined.                                                                                                                         | `workspace_e2e_tests.rs`; `overlay` composite tests                        |
| A folder unchecked that the chat has no unlanded changes in                                                   | It leaves the view at the next run, and the agent is told.                                                                                                                                                                     | `workspace_e2e_tests.rs`                                                   |
| A folder unchecked that the chat has unlanded changes in                                                      | Refused, naming them.                                                                                                                                                                                                          | app crate (`commands/workspace.rs`); `workspaceChat.test.tsx`              |
| A folder unchecked, or removed from the workspace, while the chat has a dev server running                    | Refused, naming the background commands and offering to stop them.                                                                                                                                                             | app crate (`commands/workspace.rs`); `workspaceChat.test.tsx`              |
| A missing folder the chat has no changes in                                                                   | It can be unchecked, so the chat runs without it.                                                                                                                                                                              | `workspaceChat.test.tsx`                                                   |
| A workspace chat's folders changed while it's at work, or while its Autopilot task waits on a rate limit      | Refused until the run, or the task, ends.                                                                                                                                                                                      | app crate (`commands/workspace.rs`); `workspaceChat.test.tsx`              |
| Every folder unchecked                                                                                        | Apply stays off: a chat keeps at least one folder.                                                                                                                                                                             | `workspaceChat.test.tsx`                                                   |
| A folder unchecked from a chat set to All folders                                                             | The rest stay checked, and the chat no longer takes in folders added later.                                                                                                                                                    | `workspaceChat.test.tsx`                                                   |
| Its changes in two folders                                                                                    | Land in both, under one landing; **Undo** puts both back.                                                                                                                                                                      | `overlay` land tests; `workspace_e2e_tests.rs`                             |
| A conflict in one folder                                                                                      | Nothing lands in either; the conflict shows under its folder's name.                                                                                                                                                           | `overlay` land tests                                                       |
| A crash while landing in two folders                                                                          | The landing is finished at the next start, in both.                                                                                                                                                                            | `overlay` land tests                                                       |
| Undo after a landing in two folders, once the chat's changes are gone                                         | Both folders are put back.                                                                                                                                                                                                     | `overlay` land tests                                                       |
| A folder missing when an interrupted landing is finished                                                      | Nothing is written in its place and it isn't recreated; its part of the landing is finished once the folder is back, where it's found.                                                                                         | `overlay` land tests                                                       |
| Land or Undo while one of the chat's folders is missing                                                       | Refused, naming the folder.                                                                                                                                                                                                    | `workspace_e2e_tests.rs`                                                   |
| `mv api/x web/x` in a command                                                                                 | Copied and deleted (EXDEV): web shows an add, api a delete.                                                                                                                                                                    | `overlay` composite tests (mounted)                                        |
| A file made at the view's top                                                                                 | Refused: the top is read-only.                                                                                                                                                                                                 | `overlay` composite tests                                                  |
| A command naming a folder's real path                                                                         | Under bubblewrap it reaches the view; elsewhere it's refused by text, a connector's too.                                                                                                                                       | `confine.rs` tests; `workspace_e2e_tests.rs`                               |
| Only api changed                                                                                              | Only api's checks run, in api, and a plan's own commands; a failure that was there before is set apart.                                                                                                                        | `workspace_e2e_tests.rs`                                                   |
| api's type check fails                                                                                        | web's tests still run.                                                                                                                                                                                                         | `workspace_e2e_tests.rs`                                                   |
| A check prints a folder's real path                                                                           | It's read as that folder's file in the chat.                                                                                                                                                                                   | `workspace_e2e_tests.rs`                                                   |
| A restricted file in one folder                                                                               | Refused by its path in the view; the other folder's file of that name isn't restricted. A folder named `examples` keeps its key files restricted.                                                                              | `workspace_e2e_tests.rs`                                                   |
| Each folder's `AGENTS.md`                                                                                     | Given under its name, within the shared limit, evenly.                                                                                                                                                                         | `workspace_e2e_tests.rs`                                                   |
| A plan from a workspace chat                                                                                  | Saved in the chat's first folder's `.rovik/plans` with its `Folders:` line; its `Files:` name folders; **Implement** on it uses a workspace chat whose folders include the plan's, or starts one across them, even one folder. | `workspace_e2e_tests.rs`; `plans.test.ts`                                  |
| The chat's first folder leaves it                                                                             | New plans go to its new first folder; earlier ones stay where they were written.                                                                                                                                               | `workspace_e2e_tests.rs`                                                   |
| A subagent's worker in a workspace chat                                                                       | Writes only its step's paths, through the view, into the right folder.                                                                                                                                                         | `workspace_e2e_tests.rs`                                                   |
| Thorough in a workspace chat                                                                                  | Its candidates work on copies of the whole view, with every one of its folders' context.                                                                                                                                       | `workspace_e2e_tests.rs`                                                   |
| Two chats changing one file (a folder chat and a workspace chat)                                              | Each sees the other's clash; refreshing one folder keeps the other folder's clashes; both land, one after the other.                                                                                                           | `overlay` chats and land tests                                             |
| A request whose folders aren't the chat's, or a workspace chat's request with a root alone or nothing         | Refused.                                                                                                                                                                                                                       | `agent_run` `check_project` tests                                          |
| The editor's diagnostics                                                                                      | Found by the agent through the view, for files the chat hasn't changed (fixed for isolated folder chats too).                                                                                                                  | `workspace_e2e_tests.rs`                                                   |
| Claude Code in a workspace chat                                                                               | Works in the view, keeps its session, and is told about the folders.                                                                                                                                                           | `workspace_e2e_tests.rs`                                                   |
| On Windows, a workspace chat                                                                                  | Runs in WSL as one project; WSL on an older Rovik gets no such chat, and says how to update.                                                                                                                                   | `rovik-agentd/tests/folders.rs`; `remote/tests.rs`                         |
| Moving a workspace chat to a cloud host                                                                       | The dialog lists its folders and says their git history stays here; an older host is offered its upgrade; a missing folder blocks the move.                                                                                    | `chatLocation.test.tsx`; `remote/tests.rs`                                 |
| A workspace chat on a host gains or loses a folder                                                            | Its worktree follows at its next message; a folder that left is deleted from it, ignored files and all.                                                                                                                        |
| Two workspace chats of one workspace on one host, with different folders                                      | Both use the workspace's ignore rules, so neither's result takes the other's by-products.                                                                                                                                      | `rovik-agentd/tests/folders.rs`                                            |
| A folder inside a repository, on a host                                                                       | Its ancestors' ignore rules still keep `node_modules` out of the chat's results.                                                                                                                                               | `shadow/rules.rs` tests; `rovik-agentd/tests/folders.rs`                   |
| A run on a host makes a file at the worktree's top                                                            | It isn't brought back; the result says it stays there.                                                                                                                                                                         | `rovik-agentd/tests/folders.rs`                                            |
| A check fails on a host                                                                                       | It's reported as failing: there's no code before the change to compare with there, as today.                                                                                                                                   | `rovik-agentd/tests/folders.rs`                                            |
| Autopilot's delivery in a workspace chat on a host                                                            | Its results wait for the app to land them, saying why.                                                                                                                                                                         | `rovik-agentd/tests/folders.rs`                                            |
| **Saved state**                                                                                               |                                                                                                                                                                                                                                |                                                                            |
| Quitting and starting again                                                                                   | The workspace and its tabs come back.                                                                                                                                                                                          | e2e: restart                                                               |
| An update's restart                                                                                           | The workspace opens again.                                                                                                                                                                                                     | `updates.rs` tests                                                         |
| The last workspace open in another window at start                                                            | This window starts with nothing open, as with a project.                                                                                                                                                                       | `switchTarget.test.ts`                                                     |
| Creating a workspace from the open project                                                                    | Its tabs, its running terminals and its commit message come along.                                                                                                                                                             | `terminalPanel.test.tsx`; `GitPanel.test.tsx`                              |
| **Terminals**                                                                                                 |                                                                                                                                                                                                                                |                                                                            |
| A new terminal in a workspace                                                                                 | **+** asks which folder; the terminal starts there.                                                                                                                                                                            | `terminalPanel.test.tsx`                                                   |
| A folder removed with terminals in it                                                                         | They keep running.                                                                                                                                                                                                             | `terminalPanel.test.tsx`                                                   |

---

## 6. Milestones

| Milestone                                  | What it delivers                                                                                                                                                                                                                                                                    | Done when                                                                                                                                              |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **A. A project as a list of one**          | One copy of the path helpers; `openFolders` with one folder; the watcher, the tree's controller, git's scopes and the editor's services take folders; the repository list; commits that never take what's staged outside; nested repositories left alone; the editor's marks fixed. | Every existing test passes unchanged, the new ones pass with two folders, and the e2e run is unchanged. Nothing changes for a user but the fixed bugs. |
| **B. Workspaces stored and opened**        | Schema 11 and `workspaces.rs`, the commands, the claims, `switchTarget`, what's saved, `--workspace`, the guards on the window's projects.                                                                                                                                          | Their tests pass; a workspace opens in `switchTarget.test.ts`. No menu yet, so nothing creates one.                                                    |
| **C. A window with several folders**       | Folder rows and their guards, Remove, Locate and Try again, search, tabs, terminals, the settings pages, IntelliSense and rust-analyzer.                                                                                                                                            | The tests in C01–C07 pass.                                                                                                                             |
| **R. Git across repositories**             | Sections, the commit across repositories, refreshing by repository.                                                                                                                                                                                                                 | The git tests in R02 and R03 pass, and GitPanel's tests with one repository pass unchanged.                                                            |
| **E. Folder chats in a workspace**         | The chat list of several owners, New chat's menu for one folder, a chat's folders, what's sent, the isolation bar, the cloud client's owners.                                                                                                                                       | The tests in E01–E04 pass.                                                                                                                             |
| **F. Workspace chats on this computer**    | `Folders`, the composite view, landing across folders, context, the `cd` rule and the sandbox, checks, plans, subagents, Thorough, connectors, and the UI: several folders or All folders in New chat, and a chat's folders changed between runs.                                   | The overlay, land and run tests of F01–F14 pass, with FUSE on Linux CI and macFUSE by hand.                                                            |
| **G. Workspace chats in WSL and on hosts** | The `FOLDERS` feature, the daemon's form, the composite snapshot per chat and its ignore rules, results per folder, moves and the dialog.                                                                                                                                           | `rovik-agentd/tests/folders.rs` and the client's tests pass.                                                                                           |
| **H. The menu**                            | The menu items, the dialog, workspaces in the list, the README, the e2e steps.                                                                                                                                                                                                      | The e2e steps in section 8 pass on Linux, macOS and Windows, and step 6 lands a workspace chat's changes on Linux.                                     |

Every merge to master is released (`release.yml:1`), so each task is safe to
ship alone. Until H no window has more than one folder, so New chat has
nothing to check and nothing offers a workspace chat. What a user of a
project sees change before H: the fixed bugs (section 12), tab titles told
apart (C03), the "Packages (N)" heading (C05), and a `cd` within the project
passing the allowed commands (F09). B01's schema step is permanent once
released (3.1). Milestone R's letter keeps its tasks apart from the
decisions D1–D32. Sizes: A M, B M, C M, R L, E M, F XL, G L, H S.

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
- A task that changes what a user sees updates the README in the same
  commit.
- Write each task's notes under "Notes from implementation" at the end of
  this file.

| Task | Files                                                                                                                                                                                                                                                                                                                                          | What                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Tests                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ---- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| A01  | `src/lib/fileTools.ts`, `src/lib/fileTree.ts`, `src/components/FileTree.tsx`, `src/components/NewProjectDialog.tsx`, `src/lib/tsconfig.ts`, `src/App.tsx`                                                                                                                                                                                      | One `isAtOrUnder`, `parentOf` and `samePath` in `fileTools.ts`, with the platform's case rule, drive roots and UNC roots, in place of today's copies: three of `isAtOrUnder`, three of a path's parent, and `tsconfig.ts`'s `samePath` and `startsWithPath`.                                                                                                                                                                                                                                                                                                                                                                          | `fileTools.test.ts`: POSIX and Windows paths; `D:\x`'s parent is `D:\`; `\\server\share`; case ignored on Windows and macOS only. `fileTree.test.ts`: a change at the top of `D:\` refreshes it. `tsconfig.test.ts` passes unchanged.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| A02  | `src/lib/openFolders.ts` (new), `src/App.tsx`                                                                                                                                                                                                                                                                                                  | `WindowTarget`, `target()` in place of `rootPath`, `openFolders`, `folderOf`, `activeFolder`, `sessionKey`. App.tsx's 28 uses of `rootPath()` go through them (a project is a list of one).                                                                                                                                                                                                                                                                                                                                                                                                                                           | `openFolders.test.ts`: `folderOf` with two folders and with none; `activeFolder`'s order; `sessionKey`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| A03  | `src-tauri/src/filesystem/watch.rs`, `src-tauri/src/filesystem/mod.rs`, `src/lib/commands.ts`, `src/App.tsx`                                                                                                                                                                                                                                   | `start_file_watch(paths)`: a watch and ignore rules per folder, one folder's error not stopping the others; a folder deleted or renamed reported as gone; paths under each folder's own spelling; `unknown` names its folder; `git-changed { folder }`; a repository's git folder outside the folder, and its `refs/`, watched; `project_busy` on the working folder; the tree's matcher by folder.                                                                                                                                                                                                                                   | `watch.rs`: changes in two folders are both reported; an unwatchable folder leaves the other watched; a watched folder that's deleted is reported gone; an overflow names its folder; a commit and a push from a terminal in a repository above a folder send `git-changed` for it; a folder opened through a link reports under its own path; a new set of folders stops the old watches.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| A04  | `src/lib/fileTree.ts`, `src/components/FileTree.tsx`, `src/App.tsx`                                                                                                                                                                                                                                                                            | The controller takes the roots and whether they're rows; each folder's listing uses its own root; rows keyed by folder and file id; `rootId` per folder. With one root and no rows, the rows are today's.                                                                                                                                                                                                                                                                                                                                                                                                                             | `fileTree.test.ts`: two roots as rows, each expanded, refreshed and moved across; a link in one folder to the other lists rows of its own; one root lists as before.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| A05  | `src-tauri/src/git/repos.rs` (new), `git/mod.rs`, `git/changes.rs`, `src-tauri/src/util.rs`, `rovik-core/src/overlay/chats.rs`, `src-tauri/src/lib.rs`                                                                                                                                                                                         | `git_repos(folders)`, with each repository's name; `scopes` on `git_status`, `git_commit` and `git_discard_all` (by default today's one), matched as pathspecs; line counts scoped; `rewriting` holds the working folder, and `is_busy` counts holds above and below.                                                                                                                                                                                                                                                                                                                                                                 | `git/tests.rs`: two subfolders of one repository are one repository with two scopes; a linked worktree is its own; a folder in none says so; a repository that's one folder takes its stored name; a status with two scopes lists both and nothing beside; `app2` isn't taken with `app`; line counts stay in scope. `overlay` tests: a hold on a working folder makes a folder inside it busy, and one beside it not.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| A06  | `git/changes.rs`, `git/mod.rs`, `README.md`                                                                                                                                                                                                                                                                                                    | Commit refused while something is staged outside the scopes; status leaves out nested repositories and submodules changed only inside; revert refuses both. The README's Git panel section says a commit is refused while changes are staged outside the folder (`README.md:77-80`).                                                                                                                                                                                                                                                                                                                                                  | `git/tests.rs`: a change staged outside the folder stops the commit and stays staged; a nested repository doesn't stop a commit and isn't listed; its revert is refused; a submodule changed only inside isn't listed, and no empty commit is made.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| A07  | `src/App.tsx`, `src/lib/gitMarks.ts`, `src/components/GitPanel.tsx`                                                                                                                                                                                                                                                                            | The editor's marks ask with the path relative to the file's repository; GitPanel's `onOpenFile` keeps the file it's given; tabs not on disk aren't refreshed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | `gitMarks.test.ts` (new): in a project at `repo/app`, `package.json` is asked as `app/package.json`; `disk:` and `chat:` tabs keep their bases on a refresh.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| A08  | `src/components/Editor.tsx`, `src/lib/fileFormatters.ts`, `src/lib/markdownPreview.ts`, `src/lib/diagnostics.ts`                                                                                                                                                                                                                               | The formatter's root, the Markdown preview's `/` and the diagnostics' root are the file's folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | `fileFormatters.test.ts` (new): with two folders, a file of the second is formatted with the second's config. `markdownPreview.test.ts`: its `/` links resolve in the second. `diagnostics.test.ts` (new): its markers are reported for the second.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| B01  | `rovik-core/src/chat_db/schema.rs`, `chat_db/workspaces.rs` (new), `chat_db/workspaces_tests.rs` (new), `chat_db/projects.rs`, `chat_db/projects_tests.rs`, `chat_db/store.rs`, `chat_db/rev_tests.rs`                                                                                                                                         | Schema 11 (3.1); `workspaces.rs`, with `folder_name` and the file system read before the transaction; `list`/`summary` by kind; `dialogs.folders`, written by `create_dialog` from `NewDialog.folders` and changed by `set_dialog_folders`; `DialogSummary.projectId` and `folders`. Merged once this plan is approved.                                                                                                                                                                                                                                                                                                               | `upgrades_a_version_10_database`. `workspaces_tests.rs`: each rule of D5 (a symlink, a copy's ID); names (a clash takes its parent's, then a number; case, `Ä` against `ä` too; `build`, `@context`, a drive root, 300 characters; `$`, `~`, a backtick, `*` and a leading `-` made plain; a removed name never reused; a re-added folder's name back; locating keeps it); the last folder stays; removing a folder takes it out of the folders of the chats that had it; list order and `missing`. `projects_tests.rs`: a workspace isn't listed or summarised as a project; deleting a workspace deletes its own chats and leaves its folders' projects. `chat_db/tests.rs`: a chat's summary carries its project and folders; a workspace chat is made with its folders, `"all"` or names, and they read back. `rev_tests.rs`: setting a chat's folders bumps its `rev`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| B02  | `chat_db/api.rs`, `src-tauri/src/commands/workspace.rs` (new), `commands/mod.rs`, `src-tauri/src/lib.rs`, `src/lib/chatApi.ts`, `src/types.ts`                                                                                                                                                                                                 | The commands, refusing to remove the last folder; `chatApi.ts`'s calls. (D19's guard comes with F06, `chat_set_folders` with F14.)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | `chat_db` `api` tests: the commands' calls reach `workspaces.rs`, and removing the last folder is refused. `instances.rs`: a workspace another window has open counts as open elsewhere, so deleting it is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| B03  | `src-tauri/src/instances.rs`, `src-tauri/src/lib.rs` (`project_claim`, `project_check`, `at_work_in`), `src-tauri/src/commands/chat.rs` (`reclaim`), `src/lib/instances.ts`                                                                                                                                                                    | Claim targets (D7): the stored target, the record per owner, keys under the lock and without duplicates, the keys held recomputed as the target's and every at-work owner's, `reclaim(root, id)`, holders with the workspace's name, `elsewhere()`'s copy only with a folder, the notices.                                                                                                                                                                                                                                                                                                                                            | `instances.rs`: a workspace takes its key and its folders' or none; a target's keys have no duplicates; a folder another process has stops it and is named; adding and removing a folder keep the others' keys; two folders resolved at once keep their keys; a folder resolved after the window moved on takes nothing back; leaving a workspace with its own chat at work keeps it and its folders; with two folders' chats at work keeps both; leaving a folder opened alone keeps its keys while a workspace chat that uses it is at work; a workspace open elsewhere isn't reported as a copy. `instances.test.ts`: the notices.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| B04  | `src/lib/switchTarget.ts` (new), `src/lib/projects.ts`, `src/App.tsx`                                                                                                                                                                                                                                                                          | `switchTarget(target)` for a folder or a workspace (3.3), its steps as calls it's given; each folder resolved, with a notification and a count of runs of its own; missing folders not resolved, and tried again on focus and Try again.                                                                                                                                                                                                                                                                                                                                                                                              | `switchTarget.test.ts`: the steps' order; a refused claim changes nothing; a cancelled unsaved tab stops the switch; a missing folder isn't resolved, and is when it's back; a target taken elsewhere at start leaves the window empty. `projects.test.ts` (new): two folders' copy questions are both asked, each naming its folder; when a copy's original is a member, the question offers only Separate history.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| B05  | `src-tauri/src/filesystem/mod.rs`, `src/lib/projectHistory.ts`, `src/lib/projectSessions.ts`, `src-tauri/src/lib.rs` (`--workspace`), `src-tauri/src/updates.rs`, `src/lib/updates.ts`                                                                                                                                                         | What's saved and the restart (D17).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | `updates.rs`: `restart_args` with a workspace passes `--workspace`. `projectSessions.test.ts`: a workspace's entry, and a tab of a folder that left isn't restored. A `last-project.json` without `workspace` reads as before.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| B06  | `src/components/ProjectList.tsx`, `src/App.tsx`, `rovik-core/src/chat_db/api.rs`, `src-tauri/src/commands/chat.rs`                                                                                                                                                                                                                             | Delete hidden for every project the window has; `chat_delete_project` refuses one this window has.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | `ProjectList.test.tsx` (new): no Delete on any of the window's projects. `chat_db` `api` tests: deleting a project this window has open is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| C01  | `src/components/FileTree.tsx`, `src/components/FileContextMenu.tsx`, `src/App.tsx`, `src-tauri/src/filesystem/entries.rs`, `src/styles.css`                                                                                                                                                                                                    | Folder rows (name, missing); their menu (Remove from workspace, Locate…, Try again); no rename, delete, cut, copy or drag of them by menu, keyboard or mouse; the backend's refusal; dropping on a folder's row; the project-root target for a project only; Remove (tabs settled) and Locate… (tabs pointed at the new place), and the claim.                                                                                                                                                                                                                                                                                        | `FileTree.test.tsx`: a folder row's menu; "Mod+Backspace on a folder row deletes nothing"; "a folder row can't be dragged"; a drop on it moves into that folder; a missing row's actions; Locate… points the folder's tabs at its new place. `entries.rs`: deleting, renaming or moving a watched folder is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| C02  | `src/components/SearchPanel.tsx`, `src/components/searchPanel.test.tsx` (new)                                                                                                                                                                                                                                                                  | A search per folder, grouped by folder, one limit, results opened from their folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Two folders' results under their headers; the same relative path opens two files; one folder looks as today.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| C03  | `src/components/Tabs.tsx`, `src/lib/editorTabs.ts`                                                                                                                                                                                                                                                                                             | Colliding titles told apart.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | `Tabs.test.tsx`: two `package.json` show their folders; a unique name shows alone.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| C04  | `src/components/TerminalPanel.tsx`, `src/components/FolderMenu.tsx` (new)                                                                                                                                                                                                                                                                      | Grouped by `sessionKey`; **+** asks for the folder in a workspace (`FolderMenu` as a list to pick one from, with a slot for a caller's own items); the tooltip.                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | `terminalPanel.test.tsx`: the menu lists the folders, and the terminal starts in the one picked; a removed folder's terminal keeps running.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| C05  | `src/components/SettingsDialog.tsx`, `ProjectChecksSettings.tsx`, `RestrictedFilesSettings.tsx`, `SettingsMenu.tsx`                                                                                                                                                                                                                            | The folder select in a workspace; "Packages (N)".                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | `SettingsDialog.test.tsx`: the select, and the page following it. `ProjectChecksSettings.test.tsx`: the heading. `RestrictedFilesSettings.test.tsx`: the selected folder's list.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| C06  | `src/lib/intellisense/index.ts`, `src/lib/intellisense/project.ts`, `src/lib/intellisense/activeConfig.ts`, `src/lib/intellisense/compilerOptions.ts`, `src-tauri/src/typings.rs`                                                                                                                                                              | The worker loads every folder; `owns` any; each folder's options kept, the active file's folder's applied; the limits shared.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         | `activeConfig.test.ts` (new): a TSX file in the second folder gets its folder's `jsx` and `lib`. `projectWorker.test.ts`: adding a folder loads its sources.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| C07  | `src/lib/lsp/client.ts`, `src/lib/lsp/rustAnalyzer.ts`, `src/components/Editor.tsx`, `src/App.tsx`                                                                                                                                                                                                                                             | rust-analyzer with every folder; the `workspace/workspaceFolders` answer; a restart when the folders change.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | `lsp/client.test.ts` (new): `initialize` lists both folders; the request is answered with them.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| R01  | `src/components/GitPanel.tsx`, `src/components/GitRepoSection.tsx` (new), `GitHooksView.tsx`, `ConflictResolver.tsx`, `src-tauri/src/git/history.rs`                                                                                                                                                                                           | GitPanel split into the window's part and a section per repository; one repository looks as today; drafts, publish answers and `GIT_LOG_LATEST` by repository; `opKey` with the repository; folders without one.                                                                                                                                                                                                                                                                                                                                                                                                                      | `GitPanel.test.tsx`: one repository renders as before; two show two sections with their names and branches. The `gitBranchMenu`, `gitRebase` and `conflictResolver` tests pass unchanged. `history.rs`: two repositories' logs don't stop each other.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| R02  | `git/changes.rs` (`git_commit_many`), `git/hooks.rs`, `src/lib/gitPublish.ts`, `GitPanel.tsx`, `GitRepoSection.tsx`                                                                                                                                                                                                                            | The commit across repositories (D11), holding each repository; the include boxes; `commitAction(statuses)`; pushing what was committed, and nothing after a partial failure; one publish question.                                                                                                                                                                                                                                                                                                                                                                                                                                    | `git/tests.rs`: a pre-commit failing in the second repository leaves the first uncommitted; without hooks, none runs in any; a commit-msg hook changes its repository's message only; each commit goes on its own branch; a detached HEAD gets its commit; a repository with no commit gets its branch; a held `refs/heads/<branch>.lock` in the third reports the first two. `gitPublish.test.ts`: the actions over several. `GitPanel.test.tsx`: a repository in a merge is left out, saying why; a partial failure keeps the message and pushes nothing, and after a retry commits the rest, every repository of the message is pushed; a rejected push shows in its section; a repository that was only ahead isn't pushed by a commit.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| R03  | `GitPanel.tsx`, `src/App.tsx`                                                                                                                                                                                                                                                                                                                  | A folder's `git-changed` refreshes its repository's section; the list found again for a new repository.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | `GitPanel.test.tsx`: an event refreshes its section only; `git init` in a folder without a repository adds a section.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| E01  | `rovik-core/src/chat_db/store.rs`, `chat_db/search.rs`, `chat_db/api.rs`, `src-tauri/src/commands/chat.rs`, `src/lib/chatApi.ts`, `src/lib/chatStore.ts`, `src/components/ChatPanel.tsx`, `src/components/ChatList.tsx`, `src/components/workspaceChat.test.tsx` (new)                                                                         | The chats of several owners; `setProjects` keeping the open chat; unanswered sends recovered per owner added; each row's folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | `chat_db/tests.rs`: two owners' chats listed together by recency, and searched together; a subagent of a workspace chat belongs to the workspace. `chatStore.test.ts`: "adding a folder keeps the open chat and its draft". `workspaceChat.test.tsx` (renders ChatPanel as `chatBranchOffer.test.tsx` does): "after a crash, an unanswered send in the second folder's chat comes back"; a row shows its chat's folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| E02  | `src/components/ChatPanel.tsx`, `src/components/FolderMenu.tsx`, `src/lib/openFolders.ts`, `src/lib/chatStore.ts`, `src/lib/fileReferences.ts`, `src/lib/activeContext.ts`, `src/lib/chatBranch.ts`, `src/lib/plans.ts`, `rovik-core/src/chat_db/store.rs`, `src-tauri/src/commands/chat.rs`, `src/lib/chatApi.ts`                             | New chat's menu on both buttons, with checkboxes, and the panel's menu when there's no chat; one folder checked at a time until F14; `chat_move_empty_dialog` (`move_empty_dialog`, taking the chat's folders) for reusing an empty chat; `chatRootOf`, `chatPathToAbsolute`, `chatPathOf`, `runPlace`; the label; what's sent named as the chat names files (the editor context included), another folder's file not sent; Implement's choice of chat for a folder's plan.                                                                                                                                                           | `chat_db/tests.rs`: an empty chat moves to another owner; one with a message, or a subagent's, is refused. `openFolders.test.ts`: a folder chat's path is joined to its folder; a file is named from its folder; a file outside the chat's folders has no name; a Windows file is named with `/` and found again. `workspaceChat.test.tsx`: with two folders, New chat lists them with boxes, the active one checked, and Start chat makes a folder chat in the one checked; Escape makes none; with one folder there's no menu; an empty chat is reused; with no chat, the panel shows the menu; a missing folder can't be checked; a folder chat's label shows its folder and isn't a control; the editor context names the file as the chat does; another folder's file isn't sent; Implement on another folder's plan starts a chat there, and on the open chat's folder's plan uses it. `chatBranchOffer.test.tsx`: the offer asks about the chat's folder's repository.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| E03  | `src/components/IsolationBar.tsx`, `src/lib/overlay.ts`, `src/App.tsx` (`chat:` tabs)                                                                                                                                                                                                                                                          | Paths through `chatPathToAbsolute`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | `isolationBar.test.tsx`: a changed file of the second folder's chat opens from the second folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| E04  | `rovik-client/src/remote.rs`, `rovik-client/src/remote/results.rs`, `src-tauri/src/commands/chat.rs`, `src-tauri/src/instances.rs`, `src-tauri/src/lib.rs`                                                                                                                                                                                     | The window's open owners, from the claims' record; `chat_open_project` no longer setting the client's open project; steering paths relative to the chat's own folder; the delivery test's folder.                                                                                                                                                                                                                                                                                                                                                                                                                                     | `remote/tests.rs`: a host's result for the second folder's chat lands while the workspace is open; a steering message's paths are relative to its chat's folder. App crate: a folder resolved after the window moved on doesn't become the client's open project.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| F01  | `rovik-core/src/folders.rs` (new), `agent_run/run.rs`, `repair.rs`, `repair_run.rs`, `verify.rs`, `landing.rs`, `before.rs`, `test_writer.rs`, `execute.rs`, `subagent_run.rs`, `change_review.rs`, `candidates.rs`, `agent_run/mod.rs`                                                                                                        | `Folders` (4.1); `Run.folders` in place of `Run.project_root` (4.3), one folder giving today's behaviour everywhere.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | The existing agent_run suites pass unchanged. `folders.rs` unit tests: a chat path splits by its first part; `abs` and `chat_path` go and come back; a folder chat's paths are its folder's; `output_roots` maps a folder's path, its canonical spelling and its place in the view to its name.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| F02  | `rovik-protocol/src/lib.rs`, `src/lib/agentRuns.ts`, `agent_run/mod.rs` (`check_project`), `autopilot/driver.rs`, `agent.rs` (estimate)                                                                                                                                                                                                        | `check_project` giving a workspace chat's run its `Folders` from the database (4.1), for Autopilot's stages and the estimate too; `features::FOLDERS`, which G02 advertises.                                                                                                                                                                                                                                                                                                                                                                                                                                                          | `check_project`: a workspace chat's run gets its folders at their paths, all of the workspace's for All folders; after its folders change, the next run has the new ones; a missing folder is refused by name, and so is a chat with no folders; a workspace chat's request with a root is refused; a folder chat's runs are as today. Autopilot: a stage started after the chat's folders changed gets the new ones.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| F03  | `rovik-core/src/vfs/store.rs`, `vfs/fuse.rs`, `overlay/fs.rs`                                                                                                                                                                                                                                                                                  | `VfsError::CrossDevice` (EXDEV) and its message; the shared `Budget`, with its lock and the warning's callback, and `open_in`; `trait ChatFs: Backend`. No behaviour change.                                                                                                                                                                                                                                                                                                                                                                                                                                                          | `overlay` tests: parts of one chat share its limit, and two writing at once can't both pass it; the warning comes once per chat, from whichever part reaches 80%. `vfs` tests: a cross-device error reaches the kernel as EXDEV.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| F04  | `overlay/land.rs`, `overlay/chats.rs`                                                                                                                                                                                                                                                                                                          | Landing on `Folders` (4.6): plan by `path` and `origin`, Land and Undo refused while a folder is missing, locks keyed by `file_guard`'s key, one journal with its folders and `landings/<chat>/folders.json`, recovery that writes nothing into a missing folder, undo; `folders_of` in place of `project_of`.                                                                                                                                                                                                                                                                                                                        | `land` tests: changes in two folders land under one landing; a conflict in one folder lands nothing in any; a move within a folder lands as a move; a failed write in the second folder puts the first back; an interrupted two-folder landing is finished at start; one whose folder is missing at start writes nothing there and doesn't recreate it, and is finished once the folder is back, where it's found; undo puts back every folder, also once the chat's store is gone; Land and Undo are refused while a folder is missing; such a landing writes no `project.txt`; two such chats landing at once both finish, and so do a folder chat and a workspace chat landing in one folder; a folder chat's journal has no folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| F05  | `overlay/composite.rs` (new)                                                                                                                                                                                                                                                                                                                   | `CompositeFs` (4.2), unit-tested without FUSE.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | `the_root_lists_each_folder_by_its_name`; `inode_numbers_of_two_folders_never_collide`; `a_folders_root_has_the_views_root_as_its_parent`; `nothing_can_be_created_linked_renamed_or_removed_at_the_root`; `the_roots_attributes_and_extended_attributes_cant_be_set`; `a_rename_between_folders_is_a_cross_device_error`; `a_folders_kernel_notifications_carry_the_views_inode_numbers`; `changes_name_paths_and_origins_from_the_views_root`; `a_folder_added_while_mounted_shows_and_old_inodes_stay_valid`; `a_folder_that_left_is_dropped_from_the_root`; `a_folder_located_elsewhere_is_reopened_at_its_new_path`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| F06  | `overlay/chats.rs`, `overlay/fs.rs`, `src-tauri/src/commands/workspace.rs`                                                                                                                                                                                                                                                                     | One `open` for both kinds of chat (store layout, the view matched to the chat's folders at each run's start), `exists`, refusing a store of the other kind; `ChatOverlay.project`'s readers and `project.txt`'s other readers; the watcher and peers per (chat, folder); busy per folder; children over the view; scratch views of `&Folders`; `project_for`; `changes_in_folder` from the part's manifest, and Locate… updating `folders.json`; `CARGO_TARGET_DIR` by workspace; D19's guard in the remove and locate commands, background commands counted; recovery after Try again and Locate…; matching the view at Apply (4.2). | Mounted (FUSE): common tools work across the folders of one view; `mv` between folders lands as an add and a delete; the view's root is read-only to commands. `chats` tests: a workspace chat reopens with each folder's changes, and its bar lists them while it's closed; opening its store as a folder chat's is refused and leaves the store as it was; the view gains a folder checked and drops one unchecked, deleting its part's store (folders changed with `store::set_dialog_folders`); the project changing under one folder notes paths with its name; a folder chat and a workspace chat see each other's clashes; refreshing one folder keeps the other's clashes; `changes_in_folder` answers while the folder is missing; locating it as the same project keeps the changes; a worker lands into the right folders; `CARGO_TARGET_DIR` is the workspace's for each of its chats. App crate: removing a folder a workspace chat has changes in, is at work in, or runs a background command for, is refused, and otherwise it's allowed, leaving the chat's folders; Locate… while that chat is at work is refused, and so is Locate… to another project while it has changes there; after Locate…, a landing a crash interrupted in that folder is finished.                                                                                                                                                                                                                                                                                                                                                                                                             |
| F07  | `agent_run/run.rs`, `agent_run/landing.rs`                                                                                                                                                                                                                                                                                                     | `prepare` opens the view for every run here, Plan mode too, and refuses without the driver or with isolation off, but for the daemon's form; landing through `Folders`; the busy wait over every folder.                                                                                                                                                                                                                                                                                                                                                                                                                              | `workspace_e2e_tests.rs` (new, with a `workspace_harness`): a workspace chat reads and edits files in two folders by their names; its changes land in each folder and nowhere else; a folder not checked for it isn't in its view; Plan mode works in the view and changes nothing; a workspace chat is refused without the driver, or with isolation off and no root; Land and Undo are refused while a folder is missing.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| F08  | `agent_run/project.rs`, `project_profile.rs`, `restricted_files.rs`, `agent.rs`                                                                                                                                                                                                                                                                | `load_folders` (4.4): each folder as alone (V09 inside it, its services line), instructions under the shared limit with `cut`; restricted files routed by a path's first part; the project section, with each folder's git line and the folders that joined or left since the last run.                                                                                                                                                                                                                                                                                                                                               | `workspace_e2e_tests.rs`: each folder's instructions and checks are in the prompt under its name; the instructions share 12,000 characters evenly, and a short one leaves its rest to the others; a folder's restricted file is refused by its path in the view; one folder's restricted entry doesn't restrict the other; a folder named `examples` keeps its key files restricted; each folder's services line, its V09 folders and its git line are in the prompt; after the chat's folders change (with `store::set_dialog_folders`), the next prompt says which joined and which left, from the last run's message.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| F09  | `agent_run/permissions.rs`, `README.md`, `overlay/confine.rs`, `overlay/confine/landlock.rs`, `overlay/chats.rs` (`command_problem`), `agent_run/execute.rs`, `agent_run/environment.rs`                                                                                                                                                       | The `cd` rule (4.5), for every chat, and the README's Getting started, step 3; redirects per folder; the text check for every folder's path, connectors' commands always; installs by folder.                                                                                                                                                                                                                                                                                                                                                                                                                                         | `permissions` tests: with the default allowed commands, `cd api && npm test` is permitted, and `cd .. && npm test`, `cd /tmp && ls`, `cd ~ && ls`, `cd $X && ls` and `cd api && rm x` aren't; every name `folder_name` gives passes it. `confine.rs`: each folder's path shows its place in the view (bubblewrap); a command naming any folder's path is refused where it can't be redirected. `workspace_e2e_tests.rs`: a command in each folder runs there and sees the chat's changes; a command naming a folder's real path works in the view or is refused.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| F10  | `agent_run/verify.rs`, `agent_run/before.rs`, `agent_run/causes.rs`, `agent_run/repair.rs`, `agent_run/repair_run.rs`, `agent_run/landing.rs`, `agent_run/project.rs` (helpers), `agent_run/group.rs`, `agent_run/breaker.rs`, `agent_run/test_writer.rs`                                                                                      | Checks of the changed folders, each in its folder, and those naming no folder; `checks_would_run`'s filter; the type-check rule per folder in every chat; before-runs on a scratch view; output paths through `output_roots` in the four places; the file-to-command helpers per folder.                                                                                                                                                                                                                                                                                                                                              | `workspace_e2e_tests.rs`: only the changed folder's checks run, in it; a plan's own check commands run whatever changed; a failed type check in one folder doesn't skip the other's tests; a check failing as before the change is set apart per folder; `checks_would_run` follows the same filter; a slip printed with a folder's real path is matched to the file the round wrote, and so are V05's files behind and a failure's causes; the before-run reads paths through the same roots; the red check runs a new test in its own folder.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| F11  | `agent_run/plan.rs`, `agent_run/execute.rs`, `agent_run/run.rs`, `agent_run/mod.rs`, `autopilot/driver.rs`, `autopilot/artifacts.rs`, `src/lib/plans.ts`, `src/components/ChatPanel.tsx`                                                                                                                                                       | Plans in the chat's first folder with their `Folders:` line under the title; named by chat path wherever shown or sent, the Plan-mode plan's too; `Files:` against the view; Implement's choice of chat for a plan with a `Folders:` line.                                                                                                                                                                                                                                                                                                                                                                                            | `workspace_e2e_tests.rs`: a workspace chat's plan is saved in its first folder's plans with its `Folders:` line under the title; its files are checked against the folders by name; the implementation request, the Plan-mode line and Autopilot's report name the plan by its chat path; after the chat's first folder is unchecked (with `store::set_dialog_folders`), a new plan goes to its new first folder. `plans.test.ts`: a plan with a `Folders:` line is implemented by a workspace chat whose folders include the plan's, the open one if it does, even with one folder (`Folders: api` gives a workspace chat in api, not a folder chat); one without the line goes to a folder chat; one naming a folder this workspace lacks says which and starts nothing.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| F12  | `agent_run/candidates.rs`, `agent_run/mod.rs` (candidates' records), `autopilot/driver.rs`, `agent_tools.rs`, `agent_run/tools.rs`, `agent_run/project.rs` (estimate)                                                                                                                                                                          | Thorough with folders (4.9); Autopilot with folders; diagnostics through the folders, for files the chat hasn't changed; the estimate.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | `workspace_e2e_tests.rs`: a scout reads both folders and a worker writes only its step's folder; a Thorough task makes candidates over the whole view, each following every folder's settings; an Autopilot task in a workspace chat runs to its delivery, and gets the chat's folders again after a restart; the diagnostics tool finds the editor's markers through the view (and in an isolated folder chat), and gives none for a file the chat changed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| F13  | `connectors/claude_code.rs`, `connectors/codex/mod.rs`, `connectors/mod.rs`                                                                                                                                                                                                                                                                    | The chat's section given in own-tools mode.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | `workspace_e2e_tests.rs` (scripted harness): Claude Code works in the view, keeps its session, and is told about the folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| F14  | `src/components/ChatPanel.tsx`, `src/components/FolderMenu.tsx`, `src/lib/virtualFiles.ts`, `src/lib/cloud.ts`, `rovik-client/src/hosts_page.rs`, `src-tauri/src/commands/hosts.rs`, `src-tauri/src/commands/workspace.rs`, `src/lib/chatApi.ts`, `src/lib/chatBranch.ts`, `src/components/NewBranchDialog.tsx`, `src/components/ChatList.tsx` | New chat's several folders and **All folders**, and when they can be checked, with `HostSummary.features`; a workspace chat's label opening the menu with **Apply**, through `chat_set_folders` with D32's guard; the label and rows; the chat's folders' files in `@`, the editor context and Add to chat; the branch offer across repositories.                                                                                                                                                                                                                                                                                     | `openFolders.test.ts`: a workspace chat's path is found in the folder its first part names; an unknown name names no file; a workspace chat's run is sent no root and no folders; a missing folder stops the run, naming it. `hosts_page.rs` tests: a host's summary carries its features. `virtualFiles.test.ts` (new): a second folder needs the driver and says how to get it; on Windows it needs Rovik in WSL to have `FOLDERS`. `workspaceChat.test.tsx`: checking api and web makes a workspace chat across them; checking All folders checks every box, and unchecking one unchecks it; a second box can't be checked without the driver; `@` lists the chat's folders' files under their names; Add to chat from either folder goes in as `api/…` or `web/…`; the label says the chat's folders; its menu's Apply changes them, stays off while the chat is at work, has a background command running, or has no folder checked, and refuses unchecking a folder with unlanded changes, naming them; a missing folder without changes can be unchecked; checking every box doesn't set All folders; a file of a folder the chat doesn't work in isn't sent; a folder checked on its repository's main branch is offered the chat's branch at the next send. App crate: `chat_set_folders` refuses a chat at work, one with background commands running, or one whose Autopilot task waits, and unchecking a folder with unlanded changes. `chatBranch.test.ts`: repositories on their main branch are offered one branch, named once; two folders of one repository are asked about once; a repository on another branch keeps it; a branch that fails in one says which have it. |
| G01  | `rovik-protocol/src/agent.rs`                                                                                                                                                                                                                                                                                                                  | `StartRequest.folders` (`RunFolder`: name and settings), for a daemon (F02 adds `features::FOLDERS`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Protocol: a start request without folders still parses; `covers()` samples.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| G02  | `rovik-agentd/src/session.rs`, `daemon.rs`, `store.rs`, `results.rs`, `work.rs`                                                                                                                                                                                                                                                                | `FOLDERS` advertised; `check_project`'s daemon form (`agent_run/mod.rs`: each folder as `root/<name>`), settings per folder, names in `StartSettings`; a folder that left a chat deleted from its worktree; unchecked only when none of the checks 4.7 picks would run; delivery refused for such a chat; files at the worktree's top reported as left there.                                                                                                                                                                                                                                                                         | `rovik-agentd/tests/folders.rs` (new): a two-folder chat's first sync gives it one worktree with each folder under its name; its run works in the worktree with each folder's settings; a merge checked on the server runs the changed folders' checks, and one with none to run lands unchecked; a check that fails there is reported as failing, not set apart; a chat across folders isn't delivered and says why; a file made at the worktree's top is reported as not brought back; a restart keeps the chat's folders; root and folders are refused when a folder isn't `root/<name>`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| G03  | `rovik-client/src/shadow.rs`, `rovik-client/src/shadow/composite.rs` (new)                                                                                                                                                                                                                                                                     | The `Composite` shadow over the folder shadows (alternates two levels deep) and `compose`, per chat's folders; `graft`, copying a folder part's objects into its shadow.                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Unit: composing two folders' trees puts each under its name; a chat's compose has only its folders; an unchanged compose keeps its commit; grafting a folder's result copies its objects into the folder's shadow and commits on its current tree.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| G04  | `rovik-client/src/shadow/rules.rs` (new)                                                                                                                                                                                                                                                                                                       | Ignore rules rebased under a folder's name.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | `rebased_rules_ignore_what_git_ignores_in_the_folder` (a table checked against libgit2's `is_path_ignored` in the real layout); `what_an_ancestors_gitignore_ignores_stays_out_of_the_result`; `every_chat_of_a_workspace_sends_the_same_rules`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| G05  | `rovik-client/src/remote/files.rs` (new), `remote.rs`, `remote/sync.rs`, `remote/results.rs`, `remote/moving.rs`, `remote/back.rs`, `remote/bar.rs`, `rovik-core/src/overlay/import.rs`, `src-tauri/src/commands/workspace.rs`                                                                                                                 | `ChatFiles` (with `Folders`) in place of `project_of`, and the name sent from it; sync, recheck, start, import per folder, landing's report, moves and back, the bar, for a chat of folders; `movable`, `start` and `sync` checking `FOLDERS`; `import` taking its store; D19's and D32's guards asking a host's chats too.                                                                                                                                                                                                                                                                                                           | `remote/tests.rs` (fake daemon): a chat across folders isn't moved to a daemon without `FOLDERS`, and the upgrade is offered; on Windows, its first start in a WSL without it says how to update. `rovik-agentd/tests/folders.rs`: a run editing both folders brings one result with both; the result comes back into each folder's changes and lands in both; a landing moves the base for both; a chat across folders moved back waits with its changes in each folder; a folder checked for the chat reaches its worktree, and its run, at its next message, and one unchecked leaves, deleted with its ignored files; two chats with different folders keep each other's by-products out of their results; a folder inside a repository imports with its repository's attributes. App crate: removing a folder a workspace chat on a host has changes in, or unchecking it for that chat, is refused, and so is removing one while that host can't be reached.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| G06  | `rovik-client/src/remote/moving.rs`, `src/components/MoveChatDialog.tsx`, `src/lib/cloud.ts`                                                                                                                                                                                                                                                   | The preflight's folders and notes; blocked by a missing folder or an old host.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | `chatLocation.test.tsx`: the dialog lists the chat's folders and the notes; a missing folder blocks the move.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| H01  | `src/components/ProjectSwitcher.tsx`, `ProjectList.tsx`, `NewWorkspaceDialog.tsx` (new), `src/App.tsx`, `src/styles.css`                                                                                                                                                                                                                       | The menu items; the dialog (picker and typed path); Add folder; workspaces in the list, and their delete question; the trigger; Create bringing the folder's terminals and commit message along.                                                                                                                                                                                                                                                                                                                                                                                                                                      | `ProjectSwitcher.test.tsx` (new): the items' order, and Add folder only in a workspace. `NewWorkspaceDialog.test.tsx` (new): each refusal shows on its row and keeps Create off; the open folder is listed first; a typed path is added. `ProjectList.test.tsx`: the workspace row, its missing count, and its delete question naming its chats. `terminalPanel.test.tsx` and `GitPanel.test.tsx`: Create brings the folder's terminals and commit message along.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| H02  | `README.md`                                                                                                                                                                                                                                                                                                                                    | Features, Getting started, Several windows, the Git panel, and chats across folders.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | `npm run format:check`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| H03  | `scripts/e2e/run.mjs`, `scripts/e2e/README.md`, `.github/workflows/e2e.yml`                                                                                                                                                                                                                                                                    | The e2e steps (section 8), after the project's restart step, with a restart of their own at the end; `fuse3` in the Linux e2e job.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | The steps themselves.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |

---

## 8. Testing

**Rust** (`cargo nextest run` per crate, as CI runs them on Linux, macOS and
Windows):

- The app crate: `instances.rs` (claims, and the races of section 5),
  `filesystem/watch.rs`, `entries.rs`, `git/tests.rs`, `updates.rs`, and
  the workspace commands' guards.
- `rovik-core`: `chat_db/workspaces_tests.rs`, `projects_tests.rs`,
  `chat_db/tests.rs`, `rev_tests.rs`, the schema's
  `upgrades_a_version_10_database`; `folders.rs`; the overlay's composite,
  budget, land and chats tests (the mounted ones gated by
  `vfs::mount_works()`, as today); `permissions`; `confine.rs`; the new
  `agent_run/workspace_e2e_tests.rs`, with a `workspace_harness` beside
  `harness` (`agent_run/e2e_tests.rs:132-198`): three folders, their checks
  on, the chat's folders (two of them, or all), isolation, and the request
  sent with `folders`.
- `rovik-client`: `remote/tests.rs`, `hosts_page.rs`, `shadow` and `rules`
  unit tests.
- `rovik-agentd`: the new `tests/folders.rs`, with `common/folders.rs`: `api`
  (a repository with history), `web` (a folder of a repository whose root
  `.gitignore` ignores `node_modules/`), `notes` (no git).

Tests that change process-wide state (the watcher's generation,
`GIT_LOG_LATEST`, the busy holds, the overlays' data folder) take that
state's lock, so they pass under nextest and `cargo test` alike.

**Frontend** (`npm test`): the tests named in section 7, among them these
new files: `openFolders.test.ts`, `switchTarget.test.ts`,
`projects.test.ts`, `gitMarks.test.ts`, `fileFormatters.test.ts`,
`diagnostics.test.ts`, `searchPanel.test.tsx`, `activeConfig.test.ts`,
`lsp/client.test.ts`, `virtualFiles.test.ts`, `workspaceChat.test.tsx`,
`ProjectList.test.tsx`, `ProjectSwitcher.test.tsx` and
`NewWorkspaceDialog.test.tsx`.

**End to end** (`scripts/e2e/run.mjs`). A second demo folder, a repository of
its own, is added through the dialog's path field (the native picker can't
be driven). The steps come after `restart: the project and its tabs come
back`, so the project's steps are unchanged:

1. `workspace: create one from the open project, add a second folder; each is a row in the tree`
2. `workspace: the same file name in each folder opens two tabs, told apart`
3. `workspace: search finds a match in each folder`
4. `workspace: git lists both repositories with their branches; one commit lands in each`
5. `workspace: New chat with the second folder checked starts a chat there, which changes a file there`
6. `workspace: New chat with both folders checked starts a workspace chat, which changes a file in each, and both land`
7. `workspace: a folder removed closes its tabs; the other folder stays`
8. `instances: the workspace's folder opened in another window brings this one to the front`
9. `restart: the workspace and its tabs come back`

Step 6 needs FUSE: H03 installs `fuse3` in the Linux e2e job, which has
none today. The macOS runner has no macFUSE and the Windows runner can't
run WSL (`e2e.yml:126-128`), so there the step checks that New chat can't
check a second folder, and says why.

**By hand**, once per system, for what CI can't reach:

- Windows: a folder in WSL beside a Windows folder (a chat in each); a
  workspace chat running in WSL, landing in both folders, then a folder
  checked for it between runs; WSL on an older Rovik refusing it; a drive
  root; the same folder added with two spellings of case.
- macOS: a workspace chat with macFUSE (FSKit and the kernel extension); a
  folder under `/tmp` (FSEvents reports `/private/tmp`); a folder on a
  case-insensitive volume added twice in two cases.
- Linux: four large folders (the inotify limit); a workspace chat under
  bubblewrap, and under Landlock alone (`ROVIK_CONFINE_COMMANDS=landlock`);
  a chat in each of two folders at once.
- A cloud host: moving a workspace chat there and back; a folder unchecked
  for it there; an Autopilot task there waiting for the app to land it.

---

## 9. Risks and known limits

- **The composite view is new code under FUSE**: inode numbers,
  notifications, EXDEV, parts that come and go. It's unit-tested without
  FUSE (F05) and mounted on Linux CI; a tool that mishandles EXDEV within
  one mount is possible (Linux's overlayfs does the same, so most handle
  it).
- **Workspace chats need the virtual-files driver** here: macOS without
  macFUSE has none, and New chat says why. On Windows they need WSL.
- **A `cd` within the project now passes the allowed commands** in every
  chat (D26): `cd src && npm test` runs where `npm test` is allowed. It runs
  nothing itself, and the rest of the command is checked.
- **A workspace chat's folders change between runs** (D32): its earlier
  messages may name a folder that has left, which then names no file. The
  agent is told which folders joined and left (4.4).
- **A chat started in one folder is a folder chat** (D2) and can't take in
  a second: to work across folders later, start a workspace chat.
- **Rebasing ignore rules** for WSL and hosts (G04) is the riskiest piece of
  G: a rule rebased wrongly lets by-products into a result, or keeps a file
  out. Its tests compare with libgit2 on the real layout.
- **In WSL and on hosts, a workspace chat has no git history**: git there
  shows Rovik's snapshots, and Autopilot doesn't push branches for it (D30).
  Files a run makes at the worktree's top stay there (4.11).
- **Only the changed folders' checks run** (D27): a change in `api` that
  breaks an unchanged `web` isn't caught by the checks. The prompt lists
  every one of the chat's folders' checks.
- **Instructions share 12,000 characters** evenly: a chat across many
  folders with long `AGENTS.md` files has them cut.
- **The same relative path in two folders** can make a check's output
  ambiguous; a check's own folder is tried first (4.7).
- **The repository map's limits (4 seconds, 20,000 files) are reached
  sooner** over several folders.
- **A folder is open in one window at a time**, alone or in one workspace
  (D7). Wanting it in two windows means a copy of it, which can't then be in
  the same workspace (D5).
- **Repositories inside a folder aren't listed** (D10). The repository around
  them never stages them (D12).
- **A commit with changes staged outside its folders is refused** (D12) until
  they're committed or unstaged from a terminal.
- **Commits across repositories aren't one transaction.** The hooks all run
  first (D11); a failure while writing the commits leaves the earlier ones,
  says which, keeps the message, and pushes nothing until the rest commit.
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

| Question                                      | Answer                                                                                                                                      |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| The menu items' wording                       | **Create a workspace…** and **Add folder to workspace…**, both above **New window** (D3).                                                   |
| How are a chat's folders chosen?              | New chat's menu: **All folders**, or folders checked one by one. One folder makes a folder chat, several a workspace chat across them (D2). |
| Can a chat's folders change after it starts?  | Yes, added and removed between runs; a folder is taken out only while the chat has no unlanded changes there (D32).                         |
| Does All folders take in folders added later? | Yes (D2, D22).                                                                                                                              |
| When do chats across folders ship?            | In the first release (D22–D31).                                                                                                             |

Decided while planning, for the user to check:

- **A workspace groups projects and owns its workspace chats** (D1): a
  folder's own chats stay with it.
- **A chat started in one folder is that folder's chat** (D2): it shows
  when the folder is opened alone, and keeps its folder.
- **Workspace chats need the virtual-files driver** here (D23), as the
  agent-managed context does: none on macOS without macFUSE.
- **In WSL and on hosts they carry no git history and Autopilot keeps their
  results** for the app to land (D30): Windows gets workspace chats through
  WSL in the first release.
- **A `cd` within the project passes the allowed commands**, in every chat
  (D26): without it, attended chats couldn't run a folder's commands or
  checks.
- **Only the changed folders' checks run**, with a plan's own (D27), rather
  than every folder's.
- **Plans go in the chat's first folder**, marked with a `Folders:` line
  (D28), rather than in the app's data.
- **Instructions share 12,000 characters**, evenly (D25).
- **Folder names are given once and kept**, a removed folder's too (D8).
- **Stored in the chats database** (D6), with a schema step, a workspace
  chat's folders included.
- **Create opens the workspace in this window**, bringing the open folder's
  tabs, terminals and commit message (D4).
- **Nested folders, and two folders of one project, are refused** (D5).
- **One window per folder** (D7).
- **One message box, a commit per repository**, every checking hook first,
  and nothing pushed after a partial failure (D10, D11).
- **A commit never takes what's staged outside its folders**, and is refused
  while there is some (D12). This changes today's behaviour for a project
  inside a repository, which commits it now.
- **Only the repository each folder is in** is listed (D10).
- **IntelliSense loads every folder**, the active file's folder deciding the
  compiler options (D14).
- **No workspace file, and no renaming** of workspaces or folders.

---

## 11. Not in this plan

- **A folder chat taking in a second folder**: its paths don't start with a
  folder's name, so it stays in its folder; start a workspace chat (D32).
- **A folder chat reading the other folders.** It would work here, but not
  in WSL or on hosts, where a chat has its own folder's files only.
- **Git history and branch delivery for workspace chats in WSL and on
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
9. **A late `reclaim` takes back a folder the window has left.** The new
   folder's answer can come first (a known project skips the fingerprint
   walk, `chat_db/api.rs:44-46`); the old one's then leaves the window
   holding the folder it left, and not the one it shows, until the next
   switch (`commands/chat.rs:63-71`, `instances.rs:140-200`).
   `chat_open_project` also points the cloud client at the old folder
   (`commands/chat.rs:54-56`). B03, E04.
10. **An isolated chat never sees the editor's diagnostics**: they're kept
    by the real folder and looked up by the run's root, the view
    (`agent_tools.rs:433-435, 555-571`). F12.
11. **The editor context names the active file by its absolute path**
    (`activeContext.ts:71-83`), which an isolated chat's tools refuse: they
    take paths inside the view only (`agent_tools.rs:110-133`). E02. (Seen
    in the code; not reproduced.)
12. **A connector's commands may reach the real folder** under bubblewrap.
    Claude Code and Codex run unconfined (`connectors/harness.rs:272-290`;
    Codex with `danger-full-access`, `codex/mod.rs:264`), and their commands
    reach Rovik only through hooks and approval requests
    (`execute.rs:1443-1530`), where the text check against naming the
    project's path is skipped when the agent's own commands are redirected
    (`overlay/confine.rs:710-712`; `overlay/chats.rs:66-72, 90`). F09. (Seen
    in the code; not reproduced.)
13. **An interrupted landing into a folder that's missing at the next start
    recreates the folder** with only the landing's new files in it, and
    gives up on its other files for good (`overlay/land.rs:792-794`,
    `:1090-1122`). F04.
14. **In Plan mode, an isolated chat's current plan is named by its full
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
- The file tree's check of names Windows refuses misses `COM0`, `LPT0`,
  `CONIN$` and `CONOUT$` (`filesystem/entries.rs:84-92`).

---

## Notes from implementation

A project (one folder) behaves after every task as it did before. What reaches a
user before H is the fixed bugs of section 12 and the README's Git panel
section (A06). Each note says what the task changed, the tests that prove it,
and where the code departs from its row.

**A01 — one set of path helpers.** `src/lib/fileTools.ts` holds `isAtOrUnder`,
`parentOf`, `normalizePath` and `samePath`, with one case rule (ignored on
Windows and macOS, exact on Linux), drive roots (`D:\x`'s parent is `D:\`) and
UNC roots (`\\server\share` is a root, `\\server` isn't). `tsconfig.ts`'s
`samePath` and `startsWithPath`, and the private copies of `isAtOrUnder` and of
"a path's parent" in `fileTree.ts` and `App.tsx`, are gone. `fileTools.test.ts`
covers POSIX, Windows and UNC paths and the case rule; `fileTree.test.ts` has "a
change at the top of a drive refreshes the drive root"; `tsconfig.test.ts`
passes unchanged.

**A02 — `openFolders.ts`.** `WindowTarget` (a folder, or a workspace's named
folders), `target()`, `openFolders(target)`, `folderOf`, `activeFolder`,
`innermostFolder` and `sessionKey`. App's `rootPath()` is a shim
derived from `folders()` — a project is a list of one — and its callers stayed
as they were: each has to name a folder of its own, which is milestone C's work
(C01–C07). `openFolders.test.ts` covers a project as one folder, a workspace's
folders under its stored names, and the session key.

**A03 — the watcher.** `start_file_watch` takes `StartedFolder`s: a watch and
its own `.gitignore` files per folder, one folder's error leaving the others
watched, `fs-changes` naming the folder the change is in and spelling the path
as that folder is spelled (a folder opened through a link reports under the
link), `unknown` naming its folder for a flood, and `git-changed { folder,
repositories }` — `repositories` true when a change makes or removes a
boundary of the folder's repository (its own `.git`, the `HEAD` that comes or
goes with it, or the git folder watched for it above), so the window reads
which repositories its folders are in again; a commit, a checkout or a save is
false.
`folder_of` answers the innermost folder, and a folder's own path with no
separator joined onto nothing — the event a folder's removal sends. A repository
above a folder is watched at its git folder, at the `refs/` it shares
(`common_dir/refs`: a linked worktree's branches live there) and at its own
`refs/`, each of them credited to the folder it serves (a worktree's push is
seen); a moved `HEAD` or `ORIG_HEAD` calls `project_busy` on that repository's
working folder, so a checkout from a terminal in `repo/` holds the chats of
`repo/app`. Switching folders empties the ignore cache. `watch.rs` covers two
folders at once, an unwatchable folder, a watched folder that went, a flood
naming each folder, a change under the innermost folder, a link's own spelling,
a commit or a push from a terminal in a repository above a folder, a worktree's
push in the shared refs, and which paths may change a folder's repository.

**A04 — the tree's controller.** `createFileTreeController(roots)` takes
`TreeRoot { path, name, row, id }`: a folder with `row` is a row of its own (a
project's one root isn't), each listing is read through its own folder, and
`refresh(path)` walks up to that folder's root only. With one root and no row
the rows are what they were. `fileTree.test.ts` has "a workspace lists each
folder as a row, each read from its own folder" and "a link in one folder to
another lists rows of its own", beside the one-root tests.
Departure from the row: a row's id is the backend's file id, and a folder row's
is its `root.id` (else its path), not a folder-and-id pair. Two rows showing one
file (a link to a file in another folder) are the same file on disk and share
an id, which is what the editor wants; the visible quirk is that selecting one
highlights both. Keying by folder would rename the identity the tabs, the
selection and drag-and-drop share, so it stays as it is until something needs
it.

**A05 — the repositories.** `git/repos.rs`: `repos_of(folders)` → `Repos
{ repos, without }`, with `Repo { key, workdir, name, scopes }` — one entry per
repository, whose `scopes` are the window's folders in it relative to its
working folder ("" for the working folder itself), a folder in no repository in
`without`, and its stored name for a repository that is exactly one folder (else
the working folder's name, with the parent's when two clash). `git_repos`
exposes it. `git_status`, `git_commit` and `git_discard_all` take the scopes as
pathspecs (today's one folder when none is given), so `app2` isn't taken with
`app`; line counts are read per scope and count that scope only. A repository's
rewrite holds its working folder, and `is_busy` counts holds above and below
it: a folder inside a held repository is busy, one beside it isn't. `workdir`
is spelled as the window spells the folder (its path with the scope's parts
taken off, when that leads to the same folder): git resolves links (macOS's temp
folders are under `/private`, Windows expands short names like `RUNNER~1`), and
`repoFileOf` (A07) finds a tab's file in it by the window's path. A folder opened
through a link inside the repository keeps git's spelling.

**A06 — commits that keep outsiders out.** Before anything is staged, the index
is compared with HEAD: an entry outside the scopes refuses the commit, naming
it, and what's staged stays staged. Status leaves out an untracked folder that
is a repository of its own, and a submodule whose only change is inside its own
working tree (`other_repository`); revert refuses both (D12). A submodule moved
to another commit is listed and committed here, as git records it (the gitlink);
the status can't tell the two apart, so `workdir_id` against `index_id` decides.
An empty commit isn't made. The README's Git panel section says all of this.

**A07 — the editor's marks.** `repoFileOf(absPath, workdirs)` asks a file of
`repo/app` as `app/package.json` in `repo`, the deepest of the window's working
folders, and a tab holding a version of a file (`disk:`, `chat:`) keeps its base
across a refresh. GitPanel's `onOpenFile` hands on the absolute path and the file
it was given rather than a path re-made from one project. `gitMarks.test.ts`: a
file of a project inside a repository is asked as its path in the repository, a
file in no repository has no base, a tab holding a version is never refreshed, a
base with no repository is never asked for, a tab opened before the read gets
its marks, and the repositories are read once per set of folders.

**A08 — the editor's services.** `formatterRoot(folders, path)` is the file's
own folder, so a file of the second folder is formatted with the second's
config; the Markdown preview's `/` links resolve in the file's folder; and the
diagnostics' `rootOf` is `innermostFolder` over the window's folders, so a
file's markers are reported for the folder that holds them.
No `diagnostics.test.ts`: the node harness stubs `monaco-editor` as a namespace
with a default export only, so `monaco.editor.getModelMarkers` and the editor's
events aren't there to call. The choice it makes is `innermostFolder`'s, which
`openFolders.test.ts` covers; `fileFormatters.test.ts` and
`markdownPreview.test.ts` cover theirs.

**B01 — schema 11, `workspaces.rs`.** The schema is 3.1's, to the letter:
`projects.kind` (`folder`/`workspace`, rows from before a folder's),
`workspace_folders` (key, name, path, position, `removed_at`, the unique
per-workspace name index), and `dialogs.folders`. `upgrades_a_version_10_database`
covers it: a project stays a folder, a chat has no folders of its own, a
workspace's rows and its chat's folders read back, the name index folds ASCII
case, and deleting the workspace takes its folder rows while its projects stay.
`chat_db/workspaces.rs` has `prepare` (canonical path, whether it's a folder,
its ID file: the file system read before a write transaction), `check_folder`
(pure over prepared folders: Duplicate, Inside, Contains, SameProject,
NotAFolder), `folder_name` (pure over the names given), `create`,
`add_folders`, `remove_folder`, `relocate_folder`, `opened`, `list`, `members`
and `get`. `add_folders` and `relocate_folder` take the workspace's folders as
`api.rs` prepared them (`prepared_members`, on the blocking pool), so D5's check
against them holds inside the write transaction without the database thread
waiting on a slow share; Locate… checks the new place the same way (a place
that isn't a folder, inside another member, holding one, or another member's
project is refused). Names: a drive root is its drive (`D:\` gives `D`), a name the agent's
walkers skip takes its parent's (`app-build`), another member's takes the
parent's name then a number, and uniqueness folds case in Rust (`Ä` against
`ä`), the index's `NOCASE` being ASCII only. A removed folder's row stays (its
name is taken, D8), so a folder re-added comes back by its name, at the end of
the order. `remove_folder` takes the folder out of the `folders` of the
workspace's chats that had it (`[]` when it was the only one) and leaves
`"all"` as it is. The list is by `last_opened_at` and then `rowid`, so two made
in one millisecond still come in the order they were made.
`store.rs`'s `DialogSummary` gains `projectId` and `folders` (each left out
when absent), `NewDialog` gains `folders` (serde default) written by
`create_dialog`, and `set_dialog_folders` changes them (the rev trigger bumps
the chat's revision, which `rev_tests.rs` covers). `projects::list` and
`summary` take `kind = 'folder'` only, so a workspace is never a missing project
with **Relink**. `workspaces_tests.rs` covers D5's rules (a trailing separator,
a link, a copy's ID, nested folders, what isn't a folder), D8's names, the last
folder staying, a folder leaving the chats that had it, a removed name, a
re-added folder's name, locating, and the list's order and missing folders;
`projects_tests.rs` adds the workspace that isn't listed or summarised and the
deleted workspace that leaves its folders' projects; `chat_db/tests.rs` reads a
chat's project and folders back, `"all"` and names alike. A chat's folders are
the chat's across a mirror too: `sync.rs`'s dialog columns, row and apply carry
`folders` (`StoredDialog.folders`, with a serde default, so an older peer's row
still parses and reads as no folders), and `sync_tests.rs` has "a workspace
chat's folders reach the mirror".
Departure: `NewDialog`'s new field needed `folders: None` at every construction
in the other crates (agentd, client, headless, core's tests); a stored field
still has a serde default, which is what an older peer's message needs (G).

**B02 — the commands.** `chat_db/api.rs` adds `workspace_list`, `_create`,
`_add_folders`, `_remove_folder`, `_relocate_folder`, `_check_folder`, each
reading folders off the database thread (`rt::spawn_blocking`) before the write.
`commands/workspace.rs` wraps them; `lib.rs` registers them; `chatApi.ts` wraps
them with `Workspace`, `WorkspaceFolder`, `WorkspaceSummary`, `FolderProblem`
and `folderProblemMessage` (the core's `FolderProblem::message` in the UI's
words, one copy each side, as the protocol has no such type). The api tests use
a `DbState` of their own: the calls reach `workspaces.rs`, and removing a
workspace's last folder is refused.

**B03 — the claims.** `instances.rs` keeps a target (`Target::Folder`,
`Target::Workspace { id, name, folders }`) and a record per owner
(`Owner { project, folder, workspace }`), not one folder and one project:
`claim(target, focus_other, at_work)` takes the project IDs at work here (the
window's owners whose chats are, asked of the database by `project_claim`),
locks the target's keys under the claims' lock without duplicates, keeps the
owners at work with their keys and lets the rest go. A workspace whose own
chats are at work keeps every folder of it (D19). `reclaim(root, project)` is
what `chat_open_project` and a copy or a move answer call: it records that
folder's project and claims its key, and a folder the window has left takes
nothing back; it replaces `claim` + `opened`, and `owners()` replaces
`open_project()`. Holders name their own folder and their workspace, so a
notice reads "api is open in another window, in Shop" or "Shop is open in
another window", and `elsewhere()` reports a copy only for a key with a folder:
another window's workspace isn't a copy. An `Owner` keeps the workspace as
`WorkspaceRef { id, name }`: a folder is kept at work for its own workspace by
that ID (two workspaces may be named alike; `instances.rs` tests one of each
name), and the name is only what a holder's file says for a notice. `lib.rs`'s
`project_claim` and
`project_check` take a `ClaimTarget` (`{ kind: "folder", path }` or
`{ kind: "workspace", id }`, null for no project), whose workspace's folders
`claim_target` reads from the database; `commands/chat.rs`'s `reclaim` is the
new call. `src/lib/instances.ts` wraps the target and words a workspace's
notice. Claiming a workspace also records `workspaces.opened(id)`: the plan's
step 6 of 3.3 has no command of its own, and claiming it is the moment the
window opens it. `instances.rs`'s tests cover a workspace's key and its
folders' or none, a target's keys without duplicates, a folder another process
has stopping all of it, adding and removing folders, two folders resolved at
once, a late answer, a workspace at work keeping its folders, two folders' chats
at work, a folder opened alone kept for a workspace chat, and a workspace that
isn't a copy; `instances.test.ts` covers the notices.

**B04 — switching.** `src/lib/switchTarget.ts` has the steps of 3.3 in order —
check, settle unsaved tabs, keep the tabs left, claim, show, resolve each folder
that's there — as calls the window gives it, so a switch that doesn't happen
changes nothing and the order is tested on its own (`switchTarget.test.ts`:
the order, a refused check, a claim taken meanwhile, a cancelled tab, the tabs
kept before the claim, a missing folder left out and resolved once it's back,
no folder open, `presentFolders`'s marks, `claimTargetOf`). App.tsx's
`openProjectAt` becomes `switchTo`, `showWithTabs` becomes `showTarget`, keyed
by `sessionKey` for the target's tabs and tree. The tree's saved open folders
are registered on the folder each is in (`openFolders.ts`'s `pathsByFolder`,
App's `presetTreeExpansion`): the tree applies expansion per root, and its
roots are the window's folders, never `workspace:<id>`, which would have
dropped a workspace's expansion. The start is `openAtStart`
(shown, then resolved), which resolves in every case, including a window that
opens with nothing — a first start, a fresh **New window**, or a last project
another window has: resolving no folders is what claims the chats without a
project for it (D6), and `switchTarget.test.ts`'s "a window that opens with
nothing resolves" says so. Folders are resolved in one
place, `resolveFolders` (the switch's `resolve` step, and the start), which
marks the folders that aren't there and resolves those that are
(`switchTarget.ts`'s `resolvable`, D20): the effect that resolved on every
change of folders is gone, so nothing resolves a folder that isn't there, and
nothing resolves a folder twice on a switch. Each call makes a new generation
(`newFoldersGeneration`), and `resolveProject` drops an answer asked in an
earlier one: a folder the window has left since can't take the window's chats
back, even though that folder itself asked only once. A missing folder is
retried as the window comes
to the front (`tryFolderAgain`, which checks it with `path_exists` first); its
row's **Try again** is C01's. `projects.ts` counts runs per folder and ids its
notifications `project-copy:<root>` and `project-moved:<root>`, naming the
folder in the message, so two folders' copy questions are both asked; when the
original is another folder of the window the question offers **Separate
history** only (D5). `projects.test.ts` covers both.

**B05 — what's saved.** `last-project.json` gains `workspace` (serde default;
a file without it reads as before), `save_last_project` writes it, and
`startup_workspace()` reads `--workspace <id>`; the backend claims the
workspace's own key before the window exists, as it claims a command line's
folder, and its folders are claimed when the window opens it (3.3).
`restart_args(current, folder, workspace)` passes `--workspace <id>`, a
workspace being the target even when a folder is open (a folder of it), and
`update_restart` takes the workspace; `lib/updates.ts`'s hook is optional, so a
window without one (the tests, another host) is as it was. App.tsx starts from
`startup_info`'s workspace or folder, else the last project: its tabs from
`last-project.json` when it's the one last left, else from `project-sessions`
under `sessionKey`. `projectSessions.ts`'s `sessionInFolders` leaves out a tab
(and a tree folder) of a folder that has left the workspace, as a file that's
gone is left out (`projectSessions.test.ts`: a workspace's entry under its own
key, and the tabs of a folder that left).
Departure: the claim made before the window's database is open has no name for
its holder file, so a notice for that first moment reads "api is open in
another window" without "in Shop"; the claim the window makes as it opens the
workspace writes the name.

**B06 — Delete and open projects.** `ProjectList` takes `openIds` and hides
**Delete** for every project the window has open, not only the one shown; App
passes the workspace's ID and the resolved folder's project
(`openProjectIds`). `chat_db`'s `chat_delete_project(core, id, open)` refuses a
project among `open` (the window's own), with a message saying to open another
first; `commands/chat.rs` passes `Instances::owners()`. `ProjectList.test.tsx`
(new) has no Delete on any open project; the api test
`a_project_the_window_has_open_isnt_deleted` sees the refusal and its chats
kept, then the delete once another project is the open one.
Departure: the guard is in the core's api, not the command, so it can be tested
with a `TestCore`; the command only tells it which projects are the window's.

**C01 — a window's folders in the tree and in App.** The backend refuses to
delete, rename or move one of the window's folders: `WatcherState.folders` is
what `start_file_watch` was given (stored before the watch starts, so a folder
that can't be watched is guarded too), and `entries.rs`'s
`refuse_window_folder` refuses `delete_path`, `rename_path` and `move_path`
with "…: a folder of this window: remove it from the workspace instead". It
compares paths under the platform's case rule, sharing the equality half of
`is_at_or_under` (`same_path`), so a link to a folder is still the tree's to
delete. `TreeRow` gains `folderRow` and `missing`, `TreeRoot` a `missing`;
`createFileTreeController` gives a folder its row, reads nothing of a missing
one (before, a root that failed was retried forever: `forget` never drops a
root), and exports `withoutFolderRows` for the ids copy, cut and delete act on.
`FileTree` takes `rootDrop` (a project has a project-root drop target, a
workspace none), leaves folder rows out of a drag, won't toggle a missing row,
refuses a drop on one, and draws a folder row with its path as its title
(`tree-root-row`) and "Folder not found" when it's missing. The menu gets
`folder`, `onRemoveFolder`, `onLocateFolder` and `onTryFolderAgain`:
New file, New folder, Paste, Remove from workspace, and for a missing folder
Locate…, Try again, Remove from workspace — never Rename, Copy, Cut or Delete.
App has `activeFolderNow` (the `rootId` now uses it), sends a new entry into
the active folder's row, pastes into a selected file's own folder, and its
handlers, the clipboard keys and the menu's `hasSelection` go through
`withoutFolderRows`. `removeFolder` and `locateFolder` live in
`switchTarget.ts` as ordered calls (`FolderChangeCalls`), so their order is
tested without App; `showFolderChange` changes the target in place, registering
each remaining folder's expansion with `tree.presetExpanded` (else the tree's
reset effect closes every folder), closes what left, and drops it from
`missingFolders`. Both change the backend first and claim after it — the
backend claims a workspace's folders as the database has them, so claiming
first would claim the folder that's leaving or its old place — and a claim
another window refuses doesn't stop the window from showing what the database
keeps (the folder is gone, or at its new place; a row or a target for the old
one would be a lie), with the claim's own "open elsewhere" notice and a later
switch claiming or refusing as usual. So `true` means the workspace changed,
whether or not this window could claim it again. `settleUnsavedTabs` takes
`{ folder }`, settling only that folder's tabs, with a message saying they
close as the folder leaves.
Tests: `entries.rs` "a window's folder isn't deleted, renamed or moved" and "a
folder inside a window's folder, or a link to one, is acted on"; `fileTree
.test.ts` "a missing folder's row lists nothing and isn't read again until
it's back", "a folder's row is never among the rows copied, cut or deleted",
"removing one folder keeps the others' open folders open"; `FileTree.test.tsx`
the five drag/drop and missing-row tests; `FileContextMenu.test.tsx` (new) two;
`openFolders.test.ts` and `switchTarget.test.ts` five more.
Departures: the folder menu's tests have their own file, as the menu is its own
component; Remove asks for no confirmation (it touches nothing on disk) but
settles the folder's tabs first; Locate… checks the new place with
`project_check` before the backend relocates, so the database never points at
a folder another window holds; the guard compares paths, not file identities.

**C02 — search in every folder.** `SearchPanel` takes `folders` instead of
`rootPath` and searches every folder at once; `mergeResults` keeps the matches
in folder order under one limit for the whole search, adds the folders' totals
and says more exist when any folder was truncated or the merge cut lines. The
groups carry their folder (their collapsed state is keyed by folder and path),
a workspace's results come under a header per folder, and a result opens
`toAbsolutePath(path, folder.path)`. The folders are searched with
`Promise.allSettled`: one that fails (its drive went between the window's check
and the search, the backend refused) keeps its header with the reason under it
(`search-folder-error`) while the others' results show; only when no folder
answered at all does the panel show the error itself, as it did with one
folder. `searchPanel.test.tsx` (new): two folders' results under their headers,
the same relative path opening each folder's own file, one folder as it looks
today, one folder's failure leaving the others' results, no folder answering,
the limit for the whole search.
Departure: `.search-folder-header` and `.search-folder-error` rules in
`styles.css`, off the row's files.

**C03 — tabs of one name told apart.** `tabHints` (editorTabs.ts) gives each
tab a name another tab shares the shortest end of its parent path's segments
that no other shares — "api" and "web" with several folders, "a/src" and
"b/src" in one project — over the files of the window's folders alone. `Tabs`
takes `folders` and shows the hint in a `tab-hint` span after the name; App
passes the window's folders. Tests: `editorTabs.test.ts` three,
`Tabs.test.tsx` one. Departure: a `.tab-hint` rule in `styles.css`.

**C04 — terminals by session.** `FolderMenu` (new) is the pick-one menu of a
workspace's folders, with the look and keys of `TerminalNewMenu` and a
`children` slot; `TerminalNewMenu`'s hosts are `TerminalHostItems`, used by its
own menu and by that slot. `TerminalPanel` takes `session`, `folders` and
`activeFolder` in place of `rootPath`: its tabs belong to a `session` and carry
the `folderName` they opened in, `newTerminal` starts in the active folder (or
the one picked), and a workspace's "+" always opens `FolderMenu`, with the
hosts after a separator. A tab's tooltip adds "· <folder>" in a workspace.
Tests: "in a workspace, '+' lists the folders and the terminal starts in the
one picked" and "a removed folder's terminal keeps running"; the existing tests
pass a session and one folder.

**C05 — settings pages with a folder select.** `SettingsDialog` takes `folders`
and keeps a `selected` folder (the active one first); on the project and files
pages with more than one folder it shows a `settings-folder-select`, and each
page is rendered in a `<Show when={selected()} keyed>` so another folder reads
it again. `ProjectChecksSettings`'s members' heading is "Packages (N)". Tests:
`SettingsDialog.test.tsx` "with several folders the project pages show a
folder select, the active one first", "one folder: the project pages ask
nothing", "choosing another folder shows its restricted files";
`ProjectChecksSettings.test.tsx` "the members' heading reads Packages (N)".
`SettingsMenu` and `RestrictedFilesSettings` are unchanged; the menu's "This
project · <name>" names the active folder. Departure: a
`.settings-folder-row` rule in `styles.css`.

**C06 — IntelliSense over the folders.** `sources::scan` takes `roots` and
walks them into one `Collector`, so the 5,000-file / 40 MB limit is the
project's, its folders' together, and `load_definitions`, `load_typings` and
`load_sources` take `&[PathBuf]`; `util::blocking_in_folders` keeps the roots
that are folders and errors only when none is, and the three typings commands
take `roots`. `ActiveConfig` takes the folders, configures each one and gives a
file its own folder's options (`innermostFolder`), its `covers` comparing the
folder as well as the config; `IntelliSenseProject` takes `roots` and reads each
folder's configs (`configPathsUnder`), warning by the folders' names.
`ensureProjectIntelliSense` and `isIntelliSenseReady` compare by the folders'
list, the definition opener only asks whether a project is loaded, and
`Editor` takes `loadable` (the folders that are there) for the two. Tests:
`sources.rs` "two roots' sources are both loaded" and "two roots share one
limit", `util.rs` "a root that isn't a folder is left out, and none is an
error", `activeConfig.test.ts` (new) two, `projectWorker.test.ts` "a second
folder's sources are files of the program". Departure, and a big one: the
Monaco stub in `scripts/test.mjs` now has its namespaces when a module is
loaded (Node's ESM interop copies a CommonJS module's own keys, so the stub
lists them), a `Uri` whose text is the key uris.ts builds, and an empty model
list — without them no test could load the modules IntelliSense is built on.

**C07 — rust-analyzer over the folders.** `LspClientOptions.root` is
`folders`: the process runs in the first, `rootUri` and `rootPath` are the
first, and `workspaceFolders` lists every one, in `initialize` and in the
answer to `workspace/workspaceFolders`. `rustAnalyzer.ts`'s `activeFolders`
replaces `activeRoot`, compared by `join('\n')`, and `launch`, `restartWith`,
`install` and `offerDownload` take the list; `restartRustAnalyzer()` reads it,
and only `offerRustSrc` gets `folders[0]` (the `rust-src` offer is still made
once per first folder). When the folders that are there change (one removed,
located, or back after Try again), App restarts a running server for them
through `workspaceFoldersChanged`, so another set of folders is a restart, not
a `didChangeWorkspaceFolders`; with none running it does nothing.
`Editor` calls `ensureRustSupport(props.loadable())`. Tests: `client.test.ts`
(new) "initialize lists both folders, the first one as the workspace root" and
"workspace/workspaceFolders is answered with the folders sent in initialize" —
the URI compared with the one sent, since the stub's URIs are opaque-ish.
`rustAnalyzer.test.ts` (new) counts the binary lookups: a restart for a changed
set, none for the same one, none when no server was started.
By hand: the download offer with two folders starts rust-analyzer with both.

**R01 — a section per repository.** `GitPanel.tsx` is the window's part: it
reads `git_repos` for the folders that are there (App passes
`resolvable(folders())`, so a missing folder is never offered **Initialize
repository**), shows a `GitRepoSection` (new) per repository, keyed by
`Repo.key`, and one per folder in none, keyed by `folder:<path>`, in the
window's order, and keeps the commit message and its button.
`GitRepoSection.tsx` is the old panel's body, moved as it was: status, header,
banner, errors, changes, history, stash, hooks and the resolver, each command
asked from the repository's first folder with its `scopes` — `Repo.folders`
(new) lists the window's folders in it as the window gave them. The icons moved
to `GitIcons.tsx`. One section has no title and is laid out as the panel always
was (`display: contents`); with several, each has a title row with its name and
its box for the commit, the sections scroll, and a section's changes take at
most 40% of the window's height. A section hands the panel a `GitRepoHandle`
(its status, busy, `run`, `refreshStatus`, `refreshHistory`, `push`,
`offerSkip`), which the commit works it with, and waits while the panel commits
across repositories (`locked`). Drafts are kept by `sessionKey` (a project's is
its path, as before), publish answers by repository and branch,
`GIT_LOG_LATEST` by the folder a section asks from (`Generations`, a count per
key), and ConflictResolver's `opKey` is the repository's, as each section passes
its own path. Tests: "two repositories' history reads don't stop each other"
(`git/tests.rs`, beside the superseded scan's), `repos.rs`'s shared repository
lists its `folders`, and `GitPanel.test.tsx` "one repository has no title of its
own, and asks git from its folder as before" and "two repositories show two
sections with their names and branches".
Departures: the `GitPanel`, `gitBranchMenu`, `gitRebase` and `gitChangeCounts`
tests mount the panel with one folder and a session and answer `git_repos`
(`src/test/gitRepos.ts`'s `oneRepository`); their assertions are unchanged, and
`gitChangeCounts`' event names its folder, as the watcher's has since A03.
ConflictResolver and GitHooksView needed no change: they're given the
section's path. A `git_repos` read that fails (it doesn't, in practice) shows
the panel's error with Retry.

**R02 — one commit across repositories.** `changes.rs`'s `git_commit_in` is now
`Committing`'s steps — check (nothing in progress, no conflicts, someone to
commit as), stage with the pre-commit hook, the message hooks, the commit,
post-commit — and `git_commit_many(repos, message, noVerify)` takes them across
repositories in D11's order: every repository checked first, every problem
named (`Stopped { problems }`), then every pre-commit, then every message hook
(the first failure: `Stopped`), then the commits (`Done { committed, failed }`:
one that fails is reported with those made before it, and the rest are still
made), then post-commit where one was made; "Nothing to commit." only when no
repository had anything. It holds every repository's working folder for the
whole run (`util::rewriting_all`, whose one-folder case is `rewriting`). The
panel: one section commits as it always did (`git_commit`, then a push or the
publish question); with several, `commitActionAcross` (`gitPublish.ts`) over
the included repositories picks Commit, Push or Publish, a box follows
`includable` (`includeBlocked` says why a merge, a rebase or conflicts keep one
out; nothing to commit or push leaves it unticked), and `git_commit_many` gets
the included repositories with changes. What stopped it shows above the commit
bar by repository (`stoppedText`), with **Commit without hooks** when a hook
did. After a commit that failed in some (`partialText`), the message stays and
the repositories it was made in wait in `heldPushes` (by session) until a retry
commits the rest — only the rest: a repository with the message already isn't
given it again, whatever changed in it since; then each repository the message
went to is pushed in its own section, where a failed push shows, one that was
only ahead isn't, and the branches with no remote branch get one question
(`publishQuestionAcross`). A Push that fails leaves its repository waiting with
the rest. A commit belongs to the session it was made in: its draft, what
waits for it and what stopped it are that session's, so a window switched to
another project or workspace while the hooks run keeps its own, and a
**Commit without hooks** shows only where its commit was. Each commit is dated
as it's made, after every hook has run.
Tests: `git/tests.rs`'s `commit_many_tests` — a pre-commit failing in the second
repository leaves the first uncommitted; without hooks no check runs in any; a
commit-msg hook changes its own repository's message only; each commit goes on
its repository's branch; a detached HEAD gets its commit; a repository with no
commit gets its branch; a held `refs/heads/main.lock` in the third reports the
first two; every problem a check finds is named and nothing is committed;
nothing to commit anywhere says so; a commit is dated when it's made, after its
hooks. `gitPublish.test.ts` has four, and `GitPanel.test.tsx` eight: a
repository in a merge is left out, saying why; a commit failing in one keeps
the message and pushes nothing, and the retry pushes all three; a failed push
shows in its own section; a repository that was only ahead isn't pushed by a
commit; a commit keeps to its own workspace when the window switches while it
runs; a hook that stopped one workspace's commit offers no skip in another; a
retry commits the message only where it isn't yet; a push that fails keeps its
repository waiting for the rest of its message.
Departures: `commitAction` keeps its one-repository signature (its tests
unchanged), and `commitActionAcross` is the several's. The identity is checked
with the other checks, before any hook runs, for one repository too: a commit
that can't be made no longer runs pre-commit first. A many-commit's problem
says "Changes are staged outside this window's folders: …" without the
repository's name, which the panel puts in front; `RepoCommit` carries no name,
and the outcome names a repository by the folder it was asked from.

**R03 — refreshing by repository.** A section listens to `git-changed` and
reads its status again when the event's folder is one of its own (`samePath`),
at most every 2 s, as the panel did for its one folder; the panel reads
`git_repos` again when an event says a folder's repository came or went
(`repositories`), and after **Initialize repository**. A section stays as the
list is read again (keyed by repository), with its history, menus and scroll.
A change that comes while the panel's commit keeps a section waiting is read
once the commit is done. Tests: `GitPanel.test.tsx` "a folder's change
refreshes its own repository's section only", "git init in a folder without a
repository gives it a section of its own" and "a repository left out of a
commit reads what changed in it while the commit ran".

**E01 — the chats of several owners.** `chat_db`'s `list_dialogs`, `search` and
`dialogs_of` (was `dialogs_in_project`) take the window's owners: a list of
project IDs, as one JSON array (`json_each`), so a statement's text is the same
for any number of them, or None for the chats with no project; the commands take
`owners`. `projects.ts` keeps each folder's project (`folderProjects`, by path,
started again with each new set of folders, kept on a copy's or a move's
answer), App joins it to the folders (`resolvedFolders`, apart from `folders()`,
so a folder resolving changes nothing in the tree, search or the editor) and
works out the owners (`openFolders.ts`'s `chatOwners`: the workspace's ID first,
then each folder's that's there; undefined while one resolves; `sameOwners`,
`listedBy`). `chatStore.showOwners` replaces `switchProject`: another target
lists its chats from the start, as before; the same target with other projects
reads its list again without emptying it, and keeps the open chat while its
project is listed, its loaded messages and its input with it. ChatPanel's
`showChats` recovers the unanswered sends of each project new to the window
(`unsettled_sends` keeps its one project). A row says its chat's folder with
several (`chatFolderNames`: a folder's name, a workspace chat's names or "All
folders"). Tests: `chat_db/tests.rs` "a window's owners' chats are listed
together newest first", "… are searched together", "a subagent of a workspace
chat belongs to the workspace"; `openFolders.test.ts` three for the owners;
`chatStore.test.ts` "a folder added keeps the open chat; its folder removed lets
it go"; `workspaceChat.test.tsx` (new) "adding a folder keeps the open chat and
its draft", "after a crash, an unanswered send in the second folder's chat comes
back" and "a row shows its chat's folders".
Departures: the store's test has its own name, as the draft is the panel's
(tested there). B06's `openProjectIds` now holds every folder's project, and
`resolveProject` is given the folders with their projects on Try again and
Locate…, so B04's "a copy of another folder of the window keeps its own history"
sees the other folders' projects in the app too (it was given folders without
them). The test DOM (`src/test/dom.ts`) gained `HTMLDivElement` and `NodeFilter`,
which the chat input's references use and no test reached before.

**E02 — a chat's folder.** With several folders both New chat buttons open
`NewChatMenu` (new): `FolderMenu`'s new check mode (`menuitemcheckbox` rows, Space
checks, Enter starts, a missing or unresolved folder listed but off, saying why,
as is a copy that still has its original's chats: its chat would work in the
original, the folder `chatRootOf` finds first), the folder worked in checked, one
folder at a time until F14, and **Start chat in web**. With no chat to show, the
panel shows the same menu in place instead of making an empty chat (Tab reaches
its first folder), the input waits for the chat started, read-only and saying
so, and **Add to chat**'s references go into that chat; while chats are listed
one is about to show (the open one was deleted, the next opens), and the menu
doesn't flash meanwhile. With one folder, nothing changes. An empty open chat is
reused, moved to the folder's project by `chat_move_empty_dialog`
(`store::move_empty_dialog`, which refuses a chat with a message, a subagent's,
and one on a host or moving), unless its draft names files: they're its folder's,
so the draft stays with it and the other folder gets a chat of its own. `openFolders.ts` has `chatRootOf` (the folder whose
project the chat is), `chatPathToAbsolute` and `chatPathOf` (paths with `/`,
null outside the chat's folder). ChatPanel goes through them for the run's
root, the estimate, the connector's commands, `@` (its folder's files only), the
selection, the branch offer and Implement; a reference from **Add to chat** or a
paste is named as the chat names it, and one of another folder isn't added: the
note above the input says "api/src/routes.ts is in api; this chat works in web.",
as it does while the editor's file is another folder's, which the context then
leaves out. A run of a chat whose folder isn't open here is refused. With several
folders the label by the input says "in web" (a span). **Implement** on a plan in
the editor uses the open chat when the plan is its folder's (waiting while it
works), else starts a chat in the plan's folder (`peekImplementPlanRequest`
leaves the request waiting until the chat that takes it can). Tests:
`chat_db/tests.rs` "an empty chat moves to another owner and one with a message
doesn't"; `openFolders.test.ts` six (a folder chat's root, its path joined to
its folder, a file named from its folder and one outside named nothing, a Windows
file named with `/` and found again, one whose folder is spelled in another case,
a row's names); `workspaceChat.test.tsx` nineteen, from the boxes to Implement
(the reuse that leaves a draft naming files, the input waiting with no chat, a
deletion that never shows the menu, a copy that can't be checked, Tab); `chatBranchOffer.test.tsx` "a chat in the
second folder is offered a branch for that folder's repository".
Departures: no `runPlace` yet: a run's root is its chat's folder (`rootOf`), and
`runPlace` comes with the workspace chats it tells apart (F14); **All folders**
comes with F14 too. `fileReferences.ts` and `activeContext.ts` are unchanged: the
panel names each reference as the chat does (`inChat`) wherever it comes from, so
the editor needn't know the chat. The menu shows which folder Start chat makes the
chat in ("Start chat in web"), there being one. `FolderMenu` gets the scoped
styles C04's menu missed (its icon, label, focus, separator and off rows), in
`styles.css` with the new rows' box, the note, the label and the menu in place.

**E03 — a chat's paths, opened.** App's `folderForChat` is the folder of the chat
a request names, else of the open chat (`chatFolderFor`): a subagent's chat
isn't listed, and its tool calls show inside the open chat, whose folder it
works in; the window's first folder when neither's is open here: a written file's comparison tab is `chat:<chat>:<its path in the chat>`
(`chatPathOf`), and a path from the chat opens in its own folder
(`chatPathToAbsolute`), as do the paths it mentions that become links
(`setProjectFileResolver`). The isolation bar takes the chat's `root`: a file
whose change landed since the list was read opens as itself, from the chat's
folder, rather than as a comparison of two equal versions (a deletion that
landed has no file to open, and is compared). Tests: `isolationBar.test.tsx` "a
changed file of the second folder's chat opens from the second folder", and
`openFolders.test.ts` "a file of a chat the list doesn't have, a subagent's, is
in the open chat's folder". Departure: `overlay.ts` needed no change: its paths are the
chat's, as the backend takes them.

**E04 — the client's open projects.** `rovik_client`'s `State.open` is the
window's open projects, a list (`set_open_projects`), each with its folder: the
claims' record (`Instances::give_open_projects`, the target's folders whose
project is known), which the app hands over after each claim (`project_claim`)
and each `reclaim` (`commands/chat.rs`'s `follow_claims`), while the claims are
held: two commands following them at once can't hand over the older list last.
`chat_open_project` no longer sets it, and `folder_answered` is gone: an answer
about a folder renames its project in the record, which the client follows, and
an answer for a folder the window has left is in no record. `handled_elsewhere`, `land_waiting`,
`project_of` and `is_open` look among them; a steering message's paths are made
relative to its chat's own folder (`project_of`), and a steer whose chat's folder
can't be read fails rather than send local paths; Test delivery pushes to the
first open folder with an `origin`. Tests: `remote/tests.rs` (the delivery test
with a first folder that's no repository, the steer of the second folder's chat,
a steer whose chat's folder can't be read, events shown with the project as a
workspace's second folder);
`rovik-agentd/tests/landing.rs` "a result for the second folder's chat lands
while the workspace is open"; app crate `instances.rs` "the open projects are the
window's folders' projects" and "the open projects are given before another claim
can change them", and `commands/chat.rs` "a folder resolved after the window
moved on isn't the client's open project".
Departures: the client test of `folder_answered` went with it, its case covered
by the app crate's two; the daemon's tests call `set_open_projects`, and
`landing.rs`'s answered folder is the list the claims give after the answer.

**F01 — `Folders`.** `rovik-core/src/folders.rs` is 4.1's type: `one`, `named`
(at least one folder, each a plain part, no two alike in any case), `single`,
`first`, `iter`, `split`, `abs`, `chat_path` and `output_roots`. `split` matches
a name as the file system does (folding case on macOS and Windows), `abs` refuses
a path that leaves its folder, `chat_path` tries the folder's path with links
resolved when the plain one doesn't hold the file, and `output_roots` lists the
longest first, so a folder's place in the view reads before the view's root.
`Run.project_root` is `Run.folders`; `isolated()` is the view always for a
workspace chat and today's test for a folder chat, `project_file(key)` reads the
project's copy through `abs` (the baselines, the rollback's targets and the
originals), the busy wait loops over the folders, and the uses that F04 and F10
to F12 give folders (plans, the code before a change, check roots, candidates)
read the folder chat's folder through `project_root()`, None for a workspace
chat's. Tests: `folders.rs` five (a chat path splits by its first part; `abs` and
`chat_path` go and come back; a folder chat's paths are its folder's; a workspace
chat's names are plain and none alike; `output_roots`), and every `agent_run`
suite unchanged.

**F02 — a run's folders from the chat.** `check_project` returns the run's
`Folders`: a folder chat's root, checked as before (a candidate's copy keeps its
exception, ahead of the rest), or a workspace chat's from `workspaces::chat_folders`
(the names checked for it, in the workspace's order, every folder for `"all"`, a
chat that names none taken as `"all"`, a name the workspace no longer holds left
out), with its paths. A workspace chat's request with a root is refused, and so are
a missing folder (every one named: "web isn't there: locate it, or remove it from
the workspace, to go on with this chat.") and a chat with no folder left. `prepare`
takes its folders from `check_project`, so every run, an Autopilot stage's or a
retry's included, has the chat's folders as they are when it starts: a stage's
settings hold no folders. The context estimate takes a folder chat's root as before
and a workspace chat's folders through `check_project`, counting each folder's
section. `features::FOLDERS` is defined for G02 to advertise; the frontend's
`root` says a workspace chat sends none. Tests: `check_project_tests` (a workspace
chat's run gets its folders at their paths, all of them for All folders, the new
ones after they change; a missing folder or none is refused by name; a request with
a root is refused; a folder chat's run is as before; an Autopilot stage started
after the folders changed gets the new ones) and `workspaces_tests.rs` "a workspace
chat's folders are those checked in the workspace's order". Departure: a workspace
chat's run has no root until F07 opens its view.

**F03 — what the view needs of the overlay.** `VfsError::CrossDevice` ("can't
move or link across folders") reaches the kernel as EXDEV. `overlay/fs.rs`'s
`Budget` holds a chat's limit, what its folders use together and the warning; each
`OverlayFs` keeps its own share as well (`charge` checks the limit under the
budget's lock, `count` adds what's there already, `uncount` gives back, never more
than the share), and an overlay dropped gives its share back, so a folder that
leaves the view or is opened again elsewhere isn't counted twice. The warning is
marked given only once someone is there to give it to: before, one passed before
`on_warn` was set was lost. `open(base, store, limit)` keeps its signature with a
budget of its own; `open_in(base, store, budget)` shares one. `trait ChatFs:
Backend` names what F05's view and F06 call besides the file system, implemented
by `OverlayFs` through its own methods. Tests: `overlay/tests.rs` (parts of one
chat share its limit; two writing at once can't both pass it, twenty times over;
the warning comes once, from whichever part reaches 80%; a part that goes gives
its share back) and `vfs/fuse.rs` "a cross-device error reaches the kernel as
EXDEV".

**F04 — landing across folders.** `land.rs` works on `Folders`: `plan_in`,
`land_in`, `land_checked_in` and `undo_in`, with today's `plan`, `land`,
`land_checked` and `undo` as one-folder wrappers. An action keeps its chat path
and the folder and path it resolves to (a moved file's origin too); a path in no
folder, or a folder's own name, is a conflict. Land and Undo are refused while a
folder is missing, naming every one (a folder chat's too: landing made its folder
anew with only the chat's files in it). The folders' locks are taken by
`file_guard::key`, sorted, before any file's. A workspace landing writes
`landings/<chat>/folders.json` (names and paths) in place of `project.txt`, its
journal names its folders (`folders`, empty and left out for a folder chat), and
a write that fails puts back every folder written before it. `folders_of` reads
either file in place of `project_of`; `relocate` points a workspace chat's
landings at a located folder (F06 calls it). Recovery resolves each entry
through the chat's folders and writes nothing into one that isn't there, its
entries waiting with the landing still applying ("not finished: its folder isn't
there"), for every landing. Undo after the chat's store is gone works from
`folders.json`; `overlay_undo` tells each folder's other chats of their files.
Tests: `overlay/tests.rs`'s `across_folders` (changes in two folders land under
one landing, with `folders.json` and no `project.txt`; a conflict in one folder
lands nothing in either; a move within a folder; a failed write in the second
folder puts the first back; an interrupted two-folder landing is finished at
start; one whose folder is missing waits for it and is finished where it's
located; undo once the store is gone; Land and Undo refused while a folder is
missing; three chats landing at once, two workspace chats listing the folders in
opposite orders and a folder chat, all finish; a folder chat's journal has no
folders).

**F05 — the view.** `overlay/composite.rs`'s `CompositeFs` is a `Backend` and a
`ChatFs` over the chat's folders' `OverlayFs`s, its parts. A part's inode `n` is
`(n << 16) | slot`; the root keeps 1, and is checked for before a number is
decoded. The root (0o555) lists the folders by name, looks a name up as the
overlays do (folding case on macOS and Windows), and refuses creating, linking,
removing and renaming there and setting its attributes or extended attributes
(EPERM); a rename or a link between two parts is `CrossDevice`. Calls hold the
view's lock only to find their part. `changes` names each part's from the view's
root (`OverlayChange::under`, which the land tests use too); `versions`,
`rebase_file` and `revert_file` go by a path's first part; `usage` and `used` are
the shared budget's. A part's notifications reach the kernel renumbered through
the view's listener, a part added later's too. `matched(folders, open)` matches
the view to a list of folders: one at the same name and place stays, one new or
located elsewhere is opened (before the view changes, so one that can't be
leaves it as it was) and gets a new slot, and one gone leaves, its part handed
back for its store to go; the kernel is told of each name at the root, and the
generation moves. `OverlayFs::base` names a part's project; the separator rule
(`/`, and `\` on Windows only) is the overlay's in the view and in `Folders` too.
Tests: `composite_tests.rs`, the plan's eleven.

**F06 — a workspace chat's overlay.** `chats::open(chat, &Folders, on_warn)` opens
either kind: a folder chat's store as before (`project.txt`, one `OverlayFs`), a
workspace chat's with `folders.json` (its workspace's ID and its folders at its last
run) and a part per folder (`parts/<name>/`), its view a `CompositeFs`. `Folders`
carries the workspace's ID (`in_workspace`), which `check_project` sets. An open view
is matched to the folders at every `open` (each run's start) and at Apply
(`set_folders`): a folder that joined is opened, one located elsewhere is opened
there from its saved list (saved first), and one that left goes with its store,
refused while the chat has changes there; the folders that left while the view was
closed lose their stores at its next opening, under the same rule, all or none. A
store of the other kind is refused before anything is written, and a view is
recorded only once it's mounted, so one that couldn't be leaves no workspace store
behind (`exists`: `manifest.json` or `folders.json`). `ChatOverlay.fs` is an
`Arc<dyn ChatFs>`, and `project` gave way to `folders()` (a child's: its parent's
view) and `folder()`. `folders_of` (the open view's, else the store's record) replaces
`project_of`, `saved_project` and `project.txt`'s other readers: `with_fs` opens a
closed workspace chat's parts whose folders are there in an unmounted view, a closed
chat's bar reads each part (a missing folder's from its saved list,
`fs::saved_changes`), and the project's version of a chat path is read through
`Folders::abs`. The watcher's `project_changed(folder, …)` reaches each open chat's
part on that folder and names what it notes and marks by chat paths; clashes are kept
per (chat, folder), compared by path in the folder, and refreshing one folder keeps
the chat's others (and what it was told of them). Children stay one `OverlayFs` over
the parent's view and land through it, scratch views take `&Folders`, `project_for`
keys a workspace chat's commands' history by `workspace:<id>`, and `CARGO_TARGET_DIR`
is shared by a workspace's chats. `changes_in_folder(chat, name)` reads the open
view's part, else that part's saved list; `relocate` moves one chat's record of a
located folder (its open view matched, its closed store's record, its landings'
`folders.json`). D19's guard is in `chat_db::api`, which the commands call:
`workspace_remove_folder` and `workspace_relocate_folder` are refused while a chat
working in the folder (All folders, or checked) is at work on this computer, has
background commands running, or (Remove, or Locate… to another project: the folder
found's ID file isn't the project last opened where it was) has changes there, naming
each chat and what to do. After Locate… each chat follows, and landings a crash cut in
the folder are finished (`land::recover_in`), as `chat_open_project` (Try again) does
for a folder found again. Tests: `overlay/workspace_chat_tests.rs` (common tools
across the folders of one view; `mv` between folders lands as an add and a delete;
the view's root is read-only to commands; a workspace chat reopens with each folder's
changes, and its bar lists them while it's closed; opening its store as a folder
chat's is refused and leaves the store as it was, and the other way; the view gains a
folder checked and drops one unchecked with its store, but not one with changes; the
project changing under one folder notes paths with its name; a folder chat and a
workspace chat see each other's clashes; refreshing one folder keeps the other's
clashes; `changes_in_folder` answers while the folder is missing; locating it as the
same project keeps the changes, open and closed; a worker lands into the right
folders; `CARGO_TARGET_DIR` and the commands' history are the workspace's for each of
its chats), `chat_db::api` (removing a folder a workspace chat is at work in, runs
commands in or has changes in is refused, naming it, and otherwise allowed, leaving
the chats' folders; Locate… while a chat is at work is refused, and to another project
while one has changes there, and to the same project the changes go with the folder;
after Locate…, and when a folder is found again, a landing a crash cut there is
finished), `check_project_tests` and `folders.rs` (the workspace's ID). Departures:
the guard and its tests are the core's, beside the other workspace commands, and the
app's commands stay wrappers (G05 asks the hosts there); a workspace chat's commands
see its folders' own paths as they are, read-only, and naming one is refused, until
F09 points them at the view; a folder located to another project keeps what the
chat's commands built there (it has no changes there, D19); the run still opens folder
chats only until F07.

**F07 — a workspace chat's run in its view.** `prepare` opens a workspace chat's view
for every run, Plan mode included (`workspace_view`): the view is matched to the
chat's folders as `check_project` gives them, and the run's root is the view.
Without the driver (the view can't be mounted) the run is refused with "A chat across
folders needs the virtual files driver (Settings → Virtual files), or start a chat in
one folder.", the mount's own reason after it; with isolation off it's refused saying
so; any other reason the view doesn't open (its store a folder chat's, a folder left
with changes) is named. It never works in a folder instead. Landing through `Folders`
and the busy wait over every folder came with F06 and F01. Tests:
`workspace_e2e_tests.rs` with `workspace_harness` (a workspace of folders by name and
a chat of it, its folders checked or all): a workspace chat reads and edits two
folders by their names and lands in each, nothing elsewhere; a folder not checked for
it isn't in its view; Plan mode works in the view and changes nothing; it's refused
without the driver (a file where its view would mount) or with isolation off, and
works in no folder instead; Land and Undo are refused while a folder is missing.
Departure: isolation off has a message of its own, as the driver's would mislead.

**F08 — what the agent is told of a workspace chat's folders.**
`project::load_folders(config_dir, root, &folders, verify, below, since)` gives a
workspace chat's run its checks, its project section and its restricted files. Each
folder is read in the view (`root/<name>`; at its own path without one) as when it's
open alone, by its own settings: its stack as `"{stack} ({name}/)"`, V09's folders
one level down inside it for an Autopilot task (`{name}/{sub}/`), its services line
as `{name}: …`, its checks run in it (`cd api && …`), and its instructions under its
name. The instructions share 12,000 characters (`WORKSPACE_INSTRUCTIONS_MAX_CHARS`):
each folder an even share, a short one's rest going to the others (`shares`), each
cut by `project_profile::cut`, which `read_instructions` now uses too;
`project_context` and `load_folders` share its checks and instructions parts. The
section opens with 4.4's paragraph, then a git line (a repository, `git -C api
status`; a folder of one whose root isn't in the view; none) and, when the chat's
folders changed since its last run, which joined and which left: each workspace
chat's run records its folders' names in its message's `meta.folders` (the latest
user message, or a stage's), which the next run compares with.
`RestrictedFiles::of_folders` hands a path to its first part's folder's own list
(`entry_for`, `blocks`, `blocks_file`, `skips`), so a folder named `examples` keeps
the verdicts it has alone, and `denied` scans each folder from its own root, naming
its files from the view's. Tests: `project.rs` (shares; each folder read as alone
under its name, its stack, services, checks, V09 folders and git line; the folders
that joined and left), `restricted_files.rs` (a workspace chat's lists go by a path's
first part, a harness denied each folder's files by their paths in the view), and
`workspace_e2e_tests.rs` (each folder's instructions, checks and git line are in the
prompt under its name; the instructions share 12,000 characters and a short one's
rest goes to the others; a folder's restricted files are refused by their paths in
the view, and only its own, `examples` included; after the chat's folders change,
the next prompt says which joined and which left, from the last run's message).
Departures: the services line and V09's folders are tested on `load_folders` (a
Compose of the test's, an Autopilot task's `below`), not through a run; the context
estimate keeps adding each folder's own section until F12 caches `load_folders` by
the chat's folders.

**F09 — commands in a workspace chat's view.** A `cd` into the project passes the
allowed commands, in every chat (D26, `permissions::cd_into_project`): a `cd` part
with one relative argument, no `..` part, not starting with `-`, `~` or a separator,
and with no `$`, backtick, `*`, `?`, `[`, `{` or `:` in it runs nothing, so it's let
by and counts as read-only, and the command's other parts are checked as before;
the README's Getting started, step 3, says so. `Confine` takes a list of `Redirect`s
(a place in the view, shown at a real path) in place of its `project`: a folder
chat's view at its project's path, a workspace chat's `<view>/<name>` at each
folder's own path (`redirecting_all`), each a bubblewrap bind; `redirects_path`
says whether a path is one. `command_problem(command, confined)` refuses a folder's
real path in a command unless the chat's sandbox shows the view there, so always
in a harness's commands (`execute.rs` passes `confined` false, as they run
unconfined) and where the sandbox can't redirect (Seatbelt, Landlock: `confine()` is
None, and the hiding-only policy applies). After a command fails for a missing package,
`install_dependencies` keeps the installs of the folder the command `cd`s into
(`project::folder_of`), of the project's own and of the folders below it: `cd web &&
npm test` installs web's dependencies, not api's, and a command at the top every
folder's, as before. Tests: `permissions.rs` (with the default allowed commands,
`cd api && npm test`, `cd 'my app' && npm test` and `cd api/src && ls` are
permitted, and `cd .. && npm test`, `cd api/../..`, `cd /tmp`, `cd ~`, `cd $X`,
`cd -`, `cd *`, `cd {..,x}`, `cd {/tmp,x}`, `cd api && rm x` and a bare `cd` aren't;
every name `folder_name` gives, as `in_folder` writes it, passes), `confine.rs`
(each folder's path shows its place in the view, under bubblewrap),
`workspace_chat_tests.rs` (a command naming a folder's own path is refused where it
wouldn't see the view there, and a harness's always), `environment.rs` (a command in
a folder needs that folder's install, one at the top every folder's) and
`workspace_e2e_tests.rs` (a command in each folder runs there and sees the chat's
changes; one naming a folder's real path sees the view or is refused). Departures:
the rule refuses `{` too, since a brace expansion gets past the others (`cd {..,x}`
is `cd .. x`, which some shells run as `cd ..`), so `folder_name` makes braces
plain (`a{b,c}` is `a-b,c-`) and every name still passes; the refusal of a folder's
path is tested beside the overlay that makes it (`workspace_chat_tests.rs`), not in
`confine.rs`.

**F10 — the checks of the folders a workspace chat changed.** `Run::enabled_checks`
gives a workspace chat the checks of the folders it changed (the first parts of
`touched_keys()`) and those in no folder, a plan's own commands (D27,
`checks_changed`); a folder chat's all run, and the landing's `checks_would_run` gets
the same list. With none, the record says "no checks are set up for the folders it
changed". A folder's failed type check skips only that folder's tests, in every chat.
Paths in output read through `OutputRoots` (folders.rs), which `Folders::output_roots`
now gives: an absolute path in a folder, in its resolved spelling, in its place in the
view or in the view itself is a chat path (`chat_path`, `in_text`), and `from_folder`
tries a path a `cd api && …` check printed relative to its folder as `api/<path>` when
that's in the view. `check_keys` and `failed_in` use both, so a check's keys name files
by their chat paths. `Run::output_roots` replaces `check_roots` and the `[root,
project]` lists in P3.2's slips (`printed_is_written`), V05's files behind and a
failure's causes (`causes::resolve`, `path_context`); `without_roots` and `roots_of`
went. The before-run opens a scratch view of the chat's folders under the same names
(`code_before` gives `Folders`), its output read through that view's roots, and so does
the red check, which was skipped in a workspace chat. File-to-command helpers:
`in_folder("")` is the command as it is; `light_checks` groups a chat's files by folder
and runs each folder's light checks in it; `in_files_folder` runs one file's command
(the breaker's, the red check's) in its folder; the red check's fallback is the test
check of the file's folder; V06's scoped tests are the timed-out suite's folder's, run
there. Tests: `workspace_e2e_tests.rs` (only the changed folder's checks run, in it;
with none in the folder it changed, none runs and the record says why; a failed type
check in one folder doesn't skip the other's tests, and its failure names
the folder's file; a check failing as before the change is set apart per folder,
printing paths relative to its folder or absolute, the view's and then the copy's; a
slip printed by a folder's real path, whose last part isn't its name (`y/app` is
`y-app`), is the round's own; the red check runs each new test in its folder, Go's),
`folders.rs` (a folder's path, its resolved spelling and its place in the view read as
its name, the longest first; a path printed in a folder is tried as that folder's
first), `command_output.rs` (a folder's check names its files by their chat paths),
`causes.rs` and `repair.rs` (a folder's real path reads as its name for a cause, the
files behind and a slip), `project.rs` (light checks and a file's command in each
folder) and `verify.rs` (D27's filter; V06's scoped tests by the suite's folder).
Departures: a plan's own commands running whatever changed, and `checks_would_run`'s
filter, are tested on `checks_changed`, not through a run: a plan's commands are an
Autopilot task's (F11, F12), and a local run merges the project's changes into its
view and checks them before it lands (`take_project_changes`), so the landing merges
only when the project changes a file in between; the mid-run check's locations keep
`causes::resolve`'s tail matching; Thorough's candidates run a file alone at their
copy's root until F12.

**F11 — plans of workspace chats.** `submit_plan` saves a plan in the chat's first
folder (`Folders::first`; a folder chat's is its folder, as before), and a workspace
chat's with `Folders: api, web` under its title (`FOLDERS_LINE`, `with_folders_line`),
which a revision rewrites; a folder chat's plan is saved without the line, even one its
model wrote, as Implement goes by it. A revision replaces the chat's plan file while
that's in one of the chat's folders; once its folder has left the chat, the next plan
is a new one in the new first folder, and the earlier one stays where it was written.
`plan_path_in` names a plan as the chat names files: a folder chat's relative to its
folder (`relative_plan_path`, with the system's separators, as before), a workspace
chat's by its chat path (`Folders::chat_path`). It replaces `relative_plan_path` with
the request's root or the view in "Saved the plan to", the Plan-mode "Saved in" line
(through the chat's folders, not the view: section 12, item 14), the implementation
request (`expand_implementation`, which finds a relative `planPath` with
`Folders::abs` and refuses one that leaves the chat's folders, which it used to join
to the root as it was), the plan check at the start (`plan_file_named`, through `Folders::abs`
too), the change reviewer's, the test writer's and a worker's plan, and Autopilot's
Plan stage summary and the implementation it starts, through the chat's folders
(`chat_folders`: `check_project`; a summary names the plan in full while a folder is
missing, and an implementation that can't have them doesn't start, saying why); a
candidate's names it from its task's project. `Files:` lines were already checked
against the view. In the app, `planFolders` reads the line, App sends its folders with
the editor's **Implement this plan**, and `chatForPlan` chooses (D28): the open chat
when it's a workspace chat of this workspace that works in each folder named, else a
new workspace chat across them, even one folder (an empty open chat is reused, moved to
the workspace with them: `createDialog` and `moveEmpty` take folders), or none, saying
which folders this workspace lacks, or that a folder's window has no workspace for it.
**Implement this plan** under a workspace chat's plan reads its line, so a chat that no
longer works in each of them hands the plan to one that does. `chatPlanPath` names the
plan in the message as the chat does (`chatPathOf`, after the folder's name in a
workspace chat), in place of `relativePlanPath`, which went. Tests:
`workspace_e2e_tests.rs` (a workspace chat's plan is saved in its first folder's plans
under its `Folders:` line, after one naming `src/a.ts` is sent back; the Plan-mode
line, the implementation request and its plan check name the plan by its chat path, the
check finding `web/src/gone.ts` by its folder; after the first folder is unchecked
with `store::set_dialog_folders` the next plan goes to the new first folder, and the
earlier stays; Autopilot's Plan stage summary and the implementation it starts name the
plan by its chat path), `plan.rs` (the line under the title, rewritten once, a folder
chat's left out; a plan named by its chat path; a relative `planPath` that leaves the
folder refused), `worker.rs` (a workspace plan's chat
path found in its folder), `plans.test.ts` (a plan named as the chat names files, on
Windows too; the line read only under the title; the choice of chat, even for one
folder; a lacking folder named, and a window without a workspace) and
`workspaceChat.test.tsx` (Implement on a plan naming folders makes a workspace chat
across them, even one, or uses the open one that works in them; an empty chat moves to
the workspace with them, an empty workspace chat in other folders too; a lacking folder
starts nothing and says which; Implement under a workspace chat's plan uses it while it
works in each folder, else a new chat across them).
Departures: a workspace chat started by Implement doesn't run until F14's `runPlace`:
until then the panel refuses a run of a chat with no folder of its own open here, as it
did (`beginRun`). The app names a folder chat's plan with `/` on every system
(`chatPathOf`, 3.2), while the backend's keeps the system's separators (P4.9). The
workspaceChat tests' window lists the workspace's own chats under its id, as
`chatOwners` does (two tests that expected the harness's `workspace:` key changed with
it), and Autopilot's test helpers are shared with the workspace tests.

**F12 — Thorough, Autopilot, diagnostics and the estimate with folders.** Thorough's
candidates take the task chat's `Folders` where they took its project: `copies_for`
needs the chat's folders rather than a root (a workspace chat is always isolated, D23),
`Copies::open` takes them to open the chat's view where it isn't open (the server's
worktrees ignore them, as they did the project), `make` and the Pick open the copies
so, and a candidate's record keeps the task chat's folders (`add_candidate`,
`candidate_folders` in place of `candidate_project`), from which its run loads every
folder's context, checks and restricted files (`load_folders` over its copy, which has
the folders under their names); a folder chat's candidate loads its folder's as before.
A candidate's implementation request names the plan as the task chat does
(`api/.rovik/plans/…`, which its copy has at the same path), and the Pick runs each
test written first alone in its folder of the copy (`in_files_folder`). Autopilot
needed no change of its own: each stage's run takes the chat's folders from the chat
(F02), and the delivery reads a closed chat's store of either kind (`pending`,
`exists`). The diagnostics tool finds the editor's markers through the chat's folders
(`CallContext.folders`): a workspace chat's `api/src/a.ts` as api's `src/a.ts`, a
folder chat's path in its folder, so an isolated chat finds them (section 12, item
10); they're given only for a file the run sees as the project has it (its bytes in the
view are the folder's), and one the chat changed or made says so, in its diagnostics
and the summary. The estimate's project section is what the chat's run is told
(`cached_context` over `Folders`: `load_folders` for a workspace chat, no longer each
folder's own section added up), kept for a few seconds by the folder list. Tests:
`workspace_e2e_tests.rs` (a scout reads both folders, and a step's worker writes its
step's folder and is refused the other's; an Autopilot task runs to its delivery, its
check run in its folder, and, cut between two stages with its view closed as a restart
leaves it, gets the chat's folders again; a Thorough task's candidates each work over
the whole view, told every folder's instructions, checked by api's own check and given
the plan by its chat path, and the smaller change lands while the other's file in web
doesn't; the Pick runs a test written first in its folder, so the candidate that makes
it pass wins over a smaller change that doesn't; the diagnostics tool finds the editor's
markers through the view and none for a file the chat changed, and an isolated folder
chat finds its folder's), `agent_tools.rs` (a view's file has its folder's markers
unless the chat changed or made it) and `project.rs` (the estimate's section is the
folders as the run is told them, by the folder list). Departures: 4.9's `folders` in
Autopilot's `Settings` isn't needed, as a stage's run takes them from the chat (F02).
A candidate's own folders stay its copy's (`Folders::one`), so D27's filter doesn't
apply in it: it runs every folder's checks. "Hasn't changed" is read by content, which
also covers a subagent's view over its lead's and a file changed back. The estimate
leaves out the line on folders that joined or left (it has no last run to compare
with), and the diagnostics' type-check hint still looks at the view's root, where a
workspace chat's folders' stacks aren't. Reopening a workspace chat's view from
`Copies::open` after a restart is tested for folder chats only (the candidates' restart
tests); the Autopilot and subagent test helpers it uses are shared with the workspace
tests.

**F13 — connectors told the chat's section.** A workspace chat's run marks its request
as made in a view of its folders (`ChatParams.folders_view`), and the connector's turn
then carries the chat's section of the prompt for a harness with its own tools
(`Turn.own_tools_context`), which finds no AGENTS.md or CLAUDE.md at the view's top to
read: Claude Code keeps its own prompt with the section appended from a file
(`--append-system-prompt-file`), and Codex gets it as its thread's developer
instructions. A folder chat's turn gets nothing appended: the harness reads its
folder's own instructions, as before; with Rovik's tools the section went already, in
Rovik's prompt. The harness works in the view, the run's root, and resumes its session
there, the view's path being the chat's own on every run. Tests:
`workspace_e2e_tests.rs` (Claude Code works in the view, its change landing in its
folder, keeps its session across two sends from the same path, and is told the
folders; Codex is told them as its developer instructions) and `claude_code.rs` (a
workspace chat's section is appended to Claude Code's own prompt from a file, and a
folder chat's turn has none). Departures: a file rather than `--append-system-prompt`'s
text, as Rovik's own prompt goes: the section can be long, and Windows' command line
would overflow.

**F14 — workspace chats in the UI.** A chat's root is `ChatRoot { kind, folders }`:
a folder chat's folder, or a workspace chat's folders in the workspace's order (every
one of them for All folders, else those checked that it still holds), a missing one
among them. `chatRootOf(dialog, folders, workspace)` takes the window's workspace's
ID, so a chat of another workspace works nowhere here; the chat panel gets it as a
prop from the window's target, so a workspace chat's root doesn't come and go while a
folder resolves. `chatPathToAbsolute` finds `api/src/x.ts` in the folder named `api`
(an unknown name names no file), and `chatPathOf` names a file of any of its folders
under that folder's name. `runPlace(dialog, root)` is what a run is sent: a folder
chat's folder; no root for a workspace chat, whose folders the backend reads, or for
a chat with no folder; or why it can't run here: its folder or workspace isn't open,
or a workspace chat's folder is missing, named as the backend names it. The run, the
estimate, a move's start and the connector's commands' folder go through it. `@`
lists each of the chat's folders' files (`list_project_files` per folder), and the
editor context, Add to chat and the note above the input name files as the chat
does; `chatPlanPath` gave way to `chatPathOf`. New chat's menu has All folders, a
separator and the folders' boxes: several checked, or All folders, make a workspace
chat (`startChatOf` with the names or `"all"`, an empty chat reused). Checking All
folders checks every box, unchecking a box unchecks it, unchecking All folders leaves
the boxes, and every box checked by hand isn't All folders. `acrossFoldersBlocked`
(`virtualFiles.ts`) says when a chat across folders can't start: without the driver
here ("Needs FUSE: Settings → Virtual files", the driver's own name), or on Windows
while WSL isn't connected or its Rovik doesn't serve `folders`, which the UI learns
from `HostSummary.features` (`Remote::features`, from the daemon's `Welcome`, read
again whenever a host is ready). A workspace chat's label is a button: it opens the
same menu with the chat's folders checked and Apply. `chat_set_folders` is
`chat_db::api::chat_set_folders`: refused for a folder chat and for no folder of the
workspace checked; while the chat is at work (held here or in another window, or its
Autopilot task's latest stage waiting out a rate limit, armed or not) or has
background commands running; and when a folder it would leave has changes that
haven't landed, naming the folders. It writes the folders, then matches the view
(`overlay::chats::set_folders`), and puts the folders back if the view refuses. In the
menu, Apply is off while the chat is at work, has commands running (named, with Stop
its commands), or has no folder checked; a missing folder the chat has can be
unchecked but not checked again; a refusal is shown and the chat keeps its folders.
`workspaces::checked` is the filter `chat_folders` used, shared with Apply. The branch
offer (D29) takes the chat's folders: one folder is its own repository, several are
grouped by `git_repos`, and the repositories of the folders new to the chat (all of a
new chat's; else those not among its folders at its last run, which its run records
on its message, `meta.folders`) on their main branch are named in one question with
one name, those on another branch said to stay. Yes makes the branch in each in turn;
one that fails says which have it, and Yes again makes it in the rest. Tests:
`openFolders.test.ts` (a workspace chat works in its folders checked or all of them,
and nowhere outside its workspace; its path is found in the folder its first part
names, and an unknown name names no file; a file is named under its folder's name, on
Windows too; its run is sent no root and no folders, and a missing folder stops it,
naming it; rows say its folders), `virtualFiles.test.ts` (new: a second folder needs
the driver and says how to get it; on Windows, Rovik in WSL connected and serving
`folders`), `cloud.test.ts` (a host ready again is read again, with its features),
`hosts_page.rs` (a host's summary carries what its daemon serves), `chat_db::api`
(Apply sets a workspace chat's folders and All folders, refuses none checked and a
folder chat; it's refused while the chat is at work, its task waits out a rate limit,
or it runs a command, and when a folder it leaves has changes, named, and a missing
folder without changes is unchecked), `workspaceChat.test.tsx` (checking api and web
makes a workspace chat across them; All folders checks every box, unchecking one
unchecks it, and every box by hand isn't All folders; without the driver a second box
can't be checked, and All folders says why and opens Settings; it's off while a
folder is missing; a workspace chat's run is sent no root, and doesn't start with a
folder missing; `@` lists its folders' files under their names; Add to chat from
either folder names the file under its folder's name, and another folder's stays out;
rows; the label opens its menu, whose Apply changes the folders and is off with none
checked, with a command running, named and stopped from the menu, and while the chat
is at work; a refusal is shown; a missing folder the chat has can be unchecked; a new
workspace chat asks once about its repositories; a folder that joined on its main
branch is offered the chat's branch at the next send), `isolationBar.test.tsx` (a
workspace chat's changed file opens from the folder its path names),
`chatBranch.test.ts` (repositories on their main branch are offered one branch, named
once, one on another branch keeping it; two folders of one repository are asked about
once; a branch that fails in one says which have it, and Yes again makes it in the
rest; a folder that joined is offered the branch the chat's other repositories are
on) and `NewBranchDialog.test.tsx` (the repositories it's made in, and those that
stay). Departures: without the driver a second box isn't off: checking a folder moves
the check, as in E02, and All folders carries the reason and the way to Settings,
since every other folder's row saying "Needs FUSE" would read as if no chat could
start in it; in the label's menu neither the driver nor a copy's shared history keeps
a box from being checked, as the chat exists and names its folders. The guard and its
tests are the core's, beside D19's, and the app's command is a wrapper. The branch a
joined folder is offered is the one the chat's other repositories are on when they
agree on one off their main branch, else a new name: nothing is stored for it. Found
on the way, not changed: no TypeScript config checks the component tests
(`*.test.tsx`: `tsconfig.json` excludes them and `tsconfig.test.json` takes
`*.test.ts`), so those changed here were checked by hand.

**G01 — a workspace chat's folders in a start.** `StartRequest.folders` is a list of
`RunFolder` (its stored name, and its settings as `project_settings` is a folder
chat's), in the workspace's order, for a daemon that serves `features::FOLDERS`. It's
empty for a folder chat's start and for a run here, whose folders the app reads
itself, and then it's left out, so a start without it, an older app's, parses as
before. Every start built in Rust names it (`Vec::new()`), the Autopilot driver's
stage requests included until G02 gives them the task's folders. Tests:
`rovik-protocol` (a workspace chat's start round-trips with its folders; a start
without them parses with none and writes none; a folder without settings has none).

**G02 — the daemon runs workspace chats.** The daemon serves `features::FOLDERS`.
`check_project` takes the start's folders: on a daemon a workspace chat's start names
them, and they're `Folders::under(root, names)`, each a plain name, a top-level folder
of the run's root, the chat's worktree (a Thorough candidate's the same, under its own
worktree); folders without a root are refused. `Folders::root()` marks that form: its
run works in the worktree, never a view (`run.rs`), and isn't isolated
(`Run::isolated`), so, as for a folder chat there, no code before a change is compared
(a check failing there is the change's), nothing lands and the red check is skipped.
The folders a run's checks follow (D27) also take the worktree's own `git status`
(`server::unlanded`): on a daemon that's what earlier runs and a merge changed that
hasn't landed, the worktree's index being reset to the landing base (sync.rs,
results.rs), as the overlay lists it here. Autopilot's `Settings` keep the start's
folders, so its stages, retries and candidates (`main_settings`) run with them. The
daemon checks and keeps a start's folders before the start itself (`keep_folders`):
each is a folder at the top of the worktree, or nothing yet (made then, as git keeps
no empty folder), never a file or a link; each one's settings are saved for
`<worktree>/<name>`, and the names go in `StartSettings.folders` (serde default) for
restarts and checked merges. The folders its last start named and this one doesn't
leave the worktree, ignored files and all, under the chat's hold on its files: at that
start rather than "at the next sync", as a sync names no folders and the start after
it does. A workspace chat's result holds its folders and what its base has at its top;
what its runs made there besides stays out, named in `ChatResult.outside` (serde
default), but `.rovik`, which holds the ID file the server writes there; its changes
(`WorkChanges`) leave out the same. `unchecked` is true only when no check of the
folders the result changes (its top-level entries that differ from the base) would
run, nor one that names no folder (`checks_changed`, now public). Its delivery isn't
pushed to a branch and fails saying to land its result from the app (D30). Test
helpers moved for sharing: `next_result` and `files` to `tests/common/project.rs`,
`thorough_model` and `implementation` (now given the plan and the file) to
`tests/common/task.rs`. Tests: `rovik-agentd/tests/folders.rs` (the daemon says it
runs chats across folders; a two-folder chat's first sync gives one worktree with each
folder under its name, and its run works there told each folder's checks by its
settings, checking the folder it changed; a result lands unchecked only when none of
its folders' checks would run; a merge checked on the server runs the changed folders'
checks, not another's that would fail; a check failing there is reported as failing,
not set apart; a file made at the worktree's top is named in `outside` and isn't in the
result, nor the server's `.rovik`; a restart keeps the chat's folders; a folder a later
start leaves out leaves the worktree with its ignored files; a start whose folder isn't
`root/<name>`, or is a file there, is refused before anything changes; a Thorough
task's candidates work with the chat's folders, so web's failing check never runs in
them), `results.rs` (a chat across folders isn't delivered and says why: a unit test,
as a whole task's delivery needs npm), `folders.rs`, `check_project_tests` (the daemon
form; a folder that isn't at the top of the root, and folders without a root, are
refused; an Autopilot stage on a daemon works in its start's folders), `server.rs` (a
worktree's unlanded changes are its git status), `workspace_e2e_tests.rs` (on a daemon
a workspace chat works in its worktree by its folders' names, no view; a check failing
there is reported as failing, which a copy of the worktree, where FUSE works, would
have set apart). The daemon tests run without virtual files, so the "not set apart"
case only fails without the change in the core's test.

**G03 — a workspace's composite shadow.** `shadow/composite.rs`: `Composite::open`
makes a bare repository whose alternates are the folders' shadows' object folders,
written only when they change, so it reads their objects and, through theirs, the
folders' repositories'. `compose(chat, parts)` takes each folder's name and its own
tree (`Shadow::folder_tree`: the tree at the folder's place in its repository's
snapshot, K31) and commits one tree with each under its name, on the chat's previous
one, at `refs/rovik/snapshots/<chat>`: the folders' history isn't the composite's
(D30). An empty folder is left out, as git keeps no tree of it, and an unchanged tree
keeps its commit. `graft(name, shadow, chat, (base, result))` copies the objects of
the folder's part of a fetched result and of its base from the composite into the
folder's shadow, each object after what it holds, so a tree there is whole; then it
commits the base, and the result on it, there on the folder's own tree (the chat's
snapshot in that shadow, with the part at the folder's place; a folder at its
repository's root has the part as its tree), and returns the result's commit, for
`Shadow::import`. The shadow's commit and packing became free functions
(`rovik_commit`, `upkeep`, `pack`, `empty_tree`), shared by snapshots, folds and the
composite. Where the composite lives (`shadow::dir` of the workspace's ID) and when it
opens are G05's. Tests (`tests/shadow.rs`): composing two folders' trees puts each
under its name, read through two levels of alternates; a chat's compose has only its
folders, and none that's empty; an unchanged compose keeps its commit, and a changed
one goes on it; grafting a folder's result copies its objects into the folder's shadow
and commits on its current tree, a folder of a repository keeping the rest of that
repository's tree.

Before H, a user of a project sees none of this either: a window still has one
folder unless `--workspace` asks for one, and its Git panel and its chats look
and work as they did. What does change is that two tabs of one name say where
each one is, the project checks page says "Packages (N)", and a chat names the
editor's file by its path in the project (an isolated chat's tools refuse a full
path, section 12): a file from outside the project isn't named to the chat, and
the message box says why.
