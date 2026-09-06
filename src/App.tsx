import { createSignal, createMemo, Show, onMount, createEffect, onCleanup } from 'solid-js';
import FileTree from './components/FileTree';
import Tabs from './components/Tabs';
import Editor from './components/Editor';
import ChatPanel from './components/ChatPanel';
import { createFileTreeController } from './lib/fileTree';
import { languageForFile, isLikelyBinary } from './lib/language';
import { pickProjectFolder, readFileContent, writeFileContent } from './lib/commands';
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
  const [clipboard, setClipboard] = createSignal<{ path: string; mode: 'copy' | 'cut' } | null>(
    null
  );
  const [contextMenuTarget, setContextMenuTarget] = createSignal<TreeRow | null>(null);

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
    const existing = openFiles().find((f) => f.path === row.path);
    if (existing) {
      setActivePath(row.path);
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

  function findRowAtPoint(x: number, y: number): TreeRow | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const rowEl = el?.closest<HTMLElement>('[data-file-id]');
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
      const rect = fileTree!.getBoundingClientRect();
      if (posX >= rect.left && posX <= rect.right && posY >= rect.top && posY <= rect.bottom) {
        showContextMenu = true;
        setActivePanel('files');
        setContextMenuTarget(findRowAtPoint(posX, posY));
      }
    }

    if (!showContextMenu) {
      setActivePanel(null);
      setContextMenuVisible(false);
      setContextMenuTarget(null);
      return;
    }

    setContextMenuVisible(true);
    setRawCursor({ x: posX, y: posY });
  }

  const closeContextMenu = () => {
    setContextMenuVisible(false);
    setContextMenuTarget(null);
  };

  function handleNewFile() {
    const target = contextMenuTarget();
    console.log('TODO: create file in', target?.isDir ? target.path : rootPath());
  }

  function handleNewFolder() {
    const target = contextMenuTarget();
    console.log('TODO: create folder in', target?.isDir ? target.path : rootPath());
  }

  function handleDeleteFile() {
    const target = contextMenuTarget();
    if (!target) return;
    console.log('TODO: delete', target);
  }

  function handleRenameFile() {
    const target = contextMenuTarget();
    if (!target) return;
    console.log('TODO: rename', target);
  }

  function handleCopyFile() {
    const target = contextMenuTarget();
    if (!target) return;
    setClipboard({ path: target.path, mode: 'copy' });
  }

  function handleCutFile() {
    const target = contextMenuTarget();
    if (!target) return;
    setClipboard({ path: target.path, mode: 'cut' });
  }

  function handlePasteFile() {
    const entry = clipboard();
    const target = contextMenuTarget();
    if (!entry || !target) return;
    console.log('TODO: paste', entry, 'into', target.isDir ? target.path : rootPath());
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
        case 'unknown':
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
