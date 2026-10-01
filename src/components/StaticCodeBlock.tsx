// StaticCodeBlock.tsx
//
// Code blocks render as static HTML highlighted by Monaco's own tokenizer
// (monaco.editor.colorize): same colors as the editor, but no editor
// instance, so scrolling through a code-heavy chat creates none. A real
// editor is mounted only on "Open in editor", at most MAX_EDITORS at a time,
// and turned back into static HTML once it scrolls far out of view.
import { createEffect, createSignal, on, onCleanup, Show } from 'solid-js';
import * as monaco from 'monaco-editor';
import CodeBlock, { resolveLanguage } from './CodeBlock';

const COLORIZE_LINES = 400;
const MAX_EDITORS = 3;

// colorize() uses the current theme's token classes (mtk1, mtk2, ...);
// set the theme once so they exist before any editor has been created.
let themeReady = false;
function ensureTheme() {
  if (!themeReady) {
    monaco.editor.setTheme('vs-dark');
    themeReady = true;
  }
}

// Open editors, oldest first; opening one more closes the oldest.
const openEditors: Array<() => void> = [];
function registerEditor(close: () => void) {
  openEditors.push(close);
  while (openEditors.length > MAX_EDITORS) openEditors.shift()?.();
}
function unregisterEditor(close: () => void) {
  const i = openEditors.indexOf(close);
  if (i >= 0) openEditors.splice(i, 1);
}

function escapeHtml(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export default function StaticCodeBlock(props: {
  code: string;
  language?: string;
  /** Still streaming: show plain text, colorize once complete. */
  streaming?: boolean;
}) {
  const [html, setHtml] = createSignal<string | null>(null);
  const [showAll, setShowAll] = createSignal(false);
  const [wrap, setWrap] = createSignal(false);
  const [editing, setEditing] = createSignal(false);
  const [copied, setCopied] = createSignal(false);
  let rootRef: HTMLDivElement | undefined;

  const lines = () => props.code.split('\n').length;
  const visibleCode = () => {
    if (showAll() || lines() <= COLORIZE_LINES) return props.code;
    return props.code.split('\n').slice(0, COLORIZE_LINES).join('\n');
  };

  // Colorize when the code is complete (or more of it is revealed).
  let run = 0;
  createEffect(
    on(
      [() => props.streaming, visibleCode, () => props.language],
      ([streaming, code, language]) => {
        const current = ++run;
        if (streaming) {
          setHtml(null);
          return;
        }
        ensureTheme();
        monaco.editor
          .colorize(code, resolveLanguage(language), { tabSize: 4 })
          .then((result) => {
            if (current === run) setHtml(result);
          })
          .catch(() => {
            if (current === run) setHtml(null);
          });
      }
    )
  );

  // An open editor that scrolls far away turns back into static HTML.
  const close = () => setEditing(false);
  createEffect(() => {
    if (!editing() || !rootRef) return;
    registerEditor(close);
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.every((e) => !e.isIntersecting)) close();
      },
      { rootMargin: '1000px 0px' }
    );
    observer.observe(rootRef);
    onCleanup(() => {
      observer.disconnect();
      unregisterEditor(close);
    });
  });

  async function copy() {
    try {
      await navigator.clipboard.writeText(props.code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch (err) {
      console.error('Copy failed:', err);
    }
  }

  return (
    <div class="static-code" ref={rootRef}>
      <div class="static-code-toolbar">
        <Show when={props.language}>
          <span class="static-code-language">{props.language}</span>
        </Show>
        <button type="button" onClick={() => void copy()}>
          {copied() ? 'Copied' : 'Copy'}
        </button>
        <Show when={!editing()}>
          <button type="button" aria-pressed={wrap()} onClick={() => setWrap((w) => !w)}>
            Wrap
          </button>
        </Show>
        <Show when={!props.streaming}>
          <button type="button" onClick={() => setEditing((e) => !e)}>
            {editing() ? 'Close editor' : 'Open in editor'}
          </button>
        </Show>
      </div>

      <Show
        when={editing()}
        fallback={
          <>
            <pre class="static-code-body" classList={{ wrap: wrap() }}>
              <Show when={html()} fallback={<code innerHTML={escapeHtml(visibleCode())} />}>
                {(colored) => <code innerHTML={colored()} />}
              </Show>
            </pre>
            <Show when={!showAll() && lines() > COLORIZE_LINES}>
              <button type="button" class="static-code-more" onClick={() => setShowAll(true)}>
                Show all {lines().toLocaleString()} lines
              </button>
            </Show>
          </>
        }
      >
        <CodeBlock code={props.code} language={props.language} />
      </Show>
    </div>
  );
}
