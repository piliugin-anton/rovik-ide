import {
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
import type {
  FileWatchPayload,
  LastProjectOpenFile,
  OpenFile,
  RevealRequest,
  TreeRow,
} from './types';
import { findEnclosingSymbol } from './lib/activeContext';
import type { ActiveFileContext, CursorInfo } from './lib/activeContext';
import Resizable from './components/ResizableDiv';
import TerminalPanel from './components/TerminalPanel';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import FileContextMenu from './components/FileContextMenu';
import { GitPanel } from './components/GitPanel';
import { confirmDialog, ConfirmDialogHost, type ConfirmOptions } from './components/ConfirmDialog';
import NotificationHost from './components/NotificationHost';
import ProjectSwitcher from './components/ProjectSwitcher';
import NewProjectDialog from './components/NewProjectDialog';
import ProjectList from './components/ProjectList';
import { currentProject, projectList, refreshProjects, resolveProject } from './lib/projects';
import { chatApi, type ProjectSummary } from './lib/chatApi';
import { rustFileSaved, stopRustSupport } from './lib/lsp/rustAnalyzer';
import SearchPanel, { type SearchMatchTarget } from './components/SearchPanel';
import { notifyFileWritten, onFileWritten, onRevealEdit } from './lib/approvals';
import { editorSettings } from './lib/editorSettings';
import {
  onAddToChat,
  onCompareRequest,
  onOpenFileRequest,
  setProjectFileResolver,
} from './lib/editorBridge';
import { findSnippetLine } from './lib/codeLocation';
import { isPlanFile, requestImplementPlan } from './lib/plans';
import { clearGitBase, markedFiles, setGitBase } from './lib/gitMarks';
import { diffLines, splitLines } from './lib/lineDiff';
import {
  isProjectSource,
  modelUriFor,
  notifyProjectFileWritten,
  updateProjectSource,
} from './lib/intellisense';
import { scheduleModelPrune, trackModels } from './lib/models';
import { agentChangesFor, clearAgentChanges, onAgentEdit } from './lib/agentChanges';
import { toAbsolutePath } from './lib/fileTools';
import { contextRealPath, isContextPath } from './lib/contextMode';
import { chatStore } from './lib/chatStore';
import { notify } from './lib/notifications';
import {
  startVirtualFiles,
  vfsSettingsRequested,
  clearVfsSettingsRequest,
} from './lib/virtualFilesStartup';
import { initAgentRuns, onShowChat } from './lib/agentRuns';
import AiSettingsModal from './components/AiSettingsModal';
import {
  appSettingsPage,
  closeAppSettings,
  notifyAiSettingsSaved,
  openAppSettings,
} from './lib/appSettings';
import { loadAiSettings, reconcileSelection, saveAiSettings } from './lib/aiSettings';
import { DEFAULT_PERMISSIONS, type AgentPermissions } from './lib/permissions';
import type { ModelSelection, Provider } from './types';
import { dismissBootSplash } from './lib/bootSplash';

let unlistenFSChanges: (() => void) | undefined;

export default function App() {
  const [rootPath, setRootPath] = createSignal<string | null>(null);
  /** Until the last project is restored, the Files tab shows no empty state. */
  const [restoringProject, setRestoringProject] = createSignal(true);

  // Virtual files: mount, or offer to install the driver (in the background).
  onMount(() => void startVirtualFiles());
  // Agent runs live in the backend: follow them from the start, whichever
  // sidebar tab is open (approvals show in the editor too).
  onMount(() => void initAgentRuns());

  // Load last opened project on startup
  onMount(async () => {
    try {
      const projectData = await getLastProject();
      if (!projectData) return;
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
    openFiles();
    scheduleModelPrune();
  });
  const [activePath, setActivePath] = createSignal<string | null>(null);
  /** The Files tab shows search results instead of the tree while searching. */
  const [searchActive, setSearchActive] = createSignal(false);
  const [revealRequest, setRevealRequest] = createSignal<RevealRequest | null>(null);
  const [sidebarTab, setSidebarTab] = createSignal<'files' | 'chat' | 'git'>('files');
  // App-wide Settings (the gear next to the project selector): a virtual
  // files notification's "Show steps" opens its page directly.
  createEffect(() => {
    if (!vfsSettingsRequested()) return;
    clearVfsSettingsRequest();
    openAppSettings('vfs');
  });
  // The Settings dialog's copy of the AI settings, loaded once at startup
  // (ChatPanel keeps its own copy and reloads when the dialog saves).
  const [settingsProviders, setSettingsProviders] = createSignal<Provider[]>([]);
  const [settingsPermissions, setSettingsPermissions] =
    createSignal<AgentPermissions>(DEFAULT_PERMISSIONS);
  onMount(async () => {
    try {
      const settings = await loadAiSettings();
      setSettingsProviders(settings.providers);
      setSettingsPermissions(settings.permissions);
    } catch (err) {
      console.error('Failed to load AI settings for Settings dialog:', err);
    }
  });
  function handleAppSettingsSaved(changes: {
    providers: Provider[];
    permissions: AgentPermissions;
  }) {
    setSettingsProviders(changes.providers);
    setSettingsPermissions(changes.permissions);
    // Keep the chat's current model choice: only the providers/permissions
    // changed here, so reconcile against the saved selection.
    void loadAiSettings()
      .catch(() => null)
      .then((current) => {
        const active: ModelSelection | null = reconcileSelection(
          changes.providers,
          current?.active ?? null
        );
        return saveAiSettings({
          version: 2,
          providers: changes.providers,
          active,
          permissions: changes.permissions,
        });
      })
      .then(() => notifyAiSettingsSaved())
      .catch((err) => console.error('Failed to save AI settings:', err));
  }
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
  const activeFileContext = createMemo<ActiveFileContext | null>(() => {
    const file = activeFile();
    const info = cursorInfo();
    if (!file || !info || info.path !== file.path) return null;
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

  async function getLastProject(): Promise<[string, OpenFile[], string[], boolean] | null> {
    const lastProject = await loadLastProject();
    if (!lastProject || !lastProject.path) return null;
    const openedFilesRaw: LastProjectOpenFile[] = lastProject.openedFiles ?? [];
    // A file deleted while the app was closed is skipped: one missing file
    // used to fail the whole restore.
    const restored = await Promise.all(
      openedFilesRaw.map(async (f): Promise<OpenFile | null> => {
        try {
          const content = await readFileContent(f.path);
          return { ...f, content, originalContent: content, dirty: false };
        } catch (err) {
          console.warn(`Not restoring ${f.path}:`, err);
          return null;
        }
      })
    );
    const openFiles = restored.filter((f): f is OpenFile => f !== null);

    return [
      lastProject.path,
      openFiles,
      lastProject.expandedPaths ?? [],
      lastProject.terminalCollapsed,
    ];
  }

  const [newProjectOpen, setNewProjectOpen] = createSignal(false);

  async function handlePickFolder() {
    const folder = await pickProjectFolder();
    if (folder) openProjectAt(folder);
  }

  /** Opens a project folder: Files, Chat and Git switch to it. */
  function openProjectAt(folder: string) {
    if (folder === rootPath()) return;
    for (const m of monaco.editor.getModels()) m.dispose();
    // The old project's change marks; its tabs close below.
    for (const { absPath } of markedFiles()) clearGitBase(absPath);
    setRootPath(folder);
    setOpenFiles([]);
    setActivePath(null);
    setSelectedFileIds(new Set<string>());
    setLastSelectedId(null);
  }

  // Which project the open folder is, and so whose chats are shown.
  createEffect(on(rootPath, (root) => void resolveProject(root)));

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

  async function openFile(row: TreeRow, opts: { activate?: boolean } = {}) {
    const activate = opts.activate ?? true;
    // By path too: a file opened from search results may carry a different id.
    const existing = openFiles().find((f) => f.id === row.id || f.path === row.path);
    if (existing) {
      if (activate) setActivePath(existing.path);
      return;
    }
    // Two opens at once (two quick writes by the agent): one tab.
    if (opening.has(row.path)) return;
    if (isLikelyBinary(row.name)) {
      window.alert(`"${row.name}" looks like a binary file and can't be opened in the editor.`);
      return;
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
    } catch (err) {
      console.error(`Failed to open ${row.path}:`, err);
      if (activate) window.alert(String(err));
    } finally {
      opening.delete(row.path);
    }
  }

  /** Opens a file by absolute path (e.g. an agent edit to review). */
  async function openPath(path: string, opts: { activate?: boolean } = {}) {
    const sep = path.includes('\\') && !path.includes('/') ? '\\' : '/';
    const name = path.split(sep).pop() ?? path;
    const row: TreeRow = tree.files().find((r) => r.path === path) ?? {
      id: path,
      path,
      name,
      isDir: false,
      depth: 0,
    };
    await openFile(row, opts);
  }

  // The agent's edits: open files to review them when the "Open files
  // after agent edits" editor setting is on, and refresh open tabs
  // after a write (an unsaved tab keeps the user's text).
  const stopReveal = onRevealEdit((path) => {
    if (!editorSettings().openFilesAfterAgentEdits) return;
    void openPath(path);
  });
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
    void openPath(path, { activate: activePath() === null });
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
    // Newly opened by the agent's edit: center the first changed line,
    // like openPathAtLine does for chat links and search results.
    const line = Math.max(1, agentChangesFor(path)?.hunks[0]?.origStart ?? 1);
    void (async () => {
      await openPath(path, { activate: activePath() === null });
      // Background tab (something else is being worked on): not visible,
      // so there is nothing to scroll.
      if (activePath() !== path) return;
      setRevealRequest({ path, line, column: 1, length: 0, nonce: Date.now() });
    })();
  });

  // A comparison tab (an isolated chat's change against the project):
  // read-only, the chat's version shown with what differs marked.
  const stopCompareRequests = onCompareRequest((request) => {
    const tab: OpenFile = {
      id: request.id,
      path: request.id,
      name: request.title,
      content: request.after,
      originalContent: request.after,
      dirty: false,
      language: languageForFile(request.fileName),
      readOnly: true,
    };
    setGitBase(request.id, request.fileName, request.before);
    setOpenFiles((prev) => [...prev.filter((f) => f.path !== request.id), tab]);
    setActivePath(request.id);
  });
  onCleanup(stopCompareRequests);

  // "Open in editor" anywhere in the chat (lib/editorBridge.ts): open the
  // file, then center its start position: the line given, or where the
  // given code is found. Without either, the file just opens.
  const stopOpenRequests = onOpenFileRequest(({ path, line, snippet }) => {
    const root = rootPath();
    if (!root && !isContextPath(path)) return;
    void (async () => {
      const abs = await resolveOpenPath(path, root);
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
      let target = line;
      if (target === undefined && snippet) {
        // An open tab's text, unsaved edits included; else the file on disk.
        const text =
          openFiles().find((f) => f.path === abs)?.content ??
          (await readFileContent(abs).catch(() => null));
        target = text !== null ? (findSnippetLine(text, snippet) ?? undefined) : undefined;
      }
      if (target === undefined) await openPath(abs);
      else await openPathAtLine(abs, target);
    })();
  });
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
    stopReveal();
    stopWritten();
    stopOpenRequests();
    stopAddToChat();
    stopAgentEdits();
  });

  async function openPathAtLine(path: string, line: number) {
    await openPath(path);
    // The tab may already be open under the same path; openFile activated it.
    if (activePath() !== path) return;
    setRevealRequest({ path, line, column: 1, length: 0, nonce: Date.now() });
  }

  /** Opens a search result with the match selected. */
  async function openSearchMatch(target: SearchMatchTarget) {
    const root = rootPath();
    if (!root) return;
    const sep = root.includes('\\') && !root.includes('/') ? '\\' : '/';
    const relative = target.relativePath.replace(/[\\/]/g, sep);
    const path = `${root.replace(/[\\/]+$/, '')}${sep}${relative}`;
    const name = relative.split(sep).pop() ?? relative;

    const row: TreeRow = tree.files().find((r) => r.path === path) ?? {
      id: path,
      path,
      name,
      isDir: false,
      depth: 0,
    };
    await openFile(row);
    if (activePath() === path) {
      setRevealRequest({
        path,
        line: target.line,
        column: target.column,
        length: target.length,
        nonce: Date.now(),
      });
    }
  }

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
    if (!file || file.readOnly || (!file.dirty && !file.deleted)) return;
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

  /*
   * From the Git tab: open a changed file with its changes marked against
   * git (lib/gitMarks.ts), centered on the first change.
   */
  async function openFromGit(abs: string, file: string) {
    const root = rootPath();
    if (!root) return;
    const base = await invoke<string | null>('git_base_content', { path: root, file }).catch(
      () => undefined // binary, or git trouble: open without marks
    );
    if (base !== undefined) setGitBase(abs, file, base);
    const current =
      openFiles().find((f) => f.path === abs)?.content ??
      (await readFileContent(abs).catch(() => null));
    const first =
      base !== undefined && current !== null
        ? diffLines(splitLines(base ?? '').lines, splitLines(current).lines)[0]
        : undefined;
    if (first) await openPathAtLine(abs, first.origStart);
    else await openPath(abs);
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
    let preview: import('./types').GitStashFilePreview;
    try {
      preview = await invoke<import('./types').GitStashFilePreview>('git_stash_file_preview', {
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
    const virtualPath = `stash@${index}:${abs}`;
    const existing = openFiles().find((f) => f.path === virtualPath);
    if (existing) {
      setActivePath(existing.path);
      return;
    }
    const shortName = file.split('/').pop() ?? file;
    const stashFile: OpenFile = {
      id: virtualPath,
      path: virtualPath,
      name: `${shortName} (stash@${index})`,
      content: preview.content,
      originalContent: preview.content,
      dirty: false,
      language: languageForFile(shortName),
      readOnly: true,
      stash: { index, file, message },
    };
    setGitBase(virtualPath, file, preview.base ?? '');
    setOpenFiles((prev) => [...prev, stashFile]);
    setActivePath(virtualPath);
    const first = diffLines(
      splitLines(preview.base ?? '').lines,
      splitLines(preview.content).lines
    )[0];
    if (first) {
      setRevealRequest({
        path: virtualPath,
        line: first.origStart,
        column: 1,
        length: 0,
        nonce: Date.now(),
      });
    }
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
    const short = oid.slice(0, 7);
    const virtualPath = `commit@${oid}:${abs}`;
    const existing = openFiles().find((f) => f.path === virtualPath);
    if (existing) {
      setActivePath(existing.path);
      return;
    }
    const shortName = file.split('/').pop() ?? file;
    const commitFile: OpenFile = {
      id: virtualPath,
      path: virtualPath,
      name: `${shortName} (${short})`,
      content: preview.content,
      originalContent: preview.content,
      dirty: false,
      language: languageForFile(shortName),
      readOnly: true,
      commit: { oid, file, summary },
    };
    setGitBase(virtualPath, file, preview.base ?? '');
    setOpenFiles((prev) => [...prev, commitFile]);
    setActivePath(virtualPath);
    const first = diffLines(
      splitLines(preview.base ?? '').lines,
      splitLines(preview.content).lines
    )[0];
    if (first) {
      setRevealRequest({
        path: virtualPath,
        line: first.origStart,
        column: 1,
        length: 0,
        nonce: Date.now(),
      });
    }
  }

  /**
   * "Implement this plan" over a plan file: saves it, then hands its text to
   * the chat, which starts the run in Agent mode (lib/plans.ts).
   */
  async function implementOpenPlan() {
    const file = activeFile();
    if (!file || !isPlanFile(file.path)) return;
    if (file.dirty) await saveActiveFile();
    setSidebarTab('chat');
    requestImplementPlan({ path: file.path, content: file.content });
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

    const remaining = openFiles().filter((f) => f.path !== path);
    setOpenFiles(remaining);
    if (activePath() === path) {
      setActivePath(remaining.length ? remaining[remaining.length - 1].path : null);
    }
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
          const oldPath = openFiles().find((f) => f.id === id)?.path;
          const newPath = await movePathOnDisk(id, destDirId);
          setOpenFiles((prev) => prev.map((f) => (f.id === id ? { ...f, path: newPath } : f)));
          if (oldPath && activePath() === oldPath) setActivePath(newPath);
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
      // Tabs at or inside a moved path follow it to the new location.
      setOpenFiles((prev) =>
        prev.map((f) => {
          const m = moved.find((m) => f.path === m.from || isAtOrUnder(f.path, m.from));
          if (!m) return f;
          const rest = f.path.slice(m.from.length).replace(/^[\\/]/, '');
          const path = rest ? `${m.to}/${rest}` : m.to;
          const name = path.split(/[\\/]/).pop() ?? f.name;
          return { ...f, path, name, language: languageForFile(name) };
        })
      );
      if (activePath()) {
        const m = moved.find((m) => activePath() === m.from || isAtOrUnder(activePath()!, m.from));
        if (m) {
          const rest = activePath()!
            .slice(m.from.length)
            .replace(/^[\\/]/, '');
          setActivePath(rest ? `${m.to}/${rest}` : m.to);
        }
      }
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
      setOpenFiles((prev) =>
        prev.map((f) =>
          f.id === row.id ? { ...f, path: newPath, name, language: languageForFile(name) } : f
        )
      );
      if (activePath() === row.path) setActivePath(newPath);
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
      void syncDeletedTabs(operation === 'unknown' ? undefined : path);
      // Changed outside the editor (another program, a build, git): a tab
      // without unsaved edits shows the new text. Our own saves read back
      // the same text and change nothing.
      if (!is_dir && (operation === 'create' || operation === 'modify')) {
        void refreshCleanTab(path);
      }
      // IntelliSense's copy of the project's sources (lib/intellisense.ts).
      // Batched there: read and applied together once changes settle.
      if (!is_dir && isProjectSource(path)) {
        if (operation === 'remove') updateProjectSource(path, null);
        else if (operation === 'create' || operation === 'modify') updateProjectSource(path);
      }
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
    openFile(row);
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
    <div class="app" onContextMenu={handleContextMenu}>
      <Resizable class="sidebar" initialWidth={380} minWidth={320} maxWidth={480}>
        <div class="sidebar-topbar">
          <ProjectSwitcher
            onOpenFolder={() => void handlePickFolder()}
            onNewProject={() => setNewProjectOpen(true)}
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
            <SearchPanel
              rootPath={rootPath}
              onOpenMatch={(target) => void openSearchMatch(target)}
              onActiveChange={setSearchActive}
            />
          </Show>
          <Show when={rootPath() && !searchActive()}>
            <FileTree
              forwardRef={setFileTreeEl}
              rows={displayRows}
              expanded={tree.expanded}
              activePath={activePath}
              selectedIds={selectedFileIds}
              onToggle={tree.toggle}
              onOpenFile={openFile}
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

        <Show when={sidebarTab() === 'git'}>
          <Switch>
            <Match when={!!rootPath()}>
              <GitPanel
                projectPath={rootPath()!}
                onOpenFile={(abs, file) => void openFromGit(abs, file)}
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
        <Tabs
          files={openFiles}
          activePath={activePath}
          onSelect={setActivePath}
          onClose={closeFile}
        />
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
                content={() => openFiles().find((f) => f.path === path)?.content ?? ''}
                language={() => openFiles().find((f) => f.path === path)?.language ?? 'plaintext'}
                onChange={(value) => updateContent(path, value)}
                onSave={() => saveActiveFile()}
                onCursorChange={setCursorInfo}
                reveal={revealRequest}
                onRevealed={() => setRevealRequest(null)}
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
          <AiSettingsModal
            providers={settingsProviders()}
            permissions={settingsPermissions()}
            initialPage={page}
            rootPath={rootPath()}
            onSave={handleAppSettingsSaved}
            onClose={closeAppSettings}
          />
        )}
      </Show>
    </div>
  );
}
