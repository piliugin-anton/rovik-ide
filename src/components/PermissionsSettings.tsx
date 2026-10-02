import { createMemo, createSignal, For, Show, type JSX } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import { formatModelLabel } from '../lib/aiSettings';
import {
  DEFAULT_ALLOWED_COMMANDS,
  sameModel,
  splitWords,
  usesAiReview,
  type AgentPermissions,
  type ApprovalRule,
} from '../lib/permissions';

interface PermissionsSettingsProps {
  permissions: AgentPermissions;
  /** The providers as currently edited in the settings modal. */
  providers: Provider[];
  onChange: (permissions: AgentPermissions) => void;
  /** Jumps to the Model providers page (from the "no models" message). */
  onOpenProviders: () => void;
}

interface ModelOption {
  key: string;
  selection: ModelSelection;
  label: string;
  provider: string;
}

const keyOf = (sel: ModelSelection) => `${sel.providerId}::${sel.modelId}`;

/*
 * ---------------------------------------------------------------------------
 * Small building blocks
 * ---------------------------------------------------------------------------
 */

function Segmented<T extends string>(props: {
  label: string;
  value: T;
  options: { value: T; label: string; title: string }[];
  onChange: (value: T) => void;
}) {
  return (
    <div class="segmented" role="radiogroup" aria-label={props.label}>
      <For each={props.options}>
        {(option) => (
          <button
            type="button"
            class="segmented-option"
            role="radio"
            aria-checked={props.value === option.value}
            classList={{ selected: props.value === option.value }}
            title={option.title}
            onClick={() => props.onChange(option.value)}
          >
            {option.label}
          </button>
        )}
      </For>
    </div>
  );
}

/** One "Label  [control]" line. */
function SettingRow(props: { label: string; children: JSX.Element }) {
  return (
    <div class="setting-row">
      <span class="setting-row-label">{props.label}</span>
      <div class="setting-row-control">{props.children}</div>
    </div>
  );
}

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
  onChange: (rule: ApprovalRule) => void;
}) {
  const explain = () => {
    const rule = props.rule;
    if (!rule.required) return props.offText;
    return rule.by === 'ai'
      ? `Every ${props.noun} is allowed or rejected by the AI reviewer before it runs.`
      : `Every ${props.noun} waits for you to allow or deny it.`;
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
   * AI reviewer models: every model the user added, enabled ones first.
   */

  const modelOptions = createMemo<ModelOption[]>(() =>
    props.providers.flatMap((provider) =>
      [...provider.models]
        .sort((a, b) => Number(b.enabled) - Number(a.enabled))
        .map((model) => {
          const selection = { providerId: provider.id, modelId: model.id };
          return {
            key: keyOf(selection),
            selection,
            label: formatModelLabel(provider.name, model),
            provider: provider.name || provider.baseUrl,
          };
        })
    )
  );

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
    sel ? modelOptions().find((o) => sameModel(o.selection, sel)) : undefined;

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
      {/* Commands */}
      <section class="settings-section">
        <div class="settings-section-title">Commands</div>
        <p class="settings-hint">Shell commands the agent runs in the project folder.</p>

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

      {/* Tool calls */}
      <section class="settings-section">
        <div class="settings-section-title">Tool calls</div>
        <p class="settings-hint">Reading, searching and editing files, and web access.</p>

        <ApprovalControl
          rule={p().toolApproval}
          noun="tool call"
          offText="Tool calls run right away."
          onChange={(toolApproval) => patch({ toolApproval })}
        />
      </section>

      {/* AI reviewer: only when something is approved by AI */}
      <Show when={usesAiReview(p())}>
        <section class="settings-section">
          <div class="settings-section-title">AI reviewer</div>
          <p class="settings-hint">
            Allows or rejects actions set to “By AI reviewer”. Its decision and reason show on each
            tool call. If it can’t decide, you’re asked instead.
          </p>

          <Show
            when={modelOptions().length > 0}
            fallback={
              <div class="command-list-empty">
                No approval models configured. Add models in{' '}
                <button class="link-button" onClick={() => props.onOpenProviders()}>
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
                <For each={providerGroups()}>
                  {([provider, options]) => (
                    <optgroup label={provider}>
                      <For each={options}>{(o) => <option value={o.key}>{o.label}</option>}</For>
                    </optgroup>
                  )}
                </For>
              </select>
            </SettingRow>

            <label class="setting-checkbox">
              <input
                type="checkbox"
                checked={p().approvalTypedInput}
                onChange={(e) => patch({ approvalTypedInput: e.currentTarget.checked })}
              />
              <span>
                Use typed input
                <span class="setting-checkbox-sub">
                  Sends each review as a JSON object instead of a text prompt. Only for the Typesafe
                  Jev model.
                </span>
              </span>
            </label>

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
                        <span class="fallback-label" title={o.label}>
                          {o.label}
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
          </Show>
        </section>
      </Show>
    </div>
  );
}
