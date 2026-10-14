// MarkdownViewSwitch.tsx
//
// At the end of the tab strip while a Markdown file is the active tab: its
// source, the source and the rendered preview side by side, or only the
// preview (lib/markdownView.ts; Editor.tsx shows them).
import { For, type JSX } from 'solid-js';
import { markdownView, setMarkdownView, type MarkdownView } from '../lib/markdownView';

const isMac = navigator.platform.toUpperCase().includes('MAC');
const PREVIEW_KEYS = isMac ? '⇧⌘V' : 'Ctrl+Shift+V';
const SPLIT_KEYS = isMac ? '⌘K V' : 'Ctrl+K V';

const icon = (paths: JSX.Element) => (
  <svg
    width="14"
    height="14"
    viewBox="0 0 16 16"
    fill="none"
    stroke="currentColor"
    stroke-width="1.4"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    {paths}
  </svg>
);

const OPTIONS: { view: MarkdownView; title: string; icon: () => JSX.Element }[] = [
  {
    view: 'edit',
    title: 'Source',
    icon: () => icon(<path d="M5.5 4.5 2 8l3.5 3.5M10.5 4.5 14 8l-3.5 3.5" />),
  },
  {
    view: 'split',
    title: `Source and preview side by side (${SPLIT_KEYS})`,
    icon: () =>
      icon(
        <>
          <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" />
          <path d="M8 2.75v10.5" />
        </>
      ),
  },
  {
    view: 'preview',
    title: `Preview (${PREVIEW_KEYS})`,
    icon: () =>
      icon(
        <>
          <path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8s-2.4 4.5-6.5 4.5S1.5 8 1.5 8Z" />
          <circle cx="8" cy="8" r="2" />
        </>
      ),
  },
];

export default function MarkdownViewSwitch(props: { path: string }) {
  return (
    <div class="md-view-switch" role="group" aria-label="Markdown view">
      <For each={OPTIONS}>
        {(option) => (
          <button
            type="button"
            class="md-view-option"
            classList={{ active: markdownView(props.path) === option.view }}
            aria-pressed={markdownView(props.path) === option.view}
            title={option.title}
            onClick={() => setMarkdownView(props.path, option.view)}
          >
            {option.icon()}
          </button>
        )}
      </For>
    </div>
  );
}
