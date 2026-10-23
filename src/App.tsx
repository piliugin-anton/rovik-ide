import {
  batch,
  createSignal,
  createMemo,
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
import MarkdownViewSwitch from './components/MarkdownViewSwitch';
import ChatPanel from './components/ChatPanel';
import { createFileTreeController } from './lib/fileTree';
import { languageForFile, isLikelyBinary } from './lib/language';
import {
  pickProjectFolder,
  readFileContent,
  writeFileContent,
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
import type { FileWatchPayload, GitStashFilePreview, LastProjectOpenFile, OpenFile, TreeRow } from './types';
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
import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  rememberProjectSession,
  savedProjectSession,
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
  projectList,
  refreshProjects,
  resolveProject,
} from './lib/projects';
import { chatApi, type ProjectSummary } from './lib/chatApi';
import { rustFileSaved, stopRustSupport } from './lib/lsp/rustAnalyzer';
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
import { isMarkdownLanguage } from './lib/markdownView';
import { clearGitBase, markedFiles, setGitBase } from './lib/gitMarks';
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
import { movedPath, toAbsolutePath, toRelativePath } from './lib/fileTools';
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
import { followAiSettingsRevision, reloadAiSettings } from './lib/aiSettingsStore';
import { dismissBootSplash } from './lib/bootSplash';

let unlistenFSChanges: (() => void) | undefined;

export default function App() {
  const [rootPath, setRootPath] = createSignal<string | null>(null);
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

  // Load last opened project on startup: the folder the command line asked
  // for (`rovik-ide <folder>`, claimed for this window by the backend), else
  // the last project, unless another window has it open (lib/instances.ts).
  // A window another one started ("New window") starts without one.
  onMount(async () => {
    try {
      const startup = await invoke<{ folder: string | null; newWindow: boolean }>(
        'startup_info'
      ).catch(() => ({ folder: null, newWindow: false }));
      if (startup.newWindow && !startup.folder) return;
      const asked = startup.folder;
      const projectData = await getLastProject(asked ?? undefined);
      if (!projectData) {
        if (asked) await openWithItsTabs(asked);
        return;
      }
      const [path, openFiles, expandedPaths, terminalCollapsed] = projectData;
      for (const m of monaco.editor.getModels()) m.dispose();
      tree.presetExpanded(path, expandedPaths);
      setRootPath(path);
      setOpenFiles(openFiles);
      setTerminalCollapsed(terminalCollapsed);
      if (openFiles.length > 0) {
        setActivePath(openFiles[openFiles.length - 1].path);
      }
    } finally {
      setRestoringProject(false);
      dismissBootSplash();
    }
  });
  const [rootId, setRootId] = createSignal<string | null>(null);
  const [openFiles, setOpenFiles] = createSignal<OpenFile[]>([]);
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
  onMount(() => void reloadAiSettings().catch((err) => console.error(err)));
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

  const tree = createFileTreeController(rootPath);

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

  /** The last project, as it was left, if this window may open it; only
   *  `folder` when given (its tabs come back if it was the last). */
  async function getLastProject(
    folder?: string
  ): Promise<[string, OpenFile[], string[], boolean] | null> {
    const lastProject = await loadLastProject();
    if (!lastProject || !lastProject.path) return null;
    if (folder !== undefined && lastProject.path !== folder) return null;
    // Open in another window: this one starts without a project.
    if (!(await claimProject(lastProject.path, { focusOther: false, retries: 2 }))) return null;
    const openedFilesRaw: LastProjectOpenFile[] = lastProject.openedFiles ?? [];
    const openFiles = await reopenFiles(openedFilesRaw);

    return [
      lastProject.path,
      openFiles,
      lastProject.expandedPaths ?? [],
      lastProject.terminalCollapsed,
    ];
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
   * Tabs with unsaved text, before they close (another project opens, or
   * the window closes): saved, or let go, as the user says. False: stay
   * (cancelled, or a save didn't go through). Switching projects used to
   * drop the text without a word, and so did closing the window.
   */
  async function settleUnsavedTabs(leaving: 'project' | 'app'): Promise<boolean> {
    const unsaved = openFiles().filter((f) => f.dirty && !f.readOnly);
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
    return !openFiles().some((f) => f.dirty && !f.readOnly);
  }

  // Rovik's own updates (lib/updates.ts): a look every 30 minutes, and an
  // update's restart, which reopens this window's project, its unsaved
  // tabs saved or let go first, as closing the window does. Then, once a
  // new version runs, the cloud hosts are offered it (lib/hostUpdates.ts).
  onMount(() => {
    void startUpdates({
      folder: () => rootPath(),
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
    // Open in another window (or a copy sharing its chats): that one comes
    // to the front instead (lib/instances.ts).
    if (!(await checkProject(folder))) return;
    if (!(await settleUnsavedTabs('project'))) return;
    // Chosen meanwhile (the dialog was open), or the same one again.
    if (folder === rootPath()) return;
    // Its tabs as the window that had it last left them, this one or
    // another (checked above: none has it now).
    const session = await savedProjectSession(folder);
    // Claimed elsewhere since the check, if it came to that.
    if (!(await claimProject(folder))) return;
    // The project left keeps its tabs for coming back to it.
    void keepTabsNow();
    for (const m of monaco.editor.getModels()) m.dispose();
    // The old project's change marks; its tabs close below.
    for (const { absPath } of markedFiles()) clearGitBase(absPath);
    setSelectedFileIds(new Set<string>());
    setLastSelectedId(null);
    await showWithTabs(folder, session);
  }

  /** `folder` becomes the project shown, its tabs as they were left, if kept. */
  async function showWithTabs(folder: string, session: ProjectSession | null) {
    // Not kept until they're back: the tabs shown meanwhile aren't its.
    setTabsComing(folder);
    try {
      if (session) tree.presetExpanded(folder, session.expanded);
      // Its terminal panel as it was left, with its own terminals
      // (TerminalPanel), or as it is now for a project that has none kept:
      // set together, or the panel open on the project left would start a
      // terminal in this one.
      batch(() => {
        if (session?.terminalOpen !== undefined) setTerminalCollapsed(!session.terminalOpen);
        setRootPath(folder);
      });
      setOpenFiles([]);
      setActivePath(null);
      if (!session?.files.length) return;
      const files = await reopenFiles(session.files);
      // Another project chosen since, or a file opened meanwhile: leave it.
      if (rootPath() !== folder || openFiles().length > 0 || files.length === 0) return;
      setOpenFiles(files);
      setActivePath(
        files.some((f) => f.path === session.active) ? session.active : files[files.length - 1].path
      );
    } finally {
      if (tabsComing() === folder) setTabsComing(null);
    }
  }

  /** A folder asked for at the start that wasn't the last project: its own tabs. */
  async function openWithItsTabs(folder: string) {
    await showWithTabs(folder, await savedProjectSession(folder).catch(() => null));
  }

  // Which project the open folder is, and so whose chats are shown: once
  // the start has chosen the folder, so no other's chats are claimed first.
  createEffect(
    on([rootPath, restoringProject], ([root, restoring]) => {
      if (!restoring) void resolveProject(root);
    })
  );
  // The chats without a project were another window's: maybe they're free
  // now, when this one comes to the front.
  onMount(() => {
    const again = () => {
      if (!rootPath() && chatsBlocked()?.kind === 'no-project-elsewhere') {
        void resolveProject(null);
      }
    };
    window.addEventListener('focus', again);
    onCleanup(() => window.removeEventListener('focus', again));
  });
  onMount(() => onCleanup(rereadAiSettingsOnFocus()));

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
    const root = rootPath();
    if (!root && !isContextPath(path)) return;
    // A written file as its chat has it, when that isn't the project's
    // file: an isolated chat's change that hasn't landed, a remote chat's.
    // Before the project's file is looked for: an isolated chat's new file
    // isn't there at all.
    if (request.dialogId && root && marks !== 'none' && !isContextPath(path)) {
      const rel = toRelativePath(toAbsolutePath(path, root), root);
      if (await openChatVersion(request.dialogId, rel, request)) return;
    }
    const abs = await resolveOpenPath(path, root);
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
    setGitBase(id, compare.fileName, compare.before);
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
    const remaining = openFiles().filter((f) => f.path !== path);
    setOpenFiles(remaining);
    if (activePath() === path) {
      setActivePath(remaining.length ? remaining[remaining.length - 1].path : null);
    }
  });
  // An unreviewed agent edit: when "Open files after agent edits" is on,
  // its file opens in a tab so the changes can be seen. In the background
  // while another file is being worked on, so a long run doesn't keep
  // switching tabs; focused when nothing else is open.
  // Language servers belong to a project: a different one (or none) stops them.
  createEffect(
    on(
      () => rootPath(),
      () => void stopRustSupport(),
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
   * The file a path from the chat names: relative to the project, absolute,
   * or in the active chat's context folder (`@context/…`, on the virtual
   * volume). Null when it can't be one.
   */
  async function resolveOpenPath(path: string, root: string | null): Promise<string | null> {
    if (isContextPath(path)) {
      const dialogId = chatStore.state.activeId;
      return dialogId ? contextRealPath(dialogId, path) : null;
    }
    return root ? toAbsolutePath(path, root) : null;
  }
  // Which paths mentioned in the chat are files in this project (or the
  // agent's context folder).
  setProjectFileResolver(async (path) => {
    const abs = await resolveOpenPath(path, rootPath());
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
      console.warn('[saveActiveFile] no-op', file && {
        path: file.path,
        dirty: file.dirty,
        readOnly: file.readOnly,
        deleted: file.deleted,
      });
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
    setGitBase(virtualPath, file.name, current.content);
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

  function isAtOrUnder(path: string, base: string) {
    if (path === base) return true;
    const trimmed = base.replace(/[\\/]+$/, '');
    return path.startsWith(`${trimmed}/`) || path.startsWith(`${trimmed}\\`);
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
   * for a file outside the project, a binary, or git trouble.
   */
  async function loadGitBase(
    abs: string,
    file: string = toRelativePath(abs, rootPath())
  ): Promise<string | null | undefined> {
    const root = rootPath();
    if (!root || file === abs) return undefined;
    const base = await invoke<string | null>('git_base_content', { path: root, file }).catch(
      () => undefined
    );
    if (base !== undefined) setGitBase(abs, file, base);
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
    if (current && stale.some((f) => f.path === current)) {
      const remaining = openFiles().filter((f) => !(f.readOnly && f.stash));
      setActivePath(remaining.length ? remaining[remaining.length - 1].path : null);
    }
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
    const root = rootPath();
    if (!root) return;
    const run = ++gitRefresh;
    for (const { absPath, file } of markedFiles()) {
      // Stash/commit preview tabs keep their own base commit, not HEAD.
      if (absPath.startsWith('stash@') || absPath.startsWith('commit@')) continue;
      void invoke<string | null>('git_base_content', { path: root, file })
        .then((base) => {
          if (run === gitRefresh) setGitBase(absPath, file, base);
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

    const remaining = openFiles().filter((f) => f.path !== path);
    setOpenFiles(remaining);
    if (activePath() === path) {
      setActivePath(remaining.length ? remaining[remaining.length - 1].path : null);
    }
    // Its model stays (lib/models.ts), the file to the worker from now on:
    // edits thrown away go from it, as the file on disk has none of them.
    if (file?.dirty) void followDisk(path);
  }

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
    // would fail once the folder is gone.
    const selected = effectiveSelectedRows();
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
    const selectedIds = Array.from(selectedFileIds());
    if (selectedIds.length === 0) return;
    setClipboard({ ids: selectedIds, mode: 'copy' });
  }

  function handleCutFile() {
    setContextMenuVisible(false);
    const selectedIds = Array.from(selectedFileIds());
    if (selectedIds.length === 0) return;
    setClipboard({ ids: selectedIds, mode: 'cut' });
  }

  async function handlePasteFile() {
    setContextMenuVisible(false);
    const entry = clipboard();
    const target = firstSelectedRow();
    if (!entry) return;

    const destDirId = target?.isDir ? target.id : rootId();
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
      if (parentPathOf(src.path) === destPath) continue;
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

  function parentPathOf(path: string): string | null {
    const trimmed = path.replace(/[\\/]+$/, '');
    const idx = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
    if (idx < 0) return null;
    return idx === 0 ? trimmed.slice(0, 1) : trimmed.slice(0, idx);
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

  function beginCreate(kind: 'file' | 'folder') {
    setContextMenuVisible(false);
    const folder = creationFolder(firstSelectedRow());
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
    const target = firstSelectedRow();
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

    const selected = selectedFileIds();
    const key = e.key.toLowerCase();

    if (key === 'c' && selected.size > 0) {
      e.preventDefault();
      setClipboard({ ids: Array.from(selected), mode: 'copy' });
    }

    if (key === 'x' && selected.size > 0) {
      e.preventDefault();
      setClipboard({ ids: Array.from(selected), mode: 'cut' });
    }

    if (key === 'v') {
      const entry = clipboard();
      if (entry) {
        e.preventDefault();
        handlePasteFile();
      }
    }

    // Delete with Backspace or Delete key
    if ((key === 'backspace' || key === 'delete') && selected.size > 0) {
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
      else if (!is_dir && isProjectSource(path, rootPath())) updateProjectSource(path);
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
    const root = rootPath();
    const expandedPaths = Array.from(tree.expanded());
    // Read-only preview tabs (stash content) live only for this session.
    const files = openFiles().filter((f) => !f.readOnly);
    const collapsedTerminal = terminalCollapsed();
    if (!root) return;
    saveLastProject(root, files, expandedPaths, collapsedTerminal).catch((err) => {
      console.error('Failed to save last project:', err);
    });
  });

  /** The project's tabs shown now, as kept for coming back to it; none
   *  while they're being opened. Text: it changes when they do, not as
   *  their files are typed in. */
  const tabsNow = createMemo((): string | null => {
    const root = rootPath();
    if (!root || restoringProject() || tabsComing() === root) return null;
    const files: SessionFile[] = openFiles()
      .filter((f) => !f.readOnly)
      .map(({ id, path, name, language }) => ({ id, path, name, language }));
    return JSON.stringify({
      root,
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
    const currentPath = rootPath();
    if (!currentPath) return;
    try {
      await invoke('start_file_watch', { pathStr: currentPath });
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

  createEffect(() => {
    const path = rootPath();
    if (!path) {
      setRootId(null);
      return;
    }
    getPathId(path)
      .then(setRootId)
      .catch((err) => {
        console.error('Failed to resolve root id:', err);
        setRootId(null);
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
                      onOpen={(p) => openProjectAt(p.path)}
                      onRelink={(p) => void relinkProject(p)}
                      onDelete={(p) => void deleteProject(p)}
                    />
                  </Show>
                </div>
              </Show>
            }
          >
            <SearchPanel rootPath={rootPath} onActiveChange={setSearchActive} />
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
              rootPath={rootPath}
              activeFileContext={activeFileContext}
              files={tree.files}
              projectId={() => {
                const p = currentProject();
                return p === undefined ? undefined : (p?.id ?? null);
              }}
            />
          </Show>
        </Show>

        <Show when={sidebarTab() === 'git'}>
          <Switch>
            <Match when={!!rootPath()}>
              <GitPanel
                projectPath={rootPath()!}
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
              />
            </Match>
            <Match when={!rootPath()}>
              <p class="text-center">Open a project to see git panel</p>
            </Match>
          </Switch>
        </Show>
      </Resizable>

      <div class="app-main">
        <div class="tabs-row">
          <Tabs
            files={openFiles}
            activePath={activePath}
            onSelect={(path) => {
              requestEditorFocus(path);
              setActivePath(path);
            }}
            onClose={closeFile}
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
          <Show
            when={activePath()}
            keyed
            fallback={<div class="empty-state">Open a file from the sidebar to start editing</div>}
          >
            {(path) => (
              <Editor
                projectDir={rootPath}
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
          rootPath={rootPath}
          collapsed={terminalCollapsed}
          onCollapsedChange={setTerminalCollapsed}
        />
      </div>

      <Show when={contextMenuVisible() && activePanel() === 'files'}>
        <FileContextMenu
          ref={setMenuEl}
          x={cursorPosition().x}
          y={cursorPosition().y}
          hasSelection={selectedFileIds().size > 0}
          singleSelection={selectedFileIds().size === 1}
          canPaste={!!clipboard()}
          onNewFile={handleNewFile}
          onNewFolder={handleNewFolder}
          onRename={handleRenameFile}
          onCopy={handleCopyFile}
          onCut={handleCutFile}
          onPaste={handlePasteFile}
          onDelete={handleDeleteFile}
        />
      </Show>
      <ConfirmDialogHost />
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
          <SettingsDialog initialPage={page} rootPath={rootPath()} onClose={closeAppSettings} />
        )}
      </Show>
    </div>
  );
}
