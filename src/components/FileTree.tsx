import { createVirtualizer } from '@tanstack/solid-virtual';
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  onCleanup,
  Show,
  untrack,
  type Accessor,
} from 'solid-js';
import type { TreeRow } from '../types';
import { ChevronIcon, FileIcon, FolderIcon } from './FileIcons';

const ROW_HEIGHT = 22;
/** Pixels the pointer must move before a press becomes a drag. */
const DRAG_THRESHOLD_PX = 6;
/** Hover a collapsed folder this long to expand it mid-drag. */
const HOVER_EXPAND_MS = 700;
/** Id used for the "project root" drop target (never a real row id). */
const ROOT_TARGET_ID = '__root__';

interface FileTreeProps {
  forwardRef: (el: HTMLDivElement) => void;
  rows: Accessor<TreeRow[]>;
  expanded: Accessor<Set<string>>;
  activePath: Accessor<string | null>;
  /** Current multi-selection (App owns it; a drag carries the selection). */
  selectedIds: Accessor<Set<string>>;

  onToggle: (row: TreeRow) => void;
  onOpenFile: (row: TreeRow) => void;
  /** A finished drag: move these ids into the folder, or the root when null. */
  onDrop: (sourceIds: string[], destDir: TreeRow | null) => void;

  onSelect: (
    row: TreeRow,
    options: {
      shiftKey: boolean;
      modKey: boolean;
    }
  ) => void;

  onClearSelection: () => void;

  editingId: Accessor<string | null>;
  /** Right-click anywhere in the tree (on a row or empty space). */
  onContextMenu?: (e: MouseEvent) => void;
  onEditSubmit: (row: TreeRow, name: string) => void;
  onEditCancel: (row: TreeRow) => void;
}

