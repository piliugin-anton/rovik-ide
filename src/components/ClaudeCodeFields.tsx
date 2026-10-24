// ClaudeCodeFields.tsx
//
// Claude Code's part of a connector's form (ConnectorsSettings.tsx), its
// `ConnectorFields`: how it signs in, in the Connection section; then how
// chats use it, its permissions (with its own tools), its limits, and the
// advanced ones (the program, its settings folder, more arguments, bare
// mode). The same fields as claude_code.rs `Settings` (lib/claudeCode.ts).
import { For, Show } from 'solid-js';
import {
  AUTH_KINDS,
  CLAUDE_CODE_INSTALL,
  claudeCodeSettings,
  PERMISSION_MODES,
  USE_MODES,
  type Choice,
  type ClaudeCodeSettings,
} from '../lib/claudeCode';
import { SettingRow } from './SettingsControls';
import type { ConnectorFields, ConnectorFieldsProps } from './connectorFields';

/** Its settings, as read with the defaults, and a change to some of them. */
function useSettings(props: ConnectorFieldsProps) {
  const settings = () => claudeCodeSettings(props.connector);
  const patch = (changes: Partial<ClaudeCodeSettings>) =>
    props.update({ settings: { ...settings(), ...changes } });
  return { settings, patch };
}

/** A select of `choices`; under it, the chosen one's line, all of it as its tooltip. */
function ChoiceSelect<T extends string>(props: {
  label: string;
  choices: Choice<T>[];
  value: T;
  onChange: (value: T) => void;
}) {
  const chosen = () => props.choices.find((c) => c.value === props.value);
  return (
    <>
      <SettingRow label={props.label}>
        <select
          class="settings-select"
          value={props.value}
          onChange={(e) => props.onChange(e.currentTarget.value as T)}
        >
          <For each={props.choices}>
            {(c) => (
              <option value={c.value} title={c.hint}>
                {c.label}
              </option>
            )}
          </For>
        </select>
      </SettingRow>
      <Show when={chosen()}>
        {(c) => (
          <p class="settings-hint setting-explain" title={c().detail ?? c().hint}>
            {c().hint}
          </p>
        )}
      </Show>
    </>
  );
}

/** In the Connection section: how it signs in, and its token or key. */
function Connection(props: ConnectorFieldsProps) {
  const { settings, patch } = useSettings(props);
  return (
    <>
      <ChoiceSelect
        label="Signs in with"
        choices={AUTH_KINDS}
        value={settings().auth}
        onChange={(auth) => patch({ auth })}
      />
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
    </>
  );
}

/** How chats use it, its permissions, its limits, and the advanced ones. */
function Options(props: ConnectorFieldsProps) {
  const { settings, patch } = useSettings(props);
  /** Rovik's permissions decide: Claude Code's allowed-tools rules would pass them by. */
  const followsRovik = () => settings().permissionMode === 'rovik';
  const numberValue = (raw: string, min: number, max: number): number | undefined => {
    const n = Number(raw.replace(/[^\d.]/g, ''));
    if (!raw.trim() || !Number.isFinite(n)) return undefined;
    return Math.min(max, Math.max(min, n));
  };

  return (
    <>
      <section class="settings-section">
        <div class="settings-section-title">How chats use it</div>
        <ChoiceSelect
          label="Runs as"
          choices={USE_MODES}
          value={settings().tools}
          onChange={(tools) => patch({ tools })}
        />
      </section>

      <Show when={settings().tools === 'own'}>
        <section class="settings-section">
          <div class="settings-section-title">Permissions</div>
          <ChoiceSelect
            label="Mode"
            choices={PERMISSION_MODES}
            value={settings().permissionMode}
            onChange={(permissionMode) => patch({ permissionMode })}
          />
          <Show when={!followsRovik()}>
            <label>
              Allowed tools
              <textarea
                class="settings-textarea"
                rows={2}
                value={settings().allowedTools}
                onInput={(e) => patch({ allowedTools: e.currentTarget.value })}
                placeholder={'Bash(npm test *)\nBash(git diff *)'}
                spellcheck={false}
              />
              <span class="setting-explain">
                Rules that run without asking, one per line (`Bash(npm *)`, `WebFetch`).
              </span>
            </label>
          </Show>
          <label>
            Denied tools
            <textarea
              class="settings-textarea"
              rows={1}
              value={settings().disallowedTools}
              onInput={(e) => patch({ disallowedTools: e.currentTarget.value })}
              placeholder="Bash(rm *)"
              spellcheck={false}
            />
          </label>
        </section>
      </Show>

      <section class="settings-section">
        <div class="settings-section-title">Limits</div>
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
                patch({ maxTurns: value === undefined ? undefined : Math.round(value) });
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
                patch({ maxBudgetUsd: value });
              }}
            />
          </label>
        </div>
        <p
          class="settings-hint setting-explain"
          title="A message’s run stops at either limit (the cost is Claude Code’s own estimate); what it did so far stays. Each reply shows the tokens and the cost it took."
        >
          A message’s run stops at either limit; what it did so far stays.
        </p>
      </section>

      <section class="settings-section">
        <details class="connector-advanced">
          <summary>Advanced</summary>
          <label>
            Program
            <input
              value={settings().program}
              onInput={(e) => patch({ program: e.currentTarget.value })}
              placeholder="claude"
              spellcheck={false}
            />
            <span class="setting-explain" title={`Or install it with: ${CLAUDE_CODE_INSTALL}`}>
              `claude` as the shell finds it, or the path to it. Set up installs it if it’s missing.
            </span>
          </label>
          <label>
            Settings folder (CLAUDE_CONFIG_DIR)
            <input
              value={settings().configDir}
              onInput={(e) => patch({ configDir: e.currentTarget.value })}
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
              onInput={(e) => patch({ extraArgs: e.currentTarget.value })}
              placeholder="--add-dir ../lib --mcp-config ./mcp.json"
              spellcheck={false}
            />
            <span class="setting-explain">Passed to every run, as typed on a command line.</span>
          </label>
          <label class="checkbox-row">
            <input
              type="checkbox"
              checked={settings().bare}
              onChange={(e) => patch({ bare: e.currentTarget.checked })}
            />
            Bare mode: none of the user’s hooks, skills, plugins, MCP servers or CLAUDE.md (needs an
            API key)
          </label>
        </details>
      </section>
    </>
  );
}

/** Claude Code's part of a connector's form. */
export const CLAUDE_CODE_FIELDS: ConnectorFields = { Connection, Options };
