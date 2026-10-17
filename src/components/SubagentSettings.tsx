// SubagentSettings.tsx
//
// Settings → Subagents (docs/design/subagents-plan.md, section 2): the
// master switch, the default subagent model, one row per kind (on/off,
// model, fallbacks, reasoning effort) and the limits. Everything is off by
// default; with the master switch off, chats work exactly as without
// subagents and the rows keep their choices, greyed out. The Autopilot
// section (docs/design/autopilot-plan.md, 3.5) is for chats in Autopilot,
// whatever the master switch says.
import { createResource, createSignal, For, Show } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import ModelWithFallbacks from './ModelWithFallbacks';
import { vfsStatus } from '../lib/virtualFiles';
import { EFFORTS, type Effort } from '../lib/reasoning';
import { AUTOPILOT_EFFORTS, AUTOPILOT_KINDS } from '../lib/autopilotSettings';
import {
  LIMIT_FIELDS,
  LIMITS,
  numberFieldValue,
  otherProviders,
  SUBAGENT_KINDS,
  type KindModel,
  type KindSettings,
  type SubagentKindId,
  type SubagentSettings as Settings,
} from '../lib/subagentSettings';

interface SubagentSettingsProps {
  settings: Settings;
  /** The providers as currently edited in the settings modal. */
  providers: Provider[];
  /** The chat's model, for the "code also goes to …" warning (K18). */
  chatModel: ModelSelection | null;
  onChange: (settings: Settings) => void;
}

function NumberInput(props: {
  label: string;
  value: number | undefined;
  placeholder: number | string;
  min: number;
  max: number;
  step?: number;
  disabled?: boolean;
  onChange: (value: number | undefined) => void;
}) {
  return (
    <label class="subagent-limit">
      <span>{props.label}</span>
      <input
        type="number"
        class="settings-input subagent-number"
        min={props.min}
        max={props.max}
        step={props.step ?? 1}
        value={props.value ?? ''}
        placeholder={String(props.placeholder)}
        disabled={props.disabled}
        onChange={(e) => {
          const value = numberFieldValue(
            e.currentTarget.value,
            props.min,
            props.max,
            props.step ?? 1
          );
          // Shows what is saved: when that is the value it had already, the
          // field wouldn't be redrawn and would keep the text typed.
          e.currentTarget.value = value === undefined ? '' : String(value);
          props.onChange(value);
        }}
      />
    </label>
  );
}

