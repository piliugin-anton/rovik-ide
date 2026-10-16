// FileIcons.tsx
//
// File tree icons. Same drawing language as the context menu's icons in
// App.tsx (16px grid, 1.5 stroke, currentColor, rounded joins), so the
// tree, the menu and its "New file" / "New folder" entries read as one set.
import { Show } from 'solid-js';

const base = {
  width: 14,
  height: 14,
  viewBox: '0 0 16 16',
  fill: 'none',
  stroke: 'currentColor',
  'stroke-width': 1.5,
  'stroke-linecap': 'round' as const,
  'stroke-linejoin': 'round' as const,
  'aria-hidden': true,
};

/** Disclosure arrow; rotated by CSS when the folder is open. */
export function ChevronIcon(props: { open: boolean }) {
  return (
    <svg {...base} width={12} height={12} class="tree-chevron" classList={{ open: props.open }}>
      <path d="M6 4l4 4-4 4" />
    </svg>
  );
}

/** Open or closed with its folder: a component's body runs once, so `<Show>` follows `open`. */
export function FolderIcon(props: { open: boolean }) {
  return (
    <Show
      when={props.open}
      fallback={
        <svg {...base} class="tree-icon tree-icon-folder">
          <path d="M2 4.5a1 1 0 0 1 1-1h3.5l1.2 1.5H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" />
        </svg>
      }
    >
      <svg {...base} class="tree-icon tree-icon-folder">
        <path d="M2 12.5V4.5a1 1 0 0 1 1-1h3.5l1.2 1.5H12a1 1 0 0 1 1 1v1.5" />
        <path d="M2 12.5 4 7.5h10.5l-2 5H2Z" />
      </svg>
    </Show>
  );
}

export function FileIcon() {
  return (
    <svg {...base} class="tree-icon tree-icon-file">
      <path d="M4 1.5h5.5L12.5 4.5V14.5H4V1.5Z" />
      <path d="M9.5 1.5V4.5H12.5" />
    </svg>
  );
}
