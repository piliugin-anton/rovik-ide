import { createVirtualizer } from '@tanstack/solid-virtual';
import { For, Show, type Accessor } from 'solid-js';
import type { TreeRow } from '../types';

const ROW_HEIGHT = 22;

interface FileTreeProps {
  forwardRef: (el: HTMLDivElement) => void;
  rows: Accessor<TreeRow[]>;
  expanded: Accessor<Set<string>>;
  activePath: Accessor<string | null>;
  onToggle: (row: TreeRow) => void;
  onOpenFile: (row: TreeRow) => void;
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

  function handleClick(row: TreeRow) {
    if (row.loading || props.editingId() === row.id) return;
    if (row.isDir) {
      props.onToggle(row);
    } else {
      props.onOpenFile(row);
    }
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
    >
      <div style={{ height: `${virtualizer.getTotalSize()}px`, position: 'relative' }}>
        <For each={virtualizer.getVirtualItems()}>
          {(item) => {
            const row = () => props.rows()[item.index];
            const isEditing = () => !!row() && row()!.id === props.editingId();

            return (
              <div
                class="tree-row"
                data-file-id={row()?.id}
                classList={{
                  active: row()?.path === props.activePath(),
                  dir: row()?.isDir,
                  loading: row()?.loading,
                  ignored: row()?.ignored,
                  editing: isEditing(),
                }}
                style={{
                  position: 'absolute',
                  top: 0,
                  left: 0,
                  width: '100%',
                  height: `${ROW_HEIGHT}px`,
                  transform: `translateY(${item.start}px)`,
                  'padding-left': `${8 + row()?.depth * 14}px`,
                }}
                onClick={() => handleClick(row())}
                title={isEditing() ? undefined : row()?.name}
              >
                <span class="tree-row-icon">
                  {row()?.loading
                    ? ''
                    : row()?.isDir
                      ? props.expanded().has(row()!.path)
                        ? '▾'
                        : '▸'
                      : '·'}
                </span>
                <Show
                  when={isEditing()}
                  fallback={
                    <span class="tree-row-name">
                      {row()?.name}
                      {row()?.ignored}
                    </span>
                  }
                >
                  <input
                    class="tree-row-input"
                    value={row()?.name ?? ''}
                    ref={focusAndSelect}
                    onClick={(e) => e.stopPropagation()}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === 'Enter') {
                        e.preventDefault();
                        props.onEditSubmit(row(), (e.currentTarget as HTMLInputElement).value);
                      } else if (e.key === 'Escape') {
                        e.preventDefault();
                        props.onEditCancel(row());
                      }
                    }}
                    onBlur={(e) =>
                      props.onEditSubmit(row(), (e.currentTarget as HTMLInputElement).value)
                    }
                  />
                </Show>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}
