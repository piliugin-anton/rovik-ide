// StaticCodeBlock.tsx
//
// Code blocks render as static HTML highlighted by Monaco's own tokenizer
// (monaco.editor.colorize): same colors as the editor, but no editor
// instance, so scrolling through a code-heavy chat creates none.
//
// "Open in editor" opens the file the code belongs to in the main editor
// (lib/openInEditor.ts); a block that isn't tied to a
// project file has no such button. It used to turn the block itself into
// an editable inline editor.
import { createEffect, createSignal, on, Show } from 'solid-js';
import * as monaco from 'monaco-editor';
import { resolveLanguage } from './CodeBlock';

const COLORIZE_LINES = 400;

// colorize() uses the current theme's token classes (mtk1, mtk2, ...);
// set the theme once so they exist before any editor has been created.
let themeReady = false;
function ensureTheme() {
  if (!themeReady) {
    monaco.editor.setTheme('vs-dark');
    themeReady = true;
  }
}

export function escapeHtml(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/**
 * Highlighted HTML for each line of `lines`, in the editor's colors.
 * colorize() ends every line with "<br/>", which is what it's split on.
 */
export async function colorizeLines(lines: string[], language?: string): Promise<string[]> {
  ensureTheme();
  const html = await monaco.editor.colorize(lines.join('\n'), resolveLanguage(language), {
    tabSize: 4,
  });
  const out = html.split('<br/>');
  // A mismatch would put colors on the wrong lines: plain text instead.
  if (out.length < lines.length) throw new Error('colorize: unexpected line count');
  return out.slice(0, lines.length);
}

export default function StaticCodeBlock(props: {
  code: string;
  language?: string;
  /** Still streaming: show plain text, colorize once complete. */
  streaming?: boolean;
  /** The code is (part of) a project file: "Open in editor" opens it there. */
  onOpenInEditor?: () => void;
}) {
  const [html, setHtml] = createSignal<string | null>(null);
  const [showAll, setShowAll] = createSignal(false);
  const [wrap, setWrap] = createSignal(false);
  const [copied, setCopied] = createSignal(false);

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
    <div class="static-code">
      <div class="static-code-toolbar">
        <Show when={props.language}>
          <span class="static-code-language">{props.language}</span>
        </Show>
        <button type="button" onClick={() => void copy()}>
          {copied() ? 'Copied' : 'Copy'}
        </button>
        <button type="button" aria-pressed={wrap()} onClick={() => setWrap((w) => !w)}>
          Wrap
        </button>
        <Show when={!props.streaming && props.onOpenInEditor}>
          {(open) => (
            <button type="button" onClick={() => open()()} title="Open the file in the editor">
              Open in editor
            </button>
          )}
        </Show>
      </div>

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
    </div>
  );
}
