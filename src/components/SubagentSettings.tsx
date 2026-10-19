// SubagentSettings.tsx
//
// Settings → Subagents (docs/design/subagents-plan.md, section 2): the
// master switch, the default subagent model, one row per kind (on/off,
// model, fallbacks, reasoning effort) and the limits. Everything is off by
// default; with the master switch off, chats work exactly as without
// subagents and the rows keep their choices, greyed out, except those
// Autopilot uses for its own chats (Settings → Autopilot, D17): their
// model and effort stay to set. The kinds Thorough adds have no Default:
// their models are chosen here, or Thorough tasks don't start (T09).
import { createResource, createSignal, For, Show } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import type { SettingsPage } from '../lib/appSettings';
import ModelWithFallbacks from './ModelWithFallbacks';
import { NumberField } from './SettingsControls';
import { vfsStatus } from '../lib/virtualFiles';
import { EFFORTS, type Effort } from '../lib/reasoning';
import { AUTOPILOT_KINDS, autopilotKindTag, THOROUGH_KINDS } from '../lib/autopilotSettings';
import {
  kindsWithoutModel,
  LIMIT_FIELDS,
  LIMITS,
  otherProviders,
  SUBAGENT_KINDS,
  type KindModel,
  type KindSettings,
  type SubagentKindId,
  type SubagentSettings as Settings,
} from '../lib/subagentSettings';

interface SubagentSettingsProps {
  settings: Settings;
  /** The providers as currently edited in the settings dialog. */
  providers: Provider[];
  /** The chat's model, for the "code also goes to …" warning (K18). */
  chatModel: ModelSelection | null;
  onChange: (settings: Settings) => void;
  onOpenPage: (page: SettingsPage) => void;
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
      <p class="settings-hint">
        Helpers the agent can send while it works on a request, each in a fresh context of its own,
        handing back a short report so long explorations don’t fill the agent’s context. They use
        more tokens. These settings apply to every chat on this computer.
      </p>

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
              Off: chats work exactly as without subagents, and the kinds below keep their choices
              for later. Autopilot’s chats still use the kinds marked for them.
            </span>
          </span>
        </label>
      </section>

      {/* Never greyed out: Autopilot's kinds set to "Default" use it (D17). */}
      <section class="settings-section">
        <div class="settings-section-title">Default model</div>
        <p class="settings-hint">
          Used by every kind set to “Default”. “Same as the chat” is the model the chat runs on.
        </p>
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

      <section class="settings-section" classList={{ disabled: off() }}>
        <div class="settings-section-title">Kinds</div>
        <p class="settings-hint">
          Each kind: whether ordinary chats use it, its model with fallbacks, and its reasoning
          effort. Kinds marked “Used by Autopilot” also run in Autopilot’s chats, with the model and
          effort set here, even when “Use subagents” is off (
          <button class="link-button" onClick={() => props.onOpenPage('autopilot')}>
            Autopilot
          </button>
          ).
        </p>
        <p class="settings-hint">
          The planner, the judge and the adversarial tester, which Autopilot’s Thorough adds, judge
          what the chat’s model did: they run only on the models chosen for them here, best of
          another model family than your chats’, and Thorough tasks don’t start until each has one.
        </p>
        <For each={SUBAGENT_KINDS}>
          {(kind) => {
            const k = () => s().kinds[kind.id];
            const others = () => otherProviders(props.providers, s(), kind.id, props.chatModel);
            const autopilot = AUTOPILOT_KINDS.includes(kind.id);
            const tag = autopilotKindTag(kind.id);
            // Thorough's kinds: no Default, a model of the user's choosing (T09).
            const chosenOnly = THOROUGH_KINDS.includes(kind.id);
            const unchosen = () =>
              chosenOnly && kindsWithoutModel(s(), props.providers).includes(kind.id);
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
                    <Show when={tag}>
                      <span class="autopilot-kind-tag">{tag}</span>
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
                    specials={
                      chosenOnly
                        ? []
                        : [
                            { value: 'default', label: 'Default' },
                            { value: 'chat', label: 'Same as the chat' },
                          ]
                    }
                    unset="Choose a model…"
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
                  <Show when={unchosen()}>
                    <p
                      class="settings-hint setting-explain"
                      classList={{ warning: s().autopilot.effort === 'thorough' }}
                    >
                      Choose a model for it: Thorough tasks don’t start until it has one.
                    </p>
                  </Show>
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
          <div class="setting-fields">
            <For each={LIMIT_FIELDS}>
              {({ field, label, limit }) => (
                <NumberField
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
            <p class="settings-status warning">
              No effect now: parallel groups, the red check and parallel repair need isolated chats,
              and virtual files aren’t mounted on this computer.{' '}
              <button class="link-button" onClick={() => props.onOpenPage('vfs')}>
                Virtual files
              </button>
            </p>
          </Show>
          <div class="setting-fields">
            <NumberField
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
              <div class="setting-fields">
                <span class="subagent-limits-kind">{kind.label}</span>
                <NumberField
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
                <NumberField
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
