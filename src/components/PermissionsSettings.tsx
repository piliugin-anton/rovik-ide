// PermissionsSettings.tsx
//
// Settings → Permissions & approval (lib/permissions.ts): what the agent
// may do in every chat on this computer, and what waits for an approval
// first. Commands are restricted by an allowed list (or "any command"),
// the hosts the agent reaches by another (or "any host"); commands, file
// edits and the other tool calls each have an approval rule: none, by
// you, or by the AI reviewer model chosen at the end of the page. The
// backend enforces them (agent_run/permissions.rs). What happens while a
// run goes and when it ends is on Settings → Agent runs.
import { createMemo, For, Show } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import type { SettingsPage } from '../lib/appSettings';
import { formatModelLabel } from '../lib/aiSettings';
import { isConnector } from '../lib/connectors';
import { APPROVE_AT, isJevModel, REJECT_AT } from '../lib/aiApproval';
import {
  DEFAULT_ALLOWED_COMMANDS,
  normalizeHostEntry,
  sameModel,
  splitWords,
  usesAiReview,
  type AgentPermissions,
  type ApprovalRule,
} from '../lib/permissions';
import { Segmented, SettingRow } from './SettingsControls';
import { createSignal } from 'solid-js';

interface PermissionsSettingsProps {
  permissions: AgentPermissions;
  /** The providers as currently edited in the settings dialog. */
  providers: Provider[];
  onChange: (permissions: AgentPermissions) => void;
  onOpenPage: (page: SettingsPage) => void;
}

interface ModelOption {
  key: string;
  selection: ModelSelection;
  label: string;
  provider: string;
  enabled: boolean;
}

const keyOf = (sel: ModelSelection) => `${sel.providerId}::${sel.modelId}`;

type ApprovalChoice = 'off' | 'user' | 'ai';

const APPROVAL_OPTIONS: { value: ApprovalChoice; label: string; title: string }[] = [
  { value: 'off', label: 'Off', title: 'Runs right away, without approval' },
  { value: 'user', label: 'By you', title: 'Each call pauses until you allow or deny it' },
  {
    value: 'ai',
    label: 'By AI reviewer',
    title: 'The AI reviewer below allows or rejects each call',
  },
];

const choiceOf = (rule: ApprovalRule): ApprovalChoice => (rule.required ? rule.by : 'off');

const ruleOf = (choice: ApprovalChoice, previous: ApprovalRule): ApprovalRule =>
  choice === 'off' ? { ...previous, required: false } : { required: true, by: choice };

/** One "Approval" row and a sentence saying what the choice means. */
function ApprovalControl(props: {
  rule: ApprovalRule;
  /** "command" / "tool call", for the explanation. */
  noun: string;
  /** Said when approval is off. */
  offText: string;
  /** Said for "By you", when the generic sentence isn't enough. */
  userText?: string;
  onChange: (rule: ApprovalRule) => void;
}) {
  const explain = () => {
    const rule = props.rule;
    if (!rule.required) return props.offText;
    return rule.by === 'ai'
      ? `Every ${props.noun} is allowed or rejected by the AI reviewer before it runs.`
      : (props.userText ?? `Every ${props.noun} waits for you to allow or deny it.`);
  };

  return (
    <>
      <SettingRow label="Approval">
        <Segmented
          label={`Approval for each ${props.noun}`}
          value={choiceOf(props.rule)}
          options={APPROVAL_OPTIONS}
          onChange={(choice) => props.onChange(ruleOf(choice, props.rule))}
        />
      </SettingRow>
      <p class="settings-hint setting-explain">{explain()}</p>
    </>
  );
}

/*
 * ---------------------------------------------------------------------------
 * The page
 * ---------------------------------------------------------------------------
 */

