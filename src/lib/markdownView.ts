// markdownView.ts
//
// How a Markdown tab is shown (Editor.tsx): its source (edit), the source
// with the rendered preview beside it (split), or only the preview. Each
// tab keeps its own view while the app runs; the view chosen last is what
// the next Markdown file opens in (kept in localStorage, like the editor
// settings). Also the width of the split, and the heading a link from
// another document asked to show.
import { createSignal } from 'solid-js';

export type MarkdownView = 'edit' | 'split' | 'preview';

const VIEW_KEY = 'rovik.markdownView.v1';
const SPLIT_KEY = 'rovik.markdownSplit.v1';

function loadView(): MarkdownView {
  try {
    const saved = localStorage.getItem(VIEW_KEY);
    return saved === 'split' || saved === 'preview' ? saved : 'edit';
  } catch {
    return 'edit';
  }
}

function loadSplit(): number {
  try {
    const saved = Number(localStorage.getItem(SPLIT_KEY));
    return saved >= 0.2 && saved <= 0.8 ? saved : 0.5;
  } catch {
    return 0.5;
  }
}

function save(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not kept for the next start; this session still has it.
  }
}

const [lastChosen, setLastChosen] = createSignal<MarkdownView>(loadView());
const [views, setViews] = createSignal<Readonly<Record<string, MarkdownView>>>({});

/** Whether a tab in this Monaco language has the Markdown views. */
export function isMarkdownLanguage(language: string): boolean {
  return language === 'markdown';
}

/** The tab's view (reactive): its own, else the one chosen last. */
export function markdownView(path: string): MarkdownView {
  return views()[path] ?? lastChosen();
}

/**
 * Shows the tab as `view`. `chosen`: the user picked it (the switch, a
 * shortcut), so the next Markdown file opens this way too. A view the app
 * switches to (to show a search match in the source) isn't remembered.
 */
export function setMarkdownView(path: string, view: MarkdownView, chosen = true) {
  setViews((all) => (all[path] === view ? all : { ...all, [path]: view }));
  if (!chosen || view === lastChosen()) return;
  setLastChosen(view);
  save(VIEW_KEY, view);
}

/**
 * A tab shown for the first time keeps the view it opened in: choosing
 * another view in a different tab later doesn't change it.
 */
export function settleMarkdownView(path: string) {
  if (!(path in views())) setViews((all) => ({ ...all, [path]: lastChosen() }));
}

/** Ctrl+Shift+V: the preview, or back to the source. */
export function togglePreview(path: string) {
  setMarkdownView(path, markdownView(path) === 'preview' ? 'edit' : 'preview');
}

/** Ctrl+K V: the preview beside the source, or the source alone. */
export function toggleSplit(path: string) {
  setMarkdownView(path, markdownView(path) === 'split' ? 'edit' : 'split');
}

const [split, setSplit] = createSignal(loadSplit());

/** The source's share of the width when split (reactive), 0.2–0.8. */
export function markdownSplit(): number {
  return split();
}

export function setMarkdownSplit(share: number, persist: boolean) {
  setSplit(Math.min(0.8, Math.max(0.2, share)));
  if (persist) save(SPLIT_KEY, String(split()));
}

/*
 * A link to a heading in another document (`guide.md#install`): the tab
 * opens, then shows that heading, in its preview or in the source.
 */
const [anchors, setAnchors] = createSignal<Readonly<Record<string, string>>>({});

export function requestHeading(path: string, anchor: string) {
  setAnchors((all) => ({ ...all, [path]: anchor }));
}

/** The heading asked for in this document (reactive), if one is waiting. */
export function headingRequest(path: string): string | undefined {
  return anchors()[path];
}

export function clearHeadingRequest(path: string) {
  setAnchors((all) => {
    if (!(path in all)) return all;
    const rest = { ...all };
    delete rest[path];
    return rest;
  });
}
