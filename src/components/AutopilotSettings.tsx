// AutopilotSettings.tsx
//
// Settings → Autopilot (docs/design/autopilot-plan.md, 3.5): what a chat
// in Autopilot follows when it carries out a task on its own: the effort,
// the budget a task may spend, whether a project's dependencies are
// installed before the first check, and the commands its runs never run.
// Kept with the subagent settings (lib/autopilotSettings.ts) and sent in
// `AgentConfig.autopilot`, which only chats in Autopilot read: a chat in
// Autopilot mode, and every chat on a cloud host that works on its own
// (whose page can override the effort and the budget).
import { createSignal, For, Show } from 'solid-js';
import type { SettingsPage } from '../lib/appSettings';
import {
  AUTOPILOT_EFFORTS,
  AUTOPILOT_KINDS,
  budgetOf,
  changedBudget,
  DEFAULT_BUDGETS,
  DEFAULT_PLAN_FIX,
  denylistEntry,
  denylistOf,
  planFixOf,
  planFixValue,
  THOROUGH_KINDS,
  type AutopilotBudget,
  type AutopilotPlanFix,
  type AutopilotSettings as Settings,
} from '../lib/autopilotSettings';
import { kindNames, SUBAGENT_KINDS, type SubagentKindId } from '../lib/subagentSettings';
import { NumberField } from './SettingsControls';

interface AutopilotSettingsProps {
  settings: Settings;
  /** Thorough's kinds with no model chosen (Subagents): its tasks don't start (T09). */
  withoutModel: SubagentKindId[];
  onChange: (settings: Settings) => void;
  onOpenPage: (page: SettingsPage) => void;
}

