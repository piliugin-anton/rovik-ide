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
  // 1. Reactive state for menu visibility and viewport position
  const [contextMenuVisible, setContextMenuVisible] = createSignal(false);
  const [cursorPosition, setCursorPosition] = createSignal({ x: 0, y: 0 });
  const [activePanel, setActivePanel] = createSignal<'left' | 'right' | null>(null);
  let leftPanelRef: HTMLDivElement | undefined;
  let rightPanelRef: HTMLDivElement | undefined;
  let menuRef: HTMLDivElement | undefined;

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

  function handleContextMenu(e: MouseEvent) {
    e.preventDefault(); // Blocks the default browser right-click menu
    setContextMenuVisible(true);

    let posX = e.clientX;
    let posY = e.clientY;

    // Cross-browser structural hit testing via bounding boxes
    if (leftPanelRef && rightPanelRef) {
      const leftRect = leftPanelRef.getBoundingClientRect();
      const rightRect = rightPanelRef.getBoundingClientRect();

      // Check if cursor falls within the left panel bounds
      if (
        posX >= leftRect.left &&
        posX <= leftRect.right &&
        posY >= leftRect.top &&
        posY <= leftRect.bottom
      ) {
        setActivePanel('left');
      }
      // Check if cursor falls within the right panel bounds
      else if (
        posX >= rightRect.left &&
        posX <= rightRect.right &&
        posY >= rightRect.top &&
        posY <= rightRect.bottom
      ) {
        setActivePanel('right');
      } else {
        setActivePanel(null); // Clicked in dead-zone or margins
      }
    }

    // Prevent boundary overflow
    let adjustedX = posX;
    let adjustedY = posY;
    if (menuRef) {
      const menuWidth = menuRef.offsetWidth || 160;
      const menuHeight = menuRef.offsetHeight || 150;

      if (posX + menuWidth > window.innerWidth) adjustedX = window.innerWidth - menuWidth - 10;
      if (posY + menuHeight > window.innerHeight) adjustedY = window.innerHeight - menuHeight - 10;
    }

    setCursorPosition({ x: adjustedX, y: adjustedY });

    setCursorPosition({ x: posX, y: posY });
  }

  const closeContextMenu = () => setContextMenuVisible(false);

  onMount(async () => {
    window.addEventListener('click', closeContextMenu);

    const unlistenFSChanges = await listen<FileWatchPayload>('fs-changes', (event) => {
      const { path, operation, is_dir } = event.payload;

      console.log(`Target is a ${is_dir ? 'Folder' : 'File'}`, path, operation);

      switch (operation) {
        case 'create':
          // Handle adding item to your Solid.js file tree state
          break;
        case 'modify':
          // If the file is currently open in an IDE tab, reload/prompt to save
          break;
        case 'remove':
          // Remove from UI file tree and close open tabs matching this path
          break;
        case 'unknown':
          break;
      }
    });

    onCleanup(() => {
      window.removeEventListener('click', closeContextMenu);
      unlistenFSChanges();
    });
  });

  createEffect(async () => {
    const currentPath = rootPath();

    // If no folder is open, don't ping the backend watcher
    if (!currentPath) return;

    try {
      console.log('Notifying backend to watch new folder:', currentPath);
      await invoke('start_file_watch', { pathStr: currentPath });
    } catch (error) {
      console.error('Failed to start file watcher:', error);
    }
  });

  return (
    <div class="app" onContextMenu={handleContextMenu}>
      <Resizable
        forwardRef={(el) => (leftPanelRef = el)}
        class="sidebar"
        initialWidth={380}
        minWidth={320}
        maxWidth={480}
      >
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

      <div class="main" ref={rightPanelRef}>
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
          ref={menuRef}
          style={{
            position: 'fixed',
            top: `${cursorPosition().y}px`,
            left: `${cursorPosition().x}px`,
            background: '#2a2a32',
            border: '1px solid #444',
            'border-radius': '6px',
            padding: '5px 0',
            'box-shadow': '0px 4px 12px rgba(0, 0, 0, 0.3)',
            'z-index': 9999,
            'min-width': '160px',
          }}
        >
          <ul style={{ 'list-style': 'none', margin: 0, padding: 0 }}>
            {/* 4. Conditional Menu Rendering based on side detected */}
            <Show when={activePanel() === 'left'}>
              <ContextMenuItem label="⬅️ Left Option A" onClick={() => alert('Left side click')} />
              <ContextMenuItem label="⬅️ Left Option B" onClick={() => alert('Left side action')} />
            </Show>

            <Show when={activePanel() === 'right'}>
              <ContextMenuItem
                label="➡️ Right Option X"
                onClick={() => alert('Right side click')}
              />
              <ContextMenuItem
                label="➡️ Right Option Y"
                onClick={() => alert('Right side action')}
              />
            </Show>
          </ul>
        </div>
      </Show>
    </div>
  );
}
