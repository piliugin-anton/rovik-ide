// Component tests (Phase 7): what a subagent's block, a review record and
// a check record show while they run, when they're interrupted, and when
// Stop is offered. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ToolCallBlock from './ToolCallBlock';
import ReviewRecord from './ReviewRecord';
import VerificationRecord from './VerificationRecord';
import PlannerRecord from './PlannerRecord';
import JudgeRecord from './JudgeRecord';
import BreakerRecord from './BreakerRecord';
import AcceptanceRecord from './AcceptanceRecord';
import CandidatesRecord from './CandidatesRecord';
import PickRecord from './PickRecord';
import MessageList from './MessageList';
import { chatApi } from '../lib/chatApi';
import { liveSubagent, noteSubagentProgress, noteSubagentStarted } from '../lib/subagents';
import type { ReviewMeta } from '../lib/changeReview';
import type { VerificationMeta } from '../lib/verification';
import type { ChatMessage } from '../types';
import { createStore } from 'solid-js/store';

const call = (id: string) => ({
  id,
  name: 'task',
  arguments: JSON.stringify({ kind: 'scout', goal: 'Where is the config read?' }),
});

const stopButton = (root: HTMLElement) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent?.startsWith('Stop this'));

test('a running subagent offers its own Stop; a finished or stale one doesn’t', () => {
  noteSubagentStarted({
    dialogId: 'lead-c',
    toolCallId: 'c1',
    childDialogId: 'kid-c',
    kind: 'scout',
    model: 'Test/m',
    goal: 'Where is the config read?',
  });
  const running = container();
  const dispose = render(
    () => <ToolCallBlock toolCall={call('c1')} dialogId="lead-c" running={true} />,
    running
  );
  assert.equal(stopButton(running)?.textContent, 'Stop this scout');
  dispose();

  // An older call whose id a later call reused: the live run isn't its.
  const stale = container();
  render(
    () => <ToolCallBlock toolCall={call('c1')} dialogId="lead-c" running={true} notLive={true} />,
    stale
  );
  assert.equal(stopButton(stale), undefined);

  // Its result arrived: done, nothing to stop.
  const result: ChatMessage = {
    id: 'r1',
    role: 'tool',
    tool_call_id: 'c1',
    content: 'Report from a scout\n\n## Answer\nIn config.ts.',
    meta: {
      subagent: {
        kind: 'scout',
        status: 'done',
        childDialogId: 'kid-c',
        model: 'Test/m',
        modelsTried: ['Test/m'],
        toolCalls: 3,
        ms: 1200,
      },
    },
  };
  const done = container();
  render(
    () => <ToolCallBlock toolCall={call('c1')} dialogId="lead-c" running={true} result={result} />,
    done
  );
  assert.equal(stopButton(done), undefined);

  // The run ended: no Stop either.
  const ended = container();
  render(() => <ToolCallBlock toolCall={call('c1')} dialogId="lead-c" running={false} />, ended);
  assert.equal(stopButton(ended), undefined);
});

test('a review saved as running with no run going reads as interrupted', () => {
  const meta = { kind: 'change' as const, status: 'running' as const, round: 1, maxRounds: 3 };
  const live = container();
  render(() => <ReviewRecord meta={meta} dialogId="d" recordId="m1" live={true} />, live);
  const liveRoot = live.firstElementChild!;
  assert.ok(liveRoot.classList.contains('running'), liveRoot.className);
  assert.match(liveRoot.textContent ?? '', /Reviewing the change/);

  const gone = container();
  render(() => <ReviewRecord meta={meta} dialogId="d" recordId="m2" live={false} />, gone);
  const goneRoot = gone.firstElementChild!;
  assert.ok(goneRoot.classList.contains('stopped'), goneRoot.className);
  assert.match(goneRoot.textContent ?? '', /didn’t finish/);
});

test('a review card has a Details expander with the record’s facts and a kept report', () => {
  const meta: ReviewMeta = {
    kind: 'plan',
    status: 'skipped',
    round: 1,
    maxRounds: 2,
    reason: 'its report didn’t follow the format: it has no "## Findings" section',
    report: 'Looks fine to me.',
    model: 'Test/m',
    toolCalls: 7,
    ms: 4200,
  };
  const root = container();
  render(() => <ReviewRecord meta={meta} dialogId="d" recordId="m3" live={false} />, root);
  const details = root.querySelector('details.review-details')!;
  assert.ok(details, 'a Details expander');
  assert.equal(details.querySelector('summary')?.textContent, 'Details');
  const facts = [...details.querySelectorAll('dt')].map((dt) => dt.textContent);
  assert.deepEqual(facts, ['What', 'Status', 'Reason', 'Round', 'Model', 'Tool calls', 'Time']);
  assert.match(details.textContent ?? '', /no "## Findings" section/);
  assert.equal(details.querySelector('pre.review-report')?.textContent, 'Looks fine to me.');
  // A running review has no details yet.
  const running = container();
  render(
    () => (
      <ReviewRecord
        meta={{ kind: 'plan', status: 'running', round: 1, maxRounds: 2 }}
        dialogId="d"
        recordId="m4"
        live={true}
      />
    ),
    running
  );
  assert.equal(running.querySelector('details.review-details'), null);
});