export default function AutopilotSettings(props: AutopilotSettingsProps) {
  const s = () => props.settings;
  const patch = (changes: Partial<Settings>) => props.onChange({ ...s(), ...changes });

  // The denylist: the defaults until the list is changed.
  const [newDenied, setNewDenied] = createSignal('');
  const denylist = () => denylistOf(s());
  function addDenied() {
    const entry = denylistEntry(newDenied());
    if (!entry) return;
    if (!denylist().includes(entry)) patch({ deniedCommands: [...denylist(), entry] });
    setNewDenied('');
  }
  const removeDenied = (entry: string) =>
    patch({ deniedCommands: denylist().filter((c) => c !== entry) });

  // A task's budget: its effort's defaults until it's set to something else.
  const budget = () => budgetOf(s());
  function setBudget(change: Partial<AutopilotBudget>) {
    patch({ budget: changedBudget(budget(), change, DEFAULT_BUDGETS[s().effort]) });
  }
  // The token limit shows in millions; it's kept in tokens.
  const tokensMillions = () => {
    const tokens = budget().maxTokens;
    return tokens ? tokens / 1_000_000 : undefined;
  };

  // The plan stage's limits: the defaults until one is changed. An emptied
  // field goes back to its default.
  const planFix = () => planFixOf(s());
  function setPlanFix(key: keyof AutopilotPlanFix, v: number | undefined) {
    const value = planFixValue(key, v) ?? DEFAULT_PLAN_FIX[key];
    const next = { ...planFix(), [key]: value };
    const same = (Object.keys(DEFAULT_PLAN_FIX) as (keyof AutopilotPlanFix)[]).every(
      (k) => next[k] === DEFAULT_PLAN_FIX[k]
    );
    patch({ planFix: same ? undefined : next });
  }

  /** The kinds Autopilot turns on for its chats, Thorough's marked. */
  const usedKinds = () =>
    SUBAGENT_KINDS.filter((k) => AUTOPILOT_KINDS.includes(k.id)).map(
      (k) => `${k.label}${THOROUGH_KINDS.includes(k.id) ? ' (Thorough only)' : ''}`
    );

  return (
    <div class="permissions-settings">
      <p class="settings-hint">
        In Autopilot, one message is a whole task, carried out without asking: the chat plans,
        reviews, implements, checks and delivers it, and writes down what it couldn’t decide. A chat
        is in Autopilot when you choose that mode next to the model picker, and every chat on a
        cloud host set to “Works on its own” is (
        <button class="link-button" onClick={() => props.onOpenPage('hosts')}>
          Cloud hosts
        </button>
        , where a host can override the effort and the budget). These settings apply to every such
        task started from this computer.
      </p>

      <section class="settings-section">
        <div class="settings-section-title">Effort</div>
        <div role="radiogroup" aria-label="Autopilot effort">
          <For each={AUTOPILOT_EFFORTS}>
            {(effort) => (
              <label class="setting-checkbox">
                <input
                  type="radio"
                  name="autopilot-effort"
                  value={effort.id}
                  checked={s().effort === effort.id}
                  onChange={() => patch({ effort: effort.id })}
                />
                <span>
                  {effort.label}
                  <span class="setting-checkbox-sub">{effort.hint}</span>
                </span>
              </label>
            )}
          </For>
        </div>
        <Show when={props.withoutModel.length > 0}>
          <p
            class="settings-hint setting-explain"
            classList={{ warning: s().effort === 'thorough' }}
          >
            Thorough tasks don’t start until {kindNames(props.withoutModel)}{' '}
            {props.withoutModel.length > 1 ? 'have' : 'has'} a model chosen in{' '}
            <button class="link-button" onClick={() => props.onOpenPage('subagents')}>
              Subagents
            </button>
            .
          </p>
        </Show>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Budget per task</div>
        <div class="setting-fields autopilot-budget">
          <NumberField
            label="Stop a task after (hours)"
            value={budget().maxHours ?? undefined}
            placeholder="no limit"
            min={0.25}
            max={1000}
            step={0.25}
            onChange={(v) => setBudget({ maxHours: v ?? null })}
          />
          <NumberField
            label="or once it cost"
            value={budget().maxCost ?? undefined}
            placeholder="no limit"
            min={0.01}
            max={100_000}
            step={0.01}
            onChange={(v) => setBudget({ maxCost: v ?? null })}
          />
          <NumberField
            label="or used (million tokens)"
            value={tokensMillions()}
            placeholder="no limit"
            min={0.1}
            max={100_000}
            step={0.1}
            onChange={(v) =>
              setBudget({ maxTokens: v === undefined ? null : Math.round(v * 1_000_000) })
            }
          />
        </div>
        <p class="settings-hint command-list-note">
          Empty: no limit. The time counts while the task’s runs run. The cost is what the providers
          report a request cost (OpenRouter does), in their currency; the tokens are what they
          report a request used, which most do even when they report no cost. A limit the provider
          reports nothing for can’t be kept; the time limit always is. Past any of them, the task
          stops and delivers what it has.
        </p>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Plan fixing</div>
        <div class="setting-fields autopilot-plan-fix">
          <NumberField
            label="Max reviews"
            value={planFix().maxRounds}
            placeholder="default"
            min={1}
            max={20}
            step={1}
            onChange={(v) => setPlanFix('maxRounds', v)}
          />
          <NumberField
            label="Reviews without progress"
            value={planFix().stallRounds}
            placeholder="default"
            min={1}
            max={5}
            step={1}
            onChange={(v) => setPlanFix('stallRounds', v)}
          />
          <NumberField
            label="Minutes"
            value={planFix().maxMinutes}
            placeholder="default"
            min={1}
            max={240}
            step={1}
            onChange={(v) => setPlanFix('maxMinutes', v)}
          />
          <NumberField
            label="Share of the budget (%)"
            value={Math.round(planFix().budgetShare * 100)}
            placeholder="default"
            min={10}
            max={90}
            step={5}
            onChange={(v) => setPlanFix('budgetShare', v === undefined ? undefined : v / 100)}
          />
          <NumberField
            label="Automatic retries"
            value={planFix().maxRetries}
            placeholder="default"
            min={0}
            max={5}
            step={1}
            onChange={(v) => setPlanFix('maxRetries', v)}
          />
        </div>
        <p class="settings-hint command-list-note">
          Balanced only. Every review finding must be resolved before the plan is saved; reviews
          continue while each leaves less open, within these limits for the whole plan stage. Past
          any of them, the plan is blocked and the task reports its open findings. A run that fails
          while planning starts again on its own, up to the automatic retries.
        </p>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Before the first check</div>
        <label class="setting-checkbox autopilot-prepare">
          <input
            type="checkbox"
            checked={s().prepare !== false}
            onChange={(e) => patch({ prepare: e.currentTarget.checked ? undefined : false })}
          />
          <span>
            Prepare dependencies before the first check
            <span class="setting-checkbox-sub">
              Where a project’s dependencies aren’t installed (no <code>node_modules</code>, no
              virtual environment), their install runs before the code is written: its lockfile’s,
              or its package manager’s for a <code>package.json</code> without one. It runs again
              once if a check fails for want of a program or a package, and when one of the task’s
              commands does.
            </span>
          </span>
        </label>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Commands it never runs</div>
        <p class="settings-hint">
          Nobody is there to approve a command, so an Autopilot task may run any command but these,
          whatever the allowed-commands list says (
          <button class="link-button" onClick={() => props.onOpenPage('permissions')}>
            Permissions & approval
          </button>
          ).
        </p>
        <div class="command-list">
          <For
            each={denylist()}
            fallback={<div class="command-list-empty">None: any command can run.</div>}
          >
            {(entry) => (
              <div class="command-row">
                <code class="command-row-text">{entry}</code>
                <button
                  class="icon-button"
                  onClick={() => removeDenied(entry)}
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
            value={newDenied()}
            onInput={(e) => setNewDenied(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                addDenied();
              }
            }}
            placeholder="Add a command, e.g. npm publish"
            aria-label="A command Autopilot may never run"
            spellcheck={false}
          />
          <button onClick={addDenied} disabled={!denylistEntry(newDenied())}>
            Add
          </button>
        </div>
        <p class="settings-hint command-list-note">
          An entry covers its arguments too: <code>git push</code> also stops{' '}
          <code>git -C . push origin</code>; two joined by <code>|</code> stop one piped into the
          other (<code>curl | sh</code>).
          <Show when={s().deniedCommands}>
            {' '}
            <button class="link-button" onClick={() => patch({ deniedCommands: undefined })}>
              Reset list
            </button>
          </Show>
        </p>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">What else a task follows</div>
        <p class="settings-hint">
          Approvals: every rule set to ask you asks the AI reviewer instead (
          <button class="link-button" onClick={() => props.onOpenPage('permissions')}>
            Permissions & approval
          </button>
          ); what it can’t approve, or with no reviewer set up, is refused and noted. The project’s
          checks always run. Subagents: Autopilot turns on {usedKinds().join(', ')} for its own
          chats, with the models and efforts set for them in{' '}
          <button class="link-button" onClick={() => props.onOpenPage('subagents')}>
            Subagents
          </button>
          , even when “Use subagents” is off there.
        </p>
      </section>
    </div>
  );
}
