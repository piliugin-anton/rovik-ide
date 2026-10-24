// ClaudeCodeFields.tsx
//
// Claude Code's settings in a connector's form (ConnectorsSettings.tsx):
// the program, how it signs in, whose tools its runs use, its permission
// mode and limits, its system prompt, and the advanced ones. The same
// fields as claude_code.rs `Settings`. A connector of another kind brings
// its own fields component, with the same props.
import { For, Show } from 'solid-js';
import type { Provider } from '../types';
import {
  AUTH_KINDS,
  claudeCodeSettings,
  CONNECTOR_CATALOG,
  PERMISSION_MODES,
  TOOLS_MODES,
  type ClaudeCodeSettings,
} from '../lib/connectors';
import { SettingRow } from './SettingsControls';

/** What a kind's fields component gets: the same for every kind. */
export interface ConnectorFieldsProps {
  /** The connector being edited. */
  connector: Provider;
  /** A change to what its runs use: its settings, its key. */
  update: (changes: Partial<Provider>) => void;
}

export default function ClaudeCodeFields(props: ConnectorFieldsProps) {
  const settings = () => claudeCodeSettings(props.connector);
  /** Rovik's permissions decide: Claude Code's allowed-tools rules would pass them by. */
  const followsRovik = () => settings().permissionMode === 'rovik';
  const patchSettings = (changes: Partial<ClaudeCodeSettings>) =>
    props.update({ settings: { ...settings(), ...changes } });
  const numberValue = (raw: string, min: number, max: number): number | undefined => {
    const n = Number(raw.replace(/[^\d.]/g, ''));
    if (!raw.trim() || !Number.isFinite(n)) return undefined;
    return Math.min(max, Math.max(min, n));
  };

  return (
    <>
      <label>
        Program
        <input
          value={settings().program}
          onInput={(e) => patchSettings({ program: e.currentTarget.value })}
          placeholder="claude"
          spellcheck={false}
        />
        <span class="setting-explain">
          `claude` as the shell finds it, or the path to it. Set up below installs it if it’s
          missing; or install it with <code>{CONNECTOR_CATALOG[0].install}</code>, or a package
          manager on a server.
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
            value={props.connector.apiKey}
            onInput={(e) => props.update({ apiKey: e.currentTarget.value })}
            placeholder={settings().auth === 'token' ? 'from `claude setup-token`' : 'sk-ant-…'}
            spellcheck={false}
          />
        </label>
      </Show>

      <SettingRow label="Tools">
        <select
          class="settings-select"
          value={settings().tools}
          onChange={(e) => {
            const tools = e.currentTarget.value as ClaudeCodeSettings['tools'];
            // Rovik's prompt in place of Claude Code's, whose tools are
            // off then; after it, with them.
            patchSettings({ tools, systemPrompt: tools === 'rovik' ? 'replace' : 'own' });
          }}
        >
          <For each={TOOLS_MODES}>
            {(m) => (
              <option value={m.value} title={m.hint}>
                {m.label}
              </option>
            )}
          </For>
        </select>
      </SettingRow>
      <p class="settings-hint setting-explain">
        {TOOLS_MODES.find((m) => m.value === settings().tools)?.hint}
      </p>

      <Show when={settings().tools === 'own'}>
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
          {PERMISSION_MODES.find((m) => m.value === settings().permissionMode)?.hint}.{' '}
          {followsRovik()
            ? 'Reads in the project and Claude Code’s read-only commands run without asking.'
            : 'Nothing waits for a prompt: what the mode and the rules below don’t allow is denied.'}
        </p>

        <Show when={!followsRovik()}>
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
        </Show>
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
      </Show>

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
        A message’s run stops at either limit (the cost is Claude Code’s own estimate); what it did
        so far stays. Each reply shows the tokens and the cost it took.
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
          {/* With Rovik's tools, Claude Code has none of its own for its prompt to describe. */}
          <Show when={settings().tools === 'own'}>
            <option value="own">Claude Code’s own</option>
          </Show>
          <option value="append">Rovik’s after Claude Code’s own</option>
          <option value="replace">Rovik’s alone</option>
        </select>
      </SettingRow>
      <p class="settings-hint setting-explain">
        {settings().systemPrompt === 'own' && settings().tools === 'own'
          ? 'Claude Code’s prompt as it is: Rovik’s isn’t sent, and the project’s instructions are the CLAUDE.md Claude Code reads.'
          : 'Rovik’s prompt and the project’s section (its stack, checks and agent instructions) go to every run.'}
      </p>

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
          Bare mode: none of the user’s hooks, skills, plugins, MCP servers or CLAUDE.md (needs an
          API key)
        </label>
      </details>
    </>
  );
}
