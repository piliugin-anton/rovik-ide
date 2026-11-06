// MemorySettings.tsx
//
// Settings → Long-term memory: how many memories the open project has, the
// switch that turns its memory off for the project's chats (lib/memory.ts)
// and the way into the manager that lists, corrects and removes them
// (components/MemoryManager.tsx). The count comes from the project's store
// (`memory_stats`); the switch is the project's own setting, saved with its
// other settings at once (projectProfile.ts `memory`: absent means on).
import { createEffect, createResource, createSignal, on, Show } from 'solid-js';
import { memoryStats, memorySummary } from '../lib/memory';
import { loadProjectSettings, updateProjectSettings } from '../lib/projectProfile';
import MemoryManager from './MemoryManager';
import { SettingRow, SettingStatus } from './SettingsControls';

export default function MemorySettings(props: { root: string }) {
  const [stats, { refetch }] = createResource(
    () => props.root,
    (root) => memoryStats(root).catch(() => null)
  );
  const [enabled, setEnabled] = createSignal(true);
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  /** The manager is on top: it lists and changes what the project kept. */
  const [managing, setManaging] = createSignal(false);

  // The open project's choice, read again when another project opens.
  createEffect(
    on(
      () => props.root,
      () => void read()
    )
  );
  async function read() {
    const settings = await loadProjectSettings(props.root);
    setEnabled(settings.memory !== false);
  }

  /** Saves the switch at once: off is `memory: false`, on again removes it. */
  async function change(next: boolean) {
    const before = enabled();
    setEnabled(next);
    setSaving(true);
    setError(null);
    try {
      await updateProjectSettings(props.root, { memory: next ? undefined : false });
    } catch (err) {
      setEnabled(before);
      setError(`Couldn't save: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div class="permissions-settings memory-settings">
      <section class="settings-section">
        <div class="settings-section-title">Long-term memory in this project</div>
        <p class="settings-hint">
          What the agent's runs in this project learned and kept: an approach that worked, a fact
          about the code, a failure it fixed. A run starts with what fits its task, shows it above
          the message box, and records what became of it once the checks ran. Only evidence promotes
          a memory; one a failure points at is invalidated.
        </p>
        <SettingRow label="Memories">
          <span class="memory-count" role="status">
            <Show when={!stats.loading} fallback="Counting…">
              {stats() ? memorySummary(stats()!) : 'The memory could not be counted.'}
            </Show>
          </span>
          <button class="link-button" onClick={() => setManaging(true)}>
            Manage memories…
          </button>
        </SettingRow>
        <SettingRow label="Use in chats">
          <input
            type="checkbox"
            checked={enabled()}
            disabled={saving()}
            aria-label="Use this project's memory in chats"
            title="Unticked: the project's chats recall nothing and record nothing"
            onChange={(e) => void change(e.currentTarget.checked)}
          />
        </SettingRow>
        <Show when={!enabled()}>
          <SettingStatus>
            Off for this project: its chats start without prior work and record nothing new. What it
            holds stays, and the count above still shows it.
          </SettingStatus>
        </Show>
        <Show when={error()}>
          <SettingStatus warning>{error()}</SettingStatus>
        </Show>
        <button class="link-button" disabled={stats.loading} onClick={() => void refetch()}>
          Count again
        </button>
      </section>
      <Show when={managing()}>
        <MemoryManager
          root={props.root}
          onClose={() => {
            setManaging(false);
            // What the manager changed shows in the count above.
            void refetch();
          }}
        />
      </Show>
    </div>
  );
}