test('a follow-up plan review shows its verdicts and what it didn’t hold', () => {
  const finding = (severity: 'high' | 'medium', step: number, title: string) => ({
    severity,
    location: `Step ${step}`,
    path: null,
    line: null,
    title,
    scenario: 's',
    fix: 'f',
    confidence: 'confirmed' as const,
    step,
  });
  const meta: ReviewMeta = {
    kind: 'plan',
    status: 'findings',
    round: 2,
    maxRounds: 2,
    open: 1,
    findings: [{ ...finding('high', 2, 'CLI not updated'), newEvidence: 'Still one call site.' }],
    earlier: [
      {
        finding: 1,
        severity: 'high',
        location: 'Step 2',
        title: 'CLI not updated',
        response: { action: 'fixed', note: 'Added step 3.' },
        resolved: false,
        reason: 'Still one call site.',
      },
      {
        finding: 2,
        severity: 'medium',
        location: 'Step 1',
        title: 'No index',
        response: { action: 'not_a_problem', note: 'The table is tiny.' },
        resolved: true,
        checked: true,
        reason: 'The answer holds.',
      },
      {
        finding: 3,
        severity: 'medium',
        location: 'Step 4',
        title: 'Wrong order',
        response: { action: 'fixed', note: 'Moved step 4.' },
        resolved: true,
        checked: false,
        reason: 'the reviewer gave no verdict on it; taken as settled',
      },
    ],
    notHeld: [finding('medium', 1, 'Untouched step')],
  };
  const root = container();
  render(() => <ReviewRecord meta={meta} dialogId="d" recordId="m5" live={false} />, root);
  const items = [...root.querySelectorAll('.review-earlier-item')];
  assert.equal(items.length, 3);
  assert.ok(!items[0].classList.contains('resolved') && items[1].classList.contains('resolved'));
  // No verdict from the reviewer: not shown as resolved.
  assert.ok(items[2].classList.contains('unchecked') && !items[2].classList.contains('resolved'));
  assert.match(items[2].textContent ?? '', /^not checked.*Why: the reviewer gave no verdict/);
  assert.match(
    items[0].textContent ?? '',
    /still open.*Earlier 1 · Step 2.*Agent: fixed — Added step 3\..*Why: Still one call site\./
  );
  assert.match(items[1].textContent ?? '', /resolved.*Agent: not a problem — The table is tiny\./);
  const notHeld = [...root.querySelectorAll('details.review-dropped')].find((d) =>
    d.querySelector('summary')?.textContent?.includes('not held')
  );
  assert.equal(
    notHeld?.querySelector('summary')?.textContent,
    '1 about what the revision didn’t change, not held'
  );
  assert.match(notHeld?.textContent ?? '', /Untouched step/);
});

test('a finding the reviewer gave no location shows its title alone', () => {
  const meta: ReviewMeta = {
    kind: 'plan',
    status: 'findings',
    round: 2,
    maxRounds: 2,
    findings: [
      {
        severity: 'high',
        location: '',
        path: null,
        line: null,
        title: 'No step updates the CLI',
        scenario: 's',
        fix: 'f',
        confidence: 'confirmed',
      },
    ],
    earlier: [
      {
        finding: 1,
        severity: 'medium',
        location: '',
        title: 'No test for the route',
        resolved: true,
        checked: true,
        reason: 'Validation adds it.',
      },
    ],
  };
  const root = container();
  render(() => <ReviewRecord meta={meta} dialogId="d" recordId="m6" live={false} />, root);
  const head = root.querySelector('.review-finding-head')!;
  assert.equal(head.querySelector('.review-location'), null);
  assert.equal(head.querySelector('.review-title')?.textContent, 'No step updates the CLI');
  const earlier = root.querySelector('.review-earlier-item .review-location');
  assert.equal(earlier?.textContent, 'Earlier 1');
});

test('checks saved as running with no run going read as interrupted', () => {
  const meta = {
    status: 'running' as const,
    round: 1,
    maxRounds: 3,
    checks: [],
    changedFiles: ['a.ts'],
  };
  const props = {
    dialogId: 'd',
    recordId: 'v1',
    latest: true,
    onRunChecks: () => {},
    onOpenProjectChecks: () => {},
  };
  const live = container();
  render(() => <VerificationRecord meta={meta} live={true} {...props} />, live);
  assert.ok(live.firstElementChild!.classList.contains('running'));

  const gone = container();
  render(() => <VerificationRecord meta={meta} live={false} {...props} />, gone);
  const root = gone.firstElementChild!;
  assert.ok(root.classList.contains('stopped'), root.className);
  assert.equal(
    root.querySelector('.chat-verify-title')?.textContent,
    'Checks didn’t finish: the run ended while they were running.'
  );
});

