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
  const [clipboard, setClipboard] = createSignal<{ id: string; mode: 'copy' | 'cut' } | null>(null);
  const [contextMenuTarget, setContextMenuTarget] = createSignal<TreeRow | null>(null);
  const [selectedRow, setSelectedRow] = createSignal<HTMLDivElement | null>(null); // tree selection

  // The project root doesn't come through read_dir_entries on its own (it's
  // the thing read_dir_entries is called *with*), so resolve its stable id
  // separately whenever it changes — "new file/folder" at the top level
  // needs a directory id, same as everything else.
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

  const tree = createFileTreeController(rootPath);

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

  const newFolderContextMenuItemDisabled = createMemo(() => {
    const target = contextMenuTarget();
    if (!target || target.isDir) {
      return false;
    }

    return true;
  });

  async function handlePickFolder() {
    const folder = await pickProjectFolder();
    if (folder) {
      setRootPath(folder);
      setOpenFiles([]);
      setActivePath(null);
    }
  }

  async function openFile(row: TreeRow) {
    const existing = openFiles().find((f) => f.id === row.id);
    if (existing) {
      // Reuse the casing already stored for this file rather than row.path,
      // so activePath never ends up holding two different casings for the
      // same underlying file on case-insensitive filesystems.
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
    if (file?.dirty && !window.confirm(`Discard unsaved changes to ${file.name}?`)) return;

    const remaining = openFiles().filter((f) => f.path !== path);
    setOpenFiles(remaining);
    if (activePath() === path) {
      setActivePath(remaining.length ? remaining[remaining.length - 1].path : null);
    }
  }

  function findRowAtPoint(x: number, y: number): [TreeRow, HTMLDivElement] | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const rowEl = el?.closest<HTMLDivElement>('[data-file-id]');
    const id = rowEl?.dataset.fileId;
    if (!id) return null;
    const row = tree.rows().find((r) => r.id === id);
    return row && !row.loading ? [row, rowEl] : null;
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

        const rowData = findRowAtPoint(posX, posY);
        if (rowData) {
          const [row, rowEl] = rowData;
          setContextMenuTarget(row);
          setSelectedRow(rowEl);

          rowEl.classList.add('selected');
        } else {
          setContextMenuTarget(null);
          setSelectedRow(null);
        }
      }
    }

    if (!showContextMenu) {
      setActivePanel(null);
      setContextMenuVisible(false);
      setContextMenuTarget(null);
      setSelectedRow(null);
      return;
    }

    setContextMenuVisible(true);
    setRawCursor({ x: posX, y: posY });
  }

  const closeContextMenu = () => {
    const selectedRowElement = selectedRow();
    if (selectedRowElement) {
      selectedRowElement.classList.remove('selected');
    }
    setContextMenuVisible(false);
    setContextMenuTarget(null);
    setSelectedRow(null);
  };

  async function handleNewFile() {
    setContextMenuVisible(false);
    const target = contextMenuTarget();
    const dirId = target?.isDir ? target.id : rootId();
    if (!dirId) return;
    const name = window.prompt('New file name:');
    if (!name) return;
    try {
      await createFileOnDisk(dirId, name);
    } catch (err) {
      console.error('Failed to create file:', err);
      window.alert(String(err));
    }
  }

  async function handleNewFolder() {
    setContextMenuVisible(false);
    const target = contextMenuTarget();
    const dirId = target?.isDir ? target.id : rootId();
    if (!dirId) return;
    const name = window.prompt('New folder name:');
    if (!name) return;
    try {
      await createFolderOnDisk(dirId, name);
    } catch (err) {
      console.error('Failed to create folder:', err);
      window.alert(String(err));
    }
  }

  async function handleDeleteFile() {
    setContextMenuVisible(false);
    const target = contextMenuTarget();
    if (!target) return;
    if (!window.confirm(`Delete "${target.name}"? This can't be undone.`)) return;
    try {
      await deletePathOnDisk(target.id);
      const openTarget = openFiles().find((f) => f.id === target.id);
      setOpenFiles((prev) => prev.filter((f) => f.id !== target.id));
      if (openTarget && activePath() === openTarget.path) setActivePath(null);
    } catch (err) {
      console.error('Failed to delete:', err);
      window.alert(String(err));
    }
  }

  async function handleRenameFile() {
    setContextMenuVisible(false);
    const target = contextMenuTarget();
    if (!target) return;
    const name = window.prompt('Rename to:', target.name);
    if (!name || name === target.name) return;
    try {
      const newPath = await renamePathOnDisk(target.id, name);
      const openTarget = openFiles().find((f) => f.id === target.id);
      setOpenFiles((prev) =>
        prev.map((f) => (f.id === target.id ? { ...f, path: newPath, name } : f))
      );
      if (openTarget && activePath() === openTarget.path) setActivePath(newPath);
    } catch (err) {
      console.error('Failed to rename:', err);
      window.alert(String(err));
    }
  }

  function handleCopyFile() {
    setContextMenuVisible(false);
    const target = contextMenuTarget();
    if (!target) return;
    setClipboard({ id: target.id, mode: 'copy' });
  }

  function handleCutFile() {
    setContextMenuVisible(false);
    const target = contextMenuTarget();
    if (!target) return;
    setClipboard({ id: target.id, mode: 'cut' });
  }

  async function handlePasteFile() {
    setContextMenuVisible(false);
    const entry = clipboard();
    const target = contextMenuTarget();
    if (!entry || !target) return;
    const destDirId = target.isDir ? target.id : rootId();
    if (!destDirId) return;
    try {
      if (entry.mode === 'copy') {
        await copyPathOnDisk(entry.id, destDirId);
      } else {
        const newPath = await movePathOnDisk(entry.id, destDirId);
        setOpenFiles((prev) => prev.map((f) => (f.id === entry.id ? { ...f, path: newPath } : f)));
      }
    } catch (err) {
      console.error('Failed to paste:', err);
      window.alert(String(err));
    }
    if (entry.mode === 'cut') setClipboard(null);
  }

  onMount(() => {
    window.addEventListener('click', closeContextMenu);
    onCleanup(() => {
      window.removeEventListener('click', closeContextMenu);
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

  // Reflect the current context-menu target as a `.selected` class on its
  // row element, without needing FileTree to know about context-menu state.
  let selectedRowEl: HTMLElement | null = null;
  createEffect(() => {
    const target = contextMenuTarget();
    const container = fileTreeEl();

    if (selectedRowEl) {
      selectedRowEl.classList.remove('selected');
      selectedRowEl = null;
    }

    if (!target || !container) return;

    const el = container.querySelector<HTMLElement>(`[data-file-id="${target.id}"]`);
    if (el) {
      el.classList.add('selected');
      selectedRowEl = el;
    }
  });

  // Bullet-proof positioning effect with logs
  createEffect(() => {
    const visible = contextMenuVisible();
    const panel = activePanel();
    const menu = menuEl();

    if (!visible || panel === null) {
      return;
    }

    if (!menu) {
      // Will re-run when menuEl changes
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
        </div>

        <Show when={sidebarTab() === 'files'}>
          <div class="sidebar-header">
            <button onClick={handlePickFolder}>Open Folder...</button>
          </div>
          <Show when={rootPath()} fallback={<div class="empty-state">No folder open</div>}>
            <FileTree
              forwardRef={setFileTreeEl}
              rows={tree.rows}
              expanded={tree.expanded}
              activePath={activePath}
              onToggle={tree.toggle}
              onOpenFile={openFile}
            />
          </Show>
        </Show>

        <Show when={sidebarTab() === 'chat'}>
          <ChatPanel rootPath={rootPath} activeFileContext={activeFileContext} />
        </Show>
      </Resizable>

      <div class="main">
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
                disabled={!contextMenuTarget()}
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
                disabled={!contextMenuTarget()}
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
                disabled={!contextMenuTarget()}
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
                disabled={!contextMenuTarget()}
                onClick={handleCutFile}
              />
            </Show>
          </ul>
        </div>
      </Show>
    </div>
  );
}
