import {
  batch,
  createSignal,
  createMemo,
  For,
  Show,
  onMount,
  createEffect,
  onCleanup,
  Switch,
  Match,
  on,
} from 'solid-js';
import * as monaco from 'monaco-editor';
import FileTree from './components/FileTree';
import Tabs from './components/Tabs';
import Editor from './components/Editor';
import BrowserView from './components/BrowserView';
import MarkdownViewSwitch from './components/MarkdownViewSwitch';
import ChatPanel from './components/ChatPanel';
import { actionableRows, createFileTreeController, withoutFolderRows } from './lib/fileTree';
import { languageForFile, isLikelyBinary } from './lib/language';
import {
  pickProjectFolder,
  readFileContent,
  writeFileContent,
  startFileWatch,
  CHANGED_ON_DISK,
  pathExists,
  dirEntryCount,
  getPathId,
  createFile as createFileOnDisk,
  createFolder as createFolderOnDisk,
  deletePath as deletePathOnDisk,
  isTrashUnavailable,
  renamePath as renamePathOnDisk,
  movePath as movePathOnDisk,
  copyPath as copyPathOnDisk,
  fsErrorMessage,
} from './lib/commands';
import { loadLastProject, saveLastProject } from './lib/projectHistory';
import { checkProject, claimProject, openNewWindow, showNoProjectWindow } from './lib/instances';
import {
  claimTargetOf,
  locateFolder,
  openAtStart,
  removeFolder,
  resolvable,
  switchTarget,
  type FolderChangeCalls,
} from './lib/switchTarget';
import type { FileWatchPayload, GitStashFilePreview, OpenFile, TreeRow } from './types';
import { findEnclosingSymbol } from './lib/activeContext';
import type { ActiveFileContext, CursorInfo } from './lib/activeContext';
import Resizable from './components/ResizableDiv';
import TerminalPanel from './components/TerminalPanel';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import FileContextMenu from './components/FileContextMenu';
import { GitPanel } from './components/GitPanel';
import {
  chooseDialog,
  confirmDialog,
  ConfirmDialogHost,
  type ConfirmOptions,
} from './components/ConfirmDialog';
import { NewBranchDialogHost } from './components/NewBranchDialog';
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  rememberProjectSession,
  savedProjectSession,
  sessionInFolders,
  type ProjectSession,
  type SessionFile,
} from './lib/projectSessions';
import NotificationHost from './components/NotificationHost';
import ProjectSwitcher from './components/ProjectSwitcher';
import NewProjectDialog from './components/NewProjectDialog';
import ProjectList from './components/ProjectList';
import {
  chatsBlocked,
  currentProject,
  folderProjects,
  newFoldersGeneration,
  projectList,
  refreshProjects,
  resolveProject,
  workspaceTarget,
} from './lib/projects';
import { chatApi, type ProjectSummary } from './lib/chatApi';
import { rustFileSaved, stopRustSupport, workspaceFoldersChanged } from './lib/lsp/rustAnalyzer';
import SearchPanel from './components/SearchPanel';
import { notifyFileWritten, onFileDeleted, onFileWritten } from './lib/approvals';
import { editorSettings } from './lib/editorSettings';
import { onAddToChat, requestEditorFocus, setProjectFileResolver } from './lib/editorBridge';
import {
  dropReveal,
  openInEditor,
  placeOf,
  requestReveal,
  setEditorOpener,
  type Compare,
  type Marks,
  type OpenInEditor,
} from './lib/openInEditor';
import { isPlanFile, requestImplementPlan } from './lib/plans';
import {
  isBrowserTab,
  isFileTab,
  navigated,
  unsavedIn,
  withFiles,
  withPage,
  type EditorTab,
} from './lib/editorTabs';
import { closeTabsOfClosedPorts, frameUrl, onPageWanted, setPageOpener } from './lib/browserTabs';
import { isMarkdownLanguage } from './lib/markdownView';
import {
  clearGitBase,
  createWorkdirReads,
  markedFiles,
  refreshableFiles,
  repoFileOf,
  setGitBase,
} from './lib/gitMarks';
import { diffLines, splitLines, type DiffHunk } from './lib/lineDiff';
import {
  isProjectSource,
  modelUriFor,
  notifyProjectFileWritten,
  updateProjectPath,
  updateProjectSource,
  updateWholeProject,
} from './lib/intellisense';
import { followDisk, scheduleModelPrune, syncTabModels, trackModels } from './lib/models';
import { agentChangesFor, clearAgentChanges, onAgentEdit } from './lib/agentChanges';
import { movedPath, isAtOrUnder, parentOf } from './lib/fileTools';
import {
  activeFolder,
  chatOwners,
  chatPathOf,
  chatPathToAbsolute,
  chatRootOf,
  folderOf,
  openFolders,
  pathsByFolder,
  sameOwners,
  sessionKey,
  type ChatRoot,
  type OpenFolder,
  type WindowTarget,
} from './lib/openFolders';
import { contextRealPath, isContextPath, remoteContextText } from './lib/contextMode';
import { isLiveContextPath } from './lib/contextDoc';
import { chatStore } from './lib/chatStore';
import { overlayFile } from './lib/overlay';
import { notify } from './lib/notifications';
import {
  startVirtualFiles,
  vfsSettingsRequested,
  clearVfsSettingsRequest,
} from './lib/virtualFilesStartup';
import { startBrowser } from './lib/browserStartup';
import { startSandbox } from './lib/sandboxStartup';
import { startWsl } from './lib/wslStartup';
import { startUpdates } from './lib/updates';
import { startHostUpdates } from './lib/hostUpdates';
import { initAgentRuns, onShowChat } from './lib/agentRuns';
import { cloud, hostName, initCloud } from './lib/cloud';
import SettingsDialog from './components/SettingsDialog';
import {
  appSettingsPage,
  closeAppSettings,
  openAppSettings,
  rereadAiSettingsOnFocus,
} from './lib/appSettings';
import {
  aiProviders,
  aiSettingsLoadFailed,
  followAiSettingsRevision,
  reloadAiSettings,
} from './lib/aiSettingsStore';
import { checkConnectorsOnStart } from './lib/connectors';
import { offerConnectorUpdates } from './lib/connectorUpdates';
import { dismissBootSplash } from './lib/bootSplash';

let unlistenFSChanges: (() => void) | undefined;