test('checks taken from the plan say so (V02)', () => {
  const meta = {
    status: 'passed' as const,
    round: 1,
    maxRounds: 3,
    checks: [{ kind: 'test' as const, command: 'node check.js', status: 'passed' as const }],
    changedFiles: ['ok.txt'],
  };
  const props = {
    dialogId: 'd',
    recordId: 'v1',
    latest: true,
    live: false,
    onRunChecks: () => {},
    onOpenProjectChecks: () => {},
  };
  const note = (root: Element) =>
    [...root.querySelectorAll('.chat-verify-progress')].find((n) =>
      n.textContent?.includes('Validation section')
    );
  const plan = container();
  render(() => <VerificationRecord meta={{ ...meta, fromPlan: true }} {...props} />, plan);
  assert.ok(note(plan), 'from the plan: said');
  const own = container();
  render(() => <VerificationRecord meta={meta} {...props} />, own);
  assert.equal(note(own), undefined, "the project's own: nothing to say");
});

test('Autopilot’s rounds: no count, an environment round, two without progress (V03)', () => {
  const base = {
    round: 2,
    maxRounds: 10,
    byProgress: true,
    checks: [{ kind: 'test' as const, command: 'npm test', status: 'failed' as const }],
    changedFiles: ['ok.txt'],
  };
  const props = {
    dialogId: 'd',
    recordId: 'v1',
    latest: false,
    live: false,
    onRunChecks: () => {},
    onOpenProjectChecks: () => {},
  };
  const title = (meta: VerificationMeta) => {
    const root = container();
    render(() => <VerificationRecord meta={meta} {...props} />, root);
    return { root, text: root.querySelector('.chat-verify-title')?.textContent };
  };
  // A failed round: its attempt, no count to tell.
  assert.equal(
    title({ ...base, status: 'failed' }).text,
    'Checks failed: sent to the agent to fix (attempt 2).'
  );
  assert.equal(
    title({ ...base, byProgress: undefined, maxRounds: 3, status: 'failed' }).text,
    'Checks failed: sent to the agent to fix (attempt 2 of 3).',
    'outside Autopilot, as before'
  );
  // Two rounds without progress ended it.
  assert.equal(
    title({ ...base, round: 4, status: 'gave-up', noProgress: true }).text,
    'Checks still fail after 4 attempts, the last two without progress. The agent stopped here.'
  );
  // The environment, not the change: installed and checked again.
  const installed = title({
    ...base,
    round: 1,
    status: 'failed',
    environment: {
      failure: 'sh: 1: vitest: not found',
      command: 'npm test',
      installs: [
        { command: 'npm ci', folder: '', status: 'passed', durationMs: 300 },
        {
          command: 'pnpm install --frozen-lockfile',
          folder: 'web',
          status: 'failed',
          exitCode: 1,
          output: 'ERR_PNPM_OUTDATED_LOCKFILE',
        },
      ],
    },
  });
  assert.equal(
    installed.text,
    'Checks failed on the environment, not on the change (sh: 1: vitest: not found): the dependencies were installed (1 of 2 installs failed), and the checks run again. This attempt isn’t counted.'
  );
  const rows = [...installed.root.querySelectorAll('.chat-verify-check')].map((li) => li.className);
  assert.deepEqual(rows, [
    'chat-verify-check failed',
    'chat-verify-check passed',
    'chat-verify-check failed',
  ]);
  assert.ok(
    installed.root.querySelector('details pre')?.textContent?.includes('OUTDATED'),
    'a failed install’s output'
  );
  // Nothing to install: it went to the agent.
  assert.equal(
    title({
      ...base,
      round: 1,
      status: 'failed',
      environment: { failure: 'sh: 1: vitest: not found', command: 'npm test', installs: [] },
    }).text,
    'Checks failed on the environment, not on the change (sh: 1: vitest: not found): nothing to install was found, so it went to the agent. This attempt isn’t counted.'
  );
});

