// MenuIcon.tsx
//
// The 14px line icon of a context menu's item, drawn in its paths. Shared by
// the menus of the terminal panel and of a workspace's folders, so both look
// alike; the caller gives the paths.
import type { JSX } from 'solid-js';

export function Icon(props: { children: JSX.Element }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      {props.children}
    </svg>
  );
}
