// autopilot.ts
//
// Autopilot's stage records, as the chat holds them
// (docs/design/autopilot-plan.md, 3.3): a task is a pipeline of stages,
// and each leaves an empty assistant message with `meta.stage`, which the
// backend writes and updates as the task goes on
// (src-tauri/crates/rovik-core/src/agent_run/autopilot/). The task's
// state is these records alone, so what the chat shows of it is right
// after a restart and in a remote chat's mirror, and nothing here keeps
// state of its own.
import type { ChatMessage } from '../types';

export type StageName =
  | 'plan'
  | 'planReview'
  | 'prepare'
  | 'tests'
  | 'implement'
  | 'review'
  | 'checks'
  | 'break'
  | 'pick'
  | 'acceptance'
  | 'deliver';

export type StageStatus = 'running' | 'done' | 'failed' | 'skipped';

/** How much a task spends on independence (AutopilotConfig.effort). */
export type AutopilotEffort = 'balanced' | 'thorough';

/** `meta.stage` (agent_run/autopilot/stage.rs `StageRecord`). */
export interface StageMeta {
  /** The task: the id of the message that started it. */
  task: string;
  name: StageName;
  status: StageStatus;
  /** The stage's round, from 1. */
  round: number;
  effort: AutopilotEffort;
  /** Thorough: the working copy it ran in. */
  candidate?: string;
  /** A run stage's: how its run ended; a failed stage's: the task's outcome. */
  outcome?: string;
  /** The record it produced (a review, a check round, a reply), by message id. */
  ref?: string;
  /** One line for the strip. */
  summary?: string;
  /** What its runs spent (U04): ms they ran, the cost and the tokens providers reported. */
  spent?: { ms: number; cost: number; uncosted: number; tokens?: number };
  /** The files it wrote: the Tests stage's tests (S04). */
  files?: string[];
  /** Thorough (T05): an implementation by candidates, by letter; `ref` is their record. */
  candidates?: string[];
  /** H07: the provider's rate limit holds it up; `until`: when its run goes on (ms). */
  rateLimit?: { until?: number; waits: number; waitedMs: number };
  /** U04: what the run under way spent so far, kept as it goes. */
  spending?: { ms: number; cost: number; uncosted: number; tokens?: number };
  /**
   * How many of a done stage's own steps failed, the task going on all the
   * same: the Prepare stage's installs (S03). Shown as a warning, not a tick.
   */
  failures?: number;
}

/**
 * `meta.tests`: the Tests stage's test writer at work, then what it wrote
 * before the code (S04, agent_run/test_writer.rs `tests_stage`).
 */
export interface TestsMeta {
  status: 'running' | 'done' | 'blocked' | 'skipped' | 'stopped';
  /** The test files it wrote. */
  files?: string[];
  /** Those of them that pass already, before the change (the red check). */
  passing?: string[];
  /** Its report's parts, and the report as written. */
  summary?: string;
  assumptions?: string;
  report?: string;
  /** Out of budget: its report is from what it did. */
  partial?: boolean;
  /** blocked or skipped: why. */
  reason?: string;
  childDialogId?: string;
  model?: string;
  toolCalls?: number;
  ms?: number;
  /**
   * S07: written after the change, which came with no test (the
   * test-exists rule); `red`: the red check's note on them.
   */
  after?: boolean;
  red?: string;
}

/** The headline of a Tests record; `interrupted`: saved as running, but nothing runs it. */
export function testsHeadline(meta: TestsMeta, interrupted: boolean): string {
  const n = meta.files?.length ?? 0;
  const wrote = `${n} test file${n === 1 ? '' : 's'}`;
  const passing = meta.passing?.length ?? 0;
  if (interrupted) return 'Writing the tests didn’t finish: the run ended meanwhile.';
  if (meta.after) {
    switch (meta.status) {
      case 'running':
        return 'The change came with no tests: writing them…';
      case 'done':
        return `The change came with no tests: wrote ${wrote}, and the checks run ${n === 1 ? 'it' : 'them'} next.`;
      case 'stopped':
        return 'Stopped while tests for the change were written.';
      default:
        return `${meta.reason ?? 'No tests were written'}. The checks’ result stands.`;
    }
  }
  switch (meta.status) {
    case 'running':
      return 'Writing the tests before the code, from the plan’s criteria…';
    case 'stopped':
      return 'Stopped while the tests were written.';
    case 'done':
      if (!passing) return `Wrote ${wrote} before the code: the change is made to pass them.`;
      if (passing === n)
        return `Wrote ${wrote} before the code; ${n === 1 ? 'it passes' : 'they pass'} already, so ${n === 1 ? 'it' : 'they'} may not test the change.`;
      return `Wrote ${wrote} before the code; ${passing} of them already pass${passing === 1 ? 'es' : ''}, so ${passing === 1 ? 'it' : 'they'} may not test the change.`;
    default: {
      const why = meta.reason ?? 'No tests were written';
      return n
        ? `${why}, after it wrote ${wrote}: the change is made to pass them.`
        : `${why}. The task goes on without tests written first.`;
    }
  }
}