test('a rollback of only what broke says what stayed (V05)', () => {
  const meta = {
    status: 'rolled-back' as const,
    round: 2,
    maxRounds: 3,
    checks: [{ kind: 'test' as const, command: 'npm test', status: 'failed' as const }],
    changedFiles: ['users.txt', 'posts.txt', 'tags.txt'],
    rolledBack: {
      restored: ['tags.txt'],
      skipped: [],
      broke: ['test|test|t::tags'],
      diff: '--- tags.txt',
      kept: ['posts.txt', 'users.txt'],
    },
  };
  const props = {
    dialogId: 'd',
    recordId: 'v1',
    latest: false,
    live: false,
    onRunChecks: () => {},
    onOpenProjectChecks: () => {},
  };
  const partial = container();
  render(() => <VerificationRecord meta={meta} {...props} />, partial);
  assert.equal(
    partial.querySelector('.chat-verify-title')?.textContent,
    'The fix broke something that passed before: the files behind it were put back (tags.txt), the rest of it stays (posts.txt, users.txt), and the checks run again.'
  );
  assert.ok(
    partial
      .querySelector('.chat-verify-rollback summary')
      ?.textContent?.includes('kept: posts.txt, users.txt')
  );
  const all = container();
  const { kept: _kept, ...everything } = meta.rolledBack;
  render(() => <VerificationRecord meta={{ ...meta, rolledBack: everything }} {...props} />, all);
  assert.equal(
    all.querySelector('.chat-verify-title')?.textContent,
    'The fix broke something that passed before, so it was rolled back (attempt 2 of 3).'
  );
});

test('a suite too slow for its timeout, and the tests that ran instead (V06)', () => {
  const root = container();
  render(
    () => (
      <VerificationRecord
        meta={{
          status: 'passed',
          round: 1,
          maxRounds: 10,
          byProgress: true,
          checks: [
            {
              kind: 'test',
              command: 'npm test',
              status: 'slow',
              reason: 'timed out after 600 s; the tests of the changed files ran instead',
            },
            {
              kind: 'test',
              command: 'npx vitest run src/a.test.js',
              status: 'passed',
              durationMs: 300,
            },
          ],
          changedFiles: ['src/a.js'],
        }}
        dialogId="d"
        recordId="v1"
        latest={false}
        live={false}
        onRunChecks={() => {}}
        onOpenProjectChecks={() => {}}
      />
    ),
    root
  );
  const rows = [...root.querySelectorAll('.chat-verify-check')];
  assert.deepEqual(
    rows.map((r) => r.className),
    ['chat-verify-check slow', 'chat-verify-check passed']
  );
  assert.equal(
    rows[0].querySelector('.chat-verify-note')?.textContent,
    'slow: timed out after 600 s; the tests of the changed files ran instead'
  );
  assert.equal(root.querySelector('.chat-verify-title')?.textContent, 'Checks passed.');
});

const button = (root: Element, text: string) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === text);

const subagentResult = (
  callId: string,
  status: 'done' | 'stopped' | 'failed',
  childDialogId = 'kid-x'
): ChatMessage => ({
  id: `r-${callId}`,
  role: 'tool',
  tool_call_id: callId,
  content: 'Report from a scout\n\n## Answer\nPart of it.',
  meta: {
    subagent: {
      kind: 'scout',
      status,
      childDialogId,
      model: 'Test/m',
      modelsTried: ['Test/m'],
      toolCalls: 2,
      ms: 3000,
    },
  },
});

test('task_block_states: stopped, failed and a group’s result', () => {
  noteSubagentStarted({
    dialogId: 'lead-s',
    toolCallId: 's1',
    childDialogId: 'kid-s',
    kind: 'scout',
    model: 'Test/m',
    goal: 'Where?',
  });
  const stopped = container();
  render(
    () => (
      <ToolCallBlock
        toolCall={call('s1')}
        dialogId="lead-s"
        running={true}
        result={subagentResult('s1', 'stopped', 'kid-s')}
      />
    ),
    stopped
  );
  const block = stopped.querySelector('.subagent-block')!;
  assert.match(stopped.querySelector('.tool-call-meta')?.textContent ?? '', /^stopped · /);
  assert.ok(!block.classList.contains('running'), block.className);
  assert.ok(!block.classList.contains('error'), block.className);
  assert.equal(stopButton(stopped), undefined);

  const failed = container();
  render(
    () => (
      <ToolCallBlock
        toolCall={call('f1')}
        dialogId="lead-s"
        result={subagentResult('f1', 'failed')}
      />
    ),
    failed
  );
  assert.ok(failed.querySelector('.subagent-block')!.classList.contains('error'));

  const group = container();
  const groupResult: ChatMessage = {
    id: 'r-g1',
    role: 'tool',
    tool_call_id: 'g1',
    content: 'Group “api”: …',
    meta: {
      group: {
        name: 'api',
        conflict: false,
        workers: [
          { step: 1, label: 'w1', status: 'done', childDialogId: 'kid-w1', landed: ['src/a.rs'] },
          { step: 2, label: 'w2', status: 'blocked', childDialogId: 'kid-w2' },
        ],
      },
    },
  };
  render(
    () => (
      <ToolCallBlock
        toolCall={{ id: 'g1', name: 'task', arguments: JSON.stringify({ group: 'api' }) }}
        dialogId="lead-s"
        result={groupResult}
      />
    ),
    group
  );
  assert.equal(group.querySelector('.tool-call-meta')?.textContent, '2 workers · 1 landed');
});

