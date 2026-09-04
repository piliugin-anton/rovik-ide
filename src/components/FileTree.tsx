import { createVirtualizer } from "@tanstack/solid-virtual";
import { For, type Accessor } from "solid-js";
import type { TreeRow } from "../types";

const ROW_HEIGHT = 22;

interface FileTreeProps {
  rows: Accessor<TreeRow[]>;
  expanded: Accessor<Set<string>>;
  activePath: Accessor<string | null>;
  onToggle: (row: TreeRow) => void;
  onOpenFile: (row: TreeRow) => void;
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
    if (row.loading) return;
    if (row.isDir) {
      props.onToggle(row);
    } else {
      props.onOpenFile(row);
    }
  }

  return (
    <div class="file-tree" ref={scrollRef}>
      <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
        <For each={virtualizer.getVirtualItems()}>
          {(item) => {
            const row = () => props.rows()[item.index];
            return (
              <div
                class="tree-row"
                classList={{
                  active: row()?.path === props.activePath(),
                  dir: row()?.isDir,
                  loading: row()?.loading,
                }}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  height: `${ROW_HEIGHT}px`,
                  transform: `translateY(${item.start}px)`,
                  "padding-left": `${8 + row()?.depth * 14}px`,
                }}
                onClick={() => handleClick(row())}
                title={row()?.name}
              >
                <span class="tree-row-icon">
                  {row()?.loading ? "" : row()?.isDir ? (props.expanded().has(row()!.path) ? "▾" : "▸") : "·"}
                </span>
                <span class="tree-row-name">{row()?.name}</span>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}