export default function SubagentSettings(props: SubagentSettingsProps) {
  const s = () => props.settings;
  const patch = (changes: Partial<Settings>) => props.onChange({ ...s(), ...changes });
  const patchKind = (id: SubagentKindId, changes: Partial<KindSettings>) =>
    patch({ kinds: { ...s().kinds, [id]: { ...s().kinds[id], ...changes } } });
  const [limitsOpen, setLimitsOpen] = createSignal(false);
  const off = () => !s().enabled;
  // P5.7: what needs isolated chats can't run without virtual files. When
  // their status can't be read, it isn't known: nothing is turned off.
  const [vfs] = createResource(() => vfsStatus().catch(() => undefined));
  const noIsolation = () => {
    if (vfs.state !== 'ready') return false;
    const status = vfs();
    return status !== undefined && !status.mountpoint;
  };
  const NO_ISOLATION = 'Needs isolated chats: virtual files aren’t mounted on this computer.';

  return (
    <div class="permissions-settings subagent-settings">
      <section class="settings-section">
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={s().enabled}
            onChange={(e) => patch({ enabled: e.currentTarget.checked })}
          />
          <span>
            Use subagents
            <span class="setting-checkbox-sub">
              Helpers for exploring and reviewing the code. Each works in a fresh context of its own
              and hands back a short report. They use more tokens. Off: chats work exactly as
              without subagents.
            </span>
          </span>
        </label>
      </section>

      {/* Never greyed out: Autopilot's kinds set to "Default" use it (D17). */}
      <section class="settings-section">
        <div class="settings-section-title">Default model</div>
        <p class="settings-hint">Used by every kind set to “Default”.</p>
        <ModelWithFallbacks
          providers={props.providers}
          label="Default subagent model"
          value={s().defaultModel}
          specials={[{ value: 'chat' as const, label: 'Same as the chat' }]}
          fallbacks={s().defaultFallbacks}
          onChange={(value) => patch({ defaultModel: value })}
          onFallbacksChange={(defaultFallbacks) => patch({ defaultFallbacks })}
        />
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Autopilot</div>
        <p class="settings-hint">
          A chat in Autopilot mode carries out a task on its own: it plans, reviews, implements,
          checks and delivers it without asking.
        </p>
        <div role="radiogroup" aria-label="Autopilot effort">
          <For each={AUTOPILOT_EFFORTS}>
            {(effort) => (
              <label class="setting-checkbox">
                <input
                  type="radio"
                  name="autopilot-effort"
                  value={effort.id}
                  checked={s().autopilot.effort === effort.id}
                  onChange={() => patch({ autopilot: { ...s().autopilot, effort: effort.id } })}
                />
                <span>
                  {effort.label}
                  <span class="setting-checkbox-sub">{effort.hint}</span>
                </span>
              </label>
            )}
          </For>
        </div>
        <Show when={s().autopilot.effort === 'thorough'}>
          <p class="settings-hint">
            Thorough’s own stages aren’t built yet: for now a Thorough task runs as a Balanced one,
            and its record shows them as skipped.
          </p>
        </Show>
        <p class="settings-hint">
          Autopilot turns on the kinds marked “Used by Autopilot” for its own chats, with the models
          set for them below, even when “Use subagents” is off.
        </p>
      </section>

      <section class="settings-section" classList={{ disabled: off() }}>
        <div class="settings-section-title">Kinds</div>
        <For each={SUBAGENT_KINDS}>
          {(kind) => {
            const k = () => s().kinds[kind.id];
            const others = () => otherProviders(props.providers, s(), kind.id, props.chatModel);
            const autopilot = AUTOPILOT_KINDS.includes(kind.id);
            // Its switch is for ordinary chats; Autopilot's use its model either way.
            const locked = () => off() && !autopilot;
            return (
              <div class="subagent-kind-row" classList={{ 'autopilot-used': autopilot }}>
                <label class="setting-checkbox">
                  <input
                    type="checkbox"
                    checked={k().enabled}
                    disabled={off()}
                    onChange={(e) => patchKind(kind.id, { enabled: e.currentTarget.checked })}
                  />
                  <span>
                    {kind.label}
                    <Show when={autopilot}>
                      <span class="autopilot-kind-tag">Used by Autopilot</span>
                    </Show>
                    <span class="setting-checkbox-sub">
                      {kind.hint} {kind.when}
                    </span>
                  </span>
                </label>
                <div class="subagent-kind-controls">
                  <ModelWithFallbacks<'default' | 'chat'>
                    providers={props.providers}
                    label={`${kind.label} model`}
                    value={k().model}
                    specials={[
                      { value: 'default', label: 'Default' },
                      { value: 'chat', label: 'Same as the chat' },
                    ]}
                    fallbacks={k().fallbacks}
                    disabled={locked()}
                    onChange={(model: KindModel) => patchKind(kind.id, { model })}
                    onFallbacksChange={(fallbacks) => patchKind(kind.id, { fallbacks })}
                  />
                  <select
                    class="settings-select"
                    aria-label={`${kind.label} reasoning effort`}
                    value={k().effort}
                    disabled={locked()}
                    onChange={(e) =>
                      patchKind(kind.id, { effort: e.currentTarget.value as Effort })
                    }
                  >
                    <For each={EFFORTS}>
                      {(effort) => (
                        <option value={effort.id} title={effort.hint}>
                          {effort.id === 'default' ? 'Default effort (as the chat)' : effort.label}
                        </option>
                      )}
                    </For>
                  </select>
                  <Show when={(k().enabled || autopilot) && others().length > 0}>
                    <p class="settings-hint setting-explain warning">
                      {k().enabled ? 'Code' : 'In Autopilot, code'} will also be sent to{' '}
                      {others().join(', ')}.
                    </p>
                  </Show>
                </div>
              </div>
            );
          }}
        </For>
      </section>

      <section class="settings-section" classList={{ disabled: off() }}>
        <button
          class="link-button subagent-limits-toggle"
          aria-expanded={limitsOpen()}
          onClick={() => setLimitsOpen(!limitsOpen())}
        >
          <span aria-hidden="true">{limitsOpen() ? '▾' : '▸'}</span> Limits
        </button>
        <Show when={limitsOpen()}>
          <div class="subagent-limits">
            <For each={LIMIT_FIELDS}>
              {({ field, label, limit }) => (
                <NumberInput
                  label={label}
                  value={s()[field]}
                  placeholder={limit.default}
                  min={limit.min}
                  max={limit.max}
                  disabled={off()}
                  // Whole already: the field's step is 1 (numberFieldValue).
                  onChange={(v) => patch({ [field]: v ?? limit.default })}
                />
              )}
            </For>
          </div>
          <label class="setting-checkbox">
            <input
              type="checkbox"
              checked={s().parallelWorkers}
              disabled={off() || noIsolation()}
              title={noIsolation() ? NO_ISOLATION : undefined}
              onChange={(e) => patch({ parallelWorkers: e.currentTarget.checked })}
            />
            <span>
              Run plan groups in parallel
              <span class="setting-checkbox-sub">
                Steps a plan marks with the same “Group:” are carried out by step workers at the
                same time, each in its own copy of the chat’s files, then joined and checked once.
                Isolated chats only; needs the step worker on.
              </span>
            </span>
          </label>
          <label class="setting-checkbox">
            <input
              type="checkbox"
              checked={s().redCheck}
              disabled={off() || noIsolation()}
              title={noIsolation() ? NO_ISOLATION : undefined}
              onChange={(e) => patch({ redCheck: e.currentTarget.checked })}
            />
            <span>
              Red check for the test writer’s tests
              <span class="setting-checkbox-sub">
                Its new tests also run on the code before the change; one that already passes there
                is flagged, since it may not test the change. Isolated chats only.
              </span>
            </span>
          </label>
          <label class="setting-checkbox">
            <input
              type="checkbox"
              checked={s().parallelRepair}
              disabled={off() || noIsolation()}
              title={noIsolation() ? NO_ISOLATION : undefined}
              onChange={(e) => patch({ parallelRepair: e.currentTarget.checked })}
            />
            <span>
              Fix a parallel group’s failures in parallel
              <span class="setting-checkbox-sub">
                When the checks fail after a group is joined, causes in different workers’ files
                that the failure triage rates high confidence are fixed by those workers at the same
                time, then checked once. Anything the lead owns is fixed by the lead, one at a time.
              </span>
            </span>
          </label>
          <Show when={noIsolation()}>
            <p class="settings-hint">
              Parallel groups, the red check and parallel repair need isolated chats, and virtual
              files aren’t mounted on this computer (see Virtual files).
            </p>
          </Show>
          <div class="subagent-limits">
            <NumberInput
              label="Tokens per run"
              value={s().maxRunTokens}
              placeholder="no limit"
              min={LIMITS.runTokens.min}
              max={LIMITS.runTokens.max}
              step={1000}
              disabled={off()}
              onChange={(v) => patch({ maxRunTokens: v === undefined ? undefined : Math.round(v) })}
            />
          </div>
          <p class="settings-hint">
            Past it, no new subagent starts in that run and the agent goes on alone. It counts what
            providers report: subagents on a provider that reports no usage aren’t limited.
          </p>
          <For each={SUBAGENT_KINDS}>
            {(kind) => (
              <div class="subagent-limits">
                <span class="subagent-limits-kind">{kind.label}</span>
                <NumberInput
                  label="Tool calls"
                  value={s().kinds[kind.id].maxToolCalls}
                  placeholder={kind.toolCalls}
                  min={LIMITS.toolCalls.min}
                  max={LIMITS.toolCalls.max}
                  disabled={off()}
                  onChange={(v) =>
                    patchKind(kind.id, {
                      maxToolCalls: v === undefined ? undefined : Math.round(v),
                    })
                  }
                />
                <NumberInput
                  label="Minutes"
                  value={s().kinds[kind.id].maxMinutes}
                  placeholder={kind.minutes}
                  min={LIMITS.minutes.min}
                  max={LIMITS.minutes.max}
                  step={0.5}
                  disabled={off()}
                  onChange={(v) => patchKind(kind.id, { maxMinutes: v })}
                />
              </div>
            )}
          </For>
          <p class="settings-hint">
            A subagent past its budget writes its report from what it found. Time waiting for your
            approval doesn’t count.
          </p>
        </Show>
      </section>
    </div>
  );
}