test('a group’s result lets go of its workers’ live entries (P5.9)', () => {
  for (const label of ['w1', 'w2'])
    noteSubagentStarted({
      dialogId: 'lead-gw',
      toolCallId: `g1#${label}`,
      childDialogId: `kid-${label}`,
      kind: 'worker',
      model: 'm',
      goal: 'Step',
    });
  const root = container();
  render(
    () => (
      <ToolCallBlock
        toolCall={{ id: 'g1', name: 'task', arguments: JSON.stringify({ group: 'api' }) }}
        dialogId="lead-gw"
        result={{
          id: 'r-gw',
          role: 'tool',
          tool_call_id: 'g1',
          content: '',
          meta: {
            group: {
              name: 'api',
              conflict: false,
              workers: [
                { step: 1, label: 'w1', status: 'done', childDialogId: 'kid-w1' },
                { step: 2, label: 'w2', status: 'done', childDialogId: 'kid-w2' },
              ],
            },
          },
        }}
      />
    ),
    root
  );
  assert.equal(liveSubagent('lead-gw', 'g1#w1'), undefined);
  assert.equal(liveSubagent('lead-gw', 'g1#w2'), undefined);
});

test('review_record_stop_enablement', () => {
  const meta: ReviewMeta = {
    kind: 'change',
    status: 'running',
    round: 1,
    maxRounds: 3,
    childDialogId: 'kid-rv',
  };
  const live = container();
  render(() => <ReviewRecord meta={meta} dialogId="d-rv" recordId="m1" live={true} />, live);
  assert.ok(button(live, 'Stop review'));

  const gone = container();
  render(() => <ReviewRecord meta={meta} dialogId="d-rv" recordId="m2" live={false} />, gone);
  assert.equal(button(gone, 'Stop review'), undefined);
  assert.ok(gone.firstElementChild!.classList.contains('stopped'));
});

test('verification_record_triage_states', () => {
  const base: VerificationMeta = {
    status: 'failed',
    round: 1,
    maxRounds: 3,
    checks: [],
    changedFiles: ['a.ts'],
  };
  const props = {
    dialogId: 'd-vt',
    latest: true,
    onRunChecks: () => {},
    onOpenProjectChecks: () => {},
  };
  // Saved as running, with no run going: the triage was cut short.
  const cut = container();
  render(
    () => (
      <VerificationRecord
        meta={{
          ...base,
          triage: {
            status: 'running',
            check: 'test',
            reason: 'large output',
            childDialogId: 'kid-vt1',
          },
        }}
        recordId="v1"
        live={false}
        {...props}
      />
    ),
    cut
  );
  assert.ok(cut.querySelector('.chat-verify-triage.stopped'));
  assert.equal(button(cut, 'Show its work'), undefined);

  const done = container();
  render(
    () => (
      <VerificationRecord
        meta={{
          ...base,
          integrityChecked: false,
          stateRestored: false,
          triage: {
            status: 'done',
            check: 'test',
            reason: 'large output',
            childDialogId: 'kid-vt2',
            result: {
              check: 'test',
              rootCause: 'off by one',
              files: ['src/a.ts'],
              nextStep: 'fix the loop',
              confidence: 'high',
              confidenceNote: '',
            },
          },
        }}
        recordId="v2"
        live={false}
        {...props}
      />
    ),
    done
  );
  assert.ok(done.querySelector('.chat-verify-triage.done'));
  assert.ok(button(done, 'Show its work'));
  const text = done.textContent ?? '';
  assert.match(text, /weakens tests couldn’t be checked/);
  assert.match(text, /earlier rounds’ state was gone/);
});

test('review_record_doesnt_remount_when_meta_changes (P5.1)', async () => {
  const plan = { id: 'p1', name: 'submit_plan', arguments: JSON.stringify({ plan: '# Plan' }) };
  const review = (findings: number): ReviewMeta => ({
    kind: 'plan',
    status: 'findings',
    round: 1,
    maxRounds: 2,
    childDialogId: 'kid-plan',
    findings: Array.from({ length: findings }, (_, i) => ({
      severity: 'medium' as const,
      location: `Step ${i + 1}`,
      path: null,
      line: null,
      title: `Finding ${i + 1}`,
      scenario: 's',
      fix: 'f',
      confidence: 'plausible' as const,
    })),
  });
  const [chat, setChat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Plan it' },
      { id: 'a1', role: 'assistant', content: '', toolCalls: [plan] },
      {
        id: 't1',
        role: 'tool',
        tool_call_id: 'p1',
        content: 'Saved.',
        meta: { review: review(1) },
      },
    ] as ChatMessage[],
    hasOlder: false,
    hasNewer: false,
    loadingOlder: false,
    lastUsed: 0,
  });
  const api = chatApi as { getMessages: typeof chatApi.getMessages };
  const original = api.getMessages;
  api.getMessages = () => Promise.resolve({ messages: [], hasOlder: false, hasNewer: false });
  const root = container();
  const noop = async () => 0;
  let dispose = () => {};
  try {
    dispose = render(
      () => (
        <MessageList
          dialogId="d-plan"
          chat={chat}
          sending={false}
          focusMessageId={null}
          onLoadOlder={noop}
          onLoadNewer={noop}
          onReloadLatest={async () => {}}
          onRetry={() => {}}
          onContinue={() => {}}
          onContinueRun={() => {}}
          onRunChecks={() => {}}
          onOpenProjectChecks={() => {}}
        />
      ),
      root
    );
    const before = root.querySelector('.chat-review');
    assert.ok(before, 'the plan review is shown');
    button(before!, 'Show its work')!.click();
    assert.ok(button(before!, 'Hide its work'));

    // The stored record changes (here: another finding).
    setChat('messages', 2, 'meta', { review: review(2) });
    const after = root.querySelector('.chat-review');
    assert.equal(after, before, 'the same element: not mounted again');
    assert.equal(after!.querySelectorAll('.review-finding').length, 2);
    assert.ok(button(after!, 'Hide its work'), 'its work is still shown');
  } finally {
    // Its clock (setInterval) would keep the test process alive.
    dispose();
    api.getMessages = original;
  }
});

