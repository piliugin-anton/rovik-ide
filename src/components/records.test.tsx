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
import { noteSubagentStarted } from '../lib/subagents';
import type { ChatMessage } from '../types';

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
