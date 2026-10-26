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
it: a folder inside a held repository is busy, one beside it isn't.

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

Before H, a user of a project sees none of this: no menu makes a workspace, and
a window still has one folder (A's note stands). The visible changes are still
the fixed bugs of section 12 and the README's Git panel section.