/** A stage record and the message that holds it. */
export interface StageRow {
  id: string;
  stage: StageMeta;
}

/** Every stage, in the order a task runs them. */
export const STAGES: StageName[] = [
  'plan',
  'planReview',
  'prepare',
  'tests',
  'implement',
  'review',
  'checks',
  'break',
  'pick',
  'acceptance',
  'deliver',
];

/** What the strip and the chat call each stage. */
export const STAGE_LABELS: Record<StageName, string> = {
  plan: 'Plan',
  planReview: 'Plan review',
  prepare: 'Prepare',
  tests: 'Tests',
  implement: 'Implement',
  review: 'Change review',
  checks: 'Checks',
  break: 'Break',
  pick: 'Pick',
  acceptance: 'Acceptance',
  deliver: 'Deliver',
};

/** Only Thorough has these (3.2). */
const THOROUGH_ONLY: StageName[] = ['break', 'pick'];

/** The stages of a task of this effort, in order. */
export function pipeline(effort: AutopilotEffort): StageName[] {
  return STAGES.filter((s) => effort === 'thorough' || !THOROUGH_ONLY.includes(s));
}

const STATUSES: StageStatus[] = ['running', 'done', 'failed', 'skipped'];

/** A message's stage record, if it holds one that reads. */
export function stageOf(m: ChatMessage): StageMeta | null {
  return m.role === 'assistant' ? readStage(m.meta?.stage) : null;
}

/** `meta.stage`, if it reads as a stage record. */
export function readStage(stage: StageMeta | null | undefined): StageMeta | null {
  if (!stage || typeof stage.task !== 'string') return null;
  if (!STAGES.includes(stage.name) || !STATUSES.includes(stage.status)) return null;
  return stage;
}

/**
 * A record as a patch brings it (`next`), unless it would turn one that
 * ended back into a running one. The backend never does that: a record
 * is made running and ends once. But a remote chat's pull can read its host
 * before a record ended there, and arrive after the live patch that ended
 * it.
 */
export function keptStage(current: StageMeta | undefined, next: StageMeta): StageMeta {
  return current && current.status !== 'running' && next.status === 'running' ? current : next;
}

/** The stage records among `messages`, oldest first. */
export function stageRows(messages: ChatMessage[]): StageRow[] {
  const rows: StageRow[] = [];
  for (const m of messages) {
    const stage = stageOf(m);
    if (stage) rows.push({ id: m.id, stage });
  }
  return rows;
}

/**
 * The chat's latest task, while it's the chat's current request: its
 * records. A message of the user's after it takes over from it.
 */
export function currentTask(messages: ChatMessage[]): StageRow[] | null {
  const rows = stageRows(messages);
  const last = rows[rows.length - 1];
  if (!last) return null;
  const task = rows.filter((r) => r.stage.task === last.stage.task);
  const from = messages.findIndex((m) => m.id === task[0].id);
  const later = messages.slice(from + 1);
  if (later.some((m) => m.role === 'user' && !m.meta?.harness && !m.failed)) return null;
  return task;
}

/** Whether a task is over: delivered, or stopped by the user. */
export function taskOver(rows: StageRow[]): boolean {
  const last = rows[rows.length - 1]?.stage;
  if (!last) return true;
  if (last.status === 'running') return false;
  return last.name === 'deliver' || (last.status === 'failed' && last.outcome === 'stopped');
}

/** A task the backend picks up again when asked to resume the chat. */
export function unfinishedTask(messages: ChatMessage[]): boolean {
  const task = currentTask(messages);
  return !!task && !taskOver(task);
}

/*
 * ---------------------------------------------------------------------------
 * The strip (AutopilotStrip.tsx) and the records (StageRecord.tsx)
 * ---------------------------------------------------------------------------
 */

/** A message of the user's: one that starts a request. */
const isRequest = (m: ChatMessage) => m.role === 'user' && !m.meta?.harness && !m.failed;

/**
 * The current task's records, for the strip: those stored (`stored`, as
 * `chatApi.taskStages` read them; a task's runs can be longer than the page
 * of messages the chat holds), with the chat's loaded messages over them,
 * which hold the latest of each (they change live). A task started since
 * the read is the loaded one; a message of the user's after the task's last
 * record ends it as the current one. `newest`: the loaded messages reach
 * the chat's newest (not a page opened around a search hit), so the task
 * they end with is the chat's latest. `patched`: records as patches brought
 * them for messages the chat doesn't hold (lib/chatStore.ts
 * `stagePatches`), newer than a remote chat's mirror, which a patch doesn't
 * change: its implementation's record ends while the run's page is held.
 */
