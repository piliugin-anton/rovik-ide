// HeavyCommandsSettings.tsx
//
// Settings → Heavy commands: how many CPU-heavy commands (builds, tests,
// type checks) run at once on this computer, across chats and the
// terminal. Others wait their turn and the chat says so. Auto picks one
// slot, or two on a big machine with room to spare; a fixed number
// overrides it and is kept by the backend (lib/workload.ts,
// src-tauri/crates/rovik-core/src/workload) for the next start.

import { createResource, createSignal, onCleanup, Show } from 'solid-js';
import { notify } from '../lib/notifications';
import { plural } from '../lib/textUtils';
import { describeMachine, setWorkloadSlots, workloadStatus } from '../lib/workload';
import { Segmented, SettingRow } from './SettingsControls';

export default function HeavyCommandsSettings() {
  const [status, { refetch, mutate }] = createResource(workloadStatus);
  const timer = setInterval(() => void refetch(), 2000);
  onCleanup(() => clearInterval(timer));
  // The number shown for "Fixed" before it's picked: auto's, or what was set.
  const [fixed, setFixed] = createSignal<number | null>(null);
  const fixedValue = () => fixed() ?? status()?.slots ?? status()?.autoSlots ?? 1;

  async function set(slots: number | null) {
    const s = status();
    if (s) mutate({ ...s, slots });
    try {
      await setWorkloadSlots(slots);
    } catch (e) {
      notify({ kind: 'error', title: 'Couldn’t save the setting', message: String(e) });
    }
    void refetch();
  }

  return (
    <div class="permissions-settings heavy-commands-settings">
      <p class="settings-hint">
        Builds, tests and type checks can each keep every core busy. To keep this computer
        responsive, only so many run at once, from all chats and the terminal together: the rest
        wait their turn, and the chat says what it waits for. A command in your own terminal never
        waits, but takes a slot while it’s heavy. Applies to this computer only; a cloud host has
        limits of its own, on its page.
      </p>

      <Show when={status()} fallback={<p class="settings-hint">Asking the machine…</p>}>
        {(s) => (
          <>
            <div class="settings-section">
              <div class="settings-section-title">Heavy commands at once</div>
              <SettingRow label="Slots">
                <Segmented
                  label="Heavy-command slots"
                  value={s().slots === null ? 'auto' : 'fixed'}
                  options={[
                    {
                      value: 'auto',
                      label: 'Auto',
                      title: 'As many as this machine comfortably runs',
                    },
                    { value: 'fixed', label: 'Fixed', title: 'A number you choose' },
                  ]}
                  onChange={(v) => void set(v === 'auto' ? null : fixedValue())}
                />
                <Show
                  when={s().slots !== null}
                  fallback={
                    <p class="setting-explain">
                      {plural(s().autoSlots, 'slot')} on this machine now ({describeMachine(s())}).
                      Auto gives one slot, or two on a machine with 16 or more cores and 32 GB or
                      more of memory while it has room to spare. Fewer at once is usually faster
                      overall: builds compete for memory and disk too.
                    </p>
                  }
                >
                  <input
                    class="editor-settings-number"
                    type="number"
                    min={1}
                    max={s().maxSlots}
                    step={1}
                    value={fixedValue()}
                    aria-label="Number of slots"
                    onChange={(e) => {
                      const n = Math.min(
                        s().maxSlots,
                        Math.max(1, Math.round(Number(e.currentTarget.value) || 1))
                      );
                      setFixed(n);
                      void set(n);
                    }}
                  />
                  <p class="setting-explain">
                    Auto would give {plural(s().autoSlots, 'slot')} here ({describeMachine(s())}).
                    {(s().slots ?? 0) > Math.max(2, Math.floor(s().cores / 4))
                      ? ' That many builds at once may make the machine slow to respond.'
                      : ''}
                  </p>
                </Show>
              </SettingRow>

              <SettingRow label="Now">
                <p class="setting-explain">
                  {s().busy === 0 && s().waiting === 0
                    ? 'Nothing heavy is running.'
                    : `${plural(s().busy, 'heavy command')} running` +
                      (s().waiting ? `, ${s().waiting} waiting for a slot.` : '.')}
                </p>
              </SettingRow>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
