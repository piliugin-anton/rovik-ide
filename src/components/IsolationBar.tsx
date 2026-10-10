// IsolationBar.tsx
//
// Above the composer of an isolated chat: what the chat changed that isn't
// in the project yet, and why it didn't land if it didn't. Each file opens
// as a comparison with the project's version. Land puts the changes in the
// project (Land anyway: the chat's version wins over conflicting changes);
// Discard throws them away; after a landing, Undo takes it back. Conflicts
// are resolved in the conflict resolver (Resolve…), or handed to the chat
// (Ask the chat): markers go into its files and it's asked to settle them.

import { createResource, createSignal, For, Show } from 'solid-js';
import { confirmDialog } from './ConfirmDialog';
import { ConflictResolver, type ConflictSource } from './ConflictResolver';
import type { GitOperation } from '../types';
import { requestCompare } from '../lib/editorBridge';
import { notify } from '../lib/notifications';
import {
  overlayConflictFile,
  overlayConflictResolve,
  overlayConflictsToChat,
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
  /** Sends the chat a message (starting a run): handing it the conflicts. */
  onAskChat: (request: string) => void;
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
  const [resolving, setResolving] = createSignal<string | null>(null);

  /** The chat's conflicts, in the shape the resolver takes a git merge in. */
  const operation = (): GitOperation => ({
    kind: 'merge',
    branch: null,
    incoming: null,
    step: null,
    total: null,
    commit: null,
    conflicts: status()?.pending?.conflicts.map((c) => c.path) ?? [],
    canContinue: true,
  });
  const source: ConflictSource = {
    load: (file) => overlayConflictFile(props.dialogId, file),
    resolve: async (file, content, side) => {
      await overlayConflictResolve(props.dialogId, file, content, side);
      await refetch();
    },
    title: 'This chat’s changes and the project’s',
    doneHint: 'Land the chat’s changes to finish.',
    finishLabel: 'Land',
    sideNames: { ours: 'Project', theirs: 'Chat' },
  };

  async function askChat() {
    setBusy(true);
    try {
      const handed = await overlayConflictsToChat(props.dialogId);
      if (handed.request) props.onAskChat(handed.request);
      else
        notify({
          kind: 'warning',
          title: 'Nothing to hand over',
          message: 'These conflicts aren’t in text files: resolve them here instead.',
        });
    } catch (e) {
      notify({ kind: 'error', title: 'Couldn’t hand the conflicts over', message: String(e) });
    } finally {
      setBusy(false);
      void refetch();
    }
  }

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
                  class="isolation-bar-action primary"
                  disabled={disabled()}
                  title="Choose, conflict by conflict, what lands"
                  onClick={() => setResolving(s().pending?.conflicts[0]?.path ?? null)}
                >
                  Resolve…
                </button>
                <button
                  class="isolation-bar-action"
                  disabled={disabled()}
                  title="Put conflict markers in the chat’s files and ask it to combine both changes"
                  onClick={() => void askChat()}
                >
                  Ask the chat
                </button>
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
                      <button
                        class="isolation-bar-file"
                        onClick={() =>
                          conflicted(file.path) && !props.running
                            ? setResolving(file.path)
                            : void compare(file)
                        }
                      >
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
      <Show when={resolving() !== null}>
        <ConflictResolver
          projectPath={`chat:${props.dialogId}`}
          source={source}
          operation={operation()}
          initialFile={resolving() ?? undefined}
          onFileWritten={() => void refetch()}
          onContinue={async () => {
            await land(false);
            setResolving(null);
          }}
          onAbort={async () => {
            setResolving(null);
          }}
          onClose={() => setResolving(null)}
        />
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
