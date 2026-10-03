import { createVirtualizer } from '@tanstack/solid-virtual';
import { For, Show, type Accessor } from 'solid-js';
import type { TreeRow } from '../types';
import { ChevronIcon, FileIcon, FolderIcon } from './FileIcons';

const ROW_HEIGHT = 22;

interface FileTreeProps {
  forwardRef: (el: HTMLDivElement) => void;
  rows: Accessor<TreeRow[]>;
  expanded: Accessor<Set<string>>;
  activePath: Accessor<string | null>;

  onToggle: (row: TreeRow) => void;
  onOpenFile: (row: TreeRow) => void;

  onSelect: (
    row: TreeRow,
    options: {
      shiftKey: boolean;
      modKey: boolean;
    }
  ) => void;

  onClearSelection: () => void;

  editingId: Accessor<string | null>;
  onEditSubmit: (row: TreeRow, name: string) => void;
  onEditCancel: (row: TreeRow) => void;
}

export default function FileTree(props: FileTreeProps) {
  let scrollRef: HTMLDivElement | undefined;

  const virtualizer = createVirtualizer({
    get count() {
      return props.rows().length;
    },
    getScrollElement: () => scrollRef ?? null,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

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
  }

  function handleRowClick(e: MouseEvent, row: TreeRow) {
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
      ref={(el) => {
        scrollRef = el;
        props.forwardRef(el);
      }}
      onMouseDown={handleTreeMouseDown}
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
                        value={currentRow().name}
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
    </div>
  );
}