export default function App() {
  /** What the window has open: a folder (a project), or a workspace (D1). */
  const [target, setTarget] = createSignal<WindowTarget | null>(null);
  /** The folders that aren't there (deleted, or a drive that isn't mounted):
   *  shown, not resolved, and tried again as the window comes to the front
   *  or by their row's Try again (D20). */
  const [missingFolders, setMissingFolders] = createSignal<ReadonlySet<string>>(new Set());
  /** The folders it has open, each marked when it isn't there; a project is
   *  the list of one. */
  const folders = createMemo(() =>
    openFolders(target()).map((folder) => ({
      ...folder,
      missing: missingFolders().has(folder.path),
    }))
  );
  /**
   * The window's first folder, for the parts that still take one folder and
   * not a list. With one folder it is that folder.
   */
  const rootPath = () => {
    const open = folders();
    return open.length > 0 ? open[0].path : null;
  };
  /** The open folder `path` is in, or null when it's in none of them. */
  const folderAt = (path: string) => folderOf(folders(), path);
  /** The folders the Git panel shows: those that are there (D20). */
  const gitFolders = createMemo(() => resolvable(folders()));
  /**
   * The window's folders with the project each is, once resolved: a
   * workspace's folders are each a project of their own, with chats of their
   * own (docs/design/workspaces-plan.md, 3.8). Apart from `folders()`, which
   * the tree, search, the terminals and the editor take: a folder resolving
   * changes nothing there.
   */
  const resolvedFolders = createMemo(() => {
    const known = folderProjects();
    return folders().map((folder) => {
      const project = known.get(folder.path);
      return project ? { ...folder, project } : folder;
    });
  });
  /**
   * Whose chats the chat panel shows (lib/openFolders.ts): each folder's
   * project and the workspace's own; undefined while a folder resolves.
   */
  const owners = createMemo(
    () => chatOwners(target(), resolvedFolders(), currentProject() === null),
    undefined,
    { equals: sameOwners }
  );
  /**
   * The projects this window has open: its workspace, and each folder's
   * project once resolved. The project list hides Delete for every one of
   * them (B06); the backend refuses a project this window has open anyway
   * (`chat_delete_project` with `Instances::owners()`).
   */
  const openProjectIds = createMemo(() => {
    const ids = new Set<string>();
    const at = target();
    if (at?.kind === 'workspace') ids.add(at.id);
    for (const folder of resolvedFolders()) if (folder.project) ids.add(folder.project.id);
    return ids;
  });

  /**
   * The working folders of the window's repositories: git takes a file's
   * path relative to its repository, not to the project (D10). Read per set
   * of folders, and again when git's state changes (`forget`), and waited on
   * by a file's marks — a tab opened as the window opens gets them too.
   */
  const repoWorkdirs = createWorkdirReads(async () => {
    const open = folders();
    if (open.length === 0) return [];
    try {
      const found = await invoke<{ repos: { workdir: string }[] }>('git_repos', {
        folders: open.map((folder) => ({ path: folder.path, name: folder.name })),
      });
      return found.repos.map((repo) => repo.workdir);
    } catch (err) {
      console.error('Failed to read the window’s repositories:', err);
      return [];
    }
  });
  /** The folders the repositories are read for, as a key. */
  const foldersKey = () =>
    folders()
      .map((folder) => folder.path)
      .join('\n');

  /** The window's repositories, for the folders it shows. */
  async function workdirsNow(): Promise<string[]> {
    const key = foldersKey();
    const workdirs = await repoWorkdirs.of(key);
    // The window switched folders while the read was out: that answer is for
    // the folders it left.
    return key === foldersKey() ? workdirs : repoWorkdirs.of(foldersKey());
  }

  // A folder became a repository of its own, or its repository went (a `git
  // init`, a `.git` taken away): the repositories are read again when next
  // asked. A commit, a checkout or a save leaves every folder where it was.
  onMount(() => {
    let stop: (() => void) | undefined;
    void listen<{ folder: string; repositories?: boolean }>('git-changed', ({ payload }) => {
      if (payload.repositories) repoWorkdirs.forget();
    }).then((unlisten) => (stop = unlisten));
    onCleanup(() => stop?.());
  });

  /** Until the last project is restored, the Files tab shows no empty state. */
  const [restoringProject, setRestoringProject] = createSignal(true);
  /** The project whose tabs are being opened again: until they are, the
   *  tabs shown aren't kept as its (showWithTabs). */
  const [tabsComing, setTabsComing] = createSignal<string | null>(null);

  // Virtual files: mount, or offer to install the driver (in the background).
  onMount(() => void startVirtualFiles());
  // The command sandbox: offer to install bubblewrap where it's missing.
  onMount(() => void startSandbox());
  // WSL on Windows: Rovik in it updated with the app, or its setup offered.
  onMount(() => void startWsl());
  // Agent runs live in the backend: follow them from the start, whichever
  // sidebar tab is open (approvals show in the editor too).
  onMount(() => void initAgentRuns());
  // The cloud hosts' connections and the chats' moves, likewise.
  onMount(() => void initCloud());
  // The web tools' browser: downloaded if it's missing (in the background).
  onMount(() => void startBrowser());

  // Load last opened project on startup: the workspace or folder the command
  // line asked for (`--workspace <id>`, `rovik-ide <folder>`, claimed for
  // this window by the backend), else the last project, unless another window
  // has it open (lib/instances.ts). A window another one started ("New
  // window") starts without one. A workspace's folders come from the
  // database (D17, 3.11). A window that opens with nothing still has its
  // chats — the ones without a project — which `openAtStart` has resolved
  // too.
  onMount(async () => {
    try {
      const startup = await invoke<{
        folder: string | null;
        workspace: string | null;
        newWindow: boolean;
      }>('startup_info').catch(() => ({ folder: null, workspace: null, newWindow: false }));
      // Another window's "New window", with nothing asked for: nothing opens.
      const asked = startup.newWindow && !startup.folder && !startup.workspace ? null : startup;
      const opening = asked ? await targetToOpen(asked) : null;
      await openAtStart(opening?.target ?? null, {
        show: async (target) => {
          if (!opening) return;
          for (const m of monaco.editor.getModels()) m.dispose();
          presetTreeExpansion(target, opening.expanded);
          setTarget(target);
          setTerminalCollapsed(!opening.terminalOpen);
          const files = await reopenFiles(opening.files);
          setOpenFiles(files);
          if (files.length > 0) setActivePath(files[files.length - 1].path);
        },
        // Its folders as they are now: each that's there is its own project,
        // and a missing one is shown as missing and asks nothing (D20). None
        // open: the chats without a project are this window's.
        resolve: resolveFolders,
        exists: (path) => pathExists(path),
      });
    } finally {
      setRestoringProject(false);
      dismissBootSplash();
    }
  });
  /** Each open folder's backend id, once resolved: what a file made or dropped at its top goes in. */
  const [rootIds, setRootIds] = createSignal<Record<string, string>>({});
  /** The editor's tabs, files and browser tabs, in order (lib/editorTabs.ts). */
  const [tabs, setTabs] = createSignal<EditorTab[]>([]);
  /** The file tabs among them: a new list only when one of them changed. */
  const openFiles = createMemo(() => tabs().filter(isFileTab), [], {
    equals: (a, b) => a.length === b.length && a.every((f, i) => f === b[i]),
  });
  /** Changes the file tabs; the browser tabs stay where they are. */
  function setOpenFiles(next: OpenFile[] | ((files: OpenFile[]) => OpenFile[])) {
    setTabs((all) =>
      withFiles(all, typeof next === 'function' ? next(all.filter(isFileTab)) : next)
    );
  }
  /** The browser tabs among them. */
  const browserTabs = createMemo(() => tabs().filter(isBrowserTab));
  /** The tab to show once the one shown closed: the last one left. */
  function lastTab(): string | null {
    const all = tabs();
    return all.length > 0 ? all[all.length - 1].path : null;
  }
  // Idle editor models (closed tabs, go-to-definition targets) are kept
  // up to a limit, then the least recently used are disposed (lib/models.ts).
  trackModels(() => new Set(openFiles().map((f) => modelUriFor(f.path).toString())));
  createEffect(() => {
    // Tabs in the background too: their models are what the TypeScript
    // worker and the language servers read for their files.
    syncTabModels(openFiles());
    scheduleModelPrune();
  });
  const [activePath, setActivePath] = createSignal<string | null>(null);
  /** The Files tab shows search results instead of the tree while searching. */
  const [searchActive, setSearchActive] = createSignal(false);
  const [sidebarTab, setSidebarTab] = createSignal<'files' | 'chat' | 'git'>('files');
  // The panel's full-window mode: the floating toggle in Resizable sets it,
  // and the `sidebar-expanded` class on `.app` hides the right pane.
  const [sidebarExpanded, setSidebarExpanded] = createSignal(false);
  /**
   * What the chat was asked to open, a file or a comparison tab (an
   * isolated chat's change that hasn't landed, say), is seen: the panel
   * gives the window back to the editor. Not for what opens by itself (the
   * agent's edits), which leaves the panel as it is.
   */
  const showEditor = () => setSidebarExpanded(false);

  // App-wide Settings (the gear next to the project selector): a virtual
  // files notification's "Show steps" opens its page directly.
  createEffect(() => {
    if (!vfsSettingsRequested()) return;
    clearVfsSettingsRequest();
    openAppSettings('vfs');
  });
  // The Settings dialog's AI settings (lib/aiSettingsStore.ts): read once
  // now, so the dialog opens on them, again each time it opens, and again
  // when another window may have saved them (this one came to the front:
  // rereadAiSettingsOnFocus, below).
  // Then, once: each connector's status (lib/connectors.ts), with a
  // warning for any a chat can pick that can't run, and an offer to update
  // any a newer version of is out (lib/connectorUpdates.ts).
  onMount(
    () =>
      void reloadAiSettings()
        .then(() => {
          if (aiSettingsLoadFailed()) return;
          return checkConnectorsOnStart(aiProviders(), () => openAppSettings('connectors')).then(
            (found) => offerConnectorUpdates(aiProviders(), found)
          );
        })
        .catch((err) => console.error(err))
  );
  followAiSettingsRevision();
  const [cursorInfo, setCursorInfo] = createSignal<CursorInfo | null>(null);

  const [contextMenuVisible, setContextMenuVisible] = createSignal(false);
  const [activePanel, setActivePanel] = createSignal<'files' | 'something' | null>(null);
  const [rawCursor, setRawCursor] = createSignal({ x: 0, y: 0 });
  const [cursorPosition, setCursorPosition] = createSignal({ x: 0, y: 0 });

  const [fileTreeEl, setFileTreeEl] = createSignal<HTMLDivElement | null>(null);
  const [menuEl, setMenuEl] = createSignal<HTMLDivElement | null>(null);
  const [clipboard, setClipboard] = createSignal<{ ids: string[]; mode: 'copy' | 'cut' } | null>(
    null
  );
  const [selectedFileIds, setSelectedFileIds] = createSignal<Set<string>>(new Set());
  const [lastSelectedId, setLastSelectedId] = createSignal<string | null>(null);
  const [editingId, setEditingId] = createSignal<string | null>(null);
  const [draftEntry, setDraftEntry] = createSignal<{
    parentId: string;
    parentPath: string | null;
    depth: number;
    isDir: boolean;
  } | null>(null);
  const [terminalCollapsed, setTerminalCollapsed] = createSignal(true);
  const [pendingOpenPath, setPendingOpenPath] = createSignal<string | null>(null);

  const tree = createFileTreeController(() =>
    folders().map((folder) => ({
      path: folder.path,
      name: folder.name,
      // A workspace shows each folder as a row; a project's one root is the
      // tree itself (D9).
      row: target()?.kind === 'workspace',
      id: rootIds()[folder.path],
      missing: folder.missing,
    }))
  );

  const displayRows = createMemo<TreeRow[]>(() => {
    const rows = tree.rows();
    const draft = draftEntry();
    if (!draft) return rows;

    const draftRow = {
      id: '__draft__',
      path: '__draft__',
      name: '',
      isDir: draft.isDir,
      depth: draft.depth,
      loading: false,
      ignored: false,
    } as TreeRow;

    // The draft goes last in its folder, after the folder's whole subtree.
    if (draft.depth === 0) {
      return [...rows, draftRow];
    }

    const idx = rows.findIndex((r) => r.depth === draft.depth - 1 && draft.parentPath === r.path);
    if (idx === -1) return rows;

    let insertAt = idx + 1;
    while (insertAt < rows.length && rows[insertAt].depth > rows[idx].depth) {
      insertAt++;
    }
    return [...rows.slice(0, insertAt), draftRow, ...rows.slice(insertAt)];
  });

  const activeFile = createMemo(() => openFiles().find((f) => f.path === activePath()) ?? null);
  /** The active tab's path when it's a Markdown file (it has the preview's views). */
  const activeMarkdownPath = createMemo(() => {
    const file = activeFile();
    return file && isMarkdownLanguage(file.language) ? file.path : null;
  });
  const activeFileContext = createMemo<ActiveFileContext | null>(() => {
    const file = activeFile();
    const info = cursorInfo();
    // A read-only tab (a commit's or a stash's version, a comparison) isn't
    // a file: its path ("commit@…:…") is nothing the agent can read.
    if (!file || file.readOnly || !info || info.path !== file.path) return null;
    return {
      path: file.path,
      language: file.language,
      line: info.line,
      column: info.column,
      enclosingSymbol: findEnclosingSymbol(file.content, info.line, file.language),
      selection: info.selection,
    };
  });

  const effectiveSelectedRows = createMemo<TreeRow[]>(() => {
    const selectedIds = selectedFileIds();
    const rows = tree.rows();
    return rows.filter((r) => selectedIds.has(r.id));
  });

  const firstSelectedRow = createMemo<TreeRow | null>(() => {
    const rows = effectiveSelectedRows();
    return rows.length > 0 ? rows[0] : null;
  });

  /** The selected rows the actions act on: a folder's own row never is (D9). */
  const actionable = createMemo<TreeRow[]>(() => actionableRows(effectiveSelectedRows()));

  /**
   * The folder the user is working in (D15): the active tab's, else the
   * selected row's, else the first that's there. A new entry, a terminal or
   * a settings page goes to it.
   */
  const activeFolderNow = createMemo(() =>
    activeFolder(resolvable(folders()), activePath(), firstSelectedRow()?.path ?? null)
  );

  /**
   * The folder a drop on the tree's empty space, or a new file at the top,
   * lands in: the active folder's (D15's rule, as a terminal starts in it).
   */
  const rootId = () => {
    const at = activeFolderNow();
    return at ? (rootIds()[at.path] ?? null) : null;
  };

  /** What the window opens with at its start, and the tabs it comes back with. */
  interface StartTarget {
    target: WindowTarget;
    files: SessionFile[];
    expanded: string[];
    /** Its terminal panel was open; false when none was kept. */
    terminalOpen: boolean;
  }

  /**
   * What this window starts with: the workspace or folder the command line
   * asked for, else the last project, as it was left (last-project.json,
   * 3.11). Null when another window has it: this one starts without a
   * project.
   */
  async function targetToOpen(startup: {
    folder: string | null;
    workspace: string | null;
  }): Promise<StartTarget | null> {
    const last = await loadLastProject();
    const asked: WindowTarget | null = startup.workspace
      ? await workspaceTarget(startup.workspace)
      : startup.folder
        ? { kind: 'folder', path: startup.folder }
        : null;
    const lastTarget: WindowTarget | null = last?.workspace
      ? await workspaceTarget(last.workspace)
      : last?.path
        ? { kind: 'folder', path: last.path }
        : null;
    // The last project is the one asked for (its tabs come back from
    // last-project.json), or the one opened with none asked for at all.
    const isLast = lastTarget !== null && (!asked || sameWindowTarget(asked, lastTarget));
    const target = asked ?? lastTarget;
    if (!target) return null;
    // Open in another window: this one starts without a project.
    if (!(await claimProject(claimTargetOf(target), { focusOther: false, retries: 2 }))) {
      return null;
    }
    if (isLast && last) {
      return {
        target,
        files: last.openedFiles ?? [],
        expanded: last.expandedPaths ?? [],
        terminalOpen: !last.terminalCollapsed,
      };
    }
    // Not the last project: its tabs are kept with it (project-sessions),
    // but for those of a folder that has left the workspace (3.11).
    const session = await savedProjectSession(sessionKey(target) ?? '').catch(() => null);
    const kept = session
      ? sessionInFolders(
          session,
          openFolders(target).map((folder) => folder.path)
        )
      : null;
    return {
      target,
      files: kept?.files ?? [],
      expanded: kept?.expanded ?? [],
      terminalOpen: kept?.terminalOpen ?? false,
    };
  }

  /** Whether `a` is the target `b` is: the same workspace, or the same folder. */
  function sameWindowTarget(a: WindowTarget, b: WindowTarget): boolean {
    if (a.kind === 'workspace' && b.kind === 'workspace') return a.id === b.id;
    if (a.kind === 'folder' && b.kind === 'folder') return a.path === b.path;
    return false;
  }

  /** Tabs saved for later, read again from disk. */
  async function reopenFiles(saved: SessionFile[]): Promise<OpenFile[]> {
    // A file deleted since is skipped: one missing file used to fail the
    // whole restore.
    const restored = await Promise.all(
      saved.map(async (f): Promise<OpenFile | null> => {
        try {
          const content = await readFileContent(f.path);
          return { ...f, content, originalContent: content, dirty: false };
        } catch (err) {
          console.warn(`Not restoring ${f.path}:`, err);
          return null;
        }
      })
    );
    return restored.filter((f): f is OpenFile => f !== null);
  }

  /**
   * Tabs with unsaved text, before they close (another project opens, a
   * folder leaves the workspace, or the window closes): saved, or let go, as
   * the user says. False: stay (cancelled, or a save didn't go through).
   * Switching projects used to drop the text without a word, and so did
   * closing the window. `inFolder`: only that folder's tabs, which close as
   * it leaves the workspace (D9).
   */
  async function settleUnsavedTabs(
    leaving: 'project' | 'app',
    inFolder?: { folder: OpenFolder }
  ): Promise<boolean> {
    const unsaved = unsavedIn(openFiles(), inFolder?.folder.path);
    if (unsaved.length === 0) return true;
    const choice = await chooseDialog({
      title:
        unsaved.length === 1
          ? `Save changes to ${unsaved[0].name}?`
          : `Save changes to ${unsaved.length} files?`,
      message:
        `${unsaved.map((f) => f.name).join(', ')} ${unsaved.length === 1 ? 'has' : 'have'} ` +
        `changes that aren’t saved. ` +
        (leaving === 'app'
          ? 'They’re lost when the window closes, unless they’re saved.'
          : inFolder
            ? 'They close when the folder leaves the workspace, so they’re lost unless they’re saved.'
            : 'The tabs close when another project opens, so they’re lost unless they’re saved.'),
      confirmLabel: 'Save',
      alternateLabel: 'Don’t save',
    });
    if (choice === 'cancel') return false;
    if (choice === 'alternate') return true;
    for (const f of unsaved) {
      if (!openFiles().some((o) => o.path === f.path && o.dirty)) continue;
      setActivePath(f.path);
      await saveActiveFile();
    }
    // A save refused (changed on disk and kept to compare, or an error).
    return unsavedIn(openFiles(), inFolder?.folder.path).length === 0;
  }

  // Rovik's own updates (lib/updates.ts): a look every 30 minutes, and an
  // update's restart, which reopens this window's project, its unsaved
  // tabs saved or let go first, as closing the window does. Then, once a
  // new version runs, the cloud hosts are offered it (lib/hostUpdates.ts).
  onMount(() => {
    void startUpdates({
      folder: () => rootPath(),
      workspace: () => {
        const at = target();
        return at?.kind === 'workspace' ? at.id : null;
      },
      settle: async () => {
        if (!(await settleUnsavedTabs('app'))) return false;
        await keepTabsNow();
        return true;
      },
      confirm: confirmDialog,
    });
    void startHostUpdates();
  });

  // Closing the window with unsaved tabs asks first.
  onMount(() => {
    let stop: (() => void) | undefined;
    let gone = false;
    void getCurrentWindow()
      .onCloseRequested(async (event) => {
        if (!(await settleUnsavedTabs('app'))) event.preventDefault();
        // Its tabs, for opening it again here or in another window.
        else await keepTabsNow();
      })
      .then((unlisten) => (gone ? unlisten() : (stop = unlisten)))
      .catch((err) => console.error('No close guard:', err));
    onCleanup(() => {
      gone = true;
      stop?.();
    });
  });

  const [newProjectOpen, setNewProjectOpen] = createSignal(false);

  async function handlePickFolder() {
    const folder = await pickProjectFolder();
    if (folder) openProjectAt(folder);
  }

  /** Opens a project folder: Files, Chat and Git switch to it. */
  async function openProjectAt(folder: string) {
    if (folder === rootPath()) return;
    await switchTo({ kind: 'folder', path: folder });
  }

  /**
   * Opens a target in this window — a folder, or a workspace's folders — and
   * says what happened (lib/switchTarget.ts, 3.3): another window with it
   * keeps it and comes to the front, and the tabs with unsaved text are
   * settled before anything changes.
   */
  async function switchTo(next: WindowTarget | null) {
    await switchTarget(next, {
      check: (target) => checkProject(claimTargetOf(target)),
      settle: () => settleUnsavedTabs('project'),
      keepTabs: () => void keepTabsNow(),
      claim: (target) => claimProject(claimTargetOf(target)),
      show: (target) => showTarget(target),
      // Each folder that's there is its own project, with its own chats; a
      // missing one isn't resolved, and asks nothing (D20).
      resolve: (open) => resolveFolders(open),
      exists: (path) => pathExists(path),
    });
  }

  /**
   * The window's folders as they are now, resolved in the one place that
   * does it: each that's there is its own project, with its own chats, and a
   * missing one is shown as missing and asks nothing (D20). A new generation
   * first, so an answer for a folder the window has left since is dropped
   * (lib/projects.ts).
   */
  function resolveFolders(open: readonly OpenFolder[]) {
    newFoldersGeneration();
    setMissingFolders(new Set(open.filter((folder) => folder.missing).map((f) => f.path)));
    if (open.length === 0) {
      void resolveProject(null);
      return;
    }
    for (const folder of resolvable(open)) void resolveProject(folder.path, [...open]);
  }

  /**
   * The tree's saved open folders, registered on the folder each is in: the
   * tree's roots are the window's folders, never the key its session is kept
   * under (`workspace:<id>` is no folder's path), so a workspace's saved
   * expansion would otherwise be dropped (B04).
   */
  function presetTreeExpansion(target: WindowTarget | null, expanded: readonly string[]) {
    const roots = openFolders(target).map((folder) => folder.path);
    for (const { folder, paths } of pathsByFolder(expanded, roots)) {
      tree.presetExpanded(folder, paths);
    }
  }

  /** `next` becomes what the window shows, its tabs as they were left here
   *  or in the window that had it last (lib/projectSessions.ts). */
  async function showTarget(next: WindowTarget | null) {
    const key = sessionKey(next);
    // Not kept until they're back: the tabs shown meanwhile aren't its.
    setTabsComing(key);
    try {
      const session = key ? await savedProjectSession(key) : null;
      presetTreeExpansion(next, session?.expanded ?? []);
      // Its terminal panel as it was left, with its own terminals
      // (TerminalPanel), or as it is now for a target that has none kept:
      // set together, or the panel open on the target left would start a
      // terminal in this one.
      batch(() => {
        if (session?.terminalOpen !== undefined) setTerminalCollapsed(!session.terminalOpen);
        setTarget(next);
      });
      // The target left's tabs go, its browser tabs too; so do its models
      // and the old folder's change marks.
      setTabs([]);
      setActivePath(null);
      for (const m of monaco.editor.getModels()) m.dispose();
      for (const { absPath } of markedFiles()) clearGitBase(absPath);
      setSelectedFileIds(new Set<string>());
      setLastSelectedId(null);
      // A tab of a folder that has left the workspace isn't reopened (3.11).
      const kept =
        session &&
        sessionInFolders(
          session,
          openFolders(next).map((f) => f.path)
        );
      if (!kept?.files.length) return;
      const files = await reopenFiles(kept.files);
      // Another target chosen since, or a file opened meanwhile: leave it.
      if (sessionKey(target()) !== key || openFiles().length > 0 || files.length === 0) return;
      setOpenFiles(files);
      setActivePath(
        files.some((f) => f.path === kept.active) ? kept.active : files[files.length - 1].path
      );
    } finally {
      if (tabsComing() === key) setTabsComing(null);
    }
  }

  // Which project each open folder is, and so whose chats are shown, is
  // `resolveFolders`'s: each switch, and the start, resolves the folders the
  // target has (3.3). Nothing else resolves them, so nothing resolves a
  // folder that isn't there.

  // The chats without a project were another window's: maybe they're free
  // now, when this one comes to the front. A folder that was missing is
  // tried again too: its drive may be mounted (D20).
  onMount(() => {
    const again = () => {
      if (!rootPath() && chatsBlocked()?.kind === 'no-project-elsewhere') {
        void resolveProject(null);
      }
      for (const path of missingFolders()) void tryFolderAgain(path);
    };
    window.addEventListener('focus', again);
    onCleanup(() => window.removeEventListener('focus', again));
  });

  /**
   * A folder that was missing is tried again (its row's **Try again**, or
   * the window coming to the front): resolved once it's there, and still
   * shown as missing until then. Whether it was found.
   */
  async function tryFolderAgain(path: string): Promise<boolean> {
    const there = await pathExists(path).catch(() => false);
    if (!there) return false;
    setMissingFolders((was) => new Set([...was].filter((missing) => missing !== path)));
    void resolveProject(path, resolvedFolders());
    return true;
  }
  onMount(() => onCleanup(rereadAiSettingsOnFocus()));

  /** The window folder a tree row is, when the row is a folder row (D9). */
  const folderOfRow = (row: TreeRow): OpenFolder | null =>
    folders().find((folder) => folder.path === row.path) ?? null;

  /** The menu is for one of the window's folders: exactly one folder row is selected. */
  const menuFolderRow = createMemo<TreeRow | null>(() => {
    const rows = effectiveSelectedRows();
    return rows.length === 1 && rows[0].folderRow ? rows[0] : null;
  });
  const menuFolder = createMemo(() => {
    const row = menuFolderRow();
    return row ? { missing: !!row.missing } : undefined;
  });

  /**
   * The window shows `next` after one of its folders left it or moved, in
   * place: what is open in the folders that stay stays open (the tree's reset
   * effect would close every folder otherwise), `gone`'s tabs close — a tab
   * that followed its folder to a new place is already at its new path — and
   * `gone` is no longer tried as missing, and the selection clears.
   */
  function showFolderChange(next: WindowTarget, gone: string) {
    const roots = openFolders(next).map((folder) => folder.path);
    for (const { folder, paths } of pathsByFolder([...tree.expanded()], roots)) {
      tree.presetExpanded(folder, paths);
    }
    const closed = new Set(
      openFiles()
        .filter((f) => isAtOrUnder(f.path, gone))
        .map((f) => f.path)
    );
    if (closed.size > 0) {
      setOpenFiles((prev) => prev.filter((f) => !closed.has(f.path)));
      const active = activePath();
      if (active && closed.has(active)) setActivePath(null);
    }
    setMissingFolders((was) => new Set([...was].filter((path) => path !== gone)));
    setSelectedFileIds(new Set<string>());
    setLastSelectedId(null);
    setTarget(next);
  }

  /** What Remove and Locate… need of the window, for the folder they act on. */
  function folderChangeCalls(folder: OpenFolder): FolderChangeCalls {
    return {
      settle: (leaving) => settleUnsavedTabs('project', { folder: leaving }),
      remove: (id, name) => chatApi.removeWorkspaceFolder(id, name),
      check: (path) => checkProject({ kind: 'folder', path }),
      relocate: (id, name, path) => chatApi.relocateWorkspaceFolder(id, name, path),
      follow: (from, to) => followMoves([{ from, to }]),
      claim: (next) => claimProject(claimTargetOf(next)),
      show: (next) => showFolderChange(next, folder.path),
      resolve: (path) => void resolveProject(path, resolvedFolders()),
    };
  }

  /** Remove from workspace: the folder leaves it, and nothing on disk changes. */
  async function handleRemoveFolder() {
    setContextMenuVisible(false);
    const row = menuFolderRow();
    const at = target();
    const folder = row ? folderOfRow(row) : null;
    if (!folder || !at) return;
    try {
      await removeFolder(at, folder, folderChangeCalls(folder));
    } catch (err) {
      notify({
        id: 'folder-remove',
        kind: 'error',
        title: `Couldn’t remove ${folder.name}`,
        message: String(err),
      });
    }
  }

  /** Locate…: the folder that isn't there is at another place from now on. */
  async function handleLocateFolder() {
    setContextMenuVisible(false);
    const row = menuFolderRow();
    const at = target();
    const folder = row ? folderOfRow(row) : null;
    if (!folder || !at) return;
    try {
      const to = await pickProjectFolder();
      if (!to) return;
      await locateFolder(at, folder, to, folderChangeCalls(folder));
    } catch (err) {
      notify({
        id: 'folder-locate',
        kind: 'error',
        title: `Couldn’t locate ${folder.name}`,
        message: String(err),
      });
    }
  }

  /** Try again: a missing folder's row asks whether it's back. */
  async function handleTryFolderAgain() {
    setContextMenuVisible(false);
    const row = menuFolderRow();
    if (!row) return;
    if (await tryFolderAgain(row.path)) return;
    notify({
      id: `folder-missing:${row.path}`,
      kind: 'info',
      title: `${row.name} still isn’t there`,
      message: `${row.path} can’t be read. Locate it, or remove it from the workspace.`,
    });
  }

  /** A project whose folder is gone: point it at its folder (lib/projects.ts). */
  async function relinkProject(p: ProjectSummary) {
    const folder = await pickProjectFolder();
    if (!folder) return;
    try {
      let result = await chatApi.relinkProject(p.id, folder, false);
      if (result.kind === 'isOtherProject') {
        const merge = await confirmDialog({
          title: `${result.other.name} is already a project`,
          message:
            `That folder has its own chat history (${result.other.chatCount}). ` +
            `Move ${p.name}'s chats (${p.chatCount}) into it?`,
          confirmLabel: 'Move chats',
        });
        if (!merge) return;
        result = await chatApi.relinkProject(p.id, folder, true);
      }
      await refreshProjects();
      if (folder === rootPath()) void resolveProject(folder);
      else openProjectAt(folder);
    } catch (err) {
      window.alert(`Relinking failed: ${String(err)}`);
    }
  }

  async function deleteProject(p: ProjectSummary) {
    const ok = await confirmDialog({
      title: `Delete ${p.name}'s chat history?`,
      message: `Its ${p.chatCount} chat${p.chatCount === 1 ? '' : 's'} are deleted for good. The project folder itself isn't touched.`,
      confirmLabel: 'Delete chats',
      danger: true,
    });
    if (!ok) return;
    try {
      await chatApi.deleteProject(p.id);
      await refreshProjects();
    } catch (err) {
      window.alert(`Deleting failed: ${String(err)}`);
    }
  }

  // Ctrl/Cmd+O: open a project, also while typing (nothing else uses it).
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      const isMac = navigator.platform.toUpperCase().includes('MAC');
      if (
        (isMac ? e.metaKey : e.ctrlKey) &&
        !e.shiftKey &&
        !e.altKey &&
        e.key.toLowerCase() === 'o'
      ) {
        e.preventDefault();
        e.stopPropagation();
        void handlePickFolder();
      }
    };
    window.addEventListener('keydown', onKey, true);
    onCleanup(() => window.removeEventListener('keydown', onKey, true));
  });

  /** `activate: false` opens it in a tab without switching to it. */
  /** Files being read for a new tab: a second open waits for the first. */
  const opening = new Set<string>();

  /**
   * The tab of `row`'s file, read from disk when it isn't open yet:
   * whether it was there already, came now, or couldn't (binary, unreadable).
   */
  async function openTab(row: TreeRow, activate: boolean): Promise<'created' | 'existing' | null> {
    // By path too: a file opened from search results may carry a different id.
    const existing = openFiles().find((f) => f.id === row.id || f.path === row.path);
    if (existing) {
      if (activate) setActivePath(existing.path);
      return 'existing';
    }
    // Two opens at once (two quick writes by the agent): one tab.
    if (opening.has(row.path)) return null;
    if (isLikelyBinary(row.name)) {
      window.alert(`"${row.name}" looks like a binary file and can't be opened in the editor.`);
      return null;
    }
    opening.add(row.path);
    try {
      const content = await readFileContent(row.path);
      const file: OpenFile = {
        id: row.id,
        path: row.path,
        name: row.name,
        content,
        originalContent: content,
        dirty: false,
        language: languageForFile(row.name),
      };
      setOpenFiles((prev) => [...prev, file]);
      if (activate) setActivePath(row.path);
      return 'created';
    } catch (err) {
      console.error(`Failed to open ${row.path}:`, err);
      if (activate) window.alert(`Couldn't open ${row.name}: ${String(err)}`);
      return null;
    } finally {
      opening.delete(row.path);
    }
  }

  /** The tree's row for an absolute path, or one made for a file it doesn't list. */
  function rowFor(path: string): TreeRow {
    const sep = path.includes('\\') && !path.includes('/') ? '\\' : '/';
    const name = path.split(sep).pop() ?? path;
    return (
      tree.files().find((r) => r.path === path) ?? { id: path, path, name, isDir: false, depth: 0 }
    );
  }

  const hunksBetween = (before: string, after: string) =>
    diffLines(splitLines(before).lines, splitLines(after).lines);

  /**
   * Opens a file in a tab: the one way (lib/openInEditor.ts), from the
   * tree, the chat, search, the Git tab and the agent's writes alike. The
   * marks come first, then the place to show: the one asked for, else the
   * first change when there is one, which a tab that's open already keeps
   * only when the user asked for the file (the agent's writes come up by
   * themselves).
   */
  async function openRequested(request: OpenInEditor): Promise<void> {
    const { path, activate = true, focus = false, highlight = 'flash', marks = 'auto' } = request;
    if (typeof marks === 'object') return openComparison(path, marks.compare, request);
    const folder = folderForChat(request.dialogId);
    if (!folder && !isContextPath(path)) return;
    // A written file as its chat has it, when that isn't the project's
    // file: an isolated chat's change that hasn't landed, a remote chat's.
    // Before the project's file is looked for: an isolated chat's new file
    // isn't there at all.
    if (request.dialogId && folder && marks !== 'none' && !isContextPath(path)) {
      const rel = chatPathOf(folder, chatPathToAbsolute(folder, path)!);
      if (rel !== null && (await openChatVersion(request.dialogId, rel, request))) return;
    }
    const abs = await resolveOpenPath(path, folder);
    if (!abs && (await openRemoteContext(path, request))) return;
    if (!abs) {
      notify({
        kind: 'warning',
        title: `Can't open ${path}`,
        message: path.includes('revisions')
          ? 'Context revisions are not files; open them from the context panel.'
          : "The agent's context folder is on the virtual volume, which isn't mounted.",
        timeout: 8_000,
      });
      return;
    }
    const opened = await openTab(rowFor(abs), activate);
    if (!opened) return;
    if (focus) showEditor();
    const hunks = await marksFor(abs, marks);
    const at = request.at ?? (hunks?.length ? 'first-change' : undefined);
    if (!at || (at === 'first-change' && opened === 'existing' && !focus)) return;
    const text = openFiles().find((f) => f.path === abs)?.content ?? '';
    const place = placeOf(at, text, hunks);
    if (place) requestReveal(abs, place, { highlight, focus });
  }
  setEditorOpener(openRequested);
  onCleanup(() => setEditorOpener(null));

  /**
   * The marks a tab shows, loaded: the agent's unreviewed changes when it
   * has them, else git's (`marks` may ask for one of them only). Their
   * hunks against the tab's text; none when there's nothing to mark.
   */
  async function marksFor(
    abs: string,
    marks: Exclude<Marks, { compare: Compare }>
  ): Promise<DiffHunk[] | undefined> {
    if (marks === 'none') return undefined;
    const text = () => openFiles().find((f) => f.path === abs)?.content;
    if (marks !== 'git') {
      const agent = agentChangesFor(abs);
      if (agent) return hunksBetween(agent.baseline, text() ?? agent.current);
      if (marks === 'agent') return undefined;
    }
    const base = await loadGitBase(abs);
    const current = text();
    if (base === undefined || current === undefined) return undefined;
    return hunksBetween(base ?? '', current);
  }

  /**
   * A read-only tab comparing two versions of a file: `after` shown, what
   * differs from `before` marked (an isolated chat's change against the
   * project, a stash, a commit, a remote chat's edit or plan). Asked for
   * again, the same tab shows the versions as they are now.
   */
  function openComparison(id: string, compare: Compare, request: OpenInEditor): Promise<void> {
    const { activate = true, focus = false, highlight = 'flash' } = request;
    const existed = openFiles().some((f) => f.path === id);
    const tab: OpenFile = {
      id,
      path: id,
      name: compare.title,
      content: compare.after,
      originalContent: compare.after,
      dirty: false,
      language: languageForFile(compare.fileName),
      readOnly: true,
      ...(compare.stash ? { stash: compare.stash } : {}),
      ...(compare.commit ? { commit: compare.commit } : {}),
    };
    setGitBase(id, '', compare.fileName, compare.before);
    setOpenFiles((prev) => (existed ? prev.map((f) => (f.path === id ? tab : f)) : [...prev, tab]));
    if (activate) setActivePath(id);
    if (focus) showEditor();
    const hunks = hunksBetween(compare.before, compare.after);
    const at = request.at ?? (hunks.length ? 'first-change' : undefined);
    if (!at || (at === 'first-change' && existed && !focus)) return Promise.resolve();
    const place = placeOf(at, compare.after, hunks);
    if (place) requestReveal(id, place, { highlight, focus });
    return Promise.resolve();
  }

  // The agent's writes: open tabs refresh after one (an unsaved tab keeps
  // the user's text).
  const stopWritten = onFileWritten((path, content) => {
    setOpenFiles((prev) =>
      prev.map((f) =>
        f.path === path && !f.dirty && !f.readOnly ? { ...f, content, originalContent: content } : f
      )
    );
    // A write the user approved (a new file, say) isn't an unreviewed edit,
    // which opens its file itself (onAgentEdit): open it here, in the
    // background unless nothing else is open.
    if (
      !editorSettings().openFilesAfterAgentEdits ||
      openFiles().some((f) => f.path === path) ||
      agentChangesFor(path)
    ) {
      return;
    }
    void openInEditor({ path, activate: activePath() === null });
  });
  // A file the agent removed (a rolled-back creation, P3.4): its tab goes,
  // unless it holds the user's unsaved text.
  const stopDeleted = onFileDeleted((path) => {
    const file = openFiles().find((f) => f.path === path);
    if (!file || file.dirty) return;
    setOpenFiles((files) => files.filter((f) => f.path !== path));
    if (activePath() === path) setActivePath(lastTab());
  });
  // An unreviewed agent edit: when "Open files after agent edits" is on,
  // its file opens in a tab so the changes can be seen. In the background
  // while another file is being worked on, so a long run doesn't keep
  // switching tabs; focused when nothing else is open.
  // Language servers belong to the folders that are there: when one comes
  // or goes (removed, located, tried again), a running server restarts for
  // them (C07). Nothing starts one: a Rust file does that.
  const loadableKey = createMemo(() =>
    resolvable(folders())
      .map((folder) => folder.path)
      .join('\n')
  );
  createEffect(
    on(
      loadableKey,
      () => workspaceFoldersChanged(resolvable(folders()).map((folder) => folder.path)),
      { defer: true }
    )
  );
  onCleanup(() => void stopRustSupport());

  const stopAgentEdits = onAgentEdit((path) => {
    const open = openFiles().find((f) => f.path === path);
    // The tab has unsaved edits of the user's: it keeps them and doesn't
    // show the agent's version, so the highlights would point at the wrong
    // lines. Drop them rather than show them wrong.
    if (open?.dirty) {
      clearAgentChanges(path);
      return;
    }
    if (open) return;
    if (!editorSettings().openFilesAfterAgentEdits) return;
    // Newly opened by the agent's edit: its changes marked, centered on the
    // first one when the tab is shown.
    void openInEditor({ path, activate: activePath() === null, marks: 'agent' });
  });

  /**
   * A file as the chat that wrote it has it, when that isn't the project's
   * file: an isolated chat's change that hasn't landed, or a remote chat's,
   * on its host (lib/overlay.ts). A read-only comparison, like the
   * isolation bar's Compare, at the place the request asks for, else at
   * the first change. False when the chat's version is the project's (the
   * chat isn't isolated, or the change landed), so the file itself opens.
   */
  async function openChatVersion(
    dialogId: string,
    rel: string,
    request: OpenInEditor
  ): Promise<boolean> {
    const versions = await overlayFile(dialogId, rel).catch(() => null);
    if (!versions || versions.chat === null || versions.chat === versions.project) return false;
    const name = rel.split('/').pop() ?? rel;
    await openComparison(
      `chat:${dialogId}:${rel}`,
      {
        title: `${name} (this chat)`,
        fileName: name,
        before: versions.project ?? '',
        after: versions.chat,
      },
      request
    );
    return true;
  }
  /**
   * A remote chat's context file, read-only from its latest revision: the
   * folder it's in is on the server (U04). False when `path` isn't that
   * file, or the chat isn't remote.
   */
  async function openRemoteContext(path: string, request: OpenInEditor): Promise<boolean> {
    const chat = chatStore.state.active;
    if (!chat?.place || !isLiveContextPath(path)) return false;
    const text = await remoteContextText(chat.id).catch(() => null);
    if (text === null) return false;
    await openComparison(
      `context:${chat.id}`,
      {
        title: `LIVE_CONTEXT.md (on ${hostName(chat.place.host, cloud.hosts)})`,
        fileName: 'LIVE_CONTEXT.md',
        before: text,
        after: text,
      },
      request
    );
    return true;
  }
  /**
   * The folder a path from the chat `dialogId` (else the open chat) is in
   * (docs/design/workspaces-plan.md, 3.8): the chat names its files in its
   * own folder. The window's first folder when it has no chat, or the chat's
   * folder isn't open here; null with no folder open.
   */
  function folderForChat(dialogId?: string | null): ChatRoot | null {
    const id = dialogId ?? chatStore.state.activeId;
    const own = id ? chatRootOf(chatStore.summaryOf(id), resolvedFolders()) : null;
    return own ?? resolvedFolders()[0] ?? null;
  }

  /**
   * The file a path from the chat names: in the chat's folder, absolute,
   * or in the active chat's context folder (`@context/…`, on the virtual
   * volume). Null when it can't be one.
   */
  async function resolveOpenPath(path: string, folder: ChatRoot | null): Promise<string | null> {
    if (isContextPath(path)) {
      const dialogId = chatStore.state.activeId;
      return dialogId ? contextRealPath(dialogId, path) : null;
    }
    return chatPathToAbsolute(folder, path);
  }
  // Which paths mentioned in the chat are files in its folder (or the
  // agent's context folder).
  setProjectFileResolver(async (path) => {
    const abs = await resolveOpenPath(path, folderForChat());
    if (!abs || !(await pathExists(abs))) return null;
    return (await dirEntryCount(abs, 1)) === null ? abs : null; // a file, not a folder
  });
  onCleanup(() => setProjectFileResolver(null));
  // "Add to chat" in the editor: bring the chat up; it takes the reference.
  const stopAddToChat = onAddToChat(() => setSidebarTab('chat'));
  // "Show" on an agent question's notification.
  onCleanup(onShowChat(() => setSidebarTab('chat')));
  onCleanup(() => {
    stopWritten();
    stopDeleted();
    stopAddToChat();
    stopAgentEdits();
  });

  function updateContent(path: string, content: string) {
    setOpenFiles((prev) =>
      prev.map((f) =>
        f.path === path && !f.readOnly ? { ...f, content, dirty: content !== f.originalContent } : f
      )
    );
  }

  async function saveActiveFile(overwrite = false) {
    const file = activeFile();
    // Read-only preview tabs (e.g. stashed content) are never saved.
    if (!file || file.readOnly || (!file.dirty && !file.deleted)) {
      console.warn(
        '[saveActiveFile] no-op',
        file && {
          path: file.path,
          dirty: file.dirty,
          readOnly: file.readOnly,
          deleted: file.deleted,
        }
      );
      return;
    }
    try {
      // Checked against what the tab loaded, so a save can't erase a change
      // made on disk meanwhile (an agent chat, a command). A deleted file
      // has nothing to protect.
      const expected = overwrite || file.deleted ? undefined : file.originalContent;
      await writeFileContent(file.path, file.content, expected);
      // A re-created file is a new inode, so it has a new id.
      const id = file.deleted ? await getPathId(file.path).catch(() => file.id) : file.id;
      setOpenFiles((prev) =>
        prev.map((f) =>
          // What was written is the saved state; text typed while the save
          // was in flight stays unsaved.
          f.path === file.path
            ? {
                ...f,
                id,
                originalContent: file.content,
                dirty: f.content !== file.content,
                deleted: false,
              }
            : f
        )
      );
      // Saved: the user has seen the agent's changes in it.
      clearAgentChanges(file.path);
      // rust-analyzer checks the project (cargo check) on save.
      rustFileSaved(file.path);
      // Show it in the tree right away, without waiting for the watcher
      // (which can miss files written into a just-created folder).
      if (file.deleted) tree.refreshAncestors(file.path);
    } catch (err) {
      const message = String(err);
      if (message.startsWith(CHANGED_ON_DISK)) {
        await resolveChangedOnDisk(file, message.slice(CHANGED_ON_DISK.length));
        return;
      }
      console.error(`Failed to save ${file.path}:`, err);
      window.alert(`Couldn't save ${file.name}: ${message}`);
    }
  }

  /**
   * A save refused because the file changed on disk since the tab loaded
   * it. Overwrite replaces that change with the tab's text; Compare keeps
   * the tab unsaved and opens the version on disk beside it, with the
   * differences from the tab's text marked.
   */
  async function resolveChangedOnDisk(file: OpenFile, by: string) {
    const overwrite = await confirmDialog({
      title: `${file.name} changed on disk`,
      message:
        `It was changed ${by} since you opened it. ` +
        'Overwrite replaces that change with your version. ' +
        'Compare keeps your text unsaved and opens the version on disk next to it.',
      confirmLabel: 'Overwrite',
      cancelLabel: 'Compare',
      danger: true,
    });
    if (overwrite) {
      // The tab may have been closed while the dialog was open.
      if (!openFiles().some((f) => f.path === file.path)) return;
      if (activePath() !== file.path) setActivePath(file.path);
      await saveActiveFile(true);
      return;
    }
    let disk: string;
    try {
      disk = await readFileContent(file.path);
    } catch (err) {
      window.alert(`Couldn't read ${file.name}: ${String(err)}`);
      return;
    }
    // The tab may have been closed, or typed in, while the dialog was open:
    // compare with its text as it is now.
    const current = openFiles().find((f) => f.path === file.path);
    if (!current) return;
    const virtualPath = `disk:${file.path}`;
    const tab: OpenFile = {
      id: virtualPath,
      path: virtualPath,
      name: `${file.name} (on disk)`,
      content: disk,
      originalContent: disk,
      dirty: false,
      language: file.language,
      readOnly: true,
    };
    setGitBase(virtualPath, '', file.name, current.content);
    // The user has now seen the version on disk: the tab is checked against
    // it from here on, so a save after merging by hand isn't refused again
    // (and saving the tab untouched replaces it, as Overwrite would).
    setOpenFiles((prev) => [
      ...prev
        .filter((f) => f.path !== virtualPath)
        .map((f) =>
          f.path === file.path ? { ...f, originalContent: disk, dirty: f.content !== disk } : f
        ),
      tab,
    ]);
    setActivePath(virtualPath);
  }

  /**
   * Re-checks on disk whether open files at or under `changedPath` (all of
   * them when omitted) still exist, and marks their tabs. Goes by the disk
   * rather than the event kind: renames arrive as "modify", and a remove and
   * a re-create in one debounce window can be delivered in either order.
   */
  async function syncDeletedTabs(changedPath?: string) {
    const targets = openFiles().filter(
      (f) => !f.readOnly && (!changedPath || isAtOrUnder(f.path, changedPath))
    );
    if (targets.length === 0) return;
    const states = await Promise.all(
      targets.map(async (f) => {
        const exists = await pathExists(f.path).catch(() => true);
        return { path: f.path, deleted: !exists };
      })
    );
    const changed = states.filter(
      (s) => !!openFiles().find((f) => f.path === s.path)?.deleted !== s.deleted
    );
    if (changed.length === 0) return;

    // Came back on disk (git checkout, undo in another tool): take the
    // disk's text unless the tab has unsaved edits.
    const restored = await Promise.all(
      changed
        .filter((s) => !s.deleted)
        .map(async (s) => {
          const [content, id] = await Promise.all([
            readFileContent(s.path).catch(() => null),
            getPathId(s.path).catch(() => null),
          ]);
          return { path: s.path, content, id };
        })
    );

    setOpenFiles((prev) =>
      prev.map((f) => {
        const state = changed.find((s) => s.path === f.path);
        if (!state) return f;
        if (state.deleted) return { ...f, deleted: true };
        const back = restored.find((r) => r.path === f.path);
        const next: OpenFile = { ...f, deleted: false, id: back?.id ?? f.id };
        if (!f.dirty && back?.content != null) {
          next.content = back.content;
          next.originalContent = back.content;
        }
        return next;
      })
    );
  }

  /**
   * Loads the version of a project file that a revert would restore and
   * marks the file's changes against it (lib/gitMarks.ts). Null for a file
   * git doesn't have (everything is a change); undefined, with no marks,
   * for a file in none of the window's repositories, a binary, or git
   * trouble.
   */
  async function loadGitBase(abs: string): Promise<string | null | undefined> {
    const at = repoFileOf(abs, await workdirsNow());
    if (!at) return undefined;
    const base = await invoke<string | null>('git_base_content', {
      path: at.workdir,
      file: at.file,
    }).catch(() => undefined);
    if (base !== undefined) setGitBase(abs, at.workdir, at.file, base);
    return base;
  }

  /*
   * From the Git tab's stash list: open a stashed file's stashed content in
   * a read-only tab, with change marks against the stash's base commit —
   * showing what the stash changes. Deleted files are never opened (there
   * is no content to show); binary/too-large files alert and stay closed.
   */
  async function openFromStash(abs: string, file: string, index: number, message?: string) {
    const root = rootPath();
    if (!root) return;
    let preview: GitStashFilePreview;
    try {
      preview = await invoke<GitStashFilePreview>('git_stash_file_preview', {
        path: root,
        index,
        file,
      });
    } catch (err) {
      window.alert(`Couldn't preview ${file}: ${String(err)}`);
      return;
    }
    // Deleted in the stash: nothing to open.
    if (preview.content === null) return;
    const shortName = file.split('/').pop() ?? file;
    await openInEditor({
      path: `stash@${index}:${abs}`,
      focus: true,
      marks: {
        compare: {
          title: `${shortName} (stash@${index})`,
          fileName: file,
          before: preview.base ?? '',
          after: preview.content,
          stash: { index, file, message },
        },
      },
    });
  }

  /** Stash list changed (push/pop/drop): indices shift, so drop preview tabs. */
  function closeStashTabs() {
    const stale = openFiles().filter((f) => f.readOnly && f.stash);
    if (stale.length === 0) return;
    for (const f of stale) clearGitBase(f.path);
    setOpenFiles((prev) => prev.filter((f) => !(f.readOnly && f.stash)));
    const current = activePath();
    if (current && stale.some((f) => f.path === current)) setActivePath(lastTab());
  }

  /*
   * From the Git tab's history view: open a file's content at a commit in
   * a read-only tab, with change marks against the commit's first parent —
   * showing what the commit changed. Deleted files are never opened (there
   * is no content to show); binary/too-large files alert and stay closed.
   */
  async function openFromCommit(abs: string, file: string, oid: string, summary?: string) {
    const root = rootPath();
    if (!root) return;
    let preview: import('./types').GitStashFilePreview;
    try {
      preview = await invoke<import('./types').GitStashFilePreview>('git_commit_file_preview', {
        path: root,
        oid,
        file,
      });
    } catch (err) {
      window.alert(`Couldn't preview ${file}: ${String(err)}`);
      return;
    }
    // Deleted in the commit: nothing to open.
    if (preview.content === null) return;
    const shortName = file.split('/').pop() ?? file;
    await openInEditor({
      path: `commit@${oid}:${abs}`,
      focus: true,
      marks: {
        compare: {
          title: `${shortName} (${oid.slice(0, 7)})`,
          fileName: file,
          before: preview.base ?? '',
          after: preview.content,
          commit: { oid, file, summary },
        },
      },
    });
  }

  /**
   * "Implement this plan" over a plan file: saves it, then hands it to the
   * chat, which starts the run in Agent mode (lib/plans.ts).
   */
  async function implementOpenPlan() {
    const file = activeFile();
    if (!file || !isPlanFile(file.path)) return;
    if (file.dirty) await saveActiveFile();
    setSidebarTab('chat');
    requestImplementPlan({ path: file.path });
  }

  /** A reverted file: its tab shows the restored text (unless it has unsaved edits). */
  async function reloadFromDisk(abs: string) {
    const content = await readFileContent(abs).catch(() => null);
    if (content !== null) notifyFileWritten(abs, content);
  }

  /** One file changed on disk: its tab, if open and clean, takes the new text. */
  async function refreshCleanTab(path: string) {
    const tab = openFiles().find((f) => f.path === path);
    if (!tab || tab.dirty || tab.readOnly || tab.deleted) return;
    const content = await readFileContent(tab.path).catch(() => null);
    // Applied only if still clean then (onFileWritten checks again).
    if (content !== null && content !== tab.content) notifyFileWritten(tab.path, content);
  }

  /** A pull, merge or rebase rewrote files: open tabs without unsaved
   *  edits show the disk's text; files it removed are marked deleted. */
  async function reloadOpenFiles() {
    await syncDeletedTabs();
    const clean = openFiles().filter((f) => !f.readOnly && !f.dirty && !f.deleted);
    await Promise.all(clean.map((f) => reloadFromDisk(f.path)));
  }

  /** Git's state changed: the marked files' bases may have too. */
  let gitRefresh = 0;
  function refreshGitBases() {
    const run = ++gitRefresh;
    // A base is asked of the file's own repository: in one checkout, a
    // workspace's folders may be in one repository or in several (D10). A
    // tab of a version of a file (disk:, chat:, a stash's, a commit's) keeps
    // its base: git has nothing newer to say about it.
    for (const { absPath, workdir, file } of refreshableFiles()) {
      void invoke<string | null>('git_base_content', { path: workdir, file })
        .then((base) => {
          if (run === gitRefresh) setGitBase(absPath, workdir, file, base);
        })
        .catch(() => clearGitBase(absPath));
    }
  }

  async function closeFile(path: string) {
    const file = openFiles().find((f) => f.path === path);
    if (file?.dirty) {
      const discard = await confirmDialog({
        title: `Discard changes to ${file.name}?`,
        message: 'It has changes that aren’t saved. Closing the tab throws them away.',
        confirmLabel: 'Discard',
        danger: true,
      });
      if (!discard) return;
    }
    // Closed meanwhile (or never open): nothing more to do.
    if (!openFiles().some((f) => f.path === path)) return;
    clearGitBase(path);
    dropReveal(path);

    setOpenFiles((files) => files.filter((f) => f.path !== path));
    if (activePath() === path) setActivePath(lastTab());
    // Its model stays (lib/models.ts), the file to the worker from now on:
    // edits thrown away go from it, as the file on disk has none of them.
    if (file?.dirty) void followDisk(path);
  }

  /** Closes the tab `path`: a browser tab at once, a file as closeFile does. */
  function closeTab(path: string) {
    if (!browserTabs().some((t) => t.path === path)) return void closeFile(path);
    setTabs((all) => all.filter((t) => t.path !== path));
    if (activePath() === path) setActivePath(lastTab());
  }

  /** Browser tabs opened in this window: each one's key is its number. */
  let pagesOpened = 0;

  /**
   * Shows `address` in a browser tab (lib/browserTabs.ts): the one showing
   * it already, loaded again, else a new one. The editor comes into view,
   * as for a file the user opens.
   */
  function openPageTab(address: string) {
    const opened = withPage(tabs(), address, () => `browser:${++pagesOpened}`);
    setTabs(opened.tabs);
    setActivePath(opened.path);
    showEditor();
  }
  // "Opened ports" opens a page again; the app asks for a remote chat's.
  setPageOpener(openPageTab);
  onCleanup(() => setPageOpener(null));
  onMount(() => onCleanup(onPageWanted(openPageTab)));

  /** The active tab's path, when it's a file's: its editor shows. */
  const activeFilePath = createMemo(() => {
    const path = activePath();
    return path && !browserTabs().some((t) => t.path === path) ? path : null;
  });
  /** Each forwarded port of the remote chats': browser tabs load through them. */
  const forwardedNow = () => Object.values(cloud.ports).flat();
  // A tab of a forwarded page closes with its port: its server stopped, as
  // when its command timed out, and nothing serves the page any more.
  closeTabsOfClosedPorts(browserTabs, forwardedNow, closeTab);

  function findRowAtPoint(x: number, y: number): TreeRow | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const rowEl = el?.closest<HTMLDivElement>('[data-file-id]');
    const id = rowEl?.dataset.fileId;
    if (!id) return null;
    const row = tree.rows().find((r) => r.id === id);
    return row && !row.loading ? row : null;
  }

  function handleContextMenu(e: MouseEvent) {
    e.preventDefault();

    const posX = e.clientX;
    const posY = e.clientY;

    let showContextMenu = false;

    const fileTree = fileTreeEl();
    if (fileTree) {
      const rect = fileTree.getBoundingClientRect();
      if (posX >= rect.left && posX <= rect.right && posY >= rect.top && posY <= rect.bottom) {
        showContextMenu = true;
        setActivePanel('files');
        const row = findRowAtPoint(posX, posY);

        // If right-clicking on a non-selected item, select only that item
        if (row && !selectedFileIds().has(row.id)) {
          setSelectedFileIds(new Set([row.id]));
          setLastSelectedId(row.id);
        }
      }
    }

    if (!showContextMenu) {
      setActivePanel(null);
      setContextMenuVisible(false);
      return;
    }

    setContextMenuVisible(true);
    setRawCursor({ x: posX, y: posY });
  }

  const closeContextMenu = () => {
    setContextMenuVisible(false);
  };

  /** What the delete confirmation says, for the selected rows. */
  function deleteConfirmation(rows: TreeRow[], counts: (number | null)[]): ConfirmOptions {
    const items = (n: number) =>
      n >= 10_000 ? '10,000+ items' : `${n.toLocaleString()} item${n === 1 ? '' : 's'}`;
    const ask = { confirmLabel: 'Move to Trash', danger: true };
    if (rows.length === 1) {
      const [row] = rows;
      if (!row.isDir) {
        return {
          ...ask,
          title: `Move ${row.name} to the trash?`,
          message: 'You can restore it from the trash.',
        };
      }
      const n = counts[0];
      return {
        ...ask,
        title: `Move the ${row.name} folder to the trash?`,
        message:
          n === null
            ? 'Everything in it goes with it. You can restore it from the trash.'
            : `It has ${items(n)} in it. You can restore it from the trash.`,
      };
    }
    const shown = rows.slice(0, 5).map((r) => (r.isDir ? `${r.name}/` : r.name));
    const more = rows.length > 5 ? ` and ${rows.length - 5} more` : '';
    const folders = rows.some((r, i) => r.isDir && counts[i] !== 0)
      ? '\nFolders go with everything in them.'
      : '';
    return {
      ...ask,
      title: `Move ${rows.length} items to the trash?`,
      message: `${shown.join(', ')}${more}${folders}\n\nYou can restore them from the trash.`,
    };
  }

  /** A confirmation is open: a second Ctrl+Delete mustn't open another. */
  let deleteInProgress = false;

  /**
   * Deletes the selected files and folders, from the context menu or the
   * keyboard (Ctrl/Cmd+Delete or Backspace). Asks first, unless everything
   * selected is an empty folder.
   */
  async function handleDeleteFile() {
    setContextMenuVisible(false);
    // Items inside a selected folder go with it: deleting them separately
    // would fail once the folder is gone. A folder's own row isn't deleted:
    // it leaves the workspace instead (D9).
    const selected = actionable();
    const selectedRows = selected.filter(
      (r) =>
        !selected.some(
          (o) => o !== r && o.isDir && r.path !== o.path && isAtOrUnder(r.path, o.path)
        )
    );
    if (selectedRows.length === 0 || deleteInProgress) return;
    deleteInProgress = true;
    try {
      // Entries in each folder, counted on disk (the tree hides .git, so a
      // folder with just a repository in it would look empty there).
      const counts = await Promise.all(
        selectedRows.map((r) => (r.isDir ? dirEntryCount(r.path).catch(() => null) : null))
      );
      const onlyEmptyFolders = selectedRows.every((r, i) => r.isDir && counts[i] === 0);
      if (!onlyEmptyFolders && !(await confirmDialog(deleteConfirmation(selectedRows, counts)))) {
        return;
      }
      await deleteRows(selectedRows);
    } finally {
      deleteInProgress = false;
    }
  }

  /**
   * Moves the rows to the system trash. Items that can't go there (no trash
   * on that drive) are listed once, with the choice to delete them for good.
   */
  async function deleteRows(selectedRows: TreeRow[]) {
    const markDeleted = (row: TreeRow) =>
      // Tabs stay open, marked deleted (a folder's files too): unsaved text
      // isn't lost, and saving puts the file back.
      setOpenFiles((prev) =>
        prev.map((f) =>
          f.id === row.id || isAtOrUnder(f.path, row.path) ? { ...f, deleted: true } : f
        )
      );

    const noTrash: TreeRow[] = [];
    try {
      for (const row of selectedRows) {
        try {
          await deletePathOnDisk(row.id);
          markDeleted(row);
        } catch (err) {
          if (!isTrashUnavailable(err)) throw err;
          console.warn('No trash for', row.path, err);
          noTrash.push(row);
        }
      }

      if (noTrash.length > 0) {
        const names = noTrash.map((r) => (r.isDir ? `${r.name}/` : r.name));
        const permanent = await confirmDialog({
          title:
            noTrash.length === 1
              ? `${names[0]} can’t be moved to the trash`
              : `${noTrash.length} items can’t be moved to the trash`,
          message:
            `${noTrash.length === 1 ? '' : `${names.slice(0, 5).join(', ')}${noTrash.length > 5 ? ` and ${noTrash.length - 5} more` : ''}\n\n`}` +
            'There’s no trash on that drive (common for network drives). ' +
            'Delete permanently instead? This can’t be undone.',
          confirmLabel: 'Delete permanently',
          danger: true,
        });
        if (permanent) {
          for (const row of noTrash) {
            await deletePathOnDisk(row.id, true);
            markDeleted(row);
          }
        }
      }
      setSelectedFileIds(new Set<string>());
      setLastSelectedId(null);
    } catch (err) {
      console.error('Failed to delete:', err);
      window.alert(String(err));
    }
  }

  function handleCopyFile() {
    setContextMenuVisible(false);
    // A window folder's row is never copied: it isn't the rows' to act on (D9).
    const selectedIds = withoutFolderRows(selectedFileIds(), tree.rows());
    if (selectedIds.length === 0) return;
    setClipboard({ ids: selectedIds, mode: 'copy' });
  }

  function handleCutFile() {
    setContextMenuVisible(false);
    const selectedIds = withoutFolderRows(selectedFileIds(), tree.rows());
    if (selectedIds.length === 0) return;
    setClipboard({ ids: selectedIds, mode: 'cut' });
  }

  async function handlePasteFile() {
    setContextMenuVisible(false);
    const entry = clipboard();
    const target = firstSelectedRow();
    if (!entry) return;
    // A folder row that isn't there has no id to paste into (D20).
    if (target?.missing) return;

    // The top of a selected file's own folder, else the active folder's.
    const folder = target ? folderAt(target.path) : null;
    const destDirId = target?.isDir
      ? target.id
      : folder
        ? (rootIds()[folder.path] ?? null)
        : rootId();
    if (!destDirId) {
      window.alert('No destination directory available');
      return;
    }

    try {
      for (const id of entry.ids) {
        if (entry.mode === 'copy') {
          await copyPathOnDisk(id, destDirId);
        } else {
          const oldPath =
            tree.rows().find((r) => r.id === id)?.path ??
            openFiles().find((f) => f.id === id)?.path;
          const newPath = await movePathOnDisk(id, destDirId);
          if (oldPath) followMoves([{ from: oldPath, to: newPath }]);
        }
      }
    } catch (err) {
      console.error('Failed to paste:', err);
      window.alert(fsErrorMessage(err));
    }
    if (entry.mode === 'cut') setClipboard(null);
  }

  /**
   * Drag-and-drop in the Files tree: moves each dropped id into the folder
   * (or the project root when destDir is null) via the `move_path` Tauri
   * command. Ids are stable across a move, so open tabs are re-pointed at
   * the new paths — including tabs inside a moved folder.
   */
  async function handleTreeDrop(sourceIds: string[], destDir: TreeRow | null) {
    const destDirId = destDir ? destDir.id : rootId();
    if (!destDirId || sourceIds.length === 0) return;
    const destPath = destDir ? destDir.path : rootPath();
    if (!destPath) return;

    const byId = new Map(tree.rows().map((r) => [r.id, r]));
    // A folder dropped together with its own child moves once, with the
    // parent: skip ids nested under another dragged folder.
    const draggedPaths = new Map<string, string>();
    for (const id of sourceIds) {
      const row = byId.get(id);
      if (row) draggedPaths.set(id, row.path);
    }
    const topLevel = sourceIds.filter((id) => {
      const path = draggedPaths.get(id);
      if (!path) return false;
      for (const [otherId, otherPath] of draggedPaths) {
        if (otherId !== id && byId.get(otherId)?.isDir && isAtOrUnder(path, otherPath)) {
          return false;
        }
      }
      return true;
    });

    const failures: string[] = [];
    const moved: { id: string; from: string; to: string }[] = [];
    for (const id of topLevel) {
      const src = byId.get(id);
      if (!src) continue;
      // Same folder (or onto itself): nothing to do.
      if (destDir && src.id === destDir.id) continue;
      if (destDir && isAtOrUnder(destDir.path, src.path)) {
        failures.push(`${src.name}: can't move a folder into itself`);
        continue;
      }
      if (parentOf(src.path) === destPath) continue;
      try {
        const to = await movePathOnDisk(id, destDirId);
        moved.push({ id, from: src.path, to });
      } catch (err) {
        console.error(`Failed to move ${src.path}:`, err);
        failures.push(`${src.name}: ${fsErrorMessage(err)}`);
      }
    }

    if (moved.length > 0) {
      followMoves(moved);
      // Refresh both sides right away instead of waiting for the watcher.
      for (const m of moved) {
        tree.refresh(m.from);
        tree.refresh(m.to);
      }
      // A pending cut of something just moved no longer applies.
      const entry = clipboard();
      if (entry?.mode === 'cut' && entry.ids.some((id) => moved.some((m) => m.id === id))) {
        setClipboard(null);
      }
    }
    if (failures.length > 0) window.alert(failures.join('\n'));
  }

  /**
   * Tabs at or inside a renamed or moved file or folder follow it to its new
   * path, by path: a tab opened by path (go to definition, a search result,
   * a chat link, a restored session) has no tree id to match, and the files
   * inside a folder never do.
   */
  function followMoves(moves: { from: string; to: string }[]) {
    for (const { from, to } of moves) tree.moved(from, to);
    setOpenFiles((prev) =>
      prev.map((f) => {
        const path = movedPath(f.path, moves);
        if (!path) return f;
        const name = path.split(/[\\/]/).pop() ?? f.name;
        return { ...f, path, name, language: languageForFile(name) };
      })
    );
    const active = activePath();
    const next = active ? movedPath(active, moves) : null;
    if (next) setActivePath(next);
  }

  /**
   * The folder a new file or folder goes in: the clicked folder itself, the
   * folder a clicked file is in, or the project root. With several items
   * selected, the topmost one decides.
   */
  function creationFolder(target: TreeRow | null): TreeRow | null {
    if (!target) return null;
    if (target.isDir) return target;
    // The tree is flattened in order: a file's folder is the nearest row
    // above it one level up. Top-level files are in the root.
    const rows = tree.rows();
    const at = rows.findIndex((r) => r.id === target.id);
    for (let i = at - 1; i >= 0; i--) {
      if (rows[i].depth === target.depth - 1 && rows[i].isDir) return rows[i];
    }
    return null;
  }

  /** The tree's row for one of the window's folders: a workspace's folders have one (D9). */
  function folderRowOf(folder: OpenFolder | null): TreeRow | null {
    if (!folder) return null;
    return tree.rows().find((row) => row.folderRow && row.path === folder.path) ?? null;
  }

  function beginCreate(kind: 'file' | 'folder') {
    setContextMenuVisible(false);
    const selected = firstSelectedRow();
    // With nothing selected, the new entry goes into the active folder: in a
    // workspace that is that folder's row, so the draft sits at depth 1 under
    // it; a project's root has no row, and the draft is at the top (D15).
    const folder = selected ? creationFolder(selected) : folderRowOf(activeFolderNow());
    const parentId = folder ? folder.id : rootId();
    if (!parentId) return;

    const parentPath = folder ? folder.path : rootPath();
    const parentDepth = folder ? folder.depth : -1;

    if (folder && !tree.expanded().has(folder.path)) {
      tree.toggle(folder);
    }

    setDraftEntry({ parentId, parentPath, depth: parentDepth + 1, isDir: kind === 'folder' });
    setEditingId('__draft__');
  }

  function handleNewFile() {
    beginCreate('file');
  }

  function handleNewFolder() {
    beginCreate('folder');
  }

  function handleRenameFile() {
    setContextMenuVisible(false);
    // The rename goes to the selection's first row an action can act on: a
    // folder's own row is left out, as Rename is enabled for one such row (D9).
    const target = actionable()[0];
    if (!target) return;
    setEditingId(target.id);
  }

  async function handleTreeEditSubmit(row: TreeRow, rawName: string) {
    const name = rawName.trim();

    if (row.id === '__draft__') {
      const draft = draftEntry();
      setEditingId(null);
      setDraftEntry(null);
      if (!name || !draft) return;
      try {
        if (draft.isDir) {
          await createFolderOnDisk(draft.parentId, name);
        } else {
          // Opened once the tree lists it (the effect on pendingOpenPath).
          setPendingOpenPath(await createFileOnDisk(draft.parentId, name));
        }
      } catch (err) {
        console.error(`Failed to create ${draft.isDir ? 'folder' : 'file'}:`, err);
        window.alert(fsErrorMessage(err));
      }
      return;
    }

    setEditingId(null);
    if (!name || name === row.name) return;
    try {
      const newPath = await renamePathOnDisk(row.id, name);
      followMoves([{ from: row.path, to: newPath }]);
    } catch (err) {
      console.error('Failed to rename:', err);
      window.alert(fsErrorMessage(err));
    }
  }

  function handleTreeEditCancel(row: TreeRow) {
    setEditingId(null);
    if (row.id === '__draft__') setDraftEntry(null);
  }

  function handleKeyDown(e: KeyboardEvent) {
    const isMac = navigator.platform.toUpperCase().indexOf('MAC') >= 0;
    const modKey = isMac ? e.metaKey : e.ctrlKey;

    const target = e.target as HTMLElement;
    const isTyping =
      target.tagName === 'INPUT' ||
      target.tagName === 'TEXTAREA' ||
      (target as HTMLElement).isContentEditable;

    if (isTyping) return;
    if (!modKey) return;

    // A window folder's row isn't copied, cut or deleted: it leaves the
    // workspace instead (D9).
    const selected = withoutFolderRows(selectedFileIds(), tree.rows());
    const key = e.key.toLowerCase();

    if (key === 'c' && selected.length > 0) {
      e.preventDefault();
      setClipboard({ ids: selected, mode: 'copy' });
    }

    if (key === 'x' && selected.length > 0) {
      e.preventDefault();
      setClipboard({ ids: selected, mode: 'cut' });
    }

    if (key === 'v') {
      const entry = clipboard();
      if (entry) {
        e.preventDefault();
        handlePasteFile();
      }
    }

    // Delete with Backspace or Delete key
    if ((key === 'backspace' || key === 'delete') && selected.length > 0) {
      e.preventDefault();
      handleDeleteFile();
    }
  }

  function handleFileTreeSelect(
    row: TreeRow,
    options: {
      shiftKey: boolean;
      modKey: boolean;
    }
  ) {
    const { shiftKey, modKey } = options;

    const currentSelection = selectedFileIds();
    const newSelection = new Set(currentSelection);

    if (shiftKey && lastSelectedId()) {
      const rows = tree.rows();

      const lastIndex = rows.findIndex((r) => r.id === lastSelectedId());

      const currentIndex = rows.findIndex((r) => r.id === row.id);

      if (lastIndex !== -1 && currentIndex !== -1) {
        const start = Math.min(lastIndex, currentIndex);
        const end = Math.max(lastIndex, currentIndex);

        // Shift selection starts from the existing selection and
        // adds the entire range.
        for (let i = start; i <= end; i++) {
          if (!rows[i].loading) {
            newSelection.add(rows[i].id);
          }
        }
      }
    } else if (modKey) {
      // Ctrl+Click on Windows/Linux
      // Cmd+Click on macOS
      if (newSelection.has(row.id)) {
        newSelection.delete(row.id);
      } else {
        newSelection.add(row.id);
      }
    } else {
      // Normal click selects exactly one item.
      newSelection.clear();
      newSelection.add(row.id);
    }

    setSelectedFileIds(newSelection);
    setLastSelectedId(row.id);
  }

  function handleFileTreeClearSelection() {
    setSelectedFileIds(new Set<string>());
    setLastSelectedId(null);
  }

  /*
   * Closing the context menu. It used to close only on a window "click",
   * which a right-click never fires: right-clicking the editor opened
   * Monaco's menu next to ours. Now any press or right-click outside the
   * menu closes it. In the capture phase, so it runs before anything that
   * stops the event (Monaco does); a right-click in the file tree then
   * reopens the menu where it was clicked (handleContextMenu).
   */
  const outsideMenu = (e: Event) => !menuEl()?.contains(e.target as Node);
  const closeOnPress = (e: Event) => {
    if (contextMenuVisible() && outsideMenu(e)) closeContextMenu();
  };
  const closeOnEscape = (e: KeyboardEvent) => {
    if (e.key === 'Escape' && contextMenuVisible()) {
      e.preventDefault();
      closeContextMenu();
    }
  };
  const closeNow = () => {
    if (contextMenuVisible()) closeContextMenu();
  };

  onMount(() => {
    window.addEventListener('pointerdown', closeOnPress, true);
    window.addEventListener('contextmenu', closeOnPress, true);
    // Scrolling anything but the menu leaves it pointing at nothing.
    window.addEventListener('scroll', closeOnPress, true);
    window.addEventListener('keydown', closeOnEscape, true);
    window.addEventListener('blur', closeNow);
    window.addEventListener('resize', closeNow);
    window.addEventListener('keydown', handleKeyDown);

    onCleanup(() => {
      window.removeEventListener('pointerdown', closeOnPress, true);
      window.removeEventListener('contextmenu', closeOnPress, true);
      window.removeEventListener('scroll', closeOnPress, true);
      window.removeEventListener('keydown', closeOnEscape, true);
      window.removeEventListener('blur', closeNow);
      window.removeEventListener('resize', closeNow);
      window.removeEventListener('keydown', handleKeyDown);
    });
  });

  onMount(async () => {
    if (unlistenFSChanges) {
      unlistenFSChanges();
      unlistenFSChanges = undefined;
    }

    unlistenFSChanges = await listen<FileWatchPayload>('fs-changes', (event) => {
      const { path, operation, is_dir } = event.payload;
      // Too much at once for the watcher to say what (a branch switch, an
      // unpacked archive): everything open is checked again.
      const everything = operation === 'unknown';
      switch (operation) {
        // Files are opened by whoever made them (the tree's New File, the
        // agent's writes), never because they appeared: dev servers and
        // builds create files all the time.
        case 'create':
        case 'modify':
        case 'remove':
          tree.refresh(path);
          break;
        case 'unknown':
          tree.refresh();
          break;
      }
      // Changed outside the editor (another program, a build, git): a tab
      // without unsaved edits shows the new text. Our own saves read back
      // the same text and change nothing.
      if (everything) {
        void reloadOpenFiles();
      } else {
        void syncDeletedTabs(path);
        if (!is_dir && (operation === 'create' || operation === 'modify')) {
          void refreshCleanTab(path);
        }
      }
      // Models no tab holds (closed tabs, go to definition's) are what the
      // TypeScript worker and the language servers read for their files:
      // they follow the disk too (lib/models.ts).
      void followDisk(path);
      // IntelliSense's copy of the project's sources (lib/intellisense).
      // Batched there: read and applied together once changes settle. Read
      // whatever the event, the disk saying whether the file is there: a
      // rename is a change of both paths, and a removal and a creation of
      // one path can come in either order. A path that isn't a source may
      // be a folder that went (renamed away, removed): its sources go too.
      // A folder that came is reported with what's in it.
      if (everything) updateWholeProject();
      else if (!is_dir && isProjectSource(path, folderAt(path)?.path ?? null))
        updateProjectSource(path);
      else if (!is_dir) updateProjectPath(path);
      // A manifest write (package.json, lockfiles) means node_modules may
      // have changed (node_modules itself is watcher-ignored); a tsconfig
      // change can move files between configs or change `types`. Both are
      // followed up there once writes settle.
      if (!is_dir) notifyProjectFileWritten(path);
    });
  });

  onCleanup(() => {
    if (unlistenFSChanges) {
      unlistenFSChanges();
      unlistenFSChanges = undefined;
    }
  });

  createEffect(() => {
    const at = target();
    const expandedPaths = Array.from(tree.expanded());
    // Read-only preview tabs (stash content) live only for this session.
    const files = openFiles().filter((f) => !f.readOnly);
    const collapsedTerminal = terminalCollapsed();
    if (!at) return;
    // A workspace has no path of its own: its ID opens it again (D17).
    const path = at.kind === 'folder' ? at.path : '';
    const workspace = at.kind === 'workspace' ? at.id : null;
    saveLastProject(path, workspace, files, expandedPaths, collapsedTerminal).catch((err) => {
      console.error('Failed to save last project:', err);
    });
  });

  /** The target's tabs shown now, as kept for coming back to it; none
   *  while they're being opened. Text: it changes when they do, not as
   *  their files are typed in. */
  const tabsNow = createMemo((): string | null => {
    const key = sessionKey(target());
    if (!key || restoringProject() || tabsComing() === key) return null;
    const files: SessionFile[] = openFiles()
      .filter((f) => !f.readOnly)
      .map(({ id, path, name, language }) => ({ id, path, name, language }));
    return JSON.stringify({
      root: key,
      files,
      active: activePath(),
      expanded: [...tree.expanded()],
      terminalOpen: !terminalCollapsed(),
    });
  });
  let tabsTimer: ReturnType<typeof setTimeout> | undefined;
  /** Keeps the tabs shown now as their project's (lib/projectSessions.ts). */
  function keepTabsNow(): Promise<void> {
    clearTimeout(tabsTimer);
    tabsTimer = undefined;
    const now = tabsNow();
    if (!now) return Promise.resolve();
    const { root, ...session } = JSON.parse(now) as ProjectSession & { root: string };
    return rememberProjectSession(root, session);
  }
  // Kept as they change, a moment after: the project opens with them again
  // here or in another window, this one closed or not (last-project.json
  // has one project's, whichever window wrote last).
  createEffect(
    on(tabsNow, (now) => {
      clearTimeout(tabsTimer);
      tabsTimer = now ? setTimeout(() => void keepTabsNow(), 500) : undefined;
    })
  );
  onCleanup(() => clearTimeout(tabsTimer));

  createEffect(async () => {
    const open = folders().map((folder) => folder.path);
    if (open.length === 0) return;
    try {
      await startFileWatch(open);
    } catch (error) {
      console.error('Failed to start file watcher:', error);
    }
  });

  createEffect(() => {
    const pending = pendingOpenPath();
    if (!pending) return;

    const rows = tree.rows();
    const row = rows.find((r) => r.path === pending);
    if (!row || row.loading) return;

    // Path is now in the tree → open it and clear pending
    void openInEditor({ path: row.path, focus: true });
    setPendingOpenPath(null);
  });

  /**
   * Each folder's backend id, read again as the folders change. Only the
   * newest read sets them: a reply for a folder the window has left must not
   * outlive the one for the folders it shows.
   */
  let rootIdsRead = 0;
  createEffect(() => {
    const open = folders().map((folder) => folder.path);
    const read = ++rootIdsRead;
    if (open.length === 0) {
      setRootIds({});
      return;
    }
    void Promise.all(
      open.map((path) =>
        getPathId(path)
          .then((id) => [path, id] as const)
          .catch((err) => {
            console.error(`Failed to resolve ${path}'s id:`, err);
            return null;
          })
      )
    ).then((found) => {
      if (read !== rootIdsRead) return; // a newer read is out
      const ids: Record<string, string> = {};
      for (const one of found) if (one) ids[one[0]] = one[1];
      setRootIds(ids);
    });
  });

  createEffect(() => {
    const selectedIds = selectedFileIds();
    const container = fileTreeEl();

    // Remove old selection
    document.querySelectorAll('[data-file-id].selected').forEach((el) => {
      el.classList.remove('selected');
    });

    if (selectedIds.size === 0 || !container) return;

    // Add selection to all selected items
    selectedIds.forEach((id) => {
      const el = container.querySelector<HTMLElement>(`[data-file-id="${id}"]`);
      if (el) {
        el.classList.add('selected');
      }
    });
  });

  createEffect(() => {
    const visible = contextMenuVisible();
    const panel = activePanel();
    const menu = menuEl();

    if (!visible || panel === null) {
      return;
    }

    if (!menu) {
      return;
    }

    const { x, y } = rawCursor();

    const menuWidth = menu.offsetWidth || 160;
    const menuHeight = menu.offsetHeight || 150;

    let adjustedX = x;
    let adjustedY = y;

    if (x + menuWidth > window.innerWidth) {
      adjustedX = window.innerWidth - menuWidth - 10;
    }
    if (y + menuHeight > window.innerHeight) {
      adjustedY = window.innerHeight - menuHeight - 10;
    }

    setCursorPosition({ x: adjustedX, y: adjustedY });
  });

  return (
    <div
      class="app"
      classList={{ 'sidebar-expanded': sidebarExpanded() }}
      onContextMenu={handleContextMenu}
    >
      <Resizable
        class="sidebar"
        initialWidth={380}
        minWidth={320}
        maxWidth={480}
        expanded={sidebarExpanded()}
        onExpandedChange={setSidebarExpanded}
        showFloatingButton={sidebarTab() === 'chat'}
      >
        <div class="sidebar-topbar">
          <ProjectSwitcher
            onOpenFolder={() => void handlePickFolder()}
            onNewProject={() => setNewProjectOpen(true)}
            onNewWindow={() => void openNewWindow()}
            onOpen={(p) => openProjectAt(p.path)}
            onRelink={(p) => void relinkProject(p)}
            onDelete={(p) => void deleteProject(p)}
          />
          <button
            class="icon-button sidebar-settings-button"
            onClick={() => openAppSettings('menu')}
            title="Settings"
            aria-label="Settings"
          >
            ⚙
          </button>
        </div>
        <Show when={!sidebarExpanded()}>
          <div class="sidebar-tabs">
            <button
              classList={{ active: sidebarTab() === 'files' }}
              onClick={() => setSidebarTab('files')}
            >
              Files
            </button>
            <button
              classList={{ active: sidebarTab() === 'chat' }}
              onClick={() => setSidebarTab('chat')}
            >
              AI Chat
              <Show
                when={chatStore.hasAnyRunning()}
                fallback={
                  <Show when={chatStore.hasAnyUnread()}>
                    <span
                      class="sidebar-tab-dot"
                      title="New messages from the agent"
                      aria-label="New messages from the agent"
                    />
                  </Show>
                }
              >
                <span
                  class="sidebar-tab-spinner"
                  title="The agent is working"
                  aria-label="The agent is working"
                  role="status"
                />
              </Show>
            </button>
            <button
              classList={{ active: sidebarTab() === 'git' }}
              onClick={() => setSidebarTab('git')}
            >
              Git
            </button>
          </div>
        </Show>

        <Show when={sidebarTab() === 'files'}>
          <Show
            when={rootPath()}
            fallback={
              <Show when={!restoringProject()}>
                <div class="files-empty">
                  <button
                    type="button"
                    class="files-empty-open"
                    onClick={() => void handlePickFolder()}
                  >
                    Open project…
                  </button>
                  <button
                    type="button"
                    class="files-empty-new"
                    onClick={() => setNewProjectOpen(true)}
                  >
                    New project…
                  </button>
                  <Show when={projectList().length > 0}>
                    <div class="files-empty-heading">Recent projects</div>
                    <ProjectList
                      projects={projectList()}
                      openIds={openProjectIds()}
                      onOpen={(p) => openProjectAt(p.path)}
                      onRelink={(p) => void relinkProject(p)}
                      onDelete={(p) => void deleteProject(p)}
                    />
                  </Show>
                </div>
              </Show>
            }
          >
            <SearchPanel folders={() => resolvable(folders())} onActiveChange={setSearchActive} />
          </Show>
          <Show when={rootPath() && !searchActive()}>
            <FileTree
              forwardRef={setFileTreeEl}
              rows={displayRows}
              expanded={tree.expanded}
              activePath={activePath}
              selectedIds={selectedFileIds}
              onToggle={tree.toggle}
              onOpenFile={(row) => void openInEditor({ path: row.path, focus: true })}
              onDrop={(ids, destDir) => void handleTreeDrop(ids, destDir)}
              rootDrop={() => target()?.kind !== 'workspace'}
              onSelect={handleFileTreeSelect}
              onClearSelection={handleFileTreeClearSelection}
              editingId={editingId}
              onEditSubmit={handleTreeEditSubmit}
              onEditCancel={handleTreeEditCancel}
              onContextMenu={handleContextMenu}
            />
          </Show>
        </Show>

        <Show when={sidebarTab() === 'chat'}>
          <Show
            when={!chatsBlocked()}
            fallback={
              <div class="files-empty chats-blocked">
                <Show
                  when={chatsBlocked()?.kind === 'no-project-elsewhere'}
                  fallback={
                    <p class="chats-blocked-text">
                      This project’s chats couldn’t be opened:{' '}
                      {(chatsBlocked() as { error?: string } | null)?.error}
                    </p>
                  }
                >
                  <p class="chats-blocked-text">
                    The chats without a project are open in another window. Open a project here to
                    chat about it.
                  </p>
                  <button
                    type="button"
                    class="files-empty-open"
                    onClick={() => void handlePickFolder()}
                  >
                    Open project…
                  </button>
                  <button
                    type="button"
                    class="files-empty-new"
                    onClick={() => void showNoProjectWindow()}
                  >
                    Show that window
                  </button>
                </Show>
              </div>
            }
          >
            <ChatPanel
              folders={resolvedFolders}
              activeFolder={activeFolderNow}
              activeFileContext={activeFileContext}
              files={tree.files}
              owners={owners}
            />
          </Show>
        </Show>

        <Show when={sidebarTab() === 'git'}>
          <Switch>
            <Match when={gitFolders().length > 0}>
              <GitPanel
                folders={gitFolders()}
                session={sessionKey(target())!}
                onOpenFile={(abs) => void openInEditor({ path: abs, focus: true, marks: 'git' })}
                onOpenStashFile={(abs, file, index, message) =>
                  void openFromStash(abs, file, index, message)
                }
                onOpenCommitFile={(abs, file, oid, summary) =>
                  void openFromCommit(abs, file, oid, summary)
                }
                onFileReverted={(abs) => void reloadFromDisk(abs)}
                onChanged={refreshGitBases}
                onStashChanged={closeStashTabs}
                onFilesChangedOnDisk={() => void reloadOpenFiles()}
                onOpenPath={(abs) => void openInEditor({ path: abs, focus: true, marks: 'none' })}
              />
            </Match>
            <Match when={gitFolders().length === 0}>
              <p class="text-center">Open a project to see git panel</p>
            </Match>
          </Switch>
        </Show>
      </Resizable>

      <div class="app-main">
        <div class="tabs-row">
          <Tabs
            tabs={tabs}
            activePath={activePath}
            folders={() => folders()}
            onSelect={(path) => {
              requestEditorFocus(path);
              setActivePath(path);
            }}
            onClose={closeTab}
          />
          {/* A Markdown file: source, source and preview, or preview. */}
          <Show when={activeMarkdownPath()} keyed>
            {(path) => <MarkdownViewSwitch path={path} />}
          </Show>
        </div>
        <div class="editor-area">
          {/* A plan from .rovik/plans/: edit it here, then implement it. */}
          <Show when={activePath() && isPlanFile(activePath()!)}>
            <div class="plan-editor-bar">
              <span class="plan-editor-bar-text">
                Plan. Edit it as you like; implementing uses this text, unsaved edits included.
              </span>
              <button
                type="button"
                class="chat-action-button primary"
                onClick={() => void implementOpenPlan()}
                title="Save the plan, then let the agent carry it out (Agent mode)"
              >
                Implement this plan
              </button>
            </div>
          </Show>
          {/* Each browser tab keeps its page, hidden while another is shown. */}
          <For each={browserTabs()}>
            {(tab) => (
              <BrowserView
                tab={tab}
                active={activePath() === tab.path}
                frame={frameUrl(tab.address, forwardedNow())}
                onNavigate={(address) => setTabs((all) => navigated(all, tab.path, address))}
                onReload={(address) => setTabs((all) => navigated(all, tab.path, address))}
              />
            )}
          </For>
          <Show
            when={activeFilePath()}
            keyed
            fallback={
              <Show when={!activePath()}>
                <div class="empty-state">Open a file from the sidebar to start editing</div>
              </Show>
            }
          >
            {(path) => (
              <Editor
                projectDir={rootPath}
                folders={() => folders().map((folder) => folder.path)}
                loadable={() => resolvable(folders()).map((folder) => folder.path)}
                currentFilePath={path}
                content={() => openFiles().find((f) => f.path === path)?.content}
                language={() => openFiles().find((f) => f.path === path)?.language ?? 'plaintext'}
                onChange={(value) => updateContent(path, value)}
                onSave={() => saveActiveFile()}
                onCursorChange={setCursorInfo}
                readOnly={() => openFiles().find((f) => f.path === path)?.readOnly ?? false}
                readOnlyMessage={
                  openFiles().find((f) => f.path === path)?.commit
                    ? 'Committed version — read-only.'
                    : 'Stashed preview — read-only. Pop the stash to edit.'
                }
              />
            )}
          </Show>
        </div>

        <TerminalPanel
          session={() => sessionKey(target())}
          folders={() => resolvable(folders())}
          activeFolder={activeFolderNow}
          collapsed={terminalCollapsed}
          onCollapsedChange={setTerminalCollapsed}
        />
      </div>

      <Show when={contextMenuVisible() && activePanel() === 'files'}>
        <FileContextMenu
          ref={setMenuEl}
          x={cursorPosition().x}
          y={cursorPosition().y}
          hasSelection={withoutFolderRows(selectedFileIds(), tree.rows()).length > 0}
          singleSelection={actionable().length === 1}
          folder={menuFolder()}
          canPaste={!!clipboard()}
          onNewFile={handleNewFile}
          onNewFolder={handleNewFolder}
          onRename={handleRenameFile}
          onCopy={handleCopyFile}
          onCut={handleCutFile}
          onPaste={handlePasteFile}
          onDelete={handleDeleteFile}
          onRemoveFolder={handleRemoveFolder}
          onLocateFolder={handleLocateFolder}
          onTryFolderAgain={handleTryFolderAgain}
        />
      </Show>
      <ConfirmDialogHost />
      <NewBranchDialogHost />
      <Show when={newProjectOpen()}>
        <NewProjectDialog
          currentPath={rootPath()}
          onCreated={(path) => {
            setNewProjectOpen(false);
            openProjectAt(path);
          }}
          onOpenExisting={(path) => {
            setNewProjectOpen(false);
            openProjectAt(path);
          }}
          onCancel={() => setNewProjectOpen(false)}
        />
      </Show>
      <NotificationHost />
      <Show when={appSettingsPage()} keyed>
        {(page) => (
          <SettingsDialog
            initialPage={page}
            rootPath={activeFolderNow()?.path ?? null}
            folders={resolvable(folders())}
            onClose={closeAppSettings}
          />
        )}
      </Show>
    </div>
  );
}