export function taskRecords(
  stored: StageRow[] | null,
  messages: ChatMessage[],
  newest = true,
  patched: Readonly<Record<string, StageMeta>> = {}
): StageRow[] | null {
  const loaded = newest ? currentTask(messages) : null;
  const task = loaded?.[0].stage.task ?? stored?.[0]?.stage.task;
  const fromStore = stored?.length && stored[0].stage.task === task ? stored : [];
  if (!task || (!loaded && !fromStore.length)) return null;
  const live = new Map(stageRows(messages).map((r) => [r.id, r]));
  const rows = fromStore.map((r) => {
    const held = live.get(r.id);
    if (held) return held;
    const patch = patched[r.id];
    return patch ? { id: r.id, stage: keptStage(r.stage, patch) } : r;
  });
  for (const r of loaded ?? []) if (!rows.some((x) => x.id === r.id)) rows.push(r);
  if (!loaded && newest) {
    // The newest messages held end with none of its records: current unless
    // a request of the user's came after its last one (all held messages
    // did, when that isn't held). A page around a search hit is older, and
    // the read says.
    const at = messages.findIndex((m) => m.id === rows[rows.length - 1].id);
    if (messages.slice(at + 1).some(isRequest)) return null;
  }
  return rows;
}

/** A record a stage done inside another's run leaves in the chat: how it is, its round. */
type InsideRecord = { status: string; round?: number };

/**
 * The stages done inside another stage's run (stage.rs `Carrier::Inside`),
 * by the run, each with the record it leaves in the chat as it goes: the
 * plan's review inside the plan's run; the change review, the checks, the
 * adversarial tester and the acceptance check inside the implementation's.
 */
const INSIDE: Partial<
  Record<StageName, { name: StageName; of: (m: ChatMessage) => InsideRecord | undefined }[]>
> = {
  plan: [
    {
      name: 'planReview',
      of: (m) => (m.meta?.review?.kind === 'plan' ? m.meta.review : undefined),
    },
  ],
  implement: [
    { name: 'review', of: (m) => (m.meta?.review?.kind === 'change' ? m.meta.review : undefined) },
    { name: 'checks', of: (m) => m.meta?.verification },
    { name: 'break', of: (m) => m.meta?.breaker },
    { name: 'acceptance', of: (m) => m.meta?.acceptance },
  ],
};

/** Its record says the stage didn't run (no reviewer, no checks to run). */
const SKIPPED = ['skipped', 'unverified'];

/**
 * The strip's records while a stage's run carries out stages of its own
 * (`INSIDE`): those are recorded only once the run ends (driver.rs
 * `advance`), so the run's record, running, said "implement…" through the
 * change review and the checks. They show from the records the run leaves
 * in the chat as it goes, which come live, from a host too. The furthest
 * the run got (in the stages' order) is the one going on, findings or a
 * failed round being fixed included, and the line's; those before it are
 * done, or skipped; the run's own stage is done once it got to one. A stage
 * only goes forward so, never back. The same until the driver has recorded
 * each (one after another, once the run ended). Only `messages` after the
 * run's record (all of them when it isn't held: the newest are the run's),
 * and only `messages` that reach the chat's newest.
 */
export function withLiveStages(rows: StageRow[], messages: ChatMessage[]): StageRow[] {
  let at = rows.length - 1;
  while (at >= 0 && !INSIDE[rows[at].stage.name]) at--;
  if (at < 0) return rows;
  const run = rows[at];
  const inside = INSIDE[run.stage.name]!;
  const after = rows.slice(at + 1);
  // The task went on past them: as recorded.
  if (after.some((r) => !inside.some(({ name }) => name === r.stage.name))) return rows;
  const since = messages.slice(messages.findIndex((m) => m.id === run.id) + 1);
  const latest = new Map<StageName, { id: string; record: InsideRecord }>();
  for (const m of since)
    for (const { name, of } of inside) {
      const record = of(m);
      if (record) latest.set(name, { id: m.id, record });
    }
  const reached = inside.filter(({ name }) => latest.has(name)).map(({ name }) => name);
  if (!reached.length) return rows;
  const furthest = reached[reached.length - 1];
  const live = reached
    .filter((name) => !after.some((r) => r.stage.name === name))
    .map((name): StageRow => {
      const { id, record } = latest.get(name)!;
      const skipped = SKIPPED.includes(record.status);
      const status = skipped ? 'skipped' : name === furthest ? 'running' : 'done';
      const round = record.round ?? 1;
      return { id, stage: { task: run.stage.task, name, status, round, effort: run.stage.effort } };
    });
  const own =
    run.stage.status === 'running'
      ? { id: run.id, stage: { ...run.stage, status: 'done' as const } }
      : run;
  return [...rows.slice(0, at), own, ...after, ...live];
}

