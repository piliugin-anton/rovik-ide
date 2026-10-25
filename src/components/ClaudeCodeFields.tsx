// ClaudeCodeFields.tsx
//
// Claude Code's part of a connector's form (ConnectorsSettings.tsx), its
// `ConnectorFields`: how it signs in, in the Connection section; then how
// chats use it, its permissions (with its own tools), its limits, and the
// advanced ones (the program, its config folder, more arguments, bare
// mode), each a labelled row as on the other Settings pages. The same
// fields as claude_code.rs `Settings` (lib/claudeCode.ts).
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
import { NumberField, SettingRow } from './SettingsControls';
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
          aria-label={props.label}
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

/** A text field on its own row, with its line under it when it has one. */
function TextRow(props: {
  label: string;
  value: string;
  placeholder: string;
  onInput: (value: string) => void;
  /** Its line, under it, and that line's tooltip. */
  explain?: string;
  title?: string;
  type?: 'text' | 'password';
}) {
  return (
    <>
      <SettingRow label={props.label}>
        <input
          type={props.type ?? 'text'}
          class="settings-input"
          aria-label={props.label}
          value={props.value}
          onInput={(e) => props.onInput(e.currentTarget.value)}
          placeholder={props.placeholder}
          spellcheck={false}
        />
      </SettingRow>
      <Show when={props.explain}>
        <p class="settings-hint setting-explain" title={props.title}>
          {props.explain}
        </p>
      </Show>
    </>
  );
}

/** Tool rules, one per line, on their own row, with what they do under them. */
function RulesRow(props: {
  label: string;
  value: string;
  placeholder: string;
  rows: number;
  explain: string;
  onInput: (value: string) => void;
}) {
  return (
    <>
      <SettingRow label={props.label}>
        <textarea
          class="settings-textarea"
          aria-label={props.label}
          rows={props.rows}
          value={props.value}
          onInput={(e) => props.onInput(e.currentTarget.value)}
          placeholder={props.placeholder}
          spellcheck={false}
        />
      </SettingRow>
      <p class="settings-hint setting-explain">{props.explain}</p>
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
        <TextRow
          type="password"
          label={settings().auth === 'token' ? 'Token' : 'API key'}
          value={props.connector.apiKey}
          onInput={(apiKey) => props.update({ apiKey })}
          placeholder={settings().auth === 'token' ? 'from `claude setup-token`' : 'sk-ant-…'}
        />
      </Show>
    </>
  );
}

/** How chats use it, its permissions, its limits, and the advanced ones. */
function Options(props: ConnectorFieldsProps) {
  const { settings, patch } = useSettings(props);
  /** Rovik's permissions decide: Claude Code's allowed-tools rules would pass them by. */
  const followsRovik = () => settings().permissionMode === 'rovik';

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
            <RulesRow
              label="Allowed tools"
              rows={2}
              value={settings().allowedTools}
              onInput={(allowedTools) => patch({ allowedTools })}
              placeholder={'Bash(npm test *)\nBash(git diff *)'}
              explain="Rules that run without asking, one per line (`Bash(npm *)`, `WebFetch`)."
            />
          </Show>
          <RulesRow
            label="Denied tools"
            rows={1}
            value={settings().disallowedTools}
            onInput={(disallowedTools) => patch({ disallowedTools })}
            placeholder="Bash(rm *)"
            explain="Rules that never run, one per line (`Bash(rm *)`, `WebFetch`)."
          />
        </section>
      </Show>

      <section class="settings-section">
        <div class="settings-section-title">Limits</div>
        <div class="setting-fields connector-limits">
          <NumberField
            label="Turns per message"
            value={settings().maxTurns}
            placeholder="no limit"
            min={1}
            max={1000}
            onChange={(maxTurns) => patch({ maxTurns })}
          />
          <NumberField
            label="Budget per message, $"
            value={settings().maxBudgetUsd}
            placeholder="no limit"
            min={0.01}
            max={1000}
            step={0.5}
            onChange={(maxBudgetUsd) => patch({ maxBudgetUsd })}
          />
        </div>
        <p
          class="settings-hint setting-explain"
          title="A message’s run stops at either limit (the cost is Claude Code’s own estimate); what it did so far stays. Each reply shows the tokens and the cost it took."
        >
          A message’s run stops at either limit; what it did so far stays.
        </p>
      </section>

      <details class="settings-section settings-details connector-advanced">
        <summary class="settings-details-summary">
          <span class="settings-section-title">Advanced</span>
          <span class="settings-hint">The program, its config folder, arguments, bare mode</span>
        </summary>
        <TextRow
          label="Program"
          value={settings().program}
          onInput={(program) => patch({ program })}
          placeholder="claude"
          explain="`claude` as the shell finds it, or the path to it. Set up installs it if it’s missing."
          title={`Or install it with: ${CLAUDE_CODE_INSTALL}`}
        />
        <TextRow
          label="Config folder"
          value={settings().configDir}
          onInput={(configDir) => patch({ configDir })}
          placeholder="~/.claude (its own)"
          explain="Its login and settings (CLAUDE_CONFIG_DIR) in another folder: a work account, or one shared on a server."
        />
        <TextRow
          label="Arguments"
          value={settings().extraArgs}
          onInput={(extraArgs) => patch({ extraArgs })}
          placeholder="--add-dir ../lib --mcp-config ./mcp.json"
          explain="Passed to every run, as typed on a command line."
        />
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={settings().bare}
            onChange={(e) => patch({ bare: e.currentTarget.checked })}
          />
          <span>
            Bare mode
            <span class="setting-checkbox-sub">
              None of the user’s hooks, skills, plugins, MCP servers or CLAUDE.md. Needs an API key.
            </span>
          </span>
        </label>
      </details>
    </>
  );
}

/** Claude Code's part of a connector's form. */
export const CLAUDE_CODE_FIELDS: ConnectorFields = { Connection, Options };
