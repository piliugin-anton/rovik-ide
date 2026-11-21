import { createMemo, createSignal, Show } from 'solid-js';
import type { HarnessChatContext } from '../lib/polyglotHarness';
import PolyglotHarnessPanel from './PolyglotHarnessPanel';

/** The user selects one explicit execution location; losing it never switches hosts. */
export default function PolyglotHarnessSettings(props: {
  root: string;
  unsavedFiles?: readonly string[];
  chat?: HarnessChatContext;
}) {
  const [selected, setSelected] = createSignal<HarnessChatContext | null>(null);
  const selectedAvailable = () => {
    const choice = selected();
    return (
      !choice ||
      (props.chat?.dialogId === choice.dialogId &&
        props.chat.projectId === choice.projectId &&
        props.chat.hostId === choice.hostId)
    );
  };
  const target = createMemo(() => {
    if (!selectedAvailable()) return;
    const choice = selected();
    return { dialogId: choice?.dialogId, label: choice?.label ?? 'this computer' };
  });
  return (
    <div>
      <label class="setting-row">
        <span class="setting-row-label">Verification execution host</span>
        <select
          class="settings-select"
          aria-label="Verification execution host"
          value={selected()?.dialogId ?? 'local'}
          onChange={(event) => {
            if (event.currentTarget.value === 'local') setSelected(null);
            else if (props.chat?.dialogId === event.currentTarget.value)
              setSelected({ ...props.chat });
          }}
        >
          <option value="local">This computer</option>
          <Show when={props.chat}>
            {(chat) => <option value={chat().dialogId}>Current chat’s host: {chat().label}</option>}
          </Show>
          <Show when={!selectedAvailable()}>
            <option value={selected()!.dialogId} disabled>
              Previously selected chat is unavailable
            </option>
          </Show>
        </select>
      </label>
      <Show when={!props.chat && !selected()}>
        <p class="settings-hint">
          To verify on a Linux host, open this project’s chat on a connected host, then select it
          here.
        </p>
      </Show>
      <Show
        when={target()}
        keyed
        fallback={
          <p class="settings-status warning" role="status">
            The selected execution context changed or is unavailable. Select an execution host to
            continue.
          </p>
        }
      >
        {(location) => (
          <PolyglotHarnessPanel
            root={props.root}
            dialogId={location.dialogId}
            executionLabel={location.label}
            unsavedFiles={props.unsavedFiles}
          />
        )}
      </Show>
    </div>
  );
}
