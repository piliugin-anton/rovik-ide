// EditDiffBlock.tsx
//
// A partial write's change (write with old_text) as a diff: the text it replaced and the replacement,
// line by line, removed lines red and added ones green, unchanged lines in
// between as they are. Syntax-highlighted like the editor (colorize, no
// editor instance). Line numbers once the result says where the edit landed.
import { createEffect, createMemo, createSignal, For, on, Show } from 'solid-js';
import { diffRows, snippetLines, type DiffRow as Row } from '../lib/lineDiff';
import { colorizeLines, escapeHtml } from './StaticCodeBlock';

export default function EditDiffBlock(props: {
  oldText: string;
  newText: string;
  language?: string;
  /** File line of the first old line, when known (from the tool result). */
  startLine?: number;
  onOpenInEditor?: () => void;
}) {
  const oldLines = createMemo(() => snippetLines(props.oldText));
  const newLines = createMemo(() => snippetLines(props.newText));

  const rows = createMemo(() => diffRows(oldLines(), newLines()));

  const counts = createMemo(() => ({
    removed: rows().filter((r) => r.kind === 'removed').length,
    added: rows().filter((r) => r.kind === 'added').length,
  }));

  // Highlighted lines, old and new colorized separately; plain until ready.
  const [oldHtml, setOldHtml] = createSignal<string[] | null>(null);
  const [newHtml, setNewHtml] = createSignal<string[] | null>(null);
  let run = 0;
  createEffect(
    on([oldLines, newLines, () => props.language], ([a, b, language]) => {
      const current = ++run;
      Promise.all([colorizeLines(a, language), colorizeLines(b, language)])
        .then(([oa, nb]) => {
          if (current !== run) return;
          setOldHtml(oa);
          setNewHtml(nb);
        })
        .catch(() => {
          if (current !== run) return;
          setOldHtml(null);
          setNewHtml(null);
        });
    })
  );

  const lineHtml = (row: Row): string => {
    if (row.oldIndex !== undefined && row.kind !== 'added') {
      return oldHtml()?.[row.oldIndex] ?? escapeHtml(oldLines()[row.oldIndex] ?? '');
    }
    const i = row.newIndex ?? 0;
    return newHtml()?.[i] ?? escapeHtml(newLines()[i] ?? '');
  };

  const numbered = () => props.startLine !== undefined;
  const oldNo = (row: Row) =>
    numbered() && row.oldIndex !== undefined ? props.startLine! + row.oldIndex : '';
  const newNo = (row: Row) =>
    numbered() && row.newIndex !== undefined ? props.startLine! + row.newIndex : '';

  const [copied, setCopied] = createSignal(false);
  async function copyNew() {
    try {
      await navigator.clipboard.writeText(props.newText);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch (err) {
      console.error('Copy failed:', err);
    }
  }

  return (
    <div class="static-code edit-diff">
      <div class="static-code-toolbar">
        <span class="edit-diff-counts">
          <span class="edit-diff-count added">+{counts().added}</span>
          <span class="edit-diff-count removed">−{counts().removed}</span>
        </span>
        <button type="button" onClick={() => void copyNew()} title="Copy the new text">
          {copied() ? 'Copied' : 'Copy'}
        </button>
        <Show when={props.onOpenInEditor}>
          {(open) => (
            <button type="button" onClick={() => open()()} title="Open the file in the editor">
              Open in editor
            </button>
          )}
        </Show>
      </div>
      <div class="edit-diff-body" role="table" aria-label="Changed lines">
        <div class="edit-diff-lines">
          <For each={rows()}>
            {(row) => (
              <div class={`edit-diff-row ${row.kind}`} role="row">
                <Show when={numbered()}>
                  <span class="edit-diff-no" aria-hidden="true">
                    {oldNo(row)}
                  </span>
                  <span class="edit-diff-no" aria-hidden="true">
                    {newNo(row)}
                  </span>
                </Show>
                <span class="edit-diff-sign" aria-label={row.kind}>
                  {row.kind === 'removed' ? '−' : row.kind === 'added' ? '+' : ' '}
                </span>
                <span class="edit-diff-code" innerHTML={lineHtml(row) || ' '} />
              </div>
            )}
          </For>
          <Show when={rows().length === 0}>
            <div class="edit-diff-row same">
              <span class="edit-diff-code edit-diff-empty">(empty)</span>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}
