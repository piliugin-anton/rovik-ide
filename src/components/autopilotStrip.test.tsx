// Component tests (A05): the strip above the input and a stage's record in
// the chat, in each of their states. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import { createStore } from 'solid-js/store';
import AutopilotStrip from './AutopilotStrip';
import StageRecord from './StageRecord';
import MessageList from './MessageList';
import { chatApi } from '../lib/chatApi';
import type { StageMeta, StageRow } from '../lib/autopilot';
import type { ChatMessage } from '../types';

const record = (
  id: string,
  name: StageMeta['name'],
  status: StageMeta['status'],
  extra: Partial<StageMeta> = {}
): StageRow => ({
  id,
  stage: { task: 'u1', name, status, round: 1, effort: 'balanced', ...extra },
});

/** The strip's stages: [label, state] as shown. */
const shown = (root: HTMLElement) =>
  [...root.querySelectorAll('.autopilot-stage')].map((li) => [
    li.querySelector('.autopilot-stage-label')?.textContent,
    li.className.replace('autopilot-stage ', ''),
  ]);

test('the strip shows each stage of the task with how it stands, and the task in a line', () => {
  const root = container();
  const shows: string[] = [];
  const [rows, setRows] = createSignal<StageRow[]>([
    record('r1', 'plan', 'done', { summary: 'Plan saved' }),
    record('r2', 'planReview', 'skipped', { summary: 'Not available yet' }),
    record('r3', 'implement', 'running', { round: 2 }),
  ]);
  const [running, setRunning] = createSignal(true);
  const dispose = render(
    () => <AutopilotStrip rows={rows()} running={running()} onShow={(id) => shows.push(id)} />,
    root
  );
  try {
    assert.equal(root.querySelector('.autopilot-strip-line')?.textContent, 'Autopilot: implement…');
    assert.deepEqual(shown(root), [
      ['Plan', 'done'],
      ['Plan review', 'skipped'],
      ['Prepare', 'pending'],
      ['Tests', 'pending'],
      ['Implement', 'running'],
      ['Change review', 'pending'],
      ['Checks', 'pending'],
      ['Acceptance', 'pending'],
      ['Deliver', 'pending'],
    ]);
    const buttons = [...root.querySelectorAll<HTMLButtonElement>('.autopilot-stage-button')];
    // The summary on hover; the round when it ran more than once.
    assert.equal(buttons[0].title, 'Plan: done. Plan saved');
    assert.equal(buttons[4].querySelector('.autopilot-stage-round')?.textContent, '×2');
    assert.ok(buttons[4].querySelector('.verify-spin'), 'a spinner while it runs');
    // A click shows the stage's record; a stage not reached has none.
    buttons[0].click();
    assert.deepEqual(shows, ['r1']);
    assert.equal(buttons[2].disabled, true);

    // The app closed mid-stage: nothing runs it any more.
    setRunning(false);
    assert.equal(
      root.querySelector('.autopilot-strip-line')?.textContent,
      'Autopilot: implement didn’t finish'
    );
    assert.equal(shown(root)[4][1], 'stopped');

    // It ends: each stage as recorded, the buttons the same elements.
    setRows([
      ...rows().slice(0, 2),
      record('r3', 'implement', 'done', { round: 2 }),
      record('r4', 'review', 'skipped'),
      record('r5', 'checks', 'failed', { outcome: 'gave-up', summary: 'Checks still fail' }),
      record('r6', 'deliver', 'failed', { outcome: 'gave-up' }),
    ]);
    assert.equal(
      root.querySelector('.autopilot-strip-line')?.textContent,
      'Autopilot: checks failed, not delivered'
    );
    assert.deepEqual(
      shown(root).map(([, state]) => state),
      ['done', 'skipped', 'pending', 'pending', 'done', 'skipped', 'failed', 'pending', 'failed']
    );
    const after = [...root.querySelectorAll<HTMLButtonElement>('.autopilot-stage-button')];
    assert.equal(after[4], buttons[4], 'not mounted again');
    assert.equal(after[6].title, 'Checks: failed. Checks still fail');
  } finally {
    dispose();
  }
});

