// ContextEditView.tsx
//
// An edit to the agent's own context file in CLM mode (@context/LIVE_CONTEXT.md),
// in the tool call's expanded view: block by block, what it removed, rewrote
// or added, and how much context that freed. The raw text diff is one click
// away. Plain file edits keep EditDiffBlock (ToolCallBlock.tsx).
import { createMemo, createSignal, For, Show } from 'solid-js';
import EditDiffBlock from './EditDiffBlock';
import StaticCodeBlock from './StaticCodeBlock';
import {
  estimateTokens,
  formatTokens,
  type BlockChange,
  type ContextEdit,
} from '../lib/contextDoc';

const KIND_LABEL: Record<BlockChange['kind'], string> = {
  removed: 'Removed',
  added: 'Added',
  edited: 'Rewritten',
  unchanged: 'Unchanged',
  partial: 'Rewritten',
  written: 'Written',
};

/**
 * The user's messages are protected (src-tauri/src/clm/document.rs): the app
 * puts a removed or rewritten one back. Past this size (the app's limit,
 * roughly), a rewrite is kept and a removed one comes back shortened.
 */
const PROTECTED_MAX_TOKENS = 2000;

/** First non-empty line, shortened: a removed or collapsed block's gist. */
function preview(text: string): string {
  const line = text.split('\n').find((l) => l.trim()) ?? '';
  const t = line.trim();
  return t.length > 90 ? `${t.slice(0, 87)}…` : t;
}

function TokenChange(props: { before: number; after: number }) {
  const delta = () => props.after - props.before;
  return (
    <span
      class="ctx-edit-tokens"
      classList={{ freed: delta() < 0, grew: delta() > 0 }}
      title={`${formatTokens(props.before)} → ${formatTokens(props.after)}`}
    >
      {delta() === 0 ? 'same size' : `${formatTokens(delta())} ${delta() < 0 ? 'freed' : 'added'}`}
    </span>
  );
}

function BlockRow(props: { change: BlockChange; open: boolean }) {
  const [open, setOpen] = createSignal(props.open);
  const c = () => props.change;
  const before = () => estimateTokens(c().before);
  const after = () => estimateTokens(c().after);
  const role = () => c().block?.role;
  const shortId = () => {
    const id = c().block?.id;
    return id && id.length > 10 ? `${id.slice(0, 8)}…` : id;
  };
  const gist = () => preview(c().kind === 'removed' ? c().before : c().after || c().before);
  const hasBody = () => (c().before || c().after) !== '';
  const putBack = () => {
    if (role() !== 'user' || (c().kind !== 'removed' && c().kind !== 'edited')) return undefined;
    const long = before() > PROTECTED_MAX_TOKENS;
    if (c().kind === 'removed') return long ? 'put back shortened' : 'put back';
    return long ? undefined : 'put back';
  };

  return (
    <li class={`ctx-edit-block ${c().kind}`}>
      <button
        type="button"
        class="ctx-edit-block-header"
        onClick={() => setOpen(!open())}
        aria-expanded={open()}
        disabled={!hasBody()}
      >
        <span class="ctx-edit-kind">{KIND_LABEL[c().kind]}</span>
        <Show when={c().block} fallback={<span class="ctx-edit-where">part of a block</span>}>
          <span class={`ctx-edit-role role-${role()}`}>{role()}</span>
          <Show when={c().block?.turn}>
            <span class="ctx-edit-turn">#{c().block!.turn}</span>
          </Show>
          <Show when={shortId()}>
            <span class="ctx-edit-id" title={c().block?.id}>
              {shortId()}
            </span>
          </Show>
        </Show>
        <Show when={putBack()}>
          <span
            class="ctx-edit-kept"
            title="The user's messages are protected: the app puts them back when the next request is sent."
          >
            {putBack()}
          </span>
        </Show>
        <span class="ctx-edit-gist">{gist()}</span>
        <TokenChange before={before()} after={after()} />
      </button>
      <Show when={open() && hasBody()}>
        <div class="ctx-edit-block-body">
          <Show
            when={c().kind === 'edited' || c().kind === 'partial'}
            fallback={
              <pre class="ctx-edit-text">{c().kind === 'removed' ? c().before : c().after}</pre>
            }
          >
            <EditDiffBlock oldText={c().before} newText={c().after} />
          </Show>
        </div>
      </Show>
    </li>
  );
}

export default function ContextEditView(props: {
  edit: ContextEdit;
  /** edit_file's arguments, for the raw diff. Absent for write_file. */
  oldText?: string;
  newText: string;
}) {
  const [raw, setRaw] = createSignal(false);
  const changes = createMemo(() => props.edit.changes);
  const shown = createMemo(() => changes().filter((c) => c.kind !== 'unchanged'));
  const unchanged = () => changes().length - shown().length;
  // One change: open it. More: the list is the overview.
  const openFirst = () => shown().length === 1;
  const write = () => props.oldText === undefined;

  return (
    <div class="ctx-edit">
      <div class="ctx-edit-summary">
        <Show
          when={!write()}
          fallback={<span>Rewrote the whole file · {formatTokens(props.edit.tokensAfter)}</span>}
        >
          <TokenChange before={props.edit.tokensBefore} after={props.edit.tokensAfter} />
          <span class="ctx-edit-summary-detail">
            {formatTokens(props.edit.tokensBefore)} → {formatTokens(props.edit.tokensAfter)}
          </span>
        </Show>
        <span class="ctx-edit-summary-note">Applies from the agent's next request</span>
        <button type="button" class="ctx-edit-raw-toggle" onClick={() => setRaw(!raw())}>
          {raw() ? 'Blocks' : 'Raw text'}
        </button>
      </div>
      <Show
        when={!raw()}
        fallback={
          <Show
            when={props.oldText !== undefined}
            fallback={<StaticCodeBlock code={props.newText} language="markdown" />}
          >
            <EditDiffBlock oldText={props.oldText!} newText={props.newText} />
          </Show>
        }
      >
        <ul class="ctx-edit-blocks">
          <For each={shown()}>{(c) => <BlockRow change={c} open={openFirst()} />}</For>
        </ul>
        <Show when={unchanged() > 0}>
          <div class="ctx-edit-unchanged">
            {unchanged()} quoted block{unchanged() === 1 ? '' : 's'} left unchanged
          </div>
        </Show>
        <Show when={shown().length === 0}>
          <div class="ctx-edit-unchanged">No block changed</div>
        </Show>
      </Show>
    </div>
  );
}
