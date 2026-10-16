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
import MessageList from './MessageList';
import { chatApi } from '../lib/chatApi';
import { liveSubagent, noteSubagentStarted } from '../lib/subagents';
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