test('a planner record: the plan taken, problems flagged, the plan on demand (T01)', () => {
  const root = container();
  render(
    () => (
      <>
        <PlannerRecord
          meta={{
            role: 'plan',
            candidate: 'a',
            status: 'done',
            title: 'Greet',
            steps: 2,
            plan: '# Greet\n\n## Steps\n\n1. **Say hi**',
            chosen: true,
            model: 'Test/m',
            toolCalls: 4,
            ms: 2000,
          }}
          dialogId="lead-p"
          recordId="pa"
          live={false}
        />
        <PlannerRecord
          meta={{
            role: 'plan',
            candidate: 'b',
            status: 'done',
            title: 'Greet',
            steps: 1,
            problems: ['Step 1 names `gone.txt`, which isn’t in the project'],
          }}
          dialogId="lead-p"
          recordId="pb"
          live={false}
        />
        <PlannerRecord
          meta={{ role: 'plan', candidate: 'c', status: 'running' }}
          dialogId="lead-p"
          recordId="pc"
          live={false}
        />
      </>
    ),
    root
  );
  const records = [...root.querySelectorAll('.chat-planner')];
  assert.deepEqual(
    records.map((r) => r.className),
    [
      'chat-verify chat-planner done chosen',
      'chat-verify chat-planner flagged',
      'chat-verify chat-planner stopped',
    ]
  );
  assert.equal(
    records[0].querySelector('.chat-verify-files')?.textContent,
    'Test/m · 4 tool calls · 2 s'
  );
  assert.equal(records[0].querySelector('details pre')?.textContent?.startsWith('# Greet'), true);
  assert.equal(
    records[1].querySelector('.chat-verify-flags li')?.textContent,
    'Step 1 names `gone.txt`, which isn’t in the project'
  );
  assert.equal(
    records[2].querySelector('.chat-verify-title')?.textContent,
    'Planner C didn’t finish: the run ended meanwhile.'
  );
});

test('a judge record: what it chose and why, its merged plan on demand (T02)', () => {
  const root = container();
  render(
    () => (
      <>
        <JudgeRecord
          meta={{
            role: 'plans',
            status: 'done',
            offered: ['a', 'b', 'c'],
            choice: 'merged',
            revised: true,
            title: 'Greet',
            steps: 2,
            plan: '# Greet\n\n## Steps\n\n1. **Say hi**',
            why: 'Plan a’s steps with plan c’s criteria.',
            model: 'Test/m',
          }}
          dialogId="lead-j"
          recordId="j1"
          live={false}
        />
        <JudgeRecord
          meta={{
            role: 'plans',
            status: 'skipped',
            reason: 'its model failed',
            fallback: 'b',
          }}
          dialogId="lead-j"
          recordId="j2"
          live={false}
        />
      </>
    ),
    root
  );
  const records = [...root.querySelectorAll('.chat-judge')];
  assert.deepEqual(
    records.map((r) => r.className),
    ['chat-verify chat-judge done', 'chat-verify chat-judge skipped']
  );
  assert.equal(
    records[0].querySelector('.chat-judge-why')?.textContent,
    'Plan a’s steps with plan c’s criteria.'
  );
  assert.equal(records[0].querySelector('details summary')?.textContent, 'The merged plan');
  assert.equal(records[0].querySelector('details pre')?.textContent?.startsWith('# Greet'), true);
  assert.equal(
    records[1].querySelector('.chat-verify-title')?.textContent,
    'The judge gave no choice: its model failed. The task takes planner B’s plan.'
  );
  assert.equal(records[1].querySelector('details'), null, 'no plan of its own');
});

