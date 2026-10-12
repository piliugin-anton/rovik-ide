// BuildsSettings.tsx
//
// Settings → Builds & checks: how many CPU-heavy commands (builds, tests,
// type checks) run at once across chats and the terminal. Others wait their
// turn and the chat says so. Auto picks one slot, or two on a big machine
// with room to spare; a fixed number overrides it and is kept by the
// backend (src-tauri/src/workload) for the next start.

import { createResource, createSignal, onCleanup, Show } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import { notify } from '../lib/notifications';
import { plural } from '../lib/textUtils';

interface WorkloadStatus {
  /** As set; null: auto. */
  slots: number | null;
  autoSlots: number;
  maxSlots: number;
  busy: number;
  waiting: number;
  cores: number;
  memoryGb: number;
}

export default function BuildsSettings() {
  const [status, { refetch, mutate }] = createResource(() =>
    invoke<WorkloadStatus>('workload_status')
  );
  const timer = setInterval(() => void refetch(), 2000);
  onCleanup(() => clearInterval(timer));
  // The number shown for "Fixed" before it's picked: auto's, or what was set.
  const [fixed, setFixed] = createSignal<number | null>(null);
  const fixedValue = () => fixed() ?? status()?.slots ?? status()?.autoSlots ?? 1;

  async function set(slots: number | null) {
    const s = status();
    if (s) mutate({ ...s, slots });
    try {
      await invoke('workload_set_slots', { slots });
    } catch (e) {
      notify({ kind: 'error', title: 'Couldn’t save the setting', message: String(e) });
    }
    void refetch();
  }

  return (
    <div class="permissions-settings builds-settings">
      <p class="settings-hint">
        Builds, tests and type checks can each keep every core busy. To keep the machine responsive,
        only so many run at once — from all chats and the terminal together. The rest wait their
        turn, and the chat says what it waits for. Your terminal’s commands never wait, but they
        take a slot while they’re heavy.
      </p>

      <Show when={status()}>
        {(s) => (
          <>
            <div class="settings-section">
              <div class="settings-section-title">Heavy commands at once</div>
              <div class="setting-row">
                <span class="setting-row-label">Slots</span>
                <div class="setting-row-control">
                  <div class="segmented" role="radiogroup" aria-label="Heavy-command slots">
                    <button
                      type="button"
                      role="radio"
                      aria-checked={s().slots === null}
                      class="segmented-option"
                      classList={{ selected: s().slots === null }}
                      onClick={() => void set(null)}
                    >
                      Auto
                    </button>
                    <button
                      type="button"
                      role="radio"
                      aria-checked={s().slots !== null}
                      class="segmented-option"
                      classList={{ selected: s().slots !== null }}
                      onClick={() => void set(fixedValue())}
                    >
                      Fixed
                    </button>
                  </div>
                  <Show
                    when={s().slots !== null}
                    fallback={
                      <p class="setting-explain">
                        {plural(s().autoSlots, 'slot')} on this machine now ({s().cores} cores,{' '}
                        {s().memoryGb} GB). Auto gives two only with 16+ cores, 32 GB+ and room to
                        spare; fewer is usually faster overall, since builds compete for memory and
                        disk too.
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
                      Auto would give {plural(s().autoSlots, 'slot')} here ({s().cores} cores,{' '}
                      {s().memoryGb} GB).
                      {(s().slots ?? 0) > Math.max(2, Math.floor(s().cores / 4))
                        ? ' That many builds at once may make the machine slow to respond.'
                        : ''}
                    </p>
                  </Show>
                </div>
              </div>

              <div class="setting-row">
                <span class="setting-row-label">Now</span>
                <div class="setting-row-control">
                  <p class="setting-explain">
                    {s().busy === 0 && s().waiting === 0
                      ? 'Nothing heavy is running.'
                      : `${plural(s().busy, 'heavy command')} running` +
                        (s().waiting ? `, ${s().waiting} waiting for a slot.` : '.')}
                  </p>
                </div>
              </div>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