/**
 * How a stage shows: as recorded, `pending` when not reached (yet), and,
 * when recorded running but nothing runs it (no run, nor the task's driver
 * at work between two of its runs: `running` is either), `waiting` while it waits out
 * the provider's rate limit (H07: its run goes on by itself), else
 * `stopped` (the app closed, or a crash, cut it; it goes on once the task
 * is resumed). A done stage some of whose steps failed (`failures`: the
 * Prepare stage's installs) shows a `warning`, not a tick.
 */
export type StageShown = StageStatus | 'pending' | 'stopped' | 'waiting' | 'warning';

export function shownStatus(
  status: StageStatus | 'pending',
  running: boolean,
  waiting = false,
  failures = 0
): StageShown {
  if (status === 'done' && failures > 0) return 'warning';
  if (status !== 'running' || running) return status;
  return waiting ? 'waiting' : 'stopped';
}

/** Whether a record waits out the provider's rate limit (H07). */
export const waitsOutRateLimit = (stage: Pick<StageMeta, 'status' | 'rateLimit'>): boolean =>
  stage.status === 'running' && stage.rateLimit?.until !== undefined;

/**
 * Whether a task waits out the provider's rate limit: its latest record
 * does. No run goes meanwhile, but a Stop ends the wait, and the stage.
 */
export function taskWaits(rows: StageRow[] | null | undefined): boolean {
  const last = rows?.[rows.length - 1];
  return !!last && waitsOutRateLimit(last.stage);
}

/** One stage as the strip shows it. */
export interface StripStage {
  name: StageName;
  label: string;
  /** How its latest record stands; `pending`: not reached (yet). */
  status: StageStatus | 'pending';
  /** Its latest record's round, when it ran more than once. */
  round?: number;
  summary?: string;
  /** Its latest record's message, to go to. */
  recordId?: string;
  /** Its latest record waits out the provider's rate limit (H07). */
  waiting?: boolean;
  /** Its latest record's steps that failed, the task going on (`StageMeta.failures`). */
  failures?: number;
}

/** The task's stages in order, each as its latest record says. */
export function stripStages(rows: StageRow[]): StripStage[] {
  const effort = rows[0]?.stage.effort ?? 'balanced';
  return pipeline(effort).map((name) => {
    const latest = [...rows].reverse().find((r) => r.stage.name === name);
    const stage: StripStage = { name, label: STAGE_LABELS[name], status: 'pending' };
    if (!latest) return stage;
    return {
      ...stage,
      status: latest.stage.status,
      ...(latest.stage.round > 1 ? { round: latest.stage.round } : {}),
      ...(latest.stage.summary ? { summary: latest.stage.summary } : {}),
      recordId: latest.id,
      ...(waitsOutRateLimit(latest.stage) ? { waiting: true } : {}),
      ...(latest.stage.failures ? { failures: latest.stage.failures } : {}),
    };
  });
}

const lower = (name: StageName) => STAGE_LABELS[name].toLowerCase();

/**
 * The record that ended the task, if a stage failed it: as the backend's
 * `task_outcome` reads it, the first failed stage but the delivery that
 * no later round of it ran again (a Retry's round says how it went).
 */
export function endingFailure(rows: StageRow[]): StageRow | undefined {
  return rows.find(
    (r, i) =>
      r.stage.status === 'failed' &&
      r.stage.name !== 'deliver' &&
      !rows.slice(i + 1).some((later) => later.stage.name === r.stage.name)
  );
}

/** Outcomes delivered as partial: not landed, the report says why (D7, S09). */
const PARTIAL = ['unmet', 'budget', 'verify-flagged'];

/** Whether a task that ended with `outcome` was delivered as partial. */
export const deliveredAsPartial = (outcome?: string): boolean => PARTIAL.includes(outcome ?? '');

/** One line on the task as a whole, the strip's own label. */
export function taskLine(rows: StageRow[], running: boolean): string {
  const last = rows[rows.length - 1]?.stage;
  if (!last) return 'Autopilot';
  if (last.status === 'running') {
    if (running) return `Autopilot: ${lower(last.name)}…`;
    return waitsOutRateLimit(last)
      ? `Autopilot: ${lower(last.name)} waits out the provider’s rate limit`
      : `Autopilot: ${lower(last.name)} didn’t finish`;
  }
  if (taskOver(rows)) {
    const failed = endingFailure(rows);
    if (failed?.stage.outcome === 'stopped') {
      return `Autopilot: stopped during ${lower(failed.stage.name)}`;
    }
    if (last.name === 'deliver' && last.status === 'done') {
      return deliveredAsPartial(last.outcome)
        ? 'Autopilot: delivered as partial'
        : 'Autopilot: delivered';
    }
    if (failed) return `Autopilot: ${lower(failed.stage.name)} failed, not delivered`;
    return 'Autopilot: not delivered';
  }
  return running ? 'Autopilot' : 'Autopilot: waiting to go on';
}