test('criteria records: the set written, the merge with what it assumed (T03)', () => {
  const root = container();
  render(
    () => (
      <>
        <PlannerRecord
          meta={{
            role: 'criteria',
            candidate: 'b',
            status: 'done',
            criteria: ['It greets.', 'It waves.'],
            outOfScope: ['The CLI.'],
            assumptions: [],
          }}
          dialogId="lead-c"
          recordId="cb"
          live={false}
        />
        <JudgeRecord
          meta={{
            role: 'criteria',
            status: 'done',
            offered: ['a', 'b'],
            criteria: ['It greets.'],
            outOfScope: [],
            assumptions: ['The sets disagreed on waving: none, the narrower reading.'],
          }}
          dialogId="lead-c"
          recordId="cj"
          live={false}
        />
      </>
    ),
    root
  );
  const [writer, judge] = [...root.querySelectorAll('.chat-verify')];
  assert.equal(
    writer.querySelector('.chat-verify-title')?.textContent,
    'Criteria writer B wrote 2 criteria.'
  );
  assert.deepEqual(
    [...writer.querySelectorAll('.chat-planner-criteria li')].map((li) => li.textContent),
    ['It greets.', 'It waves.']
  );
  assert.equal(writer.querySelector('.chat-planner-part')?.textContent, 'Out of scope: The CLI. ');
  assert.equal(writer.querySelector('.chat-planner-assumptions'), null);
  assert.equal(
    judge.querySelector('.chat-verify-title')?.textContent,
    'The judge merged the 2 sets into 1 criterion.'
  );
  assert.equal(
    judge.querySelector('.chat-planner-assumptions li')?.textContent,
    'The sets disagreed on waving: none, the narrower reading.'
  );
});

test('the adversarial tester’s record: its tests and marks, what it removed (T04)', () => {
  const root = container();
  render(
    () => (
      <BreakerRecord
        meta={{
          status: 'done',
          files: ['edge.test.js'],
          marks: [{ file: 'edge.test.js', mark: 'criterion 1' }],
          removed: ['guess.test.js'],
          found: true,
          summary: 'Tried the line’s ending.',
        }}
        dialogId="lead-b"
        recordId="b1"
        live={false}
      />
    ),
    root
  );
  const record = root.querySelector('.chat-breaker')!;
  assert.equal(record.className, 'chat-verify chat-tests chat-breaker found');
  assert.equal(
    record.querySelector('.chat-tests-files li')?.textContent,
    'edge.test.js · criterion 1'
  );
  assert.equal(
    record.querySelector('.chat-breaker-removed')?.textContent,
    'Removed, speculative and passing: guess.test.js '
  );
  assert.equal(
    record.querySelector('.chat-tests-summary')?.textContent,
    'Tried the line’s ending.'
  );
});

test('the acceptance record: what the check took once done, its work while it runs (S08)', () => {
  const criteria = ['It greets by name', 'It exits 0'];
  const done = container();
  render(
    () => (
      <AcceptanceRecord
        meta={{
          status: 'satisfied',
          round: 1,
          maxRounds: 2,
          criteria,
          verdicts: [
            { criterion: 1, satisfied: true, note: 'greet.ts prints it' },
            { criterion: 2, satisfied: true, note: 'the test exits 0' },
          ],
          childDialogId: 'acc-kid',
          model: 'Test/m',
          toolCalls: 3,
          ms: 12_400,
        }}
        dialogId="lead-acc"
        recordId="acc1"
        live={false}
      />
    ),
    done
  );
  const line = done.querySelector('.chat-verify-files');
  assert.equal(line?.textContent, 'Test/m · 3 tool calls · 12 s · Show its work');
  // Cut short by its budget: the line says so.
  const cut = container();
  render(
    () => (
      <AcceptanceRecord
        meta={{ status: 'skipped', round: 1, maxRounds: 2, criteria, partial: true, ms: 900 }}
        dialogId="lead-acc"
        recordId="acc2"
        live={false}
      />
    ),
    cut
  );
  assert.equal(
    cut.querySelector('.chat-verify-files')?.textContent,
    '1 s · cut short by its budget'
  );
  // While it runs, once its chat is known, its work is there as a task call's.
  noteSubagentStarted({
    dialogId: 'lead-acc',
    toolCallId: 'acc3',
    childDialogId: 'acc-kid-3',
    kind: 'acceptance',
    model: 'Test/m',
    goal: 'Check the change against its criteria',
  });
  const running = container();
  render(
    () => (
      <AcceptanceRecord
        meta={{ status: 'running', round: 1, maxRounds: 2, criteria }}
        dialogId="lead-acc"
        recordId="acc3"
        live={true}
      />
    ),
    running
  );
  assert.equal(running.querySelector('.chat-verify-files')?.textContent, 'Show its work');
  assert.equal(
    running.querySelector('.chat-verify-progress')?.textContent,
    '0 tool calls · Test/m'
  );
  // Before its chat is known: nothing to show yet.
  const starting = container();
  render(
    () => (
      <AcceptanceRecord
        meta={{ status: 'running', round: 1, maxRounds: 2, criteria }}
        dialogId="lead-acc"
        recordId="acc4"
        live={true}
      />
    ),
    starting
  );
  assert.equal(starting.querySelector('.chat-verify-files'), null);
});