export default function PermissionsSettings(props: PermissionsSettingsProps) {
  const [newCommand, setNewCommand] = createSignal('');

  const p = () => props.permissions;
  const patch = (changes: Partial<AgentPermissions>) => props.onChange({ ...p(), ...changes });

  /*
   * Allowed commands
   */

  function addCommand() {
    const entry = splitWords(newCommand()).join(' ');
    if (!entry) return;
    if (!p().allowedCommands.includes(entry)) {
      patch({ allowedCommands: [...p().allowedCommands, entry] });
    }
    setNewCommand('');
  }

  const removeCommand = (entry: string) =>
    patch({ allowedCommands: p().allowedCommands.filter((c) => c !== entry) });

  const isDefaultList = () =>
    p().allowedCommands.length === DEFAULT_ALLOWED_COMMANDS.length &&
    p().allowedCommands.every((c, i) => c === DEFAULT_ALLOWED_COMMANDS[i]);

  /*
   * Allowed hosts
   */

  const [newHost, setNewHost] = createSignal('');
  const [hostProblem, setHostProblem] = createSignal<string | null>(null);

  function addHost() {
    const typed = newHost().trim();
    if (!typed) return;
    const entry = normalizeHostEntry(typed);
    if (!entry) {
      setHostProblem(
        `“${typed}” isn’t a host: type a name (docs.rs), its subdomains (*.github.com) or an address.`
      );
      return;
    }
    if (!p().allowedHosts.includes(entry)) {
      patch({ allowedHosts: [...p().allowedHosts, entry] });
    }
    setNewHost('');
    setHostProblem(null);
  }

  const removeHost = (entry: string) =>
    patch({ allowedHosts: p().allowedHosts.filter((h) => h !== entry) });

  /*
   * AI reviewer models: the enabled models, as in the chat's model picker.
   * A saved choice whose model was turned off is still found (and marked),
   * so the page can say so instead of losing it.
   */

  const allOptions = createMemo<ModelOption[]>(() =>
    props.providers.flatMap((provider) =>
      provider.models.map((model) => {
        const selection = { providerId: provider.id, modelId: model.id };
        return {
          key: keyOf(selection),
          selection,
          label: formatModelLabel(provider.name, model),
          provider: isConnector(provider)
            ? `Connector · ${provider.name}`
            : provider.name || provider.baseUrl,
          enabled: model.enabled,
        };
      })
    )
  );

  /** What can be picked: enabled models only. */
  const modelOptions = createMemo(() => allOptions().filter((o) => o.enabled));

  const providerGroups = createMemo(() => {
    const groups = new Map<string, ModelOption[]>();
    for (const option of modelOptions()) {
      const list = groups.get(option.provider) ?? [];
      list.push(option);
      groups.set(option.provider, list);
    }
    return [...groups.entries()];
  });

  const optionFor = (sel: ModelSelection | null) =>
    sel ? allOptions().find((o) => sameModel(o.selection, sel)) : undefined;

  const reviewer = () => optionFor(p().approvalModel);

  /** Saved fallbacks that still exist, in order. */
  const fallbacks = () =>
    p()
      .approvalFallbacks.map((sel) => optionFor(sel))
      .filter((o): o is ModelOption => !!o);

  function setReviewer(key: string) {
    const option = modelOptions().find((o) => o.key === key);
    patch({
      approvalModel: option?.selection ?? null,
      // A model can't be its own fallback.
      approvalFallbacks: p().approvalFallbacks.filter((f) => !sameModel(f, option?.selection)),
    });
  }

  function addFallback(key: string) {
    const option = modelOptions().find((o) => o.key === key);
    if (!option) return;
    patch({ approvalFallbacks: [...p().approvalFallbacks, option.selection] });
  }

  const removeFallback = (sel: ModelSelection) =>
    patch({ approvalFallbacks: p().approvalFallbacks.filter((f) => !sameModel(f, sel)) });

  /** Models that can still be added as a fallback. */
  const fallbackCandidates = () =>
    modelOptions().filter(
      (o) =>
        !sameModel(o.selection, p().approvalModel) &&
        !p().approvalFallbacks.some((f) => sameModel(f, o.selection))
    );

  return (
    <div class="permissions-settings">
      <p class="settings-hint">
        What the agent may do in every chat on this computer, and what waits for an approval first.
        An approval by you shows in the chat and the editor; on a cloud host, who answers while
        you’re away is set per host (
        <button class="link-button" onClick={() => props.onOpenPage('hosts')}>
          Cloud hosts
        </button>
        ). Autopilot’s tasks follow their own command rule and ask the AI reviewer instead of you (
        <button class="link-button" onClick={() => props.onOpenPage('autopilot')}>
          Autopilot
        </button>
        ).
      </p>

      {/* Commands */}
      <section class="settings-section">
        <div class="settings-section-title">Commands</div>
        <p class="settings-hint">
          Shell commands the agent runs in the project folder, and the project’s checks after a run.
          One may start with a <code>cd</code> into a folder of the project: only what runs there
          needs to be listed.
        </p>

        <SettingRow label="Can run">
          <Segmented
            label="Which commands can run"
            value={p().allowAnyCommand ? 'any' : 'listed'}
            options={[
              { value: 'listed', label: 'Listed only', title: 'Only the commands listed below' },
              { value: 'any', label: 'Any command', title: 'No list: every command is permitted' },
            ]}
            onChange={(v) => patch({ allowAnyCommand: v === 'any' })}
          />
        </SettingRow>

        <Show when={!p().allowAnyCommand}>
          <div class="command-list">
            <For
              each={p().allowedCommands}
              fallback={<div class="command-list-empty">No commands listed: none can run.</div>}
            >
              {(entry) => (
                <div class="command-row">
                  <code class="command-row-text">{entry}</code>
                  <button
                    class="icon-button"
                    onClick={() => removeCommand(entry)}
                    aria-label={`Remove ${entry}`}
                    title="Remove"
                  >
                    ×
                  </button>
                </div>
              )}
            </For>
          </div>

          <div class="provider-manual-row">
            <input
              value={newCommand()}
              onInput={(e) => setNewCommand(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  addCommand();
                }
              }}
              placeholder="Add a command, e.g. npm run build"
              spellcheck={false}
            />
            <button onClick={addCommand} disabled={!newCommand().trim()}>
              Add
            </button>
          </div>
          <p class="settings-hint command-list-note">
            An entry covers its arguments too: <code>git diff</code> also allows{' '}
            <code>git diff --stat</code>. Unlisted commands are refused without asking.
            <Show when={!isDefaultList()}>
              {' '}
              <button
                class="link-button"
                onClick={() => patch({ allowedCommands: [...DEFAULT_ALLOWED_COMMANDS] })}
              >
                Reset list
              </button>
            </Show>
          </p>
        </Show>

        <ApprovalControl
          rule={p().commandApproval}
          noun="command"
          offText="Allowed commands run right away."
          onChange={(commandApproval) => patch({ commandApproval })}
        />
      </section>

      {/* Network */}
      <section class="settings-section">
        <div class="settings-section-title">Network</div>
        <p class="settings-hint">
          Hosts the agent may reach: its web search, the pages it opens and screenshots, its
          commands, and Claude Code’s and Codex’s runs, which have no other way out and need their
          own service’s hosts listed. This computer’s own services (localhost) are always reachable.
        </p>

        <SettingRow label="Can reach">
          <Segmented
            label="Which hosts can be reached"
            value={p().allowAnyHost ? 'any' : 'listed'}
            options={[
              { value: 'listed', label: 'Listed only', title: 'Only the hosts listed below' },
              { value: 'any', label: 'Any host', title: 'No list: every host can be reached' },
            ]}
            onChange={(v) => patch({ allowAnyHost: v === 'any' })}
          />
        </SettingRow>

        <Show when={!p().allowAnyHost}>
          <div class="command-list">
            <For
              each={p().allowedHosts}
              fallback={
                <div class="command-list-empty">No hosts listed: only localhost is reached.</div>
              }
            >
              {(entry) => (
                <div class="command-row">
                  <code class="command-row-text">{entry}</code>
                  <button
                    class="icon-button"
                    onClick={() => removeHost(entry)}
                    aria-label={`Remove ${entry}`}
                    title="Remove"
                  >
                    ×
                  </button>
                </div>
              )}
            </For>
          </div>

          <div class="provider-manual-row">
            <input
              value={newHost()}
              onInput={(e) => {
                setNewHost(e.currentTarget.value);
                setHostProblem(null);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  addHost();
                }
              }}
              placeholder="Add a host, e.g. docs.rs or *.github.com"
              aria-label="Host to allow"
              spellcheck={false}
            />
            <button onClick={addHost} disabled={!newHost().trim()}>
              Add
            </button>
          </div>
          <Show when={hostProblem()}>
            <p class="settings-hint setting-explain warning" role="alert">
              {hostProblem()}
            </p>
          </Show>
          <p class="settings-hint command-list-note">
            <code>docs.rs</code> matches that host alone, <code>*.github.com</code> its subdomains.
            Web search goes to <code>html.duckduckgo.com</code>. Other hosts are refused without
            asking.
          </p>
        </Show>
      </section>

      {/* File edits */}
      <section class="settings-section">
        <div class="settings-section-title">File edits</div>
        <p class="settings-hint">Files the agent creates or changes.</p>

        <ApprovalControl
          rule={p().editApproval}
          noun="file edit"
          offText="Edits are written right away."
          userText="You review each edit in the editor and accept or reject its changes one by one."
          onChange={(editApproval) => patch({ editApproval })}
        />
      </section>

      {/* Other tool calls */}
      <section class="settings-section">
        <div class="settings-section-title">Other tool calls</div>
        <p class="settings-hint">Reading and searching files, and web access.</p>

        <ApprovalControl
          rule={p().toolApproval}
          noun="tool call"
          offText="Tool calls run right away."
          onChange={(toolApproval) => patch({ toolApproval })}
        />
      </section>

      {/* MCP tool calls (docs/design/mcp-plan.md, Q07) */}
      <section class="settings-section">
        <div class="settings-section-title">MCP tool calls</div>
        <p class="settings-hint">
          The tools of your MCP servers (Settings → MCP servers), and of the servers Claude Code and
          Codex have of their own. They can change things outside the project: an issue, a message,
          a database.
        </p>

        <ApprovalControl
          rule={p().mcpApproval}
          noun="MCP tool call"
          offText="MCP tool calls run right away."
          onChange={(mcpApproval) => patch({ mcpApproval })}
        />
      </section>

      {/* AI reviewer: only when something is approved by AI */}
      <Show when={usesAiReview(p())}>
        <section class="settings-section">
          <div class="settings-section-title">AI reviewer</div>
          <p class="settings-hint">
            Allows or rejects the actions set to “By AI reviewer”. Its decision and reason show on
            each tool call. If it can’t decide, you’re asked instead. Pushing, deleting, deploying
            and touching secret files always ask you.
          </p>

          <Show
            when={modelOptions().length > 0}
            fallback={
              <div class="command-list-empty">
                No models enabled. Add or enable models in{' '}
                <button class="link-button" onClick={() => props.onOpenPage('providers')}>
                  Model providers
                </button>
                , then select one here.
              </div>
            }
          >
            <SettingRow label="Model">
              <select
                class="settings-select"
                value={reviewer()?.key ?? ''}
                onChange={(e) => setReviewer(e.currentTarget.value)}
              >
                <option value="">Select a model…</option>
                {/* Keeps a turned-off choice visible instead of resetting the selector. */}
                <Show when={reviewer() && !reviewer()!.enabled}>
                  <option value={reviewer()!.key}>{reviewer()!.label} (turned off)</option>
                </Show>
                <For each={providerGroups()}>
                  {([provider, options]) => (
                    <optgroup label={provider}>
                      <For each={options}>{(o) => <option value={o.key}>{o.label}</option>}</For>
                    </optgroup>
                  )}
                </For>
              </select>
            </SettingRow>

            <Show when={reviewer() && !reviewer()!.enabled}>
              <p class="settings-hint setting-explain warning">
                This model is turned off in Model providers, so it isn’t used. Turn it on there or
                pick another; until then you’re asked instead.
              </p>
            </Show>

            <Show when={!reviewer()}>
              <p class="settings-hint setting-explain warning">
                {p().approvalModel
                  ? 'The selected model was removed. Until you pick another, you’re asked instead.'
                  : 'No model selected yet. Until you pick one, you’re asked instead.'}
              </p>
            </Show>

            <Show when={reviewer()}>
              <SettingRow label="Fallbacks">
                <div class="fallback-list">
                  <For each={fallbacks()}>
                    {(o, i) => (
                      <div class="fallback-row">
                        <span class="fallback-order">{i() + 1}.</span>
                        <span
                          class="fallback-label"
                          classList={{ off: !o.enabled }}
                          title={o.enabled ? o.label : `${o.label}: turned off, skipped`}
                        >
                          {o.label}
                          {o.enabled ? '' : ' (turned off)'}
                        </span>
                        <button
                          class="icon-button"
                          onClick={() => removeFallback(o.selection)}
                          aria-label={`Remove fallback ${o.label}`}
                          title="Remove"
                        >
                          ×
                        </button>
                      </div>
                    )}
                  </For>
                  <Show when={fallbackCandidates().length > 0}>
                    <select
                      class="settings-select"
                      value=""
                      onChange={(e) => {
                        addFallback(e.currentTarget.value);
                        e.currentTarget.value = '';
                      }}
                    >
                      <option value="">
                        {fallbacks().length
                          ? 'Add another fallback…'
                          : 'Add a fallback (optional)…'}
                      </option>
                      <For each={fallbackCandidates()}>
                        {(o) => <option value={o.key}>{o.label}</option>}
                      </For>
                    </select>
                  </Show>
                </div>
              </SettingRow>
              <Show when={fallbacks().length > 0}>
                <p class="settings-hint setting-explain">
                  Tried in this order when the reviewer model fails.
                </p>
              </Show>
            </Show>

            <label class="setting-checkbox">
              <input
                type="checkbox"
                checked={p().approvalTypedInput}
                onChange={(e) => patch({ approvalTypedInput: e.currentTarget.checked })}
              />
              <span>
                Ask with typed checks instead of a prompt
                <span class="setting-checkbox-sub">
                  Only for TypeSafe’s Jev reviewer models, which don’t answer chat prompts: the
                  action goes as structured data with named yes/no checks, and the model answers a
                  probability for each. Approved when every check is at least{' '}
                  {Math.round(APPROVE_AT * 100)}% yes, rejected when any is{' '}
                  {Math.round(REJECT_AT * 100)}% or less; in between, you’re asked. Leave it off for
                  ordinary chat models.
                </span>
              </span>
            </label>

            <Show
              when={
                reviewer() && isJevModel(reviewer()!.selection.modelId) && !p().approvalTypedInput
              }
            >
              <p class="settings-hint setting-explain warning">
                Jev doesn’t answer chat prompts. Turn on typed checks to use it as the reviewer.
              </p>
            </Show>
            <Show
              when={
                reviewer() && !isJevModel(reviewer()!.selection.modelId) && p().approvalTypedInput
              }
            >
              <p class="settings-hint setting-explain warning">
                Typed checks need a Jev model; this one probably only answers chat prompts.
              </p>
            </Show>
          </Show>
        </section>
      </Show>

      <p class="settings-hint">
        What happens while the agent works and when it finishes (pauses, its checklist, the checks
        on its changes) is set in{' '}
        <button class="link-button" onClick={() => props.onOpenPage('agent')}>
          Agent runs
        </button>
        .
      </p>
    </div>
  );
}