/** What a stage's own status says, in words. */
const STATUS_WORDS: Record<StageShown, string> = {
  pending: 'not reached',
  running: 'running',
  stopped: 'didn’t finish',
  waiting: 'waiting',
  done: 'done',
  warning: 'done, with failures',
  failed: 'failed',
  skipped: 'skipped',
};

export function statusWord(status: StageShown): string {
  return STATUS_WORDS[status];
}

/** A stage's name with its round, when it ran more than once: "Implement, round 2". */
export function stageTitle(stage: Pick<StageMeta, 'name' | 'round'>): string {
  const label = STAGE_LABELS[stage.name];
  return stage.round > 1 ? `${label}, round ${stage.round}` : label;
}

/** A stage's hover text on the strip: its status, and its summary. */
export function stageHover(stage: StripStage, running: boolean): string {
  const status = statusWord(shownStatus(stage.status, running, stage.waiting, stage.failures));
  const title = stageTitle({ name: stage.name, round: stage.round ?? 1 });
  return stage.summary ? `${title}: ${status}. ${stage.summary}` : `${title}: ${status}`;
}

/** One criterion's verdict (agent_run/acceptance.rs `Verdict`). */
export interface AcceptanceVerdict {
  criterion: number;
  satisfied: boolean;
  /** The evidence, or what's missing. */
  note: string;
}

/**
 * `meta.acceptance`: an acceptance check of a task's change by its plan's
 * criteria (S08, agent_run/acceptance.rs).
 */
export interface AcceptanceMeta {
  status: 'running' | 'satisfied' | 'unsatisfied' | 'skipped' | 'stopped';
  round: number;
  maxRounds: number;
  /** Thorough (T07): the checks go on while they find fewer criteria unmet. */
  byProgress?: boolean;
  criteria: string[];
  verdicts?: AcceptanceVerdict[];
  evidence?: string;
  /**
   * The last check left a criterion unsatisfied, or couldn't judge the fix
   * of those an earlier check found unmet: the task ends unmet.
   */
  unmet?: boolean;
  /** skipped, and `unmet`: how many criteria the check before found unmet, which stand. */
  earlierUnmet?: number;
  /** skipped: why. */
  reason?: string;
  childDialogId?: string;
  model?: string;
  toolCalls?: number;
  ms?: number;
  partial?: boolean;
}

/** The headline of an acceptance record; `interrupted`: saved as running, but nothing runs it. */
export function acceptanceHeadline(meta: AcceptanceMeta, interrupted: boolean): string {
  if (interrupted) return 'The acceptance check didn’t finish: the run ended meanwhile.';
  const n = meta.criteria.length;
  const unmet = meta.verdicts?.filter((v) => !v.satisfied).length ?? 0;
  const criteria = (k: number) => `${k} criteri${k === 1 ? 'on' : 'a'}`;
  switch (meta.status) {
    case 'running':
      return `Checking the change against the plan’s ${criteria(n)}…`;
    case 'satisfied':
      return n === 1
        ? 'The change meets the plan’s criterion.'
        : `The change meets all ${n} of the plan’s criteria.`;
    case 'unsatisfied':
      return meta.unmet
        ? `${unmet} of ${criteria(n)} still unmet after the last check: the task ends unmet.`
        : `${unmet} of ${criteria(n)} unmet: sent to the agent for one more round.`;
    case 'stopped':
      return 'The acceptance check was stopped.';
    default: {
      const skipped = `Acceptance check skipped: ${meta.reason ?? 'it couldn’t judge'}.`;
      if (!meta.unmet) return `${skipped} The checks decide.`;
      const k = meta.earlierUnmet ?? 1;
      const standing =
        k === 1
          ? 'The criterion the check before found unmet stands'
          : `The ${k} criteria the check before found unmet stand`;
      return `${skipped} ${standing}: the task ends unmet.`;
    }
  }
}

/**
 * `meta.planner`: one of a Thorough task's planners (T01,
 * agent_run/planning.rs): at work, then the plan it gave, or why none.
 */
export interface PlannerMeta {
  /** What it wrote: the task's plan, or its criteria before it (T03). */
  role: 'plan' | 'criteria';
  /** Its name among the task's planners: a, b, c (criteria: a, b). */
  candidate: string;
  status: 'running' | 'done' | 'skipped' | 'stopped';
  /** done: its plan's title and steps, and the plan (cut when long). */
  title?: string;
  steps?: number;
  plan?: string;
  /** What the plan's checks find in it; none: it passes them. */
  problems?: string[];
  /** Its plan is the one the task took. */
  chosen?: boolean;
  /** Criteria, done: the numbered ones, what's out of scope, what it assumed. */
  criteria?: string[];
  outOfScope?: string[];
  assumptions?: string[];
  /** skipped: why. */
  reason?: string;
  partial?: boolean;
  childDialogId?: string;
  model?: string;
  toolCalls?: number;
  ms?: number;
}