test('the candidates’ record: each one’s line, its work on demand, one waiting (T05)', () => {
  const root = container();
  render(
    () => (
      <CandidatesRecord
        meta={{
          task: 't',
          round: 1,
          status: 'running',
          candidates: [
            {
              letter: 'a',
              chat: 'cand-a',
              status: 'done',
              outcome: 'verified',
              summary: '1 check passed',
            },
            { letter: 'b', chat: 'cand-b', status: 'waiting' },
          ],
        }}
        dialogId="lead-k"
        recordId="k1"
        live={true}
      />
    ),
    root
  );
  const record = root.querySelector('.chat-candidates')!;
  assert.equal(record.className, 'chat-verify chat-candidates running');
  const lines = [...record.querySelectorAll('.chat-candidate')].map((li) => li.textContent);
  assert.deepEqual(lines, ['A: done · 1 check passed · Show its work', 'B: waiting']);
  // D19: the installs a candidate's copy got before it started.
  const prepared = container();
  render(
    () => (
      <CandidatesRecord
        meta={{
          task: 't',
          round: 1,
          status: 'running',
          candidates: [
            {
              letter: 'a',
              chat: 'cand-a',
              status: 'running',
              prepared: 'Installed: `npm ci --no-audit --no-fund`',
            },
          ],
        }}
        dialogId="lead-k"
        recordId="k3"
        live={true}
      />
    ),
    prepared
  );
  const line = prepared.querySelector('.chat-candidate-prepared')!;
  assert.equal(line.textContent, 'Installed: npm ci --no-audit --no-fund');
  assert.equal(line.querySelector('code')?.textContent, 'npm ci --no-audit --no-fund');
  // A running candidate's line follows its run, from the progress its run
  // tells the task's chat under the record's call (candidates.rs).
  noteSubagentStarted({
    dialogId: 'lead-k',
    toolCallId: 'k4:a',
    childDialogId: 'cand-a4',
    kind: 'candidate',
    model: 'Test/m',
    goal: 'Candidate a',
  });
  noteSubagentProgress({
    dialogId: 'lead-k',
    toolCallId: 'k4:a',
    toolCalls: 3,
    currentTool: 'write',
    model: 'Test/m',
  });
  const going = container();
  render(
    () => (
      <CandidatesRecord
        meta={{
          task: 't',
          round: 1,
          status: 'running',
          candidates: [{ letter: 'a', chat: 'cand-a4', status: 'running' }],
        }}
        dialogId="lead-k"
        recordId="k4"
        live={true}
      />
    ),
    going
  );
  assert.equal(
    going.querySelector('.chat-candidate')?.textContent,
    'A: implementing… · 3 tool calls · write · Show its work'
  );
  // Interrupted: nothing runs it any more.
  const cut = container();
  render(
    () => (
      <CandidatesRecord
        meta={{ task: 't', round: 1, status: 'running', candidates: [] }}
        dialogId="lead-k"
        recordId="k2"
        live={false}
      />
    ),
    cut
  );
  assert.equal(
    cut.querySelector('.chat-verify-title')?.textContent,
    'The candidates didn’t finish: the run ended meanwhile.'
  );
});

test('the pick’s record: the scores, the winner, the loser’s diff kept (T06)', () => {
  const root = container();
  const score = {
    changed: true,
    finished: true,
    testsPassed: 1,
    checksPassed: 1,
    openFindings: 0,
    diffLines: 2,
  };
  render(
    () => (
      <PickRecord
        meta={{
          task: 't',
          status: 'done',
          winner: 'a',
          judged: false,
          adopted: true,
          candidates: [
            { letter: 'a', chat: 'cand-a', score, files: ['ok.txt'] },
            {
              letter: 'b',
              chat: 'cand-b',
              score: { ...score, diffLines: 3 },
              files: ['ok.txt', 'notes.txt'],
              diff: '--- notes.txt (new file)\n+Wrote ok.',
            },
          ],
        }}
        live={false}
      />
    ),
    root
  );
  const record = root.querySelector('.chat-pick')!;
  assert.equal(record.className, 'chat-verify chat-pick done');
  assert.equal(
    record.querySelector('.chat-verify-title')?.textContent,
    'Candidate A’s change is the task’s: it scored better.'
  );
  assert.equal(
    record.querySelector('.chat-pick-winner')?.textContent,
    'A: 1 test written first passes · 1 check passes · no findings open · 2 lines changed'
  );
  const kept = record.querySelector('details')!;
  assert.equal(kept.querySelector('summary')?.textContent, 'Candidate B’s change, not taken');
  assert.equal(kept.querySelector('pre')?.textContent, '--- notes.txt (new file)\n+Wrote ok.');
});
