// HostAutonomy.tsx
//
// Settings → Cloud hosts → While nobody is there → Works on its own
// (autopilot-plan 3.5, H01): what the host's Autopilot tasks follow over
// this computer's Settings → Autopilot, and where their results go. Each
// change is saved at once (`hosts_set_autonomy`) and checked there; the
// hosts list then loads again with what was kept.
import { createSignal, Show } from 'solid-js';
import {
  autonomyOf,
  setAutonomy,
  testDelivery,
  type AutonomySettings,
  type Delivery,
  type DeliveryTest,
  type Host,
} from '../lib/hosts';
import { planFixValue, type AutopilotPlanFix } from '../lib/autopilotSettings';
import { NumberField } from './SettingsControls';

type PushBranch = Extract<Delivery, { kind: 'pushBranch' }>;

export default function HostAutonomy(props: {
  host: Host;
  /** After a change was saved. */
  onSaved: () => Promise<void> | void;
}) {
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);
  // "Push to a branch" chosen, its key not in yet: nothing saved until it is.
  const [choosingPush, setChoosingPush] = createSignal(false);
  // "Test delivery" (H05): going, then what it found or why it didn't run.
  const [testing, setTesting] = createSignal(false);
  const [tested, setTested] = createSignal<DeliveryTest | null>(null);
  const [testError, setTestError] = createSignal<string | null>(null);
  const block = () => autonomyOf(props.host);
  const push = (): PushBranch | null => {
    const delivery = block().delivery;
    return delivery.kind === 'pushBranch' ? delivery : null;
  };
  const pushing = () => !!push() || choosingPush();
  const name = (group: string) => `autonomy-${group}-${props.host.id}`;

  async function save(change: Partial<AutonomySettings>) {
    setSaving(true);
    setError(null);
    try {
      await setAutonomy(props.host.id, { ...block(), ...change });
      await props.onSaved();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  function keepOnHost() {
    setChoosingPush(false);
    if (push()) void save({ delivery: { kind: 'keepOnHost' } });
  }

  function setDeployKey(text: string) {
    const deployKey = text.trim();
    if (!deployKey && !push()) return;
    void save({ delivery: { kind: 'pushBranch', deployKey, openPr: push()?.openPr ?? false } });
  }

  async function runTest() {
    setTesting(true);
    setTested(null);
    setTestError(null);
    try {
      setTested(await testDelivery(props.host.id));
    } catch (err) {
      setTestError(String(err));
    } finally {
      setTesting(false);
    }
  }

  /** A limit left empty: as in this computer's settings. */
  function setLimit(field: 'maxHours' | 'maxCost' | 'maxTokens', value: number | undefined) {
    const { maxTokens, ...limits } = {
      maxHours: block().budget?.maxHours ?? null,
      maxCost: block().budget?.maxCost ?? null,
      maxTokens: block().budget?.maxTokens ?? null,
      [field]: value ?? null,
    };
    const budget = { ...limits, ...(maxTokens ? { maxTokens } : {}) };
    const empty = budget.maxHours === null && budget.maxCost === null && !maxTokens;
    void save({ budget: empty ? undefined : budget });
  }
  /** A plan-fix field left empty: as in this computer's settings. */
  function setPlanFix(field: keyof AutopilotPlanFix, value: number | undefined) {
    const kept = planFixValue(field, value);
    const next: Partial<AutopilotPlanFix> = { ...block().planFix };
    if (kept === undefined) delete next[field];
    else next[field] = kept;
    void save({ planFix: Object.keys(next).length ? next : undefined });
  }

  // The token limit shows in millions; it's kept in tokens.
  const tokensMillions = () => {
    const tokens = block().budget?.maxTokens;
    return tokens ? tokens / 1_000_000 : undefined;
  };

  const effortRadio = (label: string, effort: AutonomySettings['effort']) => (
    <label class="setting-checkbox">
      <input
        type="radio"
        name={name('effort')}
        checked={block().effort === effort}
        disabled={saving()}
        onChange={() => void save({ effort })}
      />
      {label}
    </label>
  );

  return (
    <div class="host-autonomy">
      <div class="autopilot-subtitle">Effort</div>
      <div role="radiogroup" aria-label="Effort on this server">
        {effortRadio('As in Settings → Autopilot', undefined)}
        {effortRadio('Balanced', 'balanced')}
        {effortRadio('Thorough', 'thorough')}
      </div>

      <div class="autopilot-subtitle">Approvals</div>
      <p class="settings-hint host-autonomy-note">
        The AI reviewer decides. What it can’t approve is refused and reported.
      </p>

      <div class="autopilot-subtitle">Delivery</div>
      <div role="radiogroup" aria-label="Where a task’s result goes">
        <label class="setting-checkbox">
          <input
            type="radio"
            name={name('delivery')}
            checked={!pushing()}
            disabled={saving()}
            onChange={keepOnHost}
          />
          Keep the result on the server for this computer
        </label>
        <label class="setting-checkbox">
          <input
            type="radio"
            name={name('delivery')}
            checked={pushing()}
            disabled={saving()}
            onChange={() => setChoosingPush(true)}
          />
          <span>
            Push to a branch <code>rovik/&lt;task&gt;</code> on the project’s remote
          </span>
        </label>
      </div>
      <Show when={pushing()}>
        <div class="host-autonomy-push">
          <label class="host-autonomy-field">
            Deploy key on the server
            <input
              value={push()?.deployKey ?? ''}
              spellcheck={false}
              placeholder="~/.ssh/rovik_deploy"
              disabled={saving()}
              onChange={(e) => setDeployKey(e.currentTarget.value)}
            />
          </label>
          <Show when={!push()}>
            <p class="settings-hint host-autonomy-note">
              Its path on the server, never the key itself. Until it’s in, results stay on the
              server.
            </p>
          </Show>
          <label class="setting-checkbox">
            <input
              type="checkbox"
              checked={push()?.openPr ?? false}
              disabled={saving() || !push()}
              onChange={(e) => {
                const p = push();
                if (p) void save({ delivery: { ...p, openPr: e.currentTarget.checked } });
              }}
            />
            <span>
              Open a pull request (needs <code>gh</code> logged in on the server)
            </span>
          </label>
          <Show when={push()}>
            <div class="host-autonomy-test">
              <button type="button" disabled={testing()} onClick={() => void runTest()}>
                {testing() ? 'Testing…' : 'Test delivery'}
              </button>
              <span class="settings-hint">
                Pushes an empty branch to the open project’s remote, then deletes it.
              </span>
            </div>
            <Show when={tested()}>
              {(t) => (
                <p
                  class="settings-hint host-autonomy-note host-autonomy-tested"
                  classList={{ warning: !!t().problem }}
                  role="status"
                >
                  {t().problem ?? (
                    <>
                      Pushed <code>{t().branch}</code> to {t().remote} and deleted it.
                    </>
                  )}
                  <Show when={t().pullRequests}>{(line) => <> {line()}</>}</Show>
                </p>
              )}
            </Show>
            <Show when={testError()}>
              <p class="settings-hint setting-explain warning" role="alert">
                {testError()}
              </p>
            </Show>
          </Show>
        </div>
      </Show>

      <div class="autopilot-subtitle">Budget</div>
      <div class="setting-fields host-autonomy-budget">
        <NumberField
          label="Stop a task after (hours)"
          value={block().budget?.maxHours ?? undefined}
          placeholder="as set"
          min={0.25}
          max={1000}
          step={0.25}
          disabled={saving()}
          onChange={(v) => setLimit('maxHours', v)}
        />
        <NumberField
          label="or once it cost"
          value={block().budget?.maxCost ?? undefined}
          placeholder="as set"
          min={0.01}
          max={100_000}
          step={0.01}
          disabled={saving()}
          onChange={(v) => setLimit('maxCost', v)}
        />
        <NumberField
          label="or used (million tokens)"
          value={tokensMillions()}
          placeholder="as set"
          min={0.1}
          max={100_000}
          step={0.1}
          disabled={saving()}
          onChange={(millions) =>
            setLimit(
              'maxTokens',
              millions === undefined ? undefined : Math.round(millions * 1_000_000)
            )
          }
        />
      </div>
      <p class="settings-hint host-autonomy-note">Empty: as in Settings → Autopilot.</p>

      <div class="autopilot-subtitle">Plan fixing</div>
      <div class="setting-fields host-autonomy-plan-fix">
        <NumberField
          label="Max reviews"
          value={block().planFix?.maxRounds}
          placeholder="as set"
          min={1}
          max={20}
          step={1}
          disabled={saving()}
          onChange={(v) => setPlanFix('maxRounds', v)}
        />
        <NumberField
          label="Reviews without progress"
          value={block().planFix?.stallRounds}
          placeholder="as set"
          min={1}
          max={5}
          step={1}
          disabled={saving()}
          onChange={(v) => setPlanFix('stallRounds', v)}
        />
        <NumberField
          label="Minutes"
          value={block().planFix?.maxMinutes}
          placeholder="as set"
          min={1}
          max={240}
          step={1}
          disabled={saving()}
          onChange={(v) => setPlanFix('maxMinutes', v)}
        />
        <NumberField
          label="Share of the budget (%)"
          value={
            block().planFix?.budgetShare === undefined
              ? undefined
              : Math.round(block().planFix!.budgetShare! * 100)
          }
          placeholder="as set"
          min={10}
          max={90}
          step={5}
          disabled={saving()}
          onChange={(v) => setPlanFix('budgetShare', v === undefined ? undefined : v / 100)}
        />
        <NumberField
          label="Automatic retries"
          value={block().planFix?.maxRetries}
          placeholder="as set"
          min={0}
          max={5}
          step={1}
          disabled={saving()}
          onChange={(v) => setPlanFix('maxRetries', v)}
        />
      </div>
      <p class="settings-hint host-autonomy-note">
        Balanced only. Empty: as in Settings → Autopilot.
      </p>

      <div class="autopilot-subtitle">Notify</div>
      <label class="host-autonomy-field">
        Call this URL when a task ends or is blocked
        <input
          value={block().notifyUrl ?? ''}
          spellcheck={false}
          placeholder="https://…"
          disabled={saving()}
          onChange={(e) => void save({ notifyUrl: e.currentTarget.value.trim() || undefined })}
        />
      </label>

      <Show when={error()}>
        <p class="settings-hint setting-explain warning" role="alert">
          {error()}
        </p>
      </Show>
    </div>
  );
}