/** "1 criterion", "3 criteria". */
const criteriaCount = (n: number) => `${n} criteri${n === 1 ? 'on' : 'a'}`;

/** The headline of a planner's record; `interrupted`: saved as running, but nothing runs it. */
export function plannerHeadline(meta: PlannerMeta, interrupted: boolean): string {
  const writes = meta.role === 'criteria';
  const who = `${writes ? 'Criteria writer' : 'Planner'} ${meta.candidate.toUpperCase()}`;
  if (interrupted) return `${who} didn’t finish: the run ended meanwhile.`;
  switch (meta.status) {
    case 'running':
      return writes
        ? `${who} is writing what done means, on its own…`
        : `${who} is planning the task on its own…`;
    case 'stopped':
      return `${who} was stopped.`;
    case 'skipped':
      return writes
        ? `${who} gave no criteria: ${meta.reason ?? 'it couldn’t write them'}.`
        : `${who} gave no plan: ${meta.reason ?? 'it couldn’t plan'}.`;
    default: {
      if (writes) return `${who} wrote ${criteriaCount(meta.criteria?.length ?? 0)}.`;
      const steps = meta.steps ?? 0;
      const planned = `${who} planned “${meta.title || 'Untitled'}” in ${steps} step${steps === 1 ? '' : 's'}`;
      if (meta.chosen) return `${planned}: the task takes this plan.`;
      if (meta.problems?.length) return `${planned}, but the plan’s checks find problems in it.`;
      return `${planned}.`;
    }
  }
}

/**
 * `meta.judge`: a Thorough task's judge of its planners' plans (T02,
 * agent_run/judge.rs): at work, then the plan it took, or why none.
 */
export interface JudgeMeta {
  /**
   * What it judged: the planners' plans, the criteria sets it merged (T03),
   * or two candidates' changes whose scores tied (T06).
   */
  role: 'plans' | 'criteria' | 'candidates';
  status: 'running' | 'done' | 'skipped' | 'stopped';
  /** The planners whose plans it was given. */
  offered?: string[];
  /** done: the plan it took, by its planner's letter, or "merged". */
  choice?: string;
  /** It merged the plans or revised the one it took: `plan` is its own. */
  revised?: boolean;
  plan?: string;
  title?: string;
  steps?: number;
  why?: string;
  /** Criteria, done: the merged set; its assumptions say where the sets disagreed. */
  criteria?: string[];
  outOfScope?: string[];
  assumptions?: string[];
  /** skipped: why, and the planner whose plan (or criteria) the task took instead. */
  reason?: string;
  fallback?: string;
  partial?: boolean;
  childDialogId?: string;
  model?: string;
  toolCalls?: number;
  ms?: number;
}

/** The headline of a judge's record; `interrupted`: saved as running, but nothing runs it. */
export function judgeHeadline(meta: JudgeMeta, interrupted: boolean): string {
  if (interrupted) return 'The judge didn’t finish: the run ended meanwhile.';
  if (meta.role === 'candidates') {
    switch (meta.status) {
      case 'running':
        return 'The candidates’ scores tie: the judge is weighing their changes…';
      case 'stopped':
        return 'The judge was stopped.';
      case 'skipped':
        return `The judge didn’t decide: ${meta.reason ?? 'it couldn’t judge'}. The first candidate is taken.`;
      default:
        return `The judge picked candidate ${(meta.choice ?? '?').toUpperCase()}’s change.`;
    }
  }
  if (meta.role === 'criteria') {
    const sets = meta.offered?.length ?? 2;
    switch (meta.status) {
      case 'running':
        return `The judge is merging the ${sets} sets of criteria…`;
      case 'stopped':
        return 'The judge was stopped.';
      case 'skipped': {
        const instead = meta.fallback
          ? `The task takes writer ${meta.fallback.toUpperCase()}’s criteria.`
          : '';
        return `The judge couldn’t merge the criteria: ${meta.reason ?? 'it couldn’t judge'}. ${instead}`.trim();
      }
      default:
        return `The judge merged the ${sets} sets into ${criteriaCount(meta.criteria?.length ?? 0)}.`;
    }
  }
  const planner = (letter: string) => `planner ${letter.toUpperCase()}`;
  const steps = meta.steps ?? 0;
  const plan = `“${meta.title || 'Untitled'}”, ${steps} step${steps === 1 ? '' : 's'}`;
  switch (meta.status) {
    case 'running':
      return meta.offered?.length
        ? `The judge is weighing the ${meta.offered.length} plans against the code…`
        : 'The judge is weighing the planners’ plans against the code…';
    case 'stopped':
      return 'The judge was stopped.';
    case 'skipped': {
      const instead = meta.fallback
        ? `The task takes ${planner(meta.fallback)}’s plan.`
        : 'The agent plans the task itself.';
      return `The judge gave no choice: ${meta.reason ?? 'it couldn’t judge'}. ${instead}`;
    }
    default:
      if (!meta.choice || meta.choice === 'merged')
        return `The judge merged the plans into one: ${plan}.`;
      return meta.revised
        ? `The judge took ${planner(meta.choice)}’s plan, revised: ${plan}.`
        : `The judge took ${planner(meta.choice)}’s plan, ${plan}.`;
  }
}

