import { createSignal, createMemo, Show, onMount, createEffect, onCleanup } from 'solid-js';
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
  getPathId,
  createFile as createFileOnDisk,
  createFolder as createFolderOnDisk,
  deletePath as deletePathOnDisk,
  renamePath as renamePathOnDisk,
  movePath as movePathOnDisk,
  copyPath as copyPathOnDisk,
} from './lib/commands';
import type { FileWatchPayload, OpenFile, TreeRow } from './types';
import { findEnclosingSymbol } from './lib/activeContext';
import type { ActiveFileContext } from './lib/activeContext';
import Resizable from './components/ResizableDiv';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import ContextMenuItem from './components/ContextMenuItem';

let unlistenFSChanges: (() => void) | undefined;

export default function App() {
  const [rootPath, setRootPath] = createSignal<string | null>(null);
  const [rootId, setRootId] = createSignal<string | null>(null);
  const [openFiles, setOpenFiles] = createSignal<OpenFile[]>([]);
  const [activePath, setActivePath] = createSignal<string | null>(null);
  const [sidebarTab, setSidebarTab] = createSignal<'files' | 'chat'>('files');
  const [cursorInfo, setCursorInfo] = createSignal<{
    line: number;
    column: number;
    selectionStartLine: number | null;
    selectionEndLine: number | null;
  } | null>(null);

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
    if (!file || !info) return null;
    return {
      path: file.path,
      language: file.language,
      line: info.line,
      column: info.column,
      enclosingSymbol: findEnclosingSymbol(file.content, info.line, file.language),
      selectionStartLine: info.selectionStartLine,
      selectionEndLine: info.selectionEndLine,
    };
  });

  const effectiveSelectedRows = createMemo<TreeRow[]>(() => {
    const selectedIds = selectedFileIds();
    const rows = tree.rows();
    return rows.filter((r) => selectedIds.has(r.id));
  });

  const newFolderContextMenuItemDisabled = createMemo(() => {
    const selected = selectedFileIds();
    if (selected.size > 1) return true;
    const target = tree.rows().find((r) => selected.has(r.id));
    if (!target || target.isDir) return false;
    return true;
  });

  const firstSelectedRow = createMemo<TreeRow | null>(() => {
    const rows = effectiveSelectedRows();
    return rows.length > 0 ? rows[0] : null;
  });

  async function handlePickFolder() {
    const folder = await pickProjectFolder();
    if (folder) {
      setRootPath(folder);
      setOpenFiles([]);
      setActivePath(null);
      setSelectedFileIds(new Set<string>());
      setLastSelectedId(null);
    }
  }

  async function openFile(row: TreeRow) {
    const existing = openFiles().find((f) => f.id === row.id);
    if (existing) {
      setActivePath(existing.path);
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
      setActivePath(row.path);
    } catch (err) {
      console.error(`Failed to open ${row.path}:`, err);
      window.alert(String(err));
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
    if (!file || !file.dirty) return;
    try {
      await writeFileContent(file.path, file.content);
      setOpenFiles((prev) =>
        prev.map((f) =>
          f.path === file.path ? { ...f, originalContent: f.content, dirty: false } : f
        )
      );
    } catch (err) {
      console.error(`Failed to save ${file.path}:`, err);
    }
  }

  function closeFile(path: string) {
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

  async function handleDeleteFile() {
    setContextMenuVisible(false);
    const selectedRows = effectiveSelectedRows();
    if (selectedRows.length === 0) return;

    const names = selectedRows.map((r) => r.name).join(', ');
    const count = selectedRows.length;
    const message =
      count === 1
        ? `Delete "${names}"? This can't be undone.`
        : `Delete ${count} items?\n\n${names}\n\nThis can't be undone.`;

    // TODO: show confirmation modal
    //if (!window.confirm(message)) return;

    try {
      for (const row of selectedRows) {
        await deletePathOnDisk(row.id);
        const openTarget = openFiles().find((f) => f.id === row.id);
        if (openTarget && activePath() === openTarget.path) setActivePath(null);
      }
      setOpenFiles((prev) => prev.filter((f) => !selectedFileIds().has(f.id)));
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

  function beginCreate(kind: 'file' | 'folder') {
    setContextMenuVisible(false);
    const target = firstSelectedRow();
    const parentId = target?.isDir ? target.id : rootId();
    if (!parentId) return;

    const parentPath = target?.isDir ? target.path : rootPath();
    const parentDepth = target?.isDir ? target.depth : -1;

    if (target?.isDir && !tree.expanded().has(target.path)) {
      tree.toggle(target);
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

  onMount(() => {
    window.addEventListener('click', closeContextMenu);
    window.addEventListener('keydown', handleKeyDown);

    onCleanup(() => {
      window.removeEventListener('click', closeContextMenu);
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
      console.log(`Target is a ${is_dir ? 'Folder' : 'File'}`, path, operation);
      switch (operation) {
        case 'create':
        case 'modify':
        case 'remove':
          tree.refresh(path);
          break;
        case 'unknown':
          tree.refresh();
          break;
      }
    });
  });

  onCleanup(() => {
    if (unlistenFSChanges) {
      unlistenFSChanges();
      unlistenFSChanges = undefined;
    }
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

  //  onContextMenu={handleContextMenu}

  return (
    <div class="app">
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
        </div>

        <Show when={sidebarTab() === 'files'}>
          <div class="sidebar-header">
            <button onClick={handlePickFolder}>Open project...</button>
          </div>
          <Show when={rootPath()} fallback={<div class="empty-state">No project open</div>}>
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
            />
          </Show>
        </Show>

        <Show when={sidebarTab() === 'chat'}>
          <ChatPanel rootPath={rootPath} activeFileContext={activeFileContext} />
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
              />
            )}
          </Show>
        </div>
      </div>

      <Show when={contextMenuVisible() && activePanel() !== null}>
        <div
          ref={setMenuEl}
          class="context-menu"
          style={{
            position: 'fixed',
            top: `${cursorPosition().y}px`,
            left: `${cursorPosition().x}px`,
          }}
        >
          <ul class="context-menu-list">
            <Show when={activePanel() === 'files'}>
              <ContextMenuItem
                label={
                  <>
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    >
                      <path d="M4 1.5h5.5L12.5 4.5V14.5H4V1.5Z" stroke-linejoin="round" />
                      <path d="M9.5 1.5V4.5H12.5" stroke-linejoin="round" />
                      <path d="M8 7.5V11.5M6 9.5H10" stroke-linecap="round" />
                    </svg>
                    New File
                  </>
                }
                onClick={handleNewFile}
              />

              <ContextMenuItem
                label={
                  <>
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    >
                      <path
                        d="M2 4.5a1 1 0 0 1 1-1h3.5l1.2 1.5H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z"
                        stroke-linejoin="round"
                      />
                      <path d="M8 8v3.5M6.25 9.75h3.5" stroke-linecap="round" />
                    </svg>
                    New Folder
                  </>
                }
                onClick={handleNewFolder}
                disabled={newFolderContextMenuItemDisabled()}
              />

              <ContextMenuItem
                label={
                  <>
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    >
                      <path
                        d="M3 4.5h10M6.5 4.5V3a1 1 0 0 1 1-1h1a1 1 0 0 1 1 1v1.5"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                      />
                      <path
                        d="M4.5 4.5 5 13.5a1 1 0 0 0 1 1h4a1 1 0 0 0 1-1l.5-9"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                      />
                      <path d="M6.5 7v5M9.5 7v5" stroke-linecap="round" />
                    </svg>
                    Delete
                  </>
                }
                isDanger
                disabled={selectedFileIds().size === 0}
                onClick={handleDeleteFile}
              />

              <ContextMenuItem
                label={
                  <>
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    >
                      <path
                        d="M11 2.5 13.5 5 5.5 13 2.5 13.5 3 10.5 11 2.5Z"
                        stroke-linejoin="round"
                      />
                    </svg>
                    Rename
                  </>
                }
                disabled={selectedFileIds().size !== 1}
                onClick={handleRenameFile}
              />

              <ContextMenuItem
                label={
                  <>
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    >
                      <rect x="5.5" y="5.5" width="8" height="8" rx="1" stroke-linejoin="round" />
                      <path
                        d="M2.5 10.5V3a1 1 0 0 1 1-1H10"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                      />
                    </svg>
                    Copy
                  </>
                }
                disabled={selectedFileIds().size === 0}
                onClick={handleCopyFile}
              />

              <ContextMenuItem
                label={
                  <>
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    >
                      <path
                        d="M6 2.5h4a.5.5 0 0 1 .5.5v1H5.5V3a.5.5 0 0 1 .5-.5Z"
                        stroke-linejoin="round"
                      />
                      <path
                        d="M4 4.5h8a1 1 0 0 1 1 1V13a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5.5a1 1 0 0 1 1-1Z"
                        stroke-linejoin="round"
                      />
                    </svg>
                    Paste
                  </>
                }
                disabled={!clipboard()}
                onClick={handlePasteFile}
              />

              <ContextMenuItem
                label={
                  <>
                    <svg
                      width="14"
                      height="14"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                    >
                      <circle cx="4.5" cy="4.5" r="1.5" />
                      <circle cx="4.5" cy="11.5" r="1.5" />
                      <path d="M13 3 5.7 9M13 13 5.7 7" stroke-linecap="round" />
                    </svg>
                    Cut
                  </>
                }
                disabled={selectedFileIds().size === 0}
                onClick={handleCutFile}
              />
            </Show>
          </ul>
        </div>
      </Show>
    </div>
  );
}