test('a stage’s record in the chat: at work, cut, ended with its summary', () => {
  const root = container();
  const [stage, setStage] = createSignal<StageMeta>(record('r1', 'plan', 'running').stage);
  const [live, setLive] = createSignal(true);
  const dispose = render(() => <StageRecord stage={stage()} live={live()} />, root);
  try {
    const el = () => root.querySelector('.chat-stage')!;
    assert.equal(el().className, 'chat-stage running');
    assert.equal(el().querySelector('.chat-stage-name')?.textContent, 'Autopilot · Plan');
    assert.equal(el().querySelector('.chat-stage-status')?.textContent, 'running');
    setLive(false);
    assert.equal(el().className, 'chat-stage stopped');
    assert.equal(el().querySelector('.chat-stage-status')?.textContent, 'didn’t finish');
    setStage({ ...stage(), status: 'failed', round: 2, summary: 'The run ended without a plan' });
    assert.equal(el().className, 'chat-stage failed');
    assert.equal(el().querySelector('.chat-stage-name')?.textContent, 'Autopilot · Plan, round 2');
    assert.equal(
      el().querySelector('.chat-stage-summary')?.textContent,
      'The run ended without a plan'
    );
    // Files and commands in backticks show as code.
    setStage({ ...stage(), summary: 'Wrote `a.test.ts`, `b.test.ts`' });
    const codes = [...el().querySelectorAll('.chat-stage-summary code')].map((c) => c.textContent);
    assert.deepEqual(codes, ['a.test.ts', 'b.test.ts']);
    assert.equal(
      el().querySelector('.chat-stage-summary')?.textContent,
      'Wrote a.test.ts, b.test.ts'
    );
    setStage({ ...stage(), summary: 'One ` alone' });
    assert.equal(el().querySelector('.chat-stage-summary')?.textContent, 'One ` alone');
  } finally {
    dispose();
  }
});

test('a stage that waits out the provider’s rate limit shows waiting, in the strip and the chat', () => {
  const root = container();
  const waiting = record('r1', 'plan', 'running', {
    summary: 'Rate limited by the provider: again in 1 min',
    rateLimit: { until: 1_700_000_000_000, waits: 1, waitedMs: 60_000 },
  });
  const dispose = render(
    () => (
      <>
        <AutopilotStrip rows={[waiting]} running={false} onShow={() => {}} />
        <StageRecord stage={waiting.stage} live={false} />
      </>
    ),
    root
  );
  try {
    assert.equal(
      root.querySelector('.autopilot-strip-line')?.textContent,
      'Autopilot: plan waits out the provider’s rate limit'
    );
    assert.equal(shown(root)[0][1], 'waiting');
    const line = root.querySelector('.chat-stage')!;
    assert.equal(line.className, 'chat-stage waiting');
    assert.equal(line.querySelector('.chat-stage-status')?.textContent, 'waiting');
    assert.ok(line.querySelector('svg circle'), 'a clock, not a pause');
  } finally {
    dispose();
  }
});

test('the chat shows a stage record where the stage began', () => {
  const [chat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Make the test pass', meta: { mode: 'autopilot' } },
      {
        id: 's1',
        role: 'assistant',
        content: '',
        meta: { stage: record('s1', 'plan', 'done', { summary: 'Plan saved' }).stage },
      },
      { id: 'a1', role: 'assistant', content: 'The plan is saved.' },
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
          dialogId="d-autopilot"
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
    const item = root.querySelector('.chat-item[data-id="s1"]');
    assert.ok(item?.querySelector('.chat-stage.done'), 'the record, as it ended');
    assert.equal(item?.querySelector('.chat-message'), null, 'and nothing else');
    assert.equal(root.querySelectorAll('.chat-stage').length, 1);
  } finally {
    // Its clock (setInterval) would keep the test process alive.
    dispose();
    api.getMessages = original;
  }
});