/**
 * `meta.breaker`: a Thorough task's adversarial tester (T04,
 * agent_run/breaker.rs): at work, then the tests it left, the speculative
 * ones that passed and were removed, and whether its tests found a fault.
 */
export interface BreakerMeta {
  status: 'running' | 'done' | 'blocked' | 'skipped' | 'stopped';
  /** The test files that stay, with how its report marked them. */
  files?: string[];
  marks?: { file: string; mark: string }[];
  /** Speculative files that passed alone: removed. */
  removed?: string[];
  /** Speculative files there was no way to run alone: kept. */
  unjudged?: string[];
  /** The round that ran its tests: they failed on the change. */
  found?: boolean;
  summary?: string;
  assumptions?: string;
  report?: string;
  /** blocked or skipped: why. */
  reason?: string;
  partial?: boolean;
  childDialogId?: string;
  model?: string;
  toolCalls?: number;
  ms?: number;
}

/** The headline of the adversarial tester's record. */
export function breakerHeadline(meta: BreakerMeta, interrupted: boolean): string {
  if (interrupted) return 'The adversarial tester didn’t finish: the run ended meanwhile.';
  const n = meta.files?.length ?? 0;
  const files = `${n} test file${n === 1 ? '' : 's'}`;
  if (meta.status === 'running')
    return 'The checks pass: an adversarial tester is trying to break the change…';
  if (meta.status === 'stopped') return 'The adversarial tester was stopped.';
  // Blocked or skipped with tests written: they stay all the same.
  if (!n) return `The adversarial tester left no test: ${meta.reason ?? 'it wrote none'}.`;
  if (meta.found === true)
    return `The adversarial tester’s ${files} found a fault: the agent fixes the change to pass them.`;
  if (meta.found === false) return `The change passes the adversarial tester’s ${files}.`;
  return `The adversarial tester wrote ${files}; the checks run them next.`;
}

/** A candidate in `meta.candidates` (T05). */
export interface CandidateMeta {
  letter: string;
  /** Its chat, under the task's: its implementation's run and records. */
  chat: string;
  status: 'waiting' | 'running' | 'done' | 'failed' | 'skipped' | 'stopped';
  /** How its implementation's run ended. */
  outcome?: string;
  summary?: string;
  /** D19: the installs its copy got before it started, in a line. */
  prepared?: string;
}

/**
 * `meta.candidates`: a Thorough task's implementation by two candidates,
 * each in a working copy of its own, one after the other (T05,
 * agent_run/candidates.rs `CandidatesRecord`); or, `single`, why there
 * was one implementer.
 */
export interface CandidatesMeta {
  task: string;
  round: number;
  status: 'running' | 'done' | 'stopped' | 'single';
  reason?: string;
  candidates?: CandidateMeta[];
}

/** The headline of the candidates' record. */
export function candidatesHeadline(meta: CandidatesMeta, interrupted: boolean): string {
  if (meta.status === 'single')
    return `One implementer: ${meta.reason ?? 'there’s no second working copy'}.`;
  if (interrupted) return 'The candidates didn’t finish: the run ended meanwhile.';
  const all = meta.candidates ?? [];
  switch (meta.status) {
    case 'running': {
      const at = all.find((c) => c.status === 'running');
      return at
        ? `Two implementers, one after the other: candidate ${at.letter.toUpperCase()} is implementing the plan in its own copy…`
        : 'Two implementers, one after the other, each in a copy of its own…';
    }
    case 'stopped':
      return 'The candidates were stopped.';
    default: {
      const done = all.filter((c) => c.status === 'done').length;
      if (done === all.length && done > 0)
        return `Both candidates implemented the plan, each in its own copy: the pick takes the better.`;
      return `${done} of ${all.length} candidates implemented the plan: the pick takes the better.`;
    }
  }
}

/** "A: done · 1 check passed" */
export function candidateLine(c: CandidateMeta): string {
  const status: Record<CandidateMeta['status'], string> = {
    waiting: 'waiting',
    running: 'implementing…',
    done: 'done',
    failed: 'failed',
    skipped: 'skipped',
    stopped: 'stopped',
  };
  const summary = c.summary && c.status !== 'running' ? ` · ${c.summary}` : '';
  return `${c.letter.toUpperCase()}: ${status[c.status]}${summary}`;
}

/** A candidate's score (D14, agent_run/candidates.rs `Score`). */
export interface CandidateScore {
  changed: boolean;
  finished: boolean;
  /** Its run ended clean: a result Autopilot lands, no caveat (unmet, a flagged check…). */
  clean?: boolean;
  testsPassed: number;
  checksPassed: number;
  openFindings: number;
  diffLines: number;
}

