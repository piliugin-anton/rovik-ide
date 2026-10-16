// ContextControls.tsx: the chat's context management, the row under the
// model and effort (lib/contextMode.ts). "Trimming" or "CLMs"; for CLMs,
// the context file's size and revisions (view, restore), or why it can't
// run here and how to fix that (virtual files).
import { createEffect, createSignal, For, on, onCleanup, Show } from 'solid-js';
import {
  CONTEXT_MODES,
  clmStatus,
  contextModeFor,
  listContextRevisions,
  liveClmStatus,
  noteContextMode,
  readContextRevision,
  resetContext,
  restoreContextRevision,
  setContextMode,
  type ClmStatus,
  type ContextMode,
  type ContextRevision,
} from '../lib/contextMode';
import { mountVfs } from '../lib/virtualFiles';

function short(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86_400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86_400)} d ago`;
}

const KIND_LABEL: Record<ContextRevision['kind'], string> = {
  context: 'As sent',
  edit: 'Agent edit',
  restore: 'Restored',
};

/** Closes `open` on a click outside `root`. */
function closeOnOutside(root: () => HTMLElement | undefined, close: () => void) {
  const onDown = (e: PointerEvent) => {
    const el = root();
    if (el && !el.contains(e.target as Node)) close();
  };
  window.addEventListener('pointerdown', onDown, true);
  onCleanup(() => window.removeEventListener('pointerdown', onDown, true));
}

export default function ContextControls(props: {
  dialogId: string | null;
  /**
   * The budget the next request would get, from the model's context length
   * as it is now; the status only has the last request's.
   */
  budget: number | null;
  /** A run is going: status is refreshed when it ends. */
  sending: boolean;
  onOpenVirtualFiles: () => void;
}) {
  const [status, setStatus] = createSignal<ClmStatus | null>(liveClmStatus(props.dialogId));
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [revisionsOpen, setRevisionsOpen] = createSignal(false);
  const [revisions, setRevisions] = createSignal<ContextRevision[]>([]);
  const [viewing, setViewing] = createSignal<{ number: number; text: string } | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let menuRoot: HTMLDivElement | undefined;
  let revisionsRoot: HTMLDivElement | undefined;
  closeOnOutside(
    () => menuRoot,
    () => setMenuOpen(false)
  );
  closeOnOutside(
    () => revisionsRoot,
    () => {
      setRevisionsOpen(false);
      setViewing(null);
    }
  );

  const mode = (): ContextMode => contextModeFor(props.dialogId);
  const budget = () => props.budget ?? status()?.budget ?? 0;
  const current = () => CONTEXT_MODES.find((m) => m.id === mode()) ?? CONTEXT_MODES[0];

  async function refresh() {
    const id = props.dialogId;
    if (!id) {
      setStatus(null);
      return;
    }
    try {
      const s = await clmStatus(id);
      if (props.dialogId !== id) return;
      noteContextMode(id, s.mode);
      setStatus(s);
    } catch {
      // Keep whatever is shown (e.g. the live status from this session):
      // a failed refresh while remounting must not blank the row.
      if (!status()) setStatus(null);
    }
  }

  // On opening a chat (every mount: the panel unmounts when the sidebar
  // tab switches away), and whenever a run ends. The mount refresh runs
  // even while a run is going: otherwise switching tabs mid-run would
  // blank the row until the run ends.
  createEffect(
    on(
      () => props.dialogId,
      () => {
        const live = liveClmStatus(props.dialogId);
        // No live state for the new chat: drop the old chat's row at once
        // instead of showing it until the refresh answers.
        setStatus(live ?? null);
        void refresh();
      }
    )
  );
  createEffect(
    on(
      () => props.sending,
      (sending) => {
        if (!sending) void refresh();
      },
      { defer: true }
    )
  );

  // Each request a run prepares: the context as it went out, edits taken in.
  // Not deferred: a remount picks up the latest live status at once instead
  // of waiting for the next request.
  createEffect(
    on(
      () => liveClmStatus(props.dialogId),
      (live) => {
        if (!live) return;
        setStatus(live);
        if (revisionsOpen()) void reloadRevisions();
      }
    )
  );

  async function reloadRevisions() {
    const id = props.dialogId;
    if (!id) return;
    const list = await listContextRevisions(id).catch(() => null);
    if (list && props.dialogId === id) setRevisions(list.slice().reverse());
  }

  async function choose(next: ContextMode) {
    setMenuOpen(false);
    setError(null);
    await setContextMode(props.dialogId, next).catch((e) => setError(String(e)));
    await refresh();
  }

  async function openRevisions() {
    if (revisionsOpen()) {
      setRevisionsOpen(false);
      return;
    }
    if (!props.dialogId) return;
    setViewing(null);
    setRevisions((await listContextRevisions(props.dialogId).catch(() => [])).slice().reverse());
    setRevisionsOpen(true);
  }

  async function view(number: number) {
    if (!props.dialogId) return;
    if (viewing()?.number === number) {
      setViewing(null);
      return;
    }
    const text = await readContextRevision(props.dialogId, number).catch(
      (e) => `Error: ${String(e)}`
    );
    setViewing({ number, text });
  }

  async function restore(number: number) {
    if (!props.dialogId) return;
    setBusy(true);
    try {
      await restoreContextRevision(props.dialogId, number);
      setRevisionsOpen(false);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function reset() {
    if (!props.dialogId) return;
    await resetContext(props.dialogId).catch((e) => setError(String(e)));
    setRevisionsOpen(false);
    await refresh();
  }

  async function retryMount() {
    setBusy(true);
    setError(null);
    try {
      await mountVfs();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      await refresh();
    }
  }

  const clm = () => mode() === 'clm';
  const unavailable = () => clm() && status() !== null && !status()!.available;

  return (
    <div class="context-controls">
      <div class="chat-model-select-row">
        <div class="effort-selector context-mode-selector" ref={menuRoot}>
          <button
            type="button"
            class="model-picker-trigger effort-trigger"
            classList={{ set: clm() }}
            aria-haspopup="listbox"
            aria-expanded={menuOpen()}
            title={`Context management: ${current().hint}`}
            onClick={() => setMenuOpen((o) => !o)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') setMenuOpen(false);
            }}
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M3.5 1.75h6l3 3v9.5h-9z M9.5 1.75v3h3 M5.5 8h5 M5.5 10.5h5" />
            </svg>
            <span class="effort-trigger-label">{current().label}</span>
            <svg
              class="model-picker-caret"
              width="10"
              height="10"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M4 10l4-4 4 4" />
            </svg>
          </button>
          <Show when={menuOpen()}>
            <div
              class="effort-menu context-mode-menu"
              role="listbox"
              aria-label="Context management"
            >
              <For each={CONTEXT_MODES}>
                {(m) => (
                  <button
                    type="button"
                    role="option"
                    class="effort-option"
                    classList={{ active: m.id === mode() }}
                    aria-selected={m.id === mode()}
                    onClick={() => void choose(m.id)}
                  >
                    <span class="effort-option-label">{m.label}</span>
                    <span class="effort-option-hint">{m.hint}</span>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>

        <Show when={clm() && status()?.available}>
          <span
            class="context-status"
            title={
              status()!.revision
                ? `The agent's context file (${status()!.path}) is at revision ${status()!.revision}, ` +
                  `about ${status()!.tokens} of ${budget()} tokens. ` +
                  (status()!.projected
                    ? `The agent has edited it ${status()!.edits} time(s).`
                    : 'The agent has not edited it yet.') +
                  (status()!.compliance ? ` Context management: ${status()!.compliance}.` : '') +
                  (status()!.realPath ? `\n${status()!.realPath}` : '')
                : 'The context file is written when the next message is sent.'
            }
          >
            <Show when={status()!.revision} fallback="No context file yet">
              rev {status()!.revision}
              <Show when={budget() > 0}>
                {' · '}~{short(status()!.tokens)}/{short(budget())}
              </Show>
              <Show when={status()!.compliance}>
                {' · '}
                {status()!.compliance}
              </Show>
            </Show>
          </span>
          <Show when={status()!.revisions > 0}>
            <div class="context-revisions" ref={revisionsRoot}>
              <button
                type="button"
                class="link-button context-revisions-button"
                aria-expanded={revisionsOpen()}
                onClick={() => void openRevisions()}
                title="Revisions of the agent's context file"
              >
                Revisions
              </button>
              <Show when={revisionsOpen()}>
                <div class="context-revisions-menu" role="dialog" aria-label="Context revisions">
                  <div class="context-revisions-head">
                    <span>Context revisions</span>
                    <Show when={status()?.projected}>
                      <button
                        type="button"
                        class="link-button"
                        title="Drop the agent's edits: the chat is sent as stored again"
                        onClick={() => void reset()}
                      >
                        Reset to chat
                      </button>
                    </Show>
                  </div>
                  <div class="context-revisions-list">
                    <For each={revisions()}>
                      {(r) => (
                        <div class="context-revision" classList={{ edit: r.kind !== 'context' }}>
                          <span class="context-revision-number">{r.number}</span>
                          <span class="context-revision-kind">{KIND_LABEL[r.kind] ?? r.kind}</span>
                          <span class="context-revision-meta">
                            ~{short(r.tokens)} · {ago(r.createdAt)}
                          </span>
                          <button
                            type="button"
                            class="link-button"
                            onClick={() => void view(r.number)}
                          >
                            {viewing()?.number === r.number ? 'Hide' : 'View'}
                          </button>
                          <button
                            type="button"
                            class="link-button"
                            disabled={busy() || props.sending}
                            title="Make this the agent's context again, from the next message"
                            onClick={() => void restore(r.number)}
                          >
                            Restore
                          </button>
                        </div>
                      )}
                    </For>
                  </div>
                  <Show when={viewing()}>
                    {(v) => <pre class="context-revision-text">{v().text}</pre>}
                  </Show>
                </div>
              </Show>
            </div>
          </Show>
        </Show>
      </div>

      <Show when={unavailable()}>
        <div class="context-unavailable" role="alert">
          <p>
            <strong>CLMs need Virtual files.</strong> In this mode the agent's context lives in a
            file on the app's virtual volume, which the agent edits like any other file. The volume
            isn't available
            {status()!.problem ? `: ${status()!.problem}` : '.'}
          </p>
          <p>
            Install or enable the driver in Settings → Virtual files (FUSE on Linux, macFUSE on
            macOS, WinFsp on Windows). Until then this chat uses Trimming.
          </p>
          <div class="context-unavailable-actions">
            <button type="button" class="link-button" onClick={props.onOpenVirtualFiles}>
              Open Virtual files settings
            </button>
            <button
              type="button"
              class="link-button"
              disabled={busy()}
              onClick={() => void retryMount()}
            >
              {busy() ? 'Mounting…' : 'Try again'}
            </button>
          </div>
        </div>
      </Show>
      <Show when={error()}>
        {(e) => (
          <div class="context-error" role="alert" title={e()}>
            {e()}
          </div>
        )}
      </Show>
    </div>
  );
}
