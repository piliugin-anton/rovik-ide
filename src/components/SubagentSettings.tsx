// SubagentSettings.tsx
//
// Settings → Subagents (docs/design/subagents-plan.md, section 2): the
// master switch, the default subagent model, one row per kind (on/off,
// model, fallbacks, reasoning effort) and the limits. Everything is off by
// default; with the master switch off, chats work exactly as without
// subagents and the rows keep their choices, greyed out.
import { createSignal, For, Show } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import ModelWithFallbacks from './ModelWithFallbacks';
import { EFFORTS, type Effort } from '../lib/reasoning';
import {
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
  /** The providers as currently edited in the settings modal. */
  providers: Provider[];
  /** The chat's model, for the "code also goes to …" warning (K18). */
  chatModel: ModelSelection | null;
  onChange: (settings: Settings) => void;
}

function NumberInput(props: {
  label: string;
  value: number | undefined;
  placeholder: number;
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
          const raw = e.currentTarget.value.trim();
          const n = Number(raw);
          props.onChange(
            raw === '' || !Number.isFinite(n)
              ? undefined
              : Math.min(props.max, Math.max(props.min, n))
          );
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

      <section class="settings-section" classList={{ disabled: off() }}>
        <div class="settings-section-title">Default model</div>
        <p class="settings-hint">Used by every kind set to “Default”.</p>
        <ModelWithFallbacks
          providers={props.providers}
          label="Default subagent model"
          value={s().defaultModel}
          specials={[{ value: 'chat' as const, label: 'Same as the chat' }]}
          fallbacks={s().defaultFallbacks}
          disabled={off()}
          onChange={(value) => patch({ defaultModel: value })}
          onFallbacksChange={(defaultFallbacks) => patch({ defaultFallbacks })}
        />
      </section>

      <section class="settings-section" classList={{ disabled: off() }}>
        <div class="settings-section-title">Kinds</div>
        <For each={SUBAGENT_KINDS}>
          {(kind) => {
            const k = () => s().kinds[kind.id];
            const others = () => otherProviders(props.providers, s(), kind.id, props.chatModel);
            return (
              <div class="subagent-kind-row">
                <label class="setting-checkbox">
                  <input
                    type="checkbox"
                    checked={k().enabled}
                    disabled={off()}
                    onChange={(e) => patchKind(kind.id, { enabled: e.currentTarget.checked })}
                  />
                  <span>
                    {kind.label}
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
                    disabled={off()}
                    onChange={(model: KindModel) => patchKind(kind.id, { model })}
                    onFallbacksChange={(fallbacks) => patchKind(kind.id, { fallbacks })}
                  />
                  <select
                    class="settings-select"
                    aria-label={`${kind.label} reasoning effort`}
                    value={k().effort}
                    disabled={off()}
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
                  <Show when={k().enabled && others().length > 0}>
                    <p class="settings-hint setting-explain warning">
                      Code will also be sent to {others().join(', ')}.
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
            <NumberInput
              label="At once per chat"
              value={s().maxAtOnce}
              placeholder={LIMITS.atOnce.default}
              min={LIMITS.atOnce.min}
              max={LIMITS.atOnce.max}
              disabled={off()}
              onChange={(v) => patch({ maxAtOnce: Math.round(v ?? LIMITS.atOnce.default) })}
            />
            <NumberInput
              label="Per run"
              value={s().maxPerRun}
              placeholder={LIMITS.perRun.default}
              min={LIMITS.perRun.min}
              max={LIMITS.perRun.max}
              disabled={off()}
              onChange={(v) => patch({ maxPerRun: Math.round(v ?? LIMITS.perRun.default) })}
            />
            <NumberInput
              label="Review changes from (lines)"
              value={s().reviewMinLines}
              placeholder={LIMITS.reviewMinLines.default}
              min={LIMITS.reviewMinLines.min}
              max={LIMITS.reviewMinLines.max}
              disabled={off()}
              onChange={(v) =>
                patch({ reviewMinLines: Math.round(v ?? LIMITS.reviewMinLines.default) })
              }
            />
            <NumberInput
              label="Step workers from (plan steps)"
              value={s().workerMinSteps}
              placeholder={LIMITS.workerMinSteps.default}
              min={LIMITS.workerMinSteps.min}
              max={LIMITS.workerMinSteps.max}
              disabled={off()}
              onChange={(v) =>
                patch({ workerMinSteps: Math.round(v ?? LIMITS.workerMinSteps.default) })
              }
            />
          </div>
          <div class="subagent-limits">
            <NumberInput
              label="Tokens per run (empty: no limit)"
              value={s().maxRunTokens}
              placeholder={0}
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
