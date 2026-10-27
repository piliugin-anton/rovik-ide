// CodexFields.tsx
//
// Codex's part of a connector's form (ConnectorsSettings.tsx), its
// `ConnectorFields`: how it signs in, in the Connection section; then its
// permissions (for its own tools, in Agent mode), and the advanced ones
// (the program, its settings folder, more arguments), each a labelled row
// as on the other Settings pages. The same fields as codex/mod.rs
// `Settings` (lib/codex.ts).
import { Show } from 'solid-js';
import {
  CODEX_AUTH_KINDS,
  CODEX_INSTALL,
  CODEX_PERMISSION_MODES,
  codexSettings,
  type CodexSettings,
} from '../lib/codex';
import { ChoiceSelect, TextRow } from './ConnectorControls';
import type { ConnectorFields, ConnectorFieldsProps } from './connectorFields';

/** Its settings, as read with the defaults, and a change to some of them. */
function useSettings(props: ConnectorFieldsProps) {
  const settings = () => codexSettings(props.connector);
  const patch = (changes: Partial<CodexSettings>) =>
    props.update({ settings: { ...settings(), ...changes } });
  return { settings, patch };
}

/** In the Connection section: how it signs in, and its token or key. */
function Connection(props: ConnectorFieldsProps) {
  const { settings, patch } = useSettings(props);
  return (
    <>
      <ChoiceSelect
        label="Signs in with"
        choices={CODEX_AUTH_KINDS}
        value={settings().auth}
        onChange={(auth) => patch({ auth })}
      />
      <Show when={settings().auth !== 'login'}>
        <TextRow
          type="password"
          label={settings().auth === 'token' ? 'Access token' : 'API key'}
          value={props.connector.apiKey}
          onInput={(apiKey) => props.update({ apiKey })}
          placeholder={settings().auth === 'token' ? 'a Codex access token' : 'sk-…'}
        />
      </Show>
    </>
  );
}

/** Its permissions, and the advanced ones. */
function Options(props: ConnectorFieldsProps) {
  const { settings, patch } = useSettings(props);
  return (
    <>
      <section class="settings-section">
        <div class="settings-section-title">Permissions</div>
        <p class="settings-hint">
          For its own tools, in Agent mode. In Plan mode and Autopilot it runs Rovik’s tools, under
          Permissions & approval.
        </p>
        <ChoiceSelect
          label="Mode"
          choices={CODEX_PERMISSION_MODES}
          value={settings().permissionMode}
          onChange={(permissionMode) => patch({ permissionMode })}
        />
      </section>

      <details class="settings-section settings-details connector-advanced">
        <summary class="settings-details-summary">
          <span class="settings-section-title">Advanced</span>
          <span class="settings-hint">The program, its settings folder, arguments</span>
        </summary>
        <TextRow
          label="Program"
          value={settings().program}
          onInput={(program) => patch({ program })}
          placeholder="codex"
          explain="`codex` as the shell finds it, or the path to it. Set up installs it if it’s missing."
          title={`Or install it with: ${CODEX_INSTALL}`}
        />
        <TextRow
          label="Settings folder"
          value={settings().configDir}
          onInput={(configDir) => patch({ configDir })}
          placeholder="~/.codex (its own)"
          explain="Its login and settings (CODEX_HOME) in another folder: a work account, or one shared on a server."
        />
        <TextRow
          label="Arguments"
          value={settings().extraArgs}
          onInput={(extraArgs) => patch({ extraArgs })}
          placeholder={'-c model_reasoning_summary="detailed"'}
          explain="Passed to every run’s `codex app-server`, as typed on a command line: `-c key=value` sets one of its settings."
        />
      </details>
    </>
  );
}

/** Codex's part of a connector's form. */
export const CODEX_FIELDS: ConnectorFields = { Connection, Options };
