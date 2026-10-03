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
import { rustFileSaved, stopRustSupport } from './lib/lsp/rustAnalyzer';
import SearchPanel, { type SearchMatchTarget } from './components/SearchPanel';
import { notifyFileWritten, onFileWritten, onRevealEdit } from './lib/approvals';
import { onAddToChat, onOpenFileRequest, setProjectFileResolver } from './lib/editorBridge';
import { findSnippetLine } from './lib/codeLocation';
import { clearGitBase, markedFiles, setGitBase } from './lib/gitMarks';
import { diffLines, splitLines } from './lib/lineDiff';
import { isProjectSource, updateProjectSource } from './lib/intellisense';
import { clearAgentChanges, onAgentEdit } from './lib/agentChanges';
import { toAbsolutePath } from './lib/fileTools';

let unlistenFSChanges: (() => void) | undefined;

export default function App() {
  const [rootPath, setRootPath] = createSignal<string | null>(null);

  // Load last opened project on startup
  onMount(async () => {
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
  });
  const [rootId, setRootId] = createSignal<string | null>(null);
  const [openFiles, setOpenFiles] = createSignal<OpenFile[]>([]);
  const [activePath, setActivePath] = createSignal<string | null>(null);
  /** The Files tab shows search results instead of the tree while searching. */
  const [searchActive, setSearchActive] = createSignal(false);
  const [revealRequest, setRevealRequest] = createSignal<RevealRequest | null>(null);
  const [sidebarTab, setSidebarTab] = createSignal<'files' | 'chat' | 'git'>('files');
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

    if (draft.depth === 0) {
      return [draftRow, ...rows];
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

  async function handlePickFolder() {
    const folder = await pickProjectFolder();
    if (folder) {
      for (const m of monaco.editor.getModels()) m.dispose();
      setRootPath(folder);
      setOpenFiles([]);
      setActivePath(null);
      setSelectedFileIds(new Set<string>());
      setLastSelectedId(null);
    }
  }

  /** `activate: false` opens it in a tab without switching to it. */
  async function openFile(row: TreeRow, opts: { activate?: boolean } = {}) {
    const activate = opts.activate ?? true;
    // By path too: a file opened from search results may carry a different id.
    const existing = openFiles().find((f) => f.id === row.id || f.path === row.path);
    if (existing) {
      if (activate) setActivePath(existing.path);
      return;
    }
    if (isLikelyBinary(row.name)) {
      window.alert(`"${row.name}" looks like a binary file and can't be opened in the editor.`);
      return;
    }
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

  // The agent's edits: open files to review them, and refresh open tabs
  // after a write (an unsaved tab keeps the user's text).
  const stopReveal = onRevealEdit((path) => void openPath(path));
  const stopWritten = onFileWritten((path, content) =>
    setOpenFiles((prev) =>
      prev.map((f) =>
        f.path === path && !f.dirty ? { ...f, content, originalContent: content } : f
      )
    )
  );
  // An unreviewed agent edit: its file opens in a tab so the changes can be
  // seen. In the background while another file is being worked on, so a
  // long run doesn't keep switching tabs; focused when nothing else is open.
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
    void openPath(path, { activate: activePath() === null });
  });

  // "Open in editor" anywhere in the chat (lib/editorBridge.ts): open the
  // file, then center its start position: the line given, or where the
  // given code is found. Without either, the file just opens.
  const stopOpenRequests = onOpenFileRequest(({ path, line, snippet }) => {
    const root = rootPath();
    if (!root) return;
    const abs = toAbsolutePath(path, root);
    void (async () => {
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
  // Which paths mentioned in the chat are files in this project.
  setProjectFileResolver(async (path) => {
    const root = rootPath();
    if (!root) return null;
    const abs = toAbsolutePath(path, root);
    if (!(await pathExists(abs))) return null;
    return (await dirEntryCount(abs, 1)) === null ? abs : null; // a file, not a folder
  });
  onCleanup(() => setProjectFileResolver(null));
  // "Add to chat" in the editor: bring the chat up; it takes the reference.
  const stopAddToChat = onAddToChat(() => setSidebarTab('chat'));
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
        f.path === path ? { ...f, content, dirty: content !== f.originalContent } : f
      )
    );
  }

  async function saveActiveFile() {
    const file = activeFile();
    // A deleted file is saved even without edits: saving is how it comes back.
    if (!file || (!file.dirty && !file.deleted)) return;
    try {
      await writeFileContent(file.path, file.content);
      // A re-created file is a new inode, so it has a new id.
      const id = file.deleted ? await getPathId(file.path).catch(() => file.id) : file.id;
      setOpenFiles((prev) =>
        prev.map((f) =>
          f.path === file.path
            ? { ...f, id, originalContent: f.content, dirty: false, deleted: false }
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
      console.error(`Failed to save ${file.path}:`, err);
      window.alert(`Couldn't save ${file.name}: ${String(err)}`);
    }
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
    const targets = openFiles().filter((f) => !changedPath || isAtOrUnder(f.path, changedPath));
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

  /** A reverted file: its tab shows the restored text (unless it has unsaved edits). */
  async function reloadFromDisk(abs: string) {
    const content = await readFileContent(abs).catch(() => null);
    if (content !== null) notifyFileWritten(abs, content);
  }

  /** Git's state changed: the marked files' bases may have too. */
  let gitRefresh = 0;
  function refreshGitBases() {
    const root = rootPath();
    if (!root) return;
    const run = ++gitRefresh;
    for (const { absPath, file } of markedFiles()) {
      void invoke<string | null>('git_base_content', { path: root, file })
        .then((base) => {
          if (run === gitRefresh) setGitBase(absPath, file, base);
        })
        .catch(() => clearGitBase(absPath));
    }
  }

  function closeFile(path: string) {
    clearGitBase(path);
    const file = openFiles().find((f) => f.path === path);
    // TODO: show confirmation modal
    if (file?.dirty /*  && !window.confirm(`Discard unsaved changes to ${file.name}?`) */) return;

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
          const newPath = await copyPathOnDisk(id, destDirId);
          console.log('Copied to:', newPath);
        } else {
          const newPath = await movePathOnDisk(id, destDirId);
          setOpenFiles((prev) => prev.map((f) => (f.id === id ? { ...f, path: newPath } : f)));
        }
      }
    } catch (err) {
      console.error('Failed to paste:', err);
      window.alert(String(err));
    }
    if (entry.mode === 'cut') setClipboard(null);
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
          await createFileOnDisk(draft.parentId, name);
        }
      } catch (err) {
        console.error(`Failed to create ${draft.isDir ? 'folder' : 'file'}:`, err);
        window.alert(String(err));
      }
      return;
    }

    setEditingId(null);
    if (!name || name === row.name) return;
    try {
      const newPath = await renamePathOnDisk(row.id, name);
      setOpenFiles((prev) =>
        prev.map((f) => (f.id === row.id ? { ...f, path: newPath, name } : f))
      );
      if (activePath() === row.path) setActivePath(newPath);
    } catch (err) {
      console.error('Failed to rename:', err);
      window.alert(String(err));
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
        case 'create':
          tree.refresh(path);

          if (is_dir) break;

          setPendingOpenPath(path);

          break;
        case 'modify':
        case 'remove':
          tree.refresh(path);
          break;
        case 'unknown':
          tree.refresh();
          break;
      }
      void syncDeletedTabs(operation === 'unknown' ? undefined : path);
      // IntelliSense's copy of the project's sources (lib/intellisense.ts).
      if (!is_dir && isProjectSource(path)) {
        if (operation === 'remove') updateProjectSource(path, null);
        else if (operation === 'create' || operation === 'modify') {
          readFileContent(path)
            .then((content) => updateProjectSource(path, content))
            .catch(() => updateProjectSource(path, null));
        }
      }
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
    const files = openFiles();
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
      console.log('Notifying backend to watch new folder:', currentPath);
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
          </button>
          <button
            classList={{ active: sidebarTab() === 'git' }}
            onClick={() => setSidebarTab('git')}
          >
            Git
          </button>
        </div>

        <Show when={sidebarTab() === 'files'}>
          <div class="sidebar-header">
            <button onClick={handlePickFolder}>Open project...</button>
          </div>
          <Show when={rootPath()} fallback={<div class="empty-state">No project open</div>}>
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
              onToggle={tree.toggle}
              onOpenFile={openFile}
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
          <ChatPanel rootPath={rootPath} activeFileContext={activeFileContext} files={tree.files} />
        </Show>

        <Show when={sidebarTab() === 'git'}>
          <Switch>
            <Match when={!!rootPath()}>
              <GitPanel
                projectPath={rootPath()!}
                onOpenFile={(abs, file) => void openFromGit(abs, file)}
                onFileReverted={(abs) => void reloadFromDisk(abs)}
                onChanged={refreshGitBases}
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
                onSave={saveActiveFile}
                onCursorChange={setCursorInfo}
                reveal={revealRequest}
                onRevealed={() => setRevealRequest(null)}
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
      <NotificationHost />
    </div>
  );
}
