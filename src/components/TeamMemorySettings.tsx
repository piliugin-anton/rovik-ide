import { createResource, createSignal, For, Show } from 'solid-js';
import { teamMemory, type TeamMemoryLink } from '../lib/memory';
import { confirmDialog } from './ConfirmDialog';
import { SettingRow, SettingStatus } from './SettingsControls';

export default function TeamMemorySettings(props: { root: string }) {
  const [status, { refetch }] = createResource(
    () => props.root,
    (root) =>
      teamMemory<TeamMemoryLink | null>(root, { kind: 'status' }).then(
        (link) => ({ link, error: '' }),
        (error) => ({ link: null, error: String(error) })
      )
  );
  const [hub, setHub] = createSignal('');
  const [project, setProject] = createSignal('');
  const [provider, setProvider] = createSignal('github');
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  async function act(kind: string, fields = {}) {
    setBusy(true);
    setError('');
    try {
      await teamMemory(props.root, { kind, ...fields });
      await refetch();
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  }
  return (
    <section class="settings-section">
      <div class="settings-section-title">Team project memory</div>
      <p class="settings-hint">
        Shared memories are available to this project’s chats and personal cloud hosts. Select
        records to publish in Manage memories.
      </p>
      <Show
        when={status()?.link}
        fallback={
          <fieldset disabled={busy() || status.loading || !!status()?.error}>
            <SettingRow label="Repository provider">
              <select
                aria-label="Repository provider"
                value={provider()}
                onChange={(e) => setProvider(e.currentTarget.value)}
              >
                <option value="github">GitHub</option>
                <option value="gitlab">GitLab</option>
              </select>
            </SettingRow>
            <SettingRow label="Team hub">
              <input
                aria-label="Team hub"
                type="url"
                placeholder="https://memory.example.org"
                value={hub()}
                onInput={(e) => setHub(e.currentTarget.value)}
              />
            </SettingRow>
            <SettingRow label="Team project ID">
              <input
                aria-label="Team project ID"
                value={project()}
                onInput={(e) => setProject(e.currentTarget.value)}
              />
            </SettingRow>
            <button
              disabled={!hub() || !project()}
              onClick={() =>
                void act('link', { hub: hub(), project: project(), provider: provider() })
              }
            >
              Link project
            </button>
          </fieldset>
        }
      >
        {(link) => (
          <fieldset disabled={busy()}>
            <SettingRow label="Linked project">
              {link().hub} · {link().project}
            </SettingRow>
            <SettingRow label="Device replica">{link().replica}</SettingRow>
            <SettingRow label="Repository account">
              {link().provider === 'github' ? 'GitHub' : 'GitLab'} · Settings → Repository accounts
            </SettingRow>
            <Show when={link().lastError}>
              <SettingStatus warning>{link().lastError}</SettingStatus>
            </Show>
            <Show when={link().blocked?.length}>
              <details>
                <summary>Blocked records ({link().blocked.length})</summary>
                <ul>
                  <For each={link().blocked}>
                    {(row) => (
                      <li>
                        {row[0]} · {row[1]}
                      </li>
                    )}
                  </For>
                </ul>
                <button onClick={() => void act('retryBlocked')}>Recheck blocked records</button>
              </details>
            </Show>
            <SettingRow label="Last exchange">
              {link().lastSync ? new Date(link().lastSync! * 1000).toLocaleString() : 'Never'}
            </SettingRow>
            <Show when={link().lastError}>
              <button
                onClick={async () => {
                  if (
                    await confirmDialog({
                      title: 'Reconcile this team link?',
                      message:
                        'Download a fresh team replica. Divergent records are kept in personal memory for review.',
                      confirmLabel: 'Reconcile',
                    })
                  )
                    await act('reconcile');
                }}
              >
                Reconcile team link…
              </button>
            </Show>
            <button disabled={link().paused} onClick={() => void act('sync')}>
              Sync now
            </button>
            <button onClick={() => void act('pause', { paused: !link().paused })}>
              {link().paused ? 'Resume sharing' : 'Pause sharing'}
            </button>
          </fieldset>
        )}
      </Show>
      <Show when={error() || status()?.error}>
        <SettingStatus warning>{error() || status()?.error}</SettingStatus>
      </Show>
    </section>
  );
}
