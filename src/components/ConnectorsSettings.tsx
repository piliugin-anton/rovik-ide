// ConnectorsSettings.tsx
//
// Settings → Connectors: the coding harnesses chats can use in place of a
// model provider (lib/connectors.ts), Claude Code first. Each connector is
// a program on the computer the chat runs on; its page says how it's run
// (the program, how it signs in, what it may do, its limits), tests it and
// lists its models as the Model providers page does
// (ProviderModelsTable.tsx). The models enabled here show in the chat's
// model picker, marked as a connector's.
import { createResource, createSignal, For, Show } from 'solid-js';
import type { ConnectorKind, Provider } from '../types';
import { mergeRemoteModels } from '../lib/aiSettings';
import {
  AUTH_KINDS,
  claudeCodeSettings,
  CONNECTOR_CATALOG,
  connectorTitle,
  isConnector,
  kindOfProvider,
  newConnector,
  PERMISSION_MODES,
  probeConnector,
  probeModels,
  probeSummary,
  type ClaudeCodeSettings,
  type ConnectorProbe,
} from '../lib/connectors';
import { confirmDialog } from './ConfirmDialog';
import ProviderModelsTable, { CONNECTOR_COLUMNS } from './ProviderModelsTable';
import { SettingRow } from './SettingsControls';
import { listHosts } from '../lib/hosts';

/** Lets the settings modal show the open form's name and close it with Back. */
export interface ConnectorsSettingsController {
  /** The connector being edited, or null on the list. */
  draftTitle: () => string | null;
  closeDraft: () => void;
}

interface ConnectorsSettingsProps {
  /** Every saved entry: providers and connectors (only the connectors are this page's). */
  providers: Provider[];
  /** Every change to the list (saved form, deletion), saved at once. */
  onChange: (providers: Provider[]) => void;
  controller?: (api: ConnectorsSettingsController) => void;
}

type TestState =
  | { kind: 'idle' }
  | { kind: 'testing' }
  | { kind: 'ok'; probe: ConnectorProbe; added: number; first: boolean }
  | { kind: 'error'; message: string };