export default function FileTree(props: FileTreeProps) {
  let scrollRef: HTMLDivElement | undefined;

  // --- Pointer-based drag state -------------------------------------------
  // Deliberately NOT HTML5 draggable=…: Tauri renders in the OS WebView
  // (WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux), and each
  // one breaks native DnD in its own way — Tauri's file-drop handler steals
  // drags on Windows unless dragDropEnabled is off, WKWebView drops custom
  // dataTransfer types, WebKitGTK won't fire drop without setData
  // (bugs.webkit.org #265857). Pointer events behave the same everywhere,
  // keep OS file-drop enabled, and dodge the sidebar's dragstart guard.
  const [dragIds, setDragIds] = createSignal<string[] | null>(null);
  const [dropTargetId, setDropTargetId] = createSignal<string | null>(null);
  let pendingPress: { row: TreeRow; startX: number; startY: number } | null = null;
  let ghostEl: HTMLDivElement | null = null;
  let hoverTimer: number | null = null;
  let hoverTarget: string | null = null;
  let suppressClick = false;

  const dragging = createMemo(() => dragIds() !== null);

  function rowById(id: string): TreeRow | undefined {
    return props.rows().find((r) => r.id === id);
  }

  function isAtOrUnder(path: string, base: string): boolean {
    if (path === base) return true;
    const trimmed = base.replace(/[\\/]+$/, '');
    return path.startsWith(`${trimmed}/`) || path.startsWith(`${trimmed}\\`);
  }

  /**
   * The folder a drop on `row` lands in: the folder itself, or the folder
   * containing a file row. Null means the project root (top-level files
   * have no folder row above them).
   */
  function resolveDropDir(row: TreeRow): TreeRow | null {
    if (row.isDir) return row;
    const rows = props.rows();
    const at = rows.findIndex((r) => r.id === row.id);
    for (let i = at - 1; i >= 0; i--) {
      if (rows[i].depth === row.depth - 1 && rows[i].isDir) return rows[i];
    }
    return null;
  }

  /** False for drops that would be a no-op or corrupt the tree. */
  function isValidDrop(sourceIds: string[], destDir: TreeRow | null): boolean {
    const byId = new Map(props.rows().map((r) => [r.id, r]));
    for (const id of sourceIds) {
      const src = byId.get(id);
      if (!src) continue;
      if (destDir === null) {
        // Root is always fine (App skips items already at the top level).
        continue;
      }
      if (src.id === destDir.id) return false;
      // A folder can't go into itself or one of its descendants.
      if (isAtOrUnder(destDir.path, src.path)) return false;
    }
    return true;
  }

  function hitTest(x: number, y: number): { dir: TreeRow | null; id: string } | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    if (!el) return null;
    if (el.closest('[data-root-drop]')) return { dir: null, id: ROOT_TARGET_ID };
    const rowEl = el.closest<HTMLElement>('[data-file-id]');
    if (rowEl?.dataset.fileId) {
      const row = rowById(rowEl.dataset.fileId);
      if (!row || row.loading) return null;
      const dir = resolveDropDir(row);
      return { dir, id: dir ? dir.id : ROOT_TARGET_ID };
    }
    // Empty tree area: dropping there means the project root.
    if (scrollRef && el.closest('.file-tree')) return { dir: null, id: ROOT_TARGET_ID };
    return null;
  }

  function clearHoverTimer() {
    if (hoverTimer !== null) {
      window.clearTimeout(hoverTimer);
      hoverTimer = null;
    }
    hoverTarget = null;
  }

  function updateGhost(x: number, y: number) {
    if (ghostEl) {
      ghostEl.style.left = `${x + 12}px`;
      ghostEl.style.top = `${y + 12}px`;
    }
  }

  function updateDropTarget(x: number, y: number) {
    const ids = dragIds();
    if (!ids) return;
    const hit = hitTest(x, y);
    if (!hit || !isValidDrop(ids, hit.dir)) {
      setDropTargetId(null);
      clearHoverTimer();
      return;
    }
    setDropTargetId(hit.id);
    // Hover a collapsed folder to open it mid-drag.
    if (hit.dir && !props.expanded().has(hit.dir.path)) {
      if (hoverTarget !== hit.id) {
        clearHoverTimer();
        hoverTarget = hit.id;
        const dir = hit.dir;
        hoverTimer = window.setTimeout(() => {
          props.onToggle(dir);
          hoverTimer = null;
          hoverTarget = null;
        }, HOVER_EXPAND_MS);
      }
    } else {
      clearHoverTimer();
    }
    // Auto-scroll near the container edges.
    const c = scrollRef;
    if (c) {
      const rect = c.getBoundingClientRect();
      if (y < rect.top + 28) c.scrollTop -= 10;
      else if (y > rect.bottom - 28) c.scrollTop += 10;
    }
  }

  function teardownDrag() {
    clearHoverTimer();
    if (ghostEl) {
      ghostEl.remove();
      ghostEl = null;
    }
    document.body.classList.remove('tree-dragging');
    window.removeEventListener('pointermove', onDragMove);
    window.removeEventListener('pointerup', onDragUp);
    window.removeEventListener('pointercancel', onDragCancel);
    window.removeEventListener('keydown', onDragKey, true);
    setDragIds(null);
    setDropTargetId(null);
    pendingPress = null;
  }

  function beginDrag(press: TreeRow, x: number, y: number): boolean {
    const sel = props.selectedIds();
    const wanted = sel.has(press.id) ? [...sel] : [press.id];
    const byId = new Map(props.rows().map((r) => [r.id, r]));
    const ids = wanted.filter((id) => {
      const r = byId.get(id);
      return r && !r.loading && r.id !== '__draft__';
    });
    if (ids.length === 0) return false;

    suppressClick = true;
    setDragIds(ids);
    setDropTargetId(null);

    ghostEl = document.createElement('div');
    ghostEl.className = 'tree-drag-ghost';
    ghostEl.textContent =
      ids.length === 1 ? (byId.get(ids[0])?.name ?? '1 item') : `${ids.length} items`;
    updateGhost(x, y);
    document.body.appendChild(ghostEl);
    document.body.classList.add('tree-dragging');

    window.addEventListener('pointermove', onDragMove);
    window.addEventListener('pointerup', onDragUp);
    window.addEventListener('pointercancel', onDragCancel);
    window.addEventListener('keydown', onDragKey, true);
    updateDropTarget(x, y);
    return true;
  }

  function onDragMove(e: PointerEvent) {
    if (!dragIds()) {
      // Still in the press-then-move phase: cross the threshold to start.
      const p = pendingPress;
      if (!p) return;
      if (Math.hypot(e.clientX - p.startX, e.clientY - p.startY) < DRAG_THRESHOLD_PX) return;
      beginDrag(p.row, e.clientX, e.clientY);
      return;
    }
    e.preventDefault();
    updateGhost(e.clientX, e.clientY);
    updateDropTarget(e.clientX, e.clientY);
  }

  function onDragUp(e: PointerEvent) {
    const ids = dragIds();
    const wasDragging = ids !== null;
    const hit = wasDragging ? hitTest(e.clientX, e.clientY) : null;
    teardownDrag();
    if (wasDragging && ids && hit && isValidDrop(ids, hit.dir)) {
      props.onDrop(ids, hit.dir);
    }
    // When the press never became a drag, the row's click proceeds normally.
  }

  function onDragCancel() {
    teardownDrag();
  }

  function onDragKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && dragIds()) {
      e.preventDefault();
      e.stopPropagation();
      teardownDrag();
    }
  }

  onCleanup(teardownDrag);

  const virtualizer = createVirtualizer({
    get count() {
      return props.rows().length;
    },
    getScrollElement: () => scrollRef ?? null,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  // Keep the row being edited (a new entry or a rename) in view: the tree is
  // virtualized, so an off-screen row has no input to type into. The index
  // can shift while editing (e.g. a folder's children load under a draft).
  const editingIndex = createMemo(() => {
    const id = props.editingId();
    return id === null ? -1 : props.rows().findIndex((r) => r.id === id);
  });

  createEffect(
    on(editingIndex, (index) => {
      if (index >= 0) virtualizer.scrollToIndex(index, { align: 'auto' });
    })
  );

  function handleRowMouseDown(e: MouseEvent, row: TreeRow) {
    if (row.loading || props.editingId() === row.id) return;

    // Only left-click should change the selection.
    // Right-click is handled by App's context-menu logic.
    if (e.button !== 0) return;

    e.preventDefault();

    const isMac = navigator.platform.toUpperCase().includes('MAC');
    const modKey = isMac ? e.metaKey : e.ctrlKey;

    props.onSelect(row, {
      shiftKey: e.shiftKey,
      modKey,
    });

    // Shift/range and modifier toggles are selection gestures, never drags.
    // Editing rows and the new-file draft can't be dragged either.
    if (e.shiftKey || modKey || row.id === '__draft__') return;
    pendingPress = { row, startX: e.clientX, startY: e.clientY };
    window.addEventListener('pointermove', onDragMove);
    window.addEventListener('pointerup', onDragUp);
    window.addEventListener('pointercancel', onDragCancel);
  }

  function handleRowClick(e: MouseEvent, row: TreeRow) {
    if (suppressClick) {
      suppressClick = false;
      e.preventDefault();
      return;
    }
    if (row.loading || props.editingId() === row.id) return;

    // Selection is already handled by mousedown.
    // Modifier clicks should select, but must NOT open/toggle the item.
    if (e.shiftKey || e.ctrlKey || e.metaKey) {
      e.preventDefault();
      return;
    }

    if (row.isDir) {
      props.onToggle(row);
    } else {
      props.onOpenFile(row);
    }
  }

  function handleTreeMouseDown(e: MouseEvent) {
    // Only clear selection when clicking the actual empty tree area.
    // Do not interfere with rows or editable inputs.
    const target = e.target as HTMLElement;

    if (target.closest('[data-file-id]')) return;
    if (target.closest('input')) return;

    e.preventDefault();
    props.onClearSelection();
  }

  function focusAndSelect(el: HTMLInputElement) {
    queueMicrotask(() => {
      el.focus();
      el.select();
    });
  }

  return (
    <div
      class="file-tree"
      classList={{ 'tree-dragging': dragging() }}
      ref={(el) => {
        scrollRef = el;
        props.forwardRef(el);
      }}
      onMouseDown={handleTreeMouseDown}
      onContextMenu={(e) => props.onContextMenu?.(e)}
      style={{
        'user-select': 'none',
        '-webkit-user-select': 'none',
      }}
    >
      <div
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          position: 'relative',
        }}
      >
        <For each={virtualizer.getVirtualItems()}>
          {(item) => {
            const row = () => props.rows()[item.index];
            const isEditing = () => !!row() && row()!.id === props.editingId();

            return (
              <Show when={row()}>
                {(currentRow) => (
                  <div
                    class="tree-row"
                    data-file-id={currentRow().id}
                    classList={{
                      active: currentRow().path === props.activePath(),
                      dir: currentRow().isDir,
                      loading: currentRow().loading,
                      ignored: currentRow().ignored,
                      editing: isEditing(),
                      'drag-source': dragIds()?.includes(currentRow().id) ?? false,
                      'drop-destination': dropTargetId() === currentRow().id,
                    }}
                    style={{
                      position: 'absolute',
                      top: 0,
                      left: 0,
                      width: '100%',
                      height: `${ROW_HEIGHT}px`,
                      transform: `translateY(${item.start}px)`,
                      'padding-left': `${8 + currentRow().depth * 14}px`,
                      'user-select': 'none',
                      '-webkit-user-select': 'none',
                    }}
                    onMouseDown={(e) => handleRowMouseDown(e, currentRow())}
                    onClick={(e) => handleRowClick(e, currentRow())}
                    title={isEditing() ? undefined : currentRow().name}
                  >
                    <span class="tree-row-chevron">
                      <Show when={currentRow().isDir && !currentRow().loading}>
                        <ChevronIcon open={props.expanded().has(currentRow().path)} />
                      </Show>
                    </span>
                    <span class="tree-row-icon">
                      <Show when={!currentRow().loading}>
                        <Show when={currentRow().isDir} fallback={<FileIcon />}>
                          <FolderIcon open={props.expanded().has(currentRow().path)} />
                        </Show>
                      </Show>
                    </span>

                    <Show
                      when={isEditing()}
                      fallback={<span class="tree-row-name">{currentRow().name}</span>}
                    >
                      <input
                        class="tree-row-input"
                        // Once, not kept in step with the row: a refresh of
                        // the folder (any file written in it) brings a new
                        // row, and put the old name back over what was typed.
                        value={untrack(() => currentRow().name)}
                        ref={focusAndSelect}
                        onMouseDown={(e) => {
                          // Allow normal text editing/selection inside
                          // the rename input.
                          e.stopPropagation();
                        }}
                        onClick={(e) => {
                          e.stopPropagation();
                        }}
                        onKeyDown={(e) => {
                          e.stopPropagation();

                          if (e.key === 'Enter') {
                            e.preventDefault();

                            props.onEditSubmit(
                              currentRow(),
                              (e.currentTarget as HTMLInputElement).value
                            );
                          } else if (e.key === 'Escape') {
                            e.preventDefault();
                            props.onEditCancel(currentRow());
                          }
                        }}
                        onBlur={(e) =>
                          props.onEditSubmit(
                            currentRow(),
                            (e.currentTarget as HTMLInputElement).value
                          )
                        }
                      />
                    </Show>
                  </div>
                )}
              </Show>
            );
          }}
        </For>
      </div>
      {/* After the rows, sticking to the bottom: above them, it pushed every
          row down as the drag began, and the drop landed a row too high. */}
      <Show when={dragging()}>
        <div
          class="tree-root-drop"
          data-root-drop={ROOT_TARGET_ID}
          classList={{ 'drop-destination': dropTargetId() === ROOT_TARGET_ID }}
        >
          Drop here to move to the project root
        </div>
      </Show>
    </div>
  );
}
