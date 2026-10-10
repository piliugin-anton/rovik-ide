// IsolationBar.tsx
//
// Above the composer of an isolated chat: what the chat changed that isn't
// in the project yet. Each file opens as a comparison with the project's
// version; Discard throws the chat's changes away.

import { createResource, createSignal, For, Show } from 'solid-js';
import { confirmDialog } from './ConfirmDialog';
import { requestCompare } from '../lib/editorBridge';
import {
  overlayDiscard,
  overlayFile,
  overlayStatus,
  type ChangedFile,
  type OverlayStatus,
} from '../lib/overlay';

interface Props {
  dialogId: string;
  /** Changes when the chat may have changed files (a run went on or ended). */
  revision: number;
  /** A run is going: discarding would pull files out from under it. */
  running: boolean;
}

const LABEL: Record<ChangedFile['change'], string> = {
  added: 'A',
  changed: 'M',
  deleted: 'D',
  moved: 'R',
};

export default function IsolationBar(props: Props) {
  const [open, setOpen] = createSignal(false);
  const [status, { refetch }] = createResource(
    () => ({ id: props.dialogId, revision: props.revision }),
    ({ id }) => overlayStatus(id).catch((): OverlayStatus | undefined => undefined)
  );

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

  async function discard() {
    const count = status()?.files.length ?? 0;
    const ok = await confirmDialog({
      title: 'Discard this chat’s changes?',
      message: `${count} changed file${count === 1 ? '' : 's'} that aren’t in the project are thrown away for good. The project isn’t touched.`,
      confirmLabel: 'Discard',
      danger: true,
    });
    if (!ok) return;
    await overlayDiscard(props.dialogId).catch((e) => window.alert(String(e)));
    void refetch();
  }

  return (
    <Show when={status()?.isolated && status()!.files.length > 0 ? status() : undefined}>
      {(s) => (
        <div class="isolation-bar">
          <div class="isolation-bar-head">
            <button class="isolation-bar-toggle" onClick={() => setOpen(!open())}>
              <span class="isolation-bar-caret">{open() ? '▾' : '▸'}</span>
              {s().files.length} changed file{s().files.length === 1 ? '' : 's'} in this chat, not
              in the project yet
            </button>
            <button
              class="isolation-bar-discard"
              disabled={props.running}
              title={props.running ? 'Stop the run first' : 'Throw this chat’s changes away'}
              onClick={() => void discard()}
            >
              Discard
            </button>
          </div>
          <Show when={open()}>
            <ul class="isolation-bar-files">
              <For each={s().files}>
                {(file) => (
                  <li>
                    <button class="isolation-bar-file" onClick={() => void compare(file)}>
                      <span class={`isolation-bar-kind ${file.change}`}>{LABEL[file.change]}</span>
                      <span class="isolation-bar-path">{file.path}</span>
                      <Show when={file.from}>
                        <span class="isolation-bar-from">from {file.from}</span>
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
  );
}
