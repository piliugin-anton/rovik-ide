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
  overlayUndoMerge,
  type ChangedFile,
  type OverlayStatus,
} from '../lib/overlay';

interface Props {
  dialogId: string;
  /** Changes when the chat may have changed files (a run went on or ended). */
  revision: number;
  /** A run is going: landing or discarding would pull files out from under it. */
  running: boolean;
  /** A run could start in this chat now (a model is set, none is going). */
  canAskChat: boolean;
  /** Sends `dialogId` a message, starting a run; whether it started. */
  onAskChat: (dialogId: string, request: string) => Promise<boolean>;
  /** A chat's title, to name the other chats a clash is with. */
  chatTitle?: (dialogId: string) => string | undefined;
}

const LABEL: Record<ChangedFile['change'], string> = {
  added: 'A',
  changed: 'M',
  deleted: 'D',
  moved: 'R',
};

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;

/** Build output is shown from this size, and warned about from the next. */
const BY_PRODUCTS_SHOWN = 1 << 30;
const BY_PRODUCTS_WARN = 5 * (1 << 30);
const gb = (bytes: number) => `${(bytes / (1 << 30)).toFixed(1)} GB`;

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
  const peerOf = (path: string) => status()?.peerConflicts.find((c) => c.path === path);
  const chatName = (id: string) => {
    const title = props.chatTitle?.(id);
    return title ? `“${title}”` : 'another chat';
  };
  /** Other chats changing the same lines, named, for the bar. */
  const peerNames = (s: OverlayStatus) => {
    const ids = [...new Set(s.peerConflicts.flatMap((c) => c.peers))];
    return ids.length === 1 ? chatName(ids[0]) : `${ids.length} other chats`;
  };
  const [resolving, setResolving] = createSignal<string | null>(null);
  /** One round of conflicts: the resolver's saved choices belong to it. */
  const roundKey = () => {
    const p = status()?.pending;
    return p ? `${p.reason}\u0000${p.conflicts.map((c) => c.path).join('\u0000')}` : '';
  };

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
    // This chat, whichever is on screen by the time the markers are in.
    const id = props.dialogId;
    if (!props.canAskChat) {
      notify({
        kind: 'warning',
        title: 'The chat can’t take it now',
        message: 'Pick a model, and wait for its run to end, then ask again.',
      });
      return;
    }
    setBusy(true);
    try {
      const handed = await overlayConflictsToChat(id);
      if (!handed.request) {
        notify({
          kind: 'warning',
          title: 'Nothing to hand over',
          message: 'These conflicts aren’t in text files: resolve them here instead.',
        });
      } else if (!(await props.onAskChat(id, handed.request))) {
        // Nobody to resolve the markers: take them back out.
        await overlayUndoMerge(id).catch(() => {});
      }
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
      const s = status();
      const conflicted = s?.pending?.conflicts.map((c) => c.path) ?? [];
      const deleted = s?.files.filter((f) => f.change === 'deleted').map((f) => f.path) ?? [];
      const list = (paths: string[]) =>
        paths.length > 8
          ? `${paths.slice(0, 8).join(', ')} and ${paths.length - 8} more`
          : paths.join(', ');
      const ok = await confirmDialog({
        title: 'Land anyway?',
        message:
          `This chat’s version replaces the project’s in ${plural(conflicted.length, 'file')}: ${list(conflicted)}.` +
          (deleted.length ? ` These are deleted from the project: ${list(deleted)}.` : '') +
          ' Undo takes the landing back.',
        confirmLabel: 'Land anyway',
        danger: true,
      });
      // A run may have started while the dialog was open.
      if (!ok || props.running) return false;
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
        return true;
      }
      if (result.conflicts.length) {
        notify({
          kind: 'warning',
          title: 'New conflicts',
          message: `The project changed meanwhile: ${result.conflicts.map((c) => c.path).join(', ')}.`,
        });
      } else if (result.error) {
        notify({ kind: 'error', title: 'Didn’t land', message: result.error });
      }
      return false;
    } catch (e) {
      notify({ kind: 'error', title: 'Didn’t land', message: String(e) });
      return false;
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
    if (!ok || props.running) return;
    setBusy(true);
    try {
      await overlayDiscard(props.dialogId);
    } catch (e) {
      notify({ kind: 'error', title: 'Couldn’t discard', message: String(e) });
    } finally {
      setBusy(false);
      void refetch();
    }
  }

  const disabled = () => props.running || busy();

  async function undoMerge() {
    setBusy(true);
    try {
      const files = await overlayUndoMerge(props.dialogId);
      notify({
        kind: 'success',
        title: 'Merge taken back',
        message: `${plural(files.length, 'file')} are this chat’s own version again.`,
      });
    } catch (e) {
      notify({ kind: 'error', title: 'Couldn’t take the merge back', message: String(e) });
    } finally {
      setBusy(false);
      void refetch();
    }
  }

  return (
    <>
      <Show when={changed()}>
        {(s) => (
          <div class="isolation-bar">
            <div class="isolation-bar-head">
              <div class="isolation-bar-top">
                <button
                  class="isolation-bar-toggle"
                  aria-expanded={open()}
                  onClick={() => setOpen(!open())}
                >
                  <span class="isolation-bar-caret" aria-hidden="true">
                    {open() ? '▾' : '▸'}
                  </span>
                  <span class="isolation-bar-title">
                    {plural(s().files.length, 'changed file')} in this chat, not in the project yet
                  </span>
                </button>
                <button
                  class="isolation-bar-action quiet"
                  disabled={disabled()}
                  title={props.running ? 'Stop the run first' : 'Throw this chat’s changes away'}
                  onClick={() => void discard()}
                >
                  Discard
                </button>
              </div>
              <div class="isolation-bar-actions">
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
              </div>
            </div>
            <Show when={s().pending}>
              {(p) => <div class="isolation-bar-reason">{p().reason}</div>}
            </Show>
            <Show when={s().peerConflicts.length > 0}>
              <div class="isolation-bar-reason">
                {peerNames(s())} is changing{' '}
                {s().peerConflicts.length === 1 ? 'this file' : 'these files'} too:{' '}
                {s()
                  .peerConflicts.map((c) => c.path)
                  .join(', ')}
                . Only one version of {s().peerConflicts.length === 1 ? 'it' : 'them'} lands as it
                is; the other then has to be resolved.
              </div>
            </Show>
            <Show when={s().byProducts >= BY_PRODUCTS_SHOWN}>
              <div
                class="isolation-bar-merged"
                classList={{ 'isolation-bar-reason': s().byProducts >= BY_PRODUCTS_WARN }}
              >
                Build output and dependencies in this chat’s view: {gb(s().byProducts)} (not landed;
                Discard frees it).
              </div>
            </Show>
            <Show when={s().mergedIn.length > 0}>
              <div class="isolation-bar-merged">
                The project’s newer changes were merged into {plural(s().mergedIn.length, 'file')}{' '}
                here.
                <button
                  class="isolation-bar-action"
                  disabled={disabled()}
                  title="Put back this chat’s own version of those files"
                  onClick={() => void undoMerge()}
                >
                  Undo the merge
                </button>
              </div>
            </Show>
            <Show when={open() || !!s().pending?.conflicts.length}>
              <ul class="isolation-bar-files">
                <For each={s().files}>
                  {(file) => (
                    <li>
                      <button
                        class="isolation-bar-file"
                        title={
                          conflicted(file.path) && !disabled()
                            ? 'Resolve this file’s conflicts'
                            : 'Compare with the project’s version'
                        }
                        onClick={() =>
                          conflicted(file.path) && !disabled()
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
                        <Show when={!conflicted(file.path) && peerOf(file.path)}>
                          {(peer) => (
                            <span
                              class="isolation-bar-conflict"
                              title={
                                peer().sameLines
                                  ? `${peer()
                                      .peers.map(chatName)
                                      .join(
                                        ', '
                                      )} changed the same lines: only one version lands as it is`
                                  : `${peer().peers.map(chatName).join(', ')} changed this file too`
                              }
                            >
                              {peer().sameLines ? 'also edited' : 'also touched'}
                            </span>
                          )}
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
          projectPath={`chat:${props.dialogId}:${roundKey()}`}
          source={source}
          operation={operation()}
          disabled={props.running}
          initialFile={resolving() ?? undefined}
          onFileWritten={() => void refetch()}
          onContinue={async () => {
            // Kept open when new conflicts came up meanwhile.
            if (await land(false)) setResolving(null);
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