export default function ConnectorsSettings(props: ConnectorsSettingsProps) {
  const connectors = () => props.providers.filter(isConnector);
  const setConnectors = (fn: (prev: Provider[]) => Provider[]) => {
    const plain = props.providers.filter((p) => !isConnector(p));
    props.onChange([...plain, ...fn(connectors())]);
  };
  const [draft, setDraft] = createSignal<Provider | null>(null);
  const [test, setTest] = createSignal<TestState>({ kind: 'idle' });
  const [withRequest, setWithRequest] = createSignal(false);
  const [testHost, setTestHost] = createSignal('');
  const [formError, setFormError] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal(false);
  // The cloud hosts, for "Test on": a connector is a program on the
  // computer the chat runs on, so a host has its own installation.
  const [hostsList] = createResource(() => listHosts().catch(() => null));
  const hosts = () => hostsList()?.hosts ?? [];

  // Ignore results from a test that was started before the settings changed.
  let testRun = 0;

  const patch = (changes: Partial<Provider>) => {
    const d = draft();
    if (d) setDraft({ ...d, ...changes });
  };
  const settings = () => claudeCodeSettings(draft() ?? {});
  const patchSettings = (changes: Partial<ClaudeCodeSettings>) => {
    patch({ settings: { ...settings(), ...changes } });
    testRun++;
    setTest({ kind: 'idle' });
  };

  function openDraft(p: Provider) {
    setDraft({ ...p, models: p.models.map((m) => ({ ...m })) });
    setTest({ kind: 'idle' });
    setFormError(null);
    setAdding(false);
    testRun++;
  }

  function closeDraft() {
    setDraft(null);
    setAdding(false);
    testRun++;
  }

  function add(kind: ConnectorKind) {
    openDraft(newConnector(kind));
  }

  async function runTest() {
    const d = draft();
    if (!d) return;
    const run = ++testRun;
    setTest({ kind: 'testing' });
    try {
      const probe = await probeConnector(d, withRequest(), testHost() || null);
      if (run !== testRun) return;
      const current = draft();
      if (!current) return;
      // Existing choices are kept; only the very first fetch enables everything.
      const firstFetch = current.models.length === 0;
      const { models, added } = mergeRemoteModels(current.models, probeModels(probe));
      setDraft({ ...current, models });
      setTest({ kind: 'ok', probe, added, first: firstFetch });
    } catch (err) {
      if (run !== testRun) return;
      setTest({ kind: 'error', message: String(err) });
    }
  }

  const okState = () => {
    const t = test();
    return t.kind === 'ok' ? t : null;
  };
  const errorState = () => {
    const t = test();
    return t.kind === 'error' ? t : null;
  };
  const describe = (m: { id: string }) =>
    okState()?.probe.models.find((x) => x.id === m.id)?.description ?? undefined;

  function saveDraft() {
    const d = draft();
    if (!d) return;
    const name = d.name.trim();
    if (!name) {
      setFormError('A name is required.');
      return;
    }
    const clean: Provider = { ...d, name, settings: { ...settings() } };
    setConnectors((prev) =>
      prev.some((p) => p.id === clean.id)
        ? prev.map((p) => (p.id === clean.id ? clean : p))
        : [...prev, clean]
    );
    closeDraft();
  }

  async function remove(connector: Provider) {
    const enabled = connector.models.filter((m) => m.enabled).length;
    const confirmed = await confirmDialog({
      title: 'Remove this connector?',
      message: (
        <>
          <strong>{connector.name}</strong>
          {connector.models.length > 0
            ? ` and its ${connector.models.length} model${connector.models.length === 1 ? '' : 's'}${
                enabled > 0 ? ` (${enabled} enabled)` : ''
              }`
            : ''}{' '}
          will be removed from Rovik. The program stays installed; chats using it will need another
          model.
        </>
      ),
      confirmLabel: 'Remove',
      danger: true,
    });
    if (confirmed) setConnectors((prev) => prev.filter((p) => p.id !== connector.id));
  }

  props.controller?.({
    draftTitle: () => {
      const d = draft();
      return d ? d.name || 'New connector' : null;
    },
    closeDraft,
  });

  const numberValue = (raw: string, min: number, max: number): number | undefined => {
    const n = Number(raw.replace(/[^\d.]/g, ''));
    if (!raw.trim() || !Number.isFinite(n)) return undefined;
    return Math.min(max, Math.max(min, n));
  };

  return (
    <Show
      when={draft()}
      fallback={
        <>
          <p class="settings-hint">
            A connector is a coding harness installed on the computer a chat runs on, used in place
            of a model provider: the chat hands it the task and its system prompt, picks one of its
            models, and shows what it did, its tokens and its cost with the reply. The harness runs
            its own tools in the project folder, under the permission mode set here, so the IDE’s
            approval settings don’t apply to it. On a cloud host, the host needs its own
            installation.
          </p>
          <For
            each={connectors()}
            fallback={
              <div class="empty-state">
                No connectors yet. Connect Claude Code to use it in chats.
              </div>
            }
          >
            {(c) => (
              <div class="profile-row connector-row">
                <div class="profile-row-info">
                  <span class="profile-row-label">{c.name}</span>
                  <span class="profile-row-sub">
                    {connectorTitle(kindOfProvider(c) ?? 'claude-code')} ·{' '}
                    {claudeCodeSettings(c).program} ·{' '}
                    {PERMISSION_MODES.find((m) => m.value === claudeCodeSettings(c).permissionMode)
                      ?.label ?? claudeCodeSettings(c).permissionMode}
                  </span>
                  <span class="profile-row-sub">
                    {c.models.filter((m) => m.enabled).length} of {c.models.length} models enabled
                  </span>
                </div>
                <div class="profile-row-actions">
                  <button onClick={() => openDraft(c)}>Edit</button>
                  <button class="danger" onClick={() => void remove(c)}>
                    Remove
                  </button>
                </div>
              </div>
            )}
          </For>
          <Show
            when={adding()}
            fallback={
              <button class="add-profile-button" onClick={() => setAdding(true)}>
                + Add connector
              </button>
            }
          >
            <div class="connector-catalog">
              <For each={CONNECTOR_CATALOG}>
                {(entry) => (
                  <button class="connector-catalog-item" onClick={() => add(entry.kind)}>
                    <span class="profile-row-label">{entry.title}</span>
                    <span class="profile-row-sub connector-catalog-about">{entry.about}</span>
                  </button>
                )}
              </For>
              <button class="link-button" onClick={() => setAdding(false)}>
                Cancel
              </button>
            </div>
          </Show>
        </>
      }
    >
      {(d) => (
        <div class="profile-form connector-form">
          <label>
            Name
            <input
              value={d().name}
              onInput={(e) => patch({ name: e.currentTarget.value })}
              placeholder={connectorTitle(kindOfProvider(d()) ?? 'claude-code')}
            />
          </label>

          <label>
            Program
            <input
              value={settings().program}
              onInput={(e) => patchSettings({ program: e.currentTarget.value })}
              placeholder="claude"
              spellcheck={false}
            />
            <span class="setting-explain">
              `claude` as the shell finds it, or the path to it. Install it with{' '}
              <code>{CONNECTOR_CATALOG[0].install}</code>, or a package manager on a server.
            </span>
          </label>

          <SettingRow label="Signs in with">
            <select
              class="settings-select"
              value={settings().auth}
              onChange={(e) =>
                patchSettings({ auth: e.currentTarget.value as ClaudeCodeSettings['auth'] })
              }
            >
              <For each={AUTH_KINDS}>
                {(a) => (
                  <option value={a.value} title={a.hint}>
                    {a.label}
                  </option>
                )}
              </For>
            </select>
          </SettingRow>
          <p class="settings-hint setting-explain">
            {AUTH_KINDS.find((a) => a.value === settings().auth)?.hint}
          </p>
          <Show when={settings().auth !== 'login'}>
            <label>
              {settings().auth === 'token' ? 'Token' : 'API key'}
              <input
                type="password"
                value={d().apiKey}
                onInput={(e) => {
                  patch({ apiKey: e.currentTarget.value });
                  testRun++;
                  setTest({ kind: 'idle' });
                }}
                placeholder={settings().auth === 'token' ? 'from `claude setup-token`' : 'sk-ant-…'}
                spellcheck={false}
              />
            </label>
          </Show>

          <SettingRow label="Permission mode">
            <select
              class="settings-select"
              value={settings().permissionMode}
              onChange={(e) =>
                patchSettings({
                  permissionMode: e.currentTarget.value as ClaudeCodeSettings['permissionMode'],
                })
              }
            >
              <For each={PERMISSION_MODES}>
                {(m) => (
                  <option value={m.value} title={m.hint}>
                    {m.label}
                  </option>
                )}
              </For>
            </select>
          </SettingRow>
          <p class="settings-hint setting-explain">
            {PERMISSION_MODES.find((m) => m.value === settings().permissionMode)?.hint}. Nothing
            waits for a prompt: what the mode and the rules below don’t allow is denied.
          </p>

          <label>
            Allowed tools
            <textarea
              class="settings-textarea"
              rows={2}
              value={settings().allowedTools}
              onInput={(e) => patchSettings({ allowedTools: e.currentTarget.value })}
              placeholder={'Bash(npm test *)\nBash(git diff *)'}
              spellcheck={false}
            />
            <span class="setting-explain">
              Permission rules that run without asking, one per line (`Bash(npm *)`, `WebFetch`).
            </span>
          </label>
          <label>
            Denied tools
            <textarea
              class="settings-textarea"
              rows={1}
              value={settings().disallowedTools}
              onInput={(e) => patchSettings({ disallowedTools: e.currentTarget.value })}
              placeholder="Bash(rm *)"
              spellcheck={false}
            />
          </label>

          <div class="connector-limits">
            <label class="setting-number-field">
              <span>Turns per message</span>
              <input
                type="number"
                class="settings-input setting-number"
                min={1}
                max={1000}
                value={settings().maxTurns ?? ''}
                placeholder="no limit"
                onChange={(e) => {
                  const value = numberValue(e.currentTarget.value, 1, 1000);
                  e.currentTarget.value = value === undefined ? '' : String(Math.round(value));
                  patchSettings({ maxTurns: value === undefined ? undefined : Math.round(value) });
                }}
              />
            </label>
            <label class="setting-number-field">
              <span>Budget per message, $</span>
              <input
                type="number"
                class="settings-input setting-number"
                min={0.01}
                max={1000}
                step={0.5}
                value={settings().maxBudgetUsd ?? ''}
                placeholder="no limit"
                onChange={(e) => {
                  const value = numberValue(e.currentTarget.value, 0.01, 1000);
                  e.currentTarget.value = value === undefined ? '' : String(value);
                  patchSettings({ maxBudgetUsd: value });
                }}
              />
            </label>
          </div>
          <p class="settings-hint setting-explain">
            A message’s run stops at either limit (the cost is Claude Code’s own estimate); what it
            did so far stays. Each reply shows the tokens and the cost it took.
          </p>

          <SettingRow label="System prompt">
            <select
              class="settings-select"
              value={settings().systemPrompt}
              onChange={(e) =>
                patchSettings({
                  systemPrompt: e.currentTarget.value as ClaudeCodeSettings['systemPrompt'],
                })
              }
            >
              <option value="append">Rovik’s after Claude Code’s own</option>
              <option value="replace">Rovik’s alone</option>
            </select>
          </SettingRow>

          <details class="connector-advanced">
            <summary>Advanced</summary>
            <label>
              Settings folder (CLAUDE_CONFIG_DIR)
              <input
                value={settings().configDir}
                onInput={(e) => patchSettings({ configDir: e.currentTarget.value })}
                placeholder="~/.claude (its own)"
                spellcheck={false}
              />
              <span class="setting-explain">
                Another login and settings folder: a work account, or one shared on a server.
              </span>
            </label>
            <label>
              Extra arguments
              <input
                value={settings().extraArgs}
                onInput={(e) => patchSettings({ extraArgs: e.currentTarget.value })}
                placeholder="--add-dir ../lib --mcp-config ./mcp.json"
                spellcheck={false}
              />
              <span class="setting-explain">Passed to every run, as typed on a command line.</span>
            </label>
            <label class="checkbox-row">
              <input
                type="checkbox"
                checked={settings().bare}
                onChange={(e) => patchSettings({ bare: e.currentTarget.checked })}
              />
              Bare mode: none of the user’s hooks, skills, plugins, MCP servers or CLAUDE.md (needs
              an API key)
            </label>
          </details>

          <div class="provider-test-row connector-test-row">
            <button onClick={() => void runTest()} disabled={test().kind === 'testing'}>
              {test().kind === 'testing'
                ? 'Testing…'
                : d().models.length > 0
                  ? 'Test & refresh models'
                  : 'Test connection'}
            </button>
            <label
              class="checkbox-row connector-test-option"
              title="Makes one small request too, to see that it's signed in (a few tokens)"
            >
              <input
                type="checkbox"
                checked={withRequest()}
                onChange={(e) => setWithRequest(e.currentTarget.checked)}
              />
              with a request
            </label>
            <Show when={hosts().length > 0}>
              <select
                class="settings-select connector-test-host"
                value={testHost()}
                onChange={(e) => setTestHost(e.currentTarget.value)}
                aria-label="Where to test"
                title="A connector is a program on the computer the chat runs on: test it where the chats run"
              >
                <option value="">on this computer</option>
                <For each={hosts()}>{(h) => <option value={h.id}>on {h.name}</option>}</For>
              </select>
            </Show>
          </div>
          <Show when={okState()}>
            {(ok) => (
              <div class="provider-test-status ok connector-test-status">
                {probeSummary(ok().probe)}, {ok().probe.models.length} models
                {!ok().first && ok().added > 0 ? ` (${ok().added} new, not enabled)` : ''}
                <For each={ok().probe.notes ?? []}>
                  {(note) => <div class="connector-test-note">{note}</div>}
                </For>
              </div>
            )}
          </Show>
          <Show when={errorState()}>
            {(err) => (
              <div class="provider-test-status error connector-test-status" title={err().message}>
                {err().message}
              </div>
            )}
          </Show>

          <ProviderModelsTable
            providerName={d().name}
            models={d().models}
            onChange={(models) => patch({ models })}
            columns={CONNECTOR_COLUMNS}
            describe={describe}
            emptyHint="Test the connection to list its models (aliases such as sonnet and opus, and full names), or add one by ID below."
          />

          <Show when={formError()}>
            <div class="provider-test-status error">{formError()}</div>
          </Show>

          <div class="profile-form-actions">
            <button onClick={closeDraft}>Cancel</button>
            <button class="primary" onClick={saveDraft}>
              Save
            </button>
          </div>
        </div>
      )}
    </Show>
  );
}