/** A candidate as the pick weighed it. */
export interface ScoredMeta {
  letter: string;
  chat: string;
  outcome?: string;
  score: CandidateScore;
  files: string[];
  /** The loser's diff, kept (cut when `reduced`). */
  diff?: string;
  reduced?: boolean;
}

/**
 * `meta.pick`: a Thorough task's Pick (T06, agent_run/candidates.rs
 * `PickRecord`): each candidate's score, the winner and how it won, and
 * the loser's diff.
 */
export interface PickMeta {
  task: string;
  status: 'running' | 'done' | 'failed' | 'stopped';
  winner?: string;
  chat?: string;
  /** The scores tied: the judge decided, or (`why`) the first was taken. */
  judged: boolean;
  why?: string;
  adopted: boolean;
  reason?: string;
  candidates: ScoredMeta[];
}

/** The headline of the pick's record. */
export function pickHeadline(meta: PickMeta, interrupted: boolean): string {
  if (interrupted) return 'The pick didn’t finish: the run ended meanwhile.';
  const winner = (meta.winner ?? '?').toUpperCase();
  switch (meta.status) {
    case 'running':
      return 'Scoring the candidates: the tests written first, the checks, the open findings, the size…';
    case 'stopped':
      return 'The pick was stopped.';
    case 'failed':
      return `The pick failed: ${meta.reason ?? 'the winner’s change couldn’t be taken'}.`;
    default:
      if (!meta.judged) return `Candidate ${winner}’s change is the task’s: it scored better.`;
      if (meta.why)
        return `Candidate ${winner}’s change is the task’s: the scores tied, and ${meta.why}.`;
      return `Candidate ${winner}’s change is the task’s: the scores tied, and the judge picked it.`;
  }
}

/** "2 tests pass · 1 check passes · no findings open · 12 lines changed" */
export function scoreLine(s: CandidateScore): string {
  if (!s.changed) return 'no change';
  const n = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;
  const parts = [
    n(s.testsPassed, 'test written first passes', 'tests written first pass'),
    n(s.checksPassed, 'check passes', 'checks pass'),
    s.openFindings ? n(s.openFindings, 'finding open', 'findings open') : 'no findings open',
    n(s.diffLines, 'line changed', 'lines changed'),
  ];
  if (!s.finished) parts.unshift('its run didn’t finish');
  else if (s.clean === false) parts.unshift('its run ended with a caveat');
  return parts.join(' · ');
}

/**
 * `meta.report`: the facts a task's report was written from (S09,
 * agent_run/autopilot/delivery.rs `ReportFacts`), on its final message.
 */
export interface ReportMeta {
  outcome: string;
  /** How it ended, in a line. */
  ending: string;
  delivered: boolean;
  /** Not landed, but the task's result: unmet, budget, verify-flagged. */
  partial: boolean;
  /** What became of the result. */
  delivery: string;
  /** On a server that pushes its results (H04): where it went. */
  branch?: string;
  pr?: string;
  /** Thorough (T08): the change reviewer's view is under the report, as "The reviewer's view". */
  reviewerView?: boolean;
  assumptions: string[];
  criteria: string[];
  verdicts: AcceptanceVerdict[];
  tests: string[];
  checks: { command: string; status: string }[];
  review?: string | null;
  spent: { ms: number; cost: number; uncosted: number; tokens?: number };
  /** The model couldn't write it: the IDE did, from these facts. */
  byIde?: boolean;
}

/** "2 min", "1 h 5 min", "40 s" (agent_run/report.rs `duration`). */
export function reportDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
}

/** "330", "45,200", "2.15 million" (agent_run/budget.rs `tokens_text`). */
export function tokensText(tokens: number): string {
  if (tokens >= 1_000_000) {
    return `${Number((tokens / 1_000_000).toFixed(2))} million`;
  }
  return Math.round(tokens).toLocaleString('en-US');
}

/** What the task spent, in a line: its time, its cost, and its tokens where reported. */
export function reportSpent(spent: ReportMeta['spent']): string {
  const time = reportDuration(spent.ms);
  const cost = spent.cost.toFixed(2);
  const line =
    spent.cost <= 0
      ? `${time}; no cost reported`
      : spent.uncosted
        ? `${time}, at least ${cost}`
        : `${time}, ${cost}`;
  return spent.tokens ? `${line}; ${tokensText(spent.tokens)} tokens` : line;
}

/** The report card's headline: how the task ended and what became of it. */
export function reportHeadline(meta: ReportMeta): string {
  if (meta.delivered) return `Task done: ${meta.ending.toLowerCase()}.`;
  if (meta.partial) return `Task delivered as partial: ${meta.ending.toLowerCase()}.`;
  return `Task ended: ${meta.ending.toLowerCase()}.`;
}