test('the Prepare stage’s installs show in the chat, live and as they ended (S03)', () => {
  const [chat, setChat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Make the test pass', meta: { mode: 'autopilot' } },
      {
        id: 's2',
        role: 'assistant',
        content: '',
        meta: { stage: record('s2', 'prepare', 'running').stage },
      },
      {
        id: 'p1',
        role: 'assistant',
        content: '',
        meta: {
          prepare: {
            status: 'running',
            installs: [
              { command: 'npm ci --no-audit --no-fund', folder: '', status: 'running' },
              { command: 'uv sync', folder: 'api', status: 'pending' },
            ],
          },
        },
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
  const [sending, setSending] = createSignal(true);
  let dispose = () => {};
  try {
    dispose = render(
      () => (
        <MessageList
          dialogId="d-prepare"
          chat={chat}
          sending={sending()}
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
    const card = () => root.querySelector('.chat-item[data-id="p1"] .chat-verify');
    const rows = () =>
      [...card()!.querySelectorAll('.chat-verify-check')].map((li) => [
        li.className.replace('chat-verify-check ', ''),
        li.querySelector('.chat-verify-command')?.textContent,
        li.querySelector('.chat-verify-note')?.textContent,
      ]);
    assert.equal(card()?.className, 'chat-verify running');
    assert.equal(
      card()?.querySelector('.chat-verify-title')?.textContent,
      'Installing the dependencies the lockfiles name…'
    );
    assert.deepEqual(rows(), [
      ['running', 'npm ci --no-audit --no-fund', ''],
      ['pending', 'uv sync', 'in api'],
    ]);
    // One failed: its output opens from its row.
    setChat('messages', 2, 'meta', {
      prepare: {
        status: 'failed',
        installs: [
          {
            command: 'npm ci --no-audit --no-fund',
            folder: '',
            status: 'passed',
            durationMs: 2300,
          },
          {
            command: 'uv sync',
            folder: 'api',
            status: 'failed',
            exitCode: 2,
            output: 'error: No interpreter found for Python 3.13',
          },
        ],
      },
    });
    assert.equal(card()?.className, 'chat-verify failed');
    assert.equal(
      card()?.querySelector('.chat-verify-title')?.textContent,
      '1 of 2 installs failed; the checks will say what’s missing.'
    );
    assert.deepEqual(rows(), [
      ['passed', 'npm ci --no-audit --no-fund', '2.3 s'],
      ['failed', 'uv sync', 'exit 2 · in api'],
    ]);
    assert.match(card()!.querySelector('details pre')!.textContent!, /No interpreter/);
    // Saved as running, with nothing running it: it didn't finish.
    setChat('messages', 2, 'meta', {
      prepare: { status: 'running', installs: [] },
    });
    setSending(false);
    assert.equal(card()?.className, 'chat-verify stopped');
    assert.equal(
      card()?.querySelector('.chat-verify-title')?.textContent,
      'Installing the dependencies didn’t finish: the run ended meanwhile.'
    );
    assert.equal(root.querySelector('.chat-item[data-id="p1"] .chat-message'), null, 'no bubble');
  } finally {
    dispose();
    api.getMessages = original;
  }
});

test('the tests written first show in the chat, with what passes already (S04)', () => {
  const [chat, setChat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Make the test pass', meta: { mode: 'autopilot' } },
      {
        id: 's3',
        role: 'assistant',
        content: '',
        meta: { stage: record('s3', 'tests', 'running').stage },
      },
      { id: 't1', role: 'assistant', content: '', meta: { tests: { status: 'running' } } },
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
          dialogId="d-tests"
          chat={chat}
          sending={true}
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
    const card = () => root.querySelector('.chat-item[data-id="t1"] .chat-tests');
    const title = () => card()?.querySelector('.chat-verify-title')?.textContent;
    assert.equal(card()?.className, 'chat-verify chat-tests running');
    assert.equal(title(), 'Writing the tests before the code, from the plan’s criteria…');
    setChat('messages', 2, 'meta', {
      tests: {
        status: 'done',
        files: ['test/a.test.js', 'test/b.test.js'],
        passing: ['test/b.test.js'],
        summary: 'Checks the greeting (criterion 1) and an empty name (criterion 2).',
        assumptions: 'None.',
        report: '## Summary\n\nChecks the greeting.',
        childDialogId: 'c1',
        model: 'Test/m',
        toolCalls: 3,
        ms: 4200,
      },
    });
    assert.equal(card()?.className, 'chat-verify chat-tests done');
    assert.equal(
      title(),
      'Wrote 2 test files before the code; 1 of them already passes, so it may not test the change.'
    );
    const files = [...card()!.querySelectorAll('.chat-tests-files li')].map((li) => li.textContent);
    assert.deepEqual(files, ['test/a.test.js', 'test/b.test.js · passes already']);
    assert.match(card()!.querySelector('.chat-tests-summary')!.textContent!, /criterion 2/);
    assert.equal(
      card()!.querySelector('.chat-verify-files')?.textContent,
      'Test/m · 3 tool calls · 4 s · Show its work'
    );
    assert.ok(card()!.querySelector('details.review-details pre.review-report'));
    // Blocked, nothing written: why, and the task goes on.
    setChat('messages', 2, 'meta', {
      tests: { status: 'blocked', reason: 'The test writer was blocked: Nothing to test' },
    });
    assert.equal(card()?.className, 'chat-verify chat-tests blocked');
    assert.equal(
      title(),
      'The test writer was blocked: Nothing to test. The task goes on without tests written first.'
    );
    assert.equal(card()!.querySelector('.chat-tests-files'), null);
    assert.equal(root.querySelector('.chat-item[data-id="t1"] .chat-message'), null, 'no bubble');
  } finally {
    dispose();
    api.getMessages = original;
  }
});

test('an acceptance check shows each criterion with its verdict (S08)', () => {
  const [chat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Add a greeting', meta: { mode: 'autopilot' } },
      {
        id: 'c1',
        role: 'assistant',
        content: '',
        meta: {
          acceptance: {
            status: 'unsatisfied',
            round: 2,
            maxRounds: 2,
            unmet: true,
            criteria: ['greet("Ann") returns "Hello, Ann".', 'An empty name throws.'],
            verdicts: [
              { criterion: 1, satisfied: true, note: 'src/greet.ts:3 returns it.' },
              { criterion: 2, satisfied: false, note: 'nothing throws on "".' },
            ],
            childDialogId: 'k1',
          },
        },
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
          dialogId="d-acceptance"
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
    const card = root.querySelector('.chat-item[data-id="c1"] .chat-acceptance');
    assert.equal(card?.className, 'chat-verify chat-acceptance unmet');
    assert.equal(
      card?.querySelector('.chat-verify-title')?.textContent,
      '1 of 2 criteria still unmet after the last check: the task ends unmet.'
    );
    const rows = [...card!.querySelectorAll('.chat-acceptance-criteria li')].map((li) => [
      li.className,
      li.querySelector('.chat-acceptance-verdict')?.textContent,
    ]);
    assert.deepEqual(rows, [
      ['met', 'Met: src/greet.ts:3 returns it.'],
      ['unmet', 'Unmet: nothing throws on "".'],
    ]);
    assert.ok(card!.querySelector('.chat-verify-files button'), 'its work on demand');
  } finally {
    dispose();
    api.getMessages = original;
  }
});

test('a task’s report shows as its last message, with the facts under it (S09)', () => {
  const [chat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Make the test pass', meta: { mode: 'autopilot' } },
      {
        id: 'b1',
        role: 'user',
        content: '# Write the task’s report …',
        meta: { harness: 'report' },
      },
      {
        id: 'r1',
        role: 'assistant',
        content: '## Assumptions\n\nNone.\n\n## Open\n\nNothing.',
        meta: {
          report: {
            outcome: 'unmet',
            ending: 'A criterion is still unmet after the last acceptance check',
            delivered: false,
            partial: true,
            delivery:
              'Delivered as partial, not landed: the acceptance check finds a criterion unmet',
            assumptions: [],
            criteria: ['ok.txt says ok.'],
            verdicts: [{ criterion: 1, satisfied: false, note: 'a stray space' }],
            tests: ['test/ok.test.js'],
            checks: [{ command: 'npm test', status: 'passed' }],
            spent: { ms: 61_000, cost: 0.05, uncosted: 0 },
          },
        },
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
          dialogId="d-report"
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
    // The IDE's brief isn't shown; the report is, as a reply.
    assert.equal(root.querySelector('.chat-item[data-id="b1"] .chat-message'), null);
    const item = root.querySelector('.chat-item[data-id="r1"]');
    assert.match(item?.querySelector('.chat-message')?.textContent ?? '', /Assumptions/);
    const card = item?.querySelector('.chat-report');
    assert.equal(card?.className, 'chat-verify chat-report partial');
    assert.equal(
      card?.querySelector('.chat-verify-title')?.textContent,
      'Task delivered as partial: a criterion is still unmet after the last acceptance check.'
    );
    const facts = [...card!.querySelectorAll('dt')].map((dt) => dt.textContent);
    assert.deepEqual(facts, ['Result', 'Criteria', 'Tests written', 'Checks', 'Spent']);
    assert.equal(card!.querySelector('li.unmet .chat-acceptance-verdict')?.textContent, 'Unmet');
    assert.match(card!.textContent ?? '', /1 min, 0\.05/);
  } finally {
    dispose();
    api.getMessages = original;
  }
});

test('a stage record spins only while its task’s run goes, not when another took over', () => {
  const plan = (id: string, task: string) => ({
    id,
    role: 'assistant',
    content: '',
    meta: { stage: { ...record(id, 'plan', 'running').stage, task } },
  });
  const [chat, setChat] = createStore({
    messages: [
      { id: 'u1', role: 'user', content: 'Task one', meta: { mode: 'autopilot' } },
      plan('s1', 'u1'),
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
          dialogId="d-takeover"
          chat={chat}
          sending={true}
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
    const state = (id: string) =>
      root.querySelector(`.chat-item[data-id="${id}"] .chat-stage`)?.className;
    assert.equal(state('s1'), 'chat-stage running', 'its task’s run');
    // Cut by a crash, then a new request took over: what runs now isn't its.
    setChat('messages', (list) => [
      ...list,
      { id: 'u2', role: 'user', content: 'Never mind', meta: { mode: 'agent' } },
      { id: 'a2', role: 'assistant', content: 'On it', pending: true },
    ]);
    assert.equal(state('s1'), 'chat-stage stopped');
    // A new task: its own plan spins, the old one stays cut.
    setChat('messages', (list) => [
      ...list,
      { id: 'u3', role: 'user', content: 'Task three', meta: { mode: 'autopilot' } },
      plan('s3', 'u3'),
    ]);
    assert.equal(state('s3'), 'chat-stage running');
    assert.equal(state('s1'), 'chat-stage stopped');
  } finally {
    dispose();
    api.getMessages = original;
  }
});
