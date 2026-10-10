// IsolationBar.tsx
//
// Above the composer of an isolated chat: what the chat changed that isn't
// in the project yet, and why it didn't land if it didn't. Each file opens
// as a comparison with the project's version. Land puts the changes in the
// project (Land anyway: the chat's version wins over conflicting changes);
// Discard throws them away; after a landing, Undo takes it back.

import { createResource, createSignal, For, Show } from 'solid-js';
import { confirmDialog } from './ConfirmDialog';
import { requestCompare } from '../lib/editorBridge';
import { notify } from '../lib/notifications';
import {
  overlayDiscard,
  overlayFile,
  overlayLand,
  overlayStatus,
  overlayUndo,
  type ChangedFile,
  type OverlayStatus,
} from '../lib/overlay';

interface Props {
  dialogId: string;
  /** Changes when the chat may have changed files (a run went on or ended). */
  revision: number;
  /** A run is going: landing or discarding would pull files out from under it. */
  running: boolean;
}

const LABEL: Record<ChangedFile['change'], string> = {
  added: 'A',
  changed: 'M',
  deleted: 'D',
  moved: 'R',
};

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;

export default function IsolationBar(props: Props) {
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [status, { refetch }] = createResource(
    () => ({ id: props.dialogId, revision: props.revision }),
    ({ id }) => overlayStatus(id).catch((): OverlayStatus | undefined => undefined)
  );
  const changed = () => {
    const s = status();
    return s?.isolated && s.files.length > 0 ? s : undefined;
  };
  const conflicted = (path: string) => !!status()?.pending?.conflicts.some((c) => c.path === path);

  async function compare(file: ChangedFile) {
    const versions = await overlayFile(props.dialogId, file.path).catch(() => null);
    if (!versions) return;
    const name = file.path.split('/').pop() ?? file.path;
    requestCompare({
      id: `chat:${props.dialogId}:${file.path}`,
      title: `${name} (this chat)`,
      fileName: name,
      before: versions.project ?? '',
      after: versions.chat ?? '',
    });
  }

  async function land(keepChat: boolean) {
    if (keepChat) {
      const ok = await confirmDialog({
        title: 'Land anyway?',
        message:
          'Where this chat and the project changed the same lines, this chat’s version replaces the project’s. Undo takes the landing back.',
        confirmLabel: 'Land anyway',
        danger: true,
      });
      if (!ok) return;
    }
    setBusy(true);
    try {
      const result = await overlayLand(props.dialogId, keepChat);
      if (result.landed) {
        notify({
          kind: 'success',
          title: 'Landed',
          message: `${plural(result.landed.paths.length, 'change')} from this chat are in the project.`,
        });
      } else if (result.error) {
        notify({ kind: 'error', title: 'Didn’t land', message: result.error });
      }
    } finally {
      setBusy(false);
      void refetch();
    }
  }

  async function undo(id: string) {
    setBusy(true);
    try {
      const kept = await overlayUndo(props.dialogId, id);
      notify({
        kind: kept.length ? 'warning' : 'success',
        title: 'Landing undone',
        message: kept.length
          ? `${plural(kept.length, 'file')} changed since and stayed as they are: ${kept.map((k) => k.path).join(', ')}.`
          : 'The project is back to how it was before.',
      });
    } catch (e) {
      notify({ kind: 'error', title: 'Couldn’t undo', message: String(e) });
    } finally {
      setBusy(false);
      void refetch();
    }
  }

  async function discard() {
    const count = status()?.files.length ?? 0;
    const ok = await confirmDialog({
      title: 'Discard this chat’s changes?',
      message: `${plural(count, 'changed file')} that aren’t in the project are thrown away for good. The project isn’t touched.`,
      confirmLabel: 'Discard',
      danger: true,
    });
    if (!ok) return;
    await overlayDiscard(props.dialogId).catch((e) => window.alert(String(e)));
    void refetch();
  }

  const disabled = () => props.running || busy();

  return (
    <>
      <Show when={changed()}>
        {(s) => (
          <div class="isolation-bar">
            <div class="isolation-bar-head">
              <button class="isolation-bar-toggle" onClick={() => setOpen(!open())}>
                <span class="isolation-bar-caret">{open() ? '▾' : '▸'}</span>
                {plural(s().files.length, 'changed file')} in this chat, not in the project yet
              </button>
              <Show
                when={s().pending?.conflicts.length}
                fallback={
                  <button
                    class="isolation-bar-action primary"
                    disabled={disabled()}
                    onClick={() => void land(false)}
                  >
                    Land
                  </button>
                }
              >
                <button
                  class="isolation-bar-action"
                  disabled={disabled()}
                  onClick={() => void land(true)}
                >
                  Land anyway
                </button>
              </Show>
              <button
                class="isolation-bar-action"
                disabled={disabled()}
                title={props.running ? 'Stop the run first' : 'Throw this chat’s changes away'}
                onClick={() => void discard()}
              >
                Discard
              </button>
            </div>
            <Show when={s().pending}>
              {(p) => <div class="isolation-bar-reason">{p().reason}</div>}
            </Show>
            <Show when={open() || !!s().pending?.conflicts.length}>
              <ul class="isolation-bar-files">
                <For each={s().files}>
                  {(file) => (
                    <li>
                      <button class="isolation-bar-file" onClick={() => void compare(file)}>
                        <span class={`isolation-bar-kind ${file.change}`}>
                          {LABEL[file.change]}
                        </span>
                        <span class="isolation-bar-path">{file.path}</span>
                        <Show when={file.from}>
                          <span class="isolation-bar-from">from {file.from}</span>
                        </Show>
                        <Show when={conflicted(file.path)}>
                          <span class="isolation-bar-conflict">conflict</span>
                        </Show>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>
        )}
      </Show>
      <Show when={!changed() && status()?.lastLanding}>
        {(last) => (
          <div class="isolation-bar landed">
            <div class="isolation-bar-head">
              <span class="isolation-bar-toggle">
                Landed {plural(last().paths.length, 'change')} from this chat
              </span>
              <button
                class="isolation-bar-action"
                disabled={disabled()}
                title="Put the project back to how it was before this landing"
                onClick={() => void undo(last().id)}
              >
                Undo
              </button>
            </div>
          </div>
        )}
      </Show>
    </>
  );
}
