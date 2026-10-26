// steering.ts tests (node:test): what Enter does while a run goes, what a
// queued message's label says, when the hint shows, and how the drafts of
// messages no run holds any more are judged. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  checksRunning,
  HINT_FROM,
  judgeSteerDrafts,
  queuedLabel,
  returnedNotice,
  sendAction,
  showsSteerHint,
  type SteerJudging,
} from './steering';
import type { SteerDraft } from './steerDrafts';
import type { ChatMessage, ChatPlace } from '../types';

test('Enter: the answer to a question, a message, a message to the run, or nothing', () => {
  const at = (questionWaits: boolean, running: boolean, steerable: boolean) =>
    sendAction({ questionWaits, running, steerable, command: false });
  assert.equal(at(true, true, true), 'answer');
  assert.equal(at(false, false, false), 'send');
  assert.equal(at(false, true, true), 'steer');
  // Plan mode, an Autopilot task's run, one still starting: the text stays.
  assert.equal(at(false, true, false), 'hold');
});

test('Enter on a command picked from the / menu: never to the run, which can’t run it mid-turn', () => {
  const at = (running: boolean, steerable: boolean) =>
    sendAction({ questionWaits: false, running, steerable, command: true });
  // Nothing runs: the command is the request of a new run.
  assert.equal(at(false, false), 'send');
  // A run that takes messages goes: it waits, and goes once the run has ended.
  assert.equal(at(true, true), 'after');
  // One that takes none: the text stays, as a message's does.
  assert.equal(at(true, false), 'hold');
});

test('a queued message says what the run is busy with', () => {
  const none = { approval: false, subagents: false, checks: false };
  assert.equal(
    queuedLabel({ ...none, approval: true, subagents: true }),
    'Queued: the agent reads it after you answer the approval'
  );
  assert.equal(
    queuedLabel({ ...none, subagents: true, checks: true }),
    'Queued: the agent reads it when its subagents report back'
  );
  assert.equal(
    queuedLabel({ ...none, checks: true }),
    'Queued: the agent reads it when the checks finish'
  );
  assert.equal(queuedLabel(none), 'Queued: the agent reads it at its next step');
});

const user = (id: string, meta?: ChatMessage['meta']): ChatMessage => ({
  id,
  role: 'user',
  content: id,
  meta,
});
const steer = (id: string) => user(id, { steer: { queuedAt: 1 } });
const record = (id: string, meta: ChatMessage['meta']): ChatMessage => ({
  id,
  role: 'assistant',
  content: '',
  meta,
});

test('the checks of the current request that are running: its change review or its checks', () => {
  const running = (meta: ChatMessage['meta']) => checksRunning([user('u'), record('r', meta)]);
  const verification = (status: 'running' | 'passed') => ({
    status,
    round: 1,
    maxRounds: 3,
    checks: [],
    changedFiles: [],
  });
  assert.ok(running({ verification: verification('running') }));
  assert.ok(!running({ verification: verification('passed') }));
  assert.ok(running({ review: { kind: 'change', status: 'running', maxRounds: 3 } }));
  assert.ok(!running({ review: { kind: 'plan', status: 'running', maxRounds: 3 } }));
  // An earlier request's record that never finished isn't this one's.
  assert.ok(
    !checksRunning([user('u1'), record('r', { verification: verification('running') }), user('u2')])
  );
});

test('the hint: from the third message sent to one request’s run, not across requests', () => {
  const two = [user('u1'), steer('s1'), steer('s2')];
  assert.equal(HINT_FROM, 3);
  assert.ok(!showsSteerHint(two, 0));
  assert.ok(showsSteerHint(two, 1), 'a queued one counts');
  assert.ok(showsSteerHint([...two, steer('s3')], 0));
  // A new request starts the count again.
  assert.ok(!showsSteerHint([...two, steer('s3'), user('u2'), steer('t1')], 1));
});

test('why a message came back, by how the run ended', () => {
  assert.match(returnedNotice('stopped').message, /stopped before it read/);
  assert.match(returnedNotice('verify-stopped').message, /stopped before it read/);
  assert.match(returnedNotice('error').message, /error/);
  assert.match(returnedNotice('verify-needs-approval').message, /checks waiting/);
  assert.match(returnedNotice('repetition').message, /ended before it read/);
  assert.equal(returnedNotice('stopped').title, 'Your message is back in the input');
});

/** The judge with these chats: `running`, `hosts` (chat → place), and what's stored and answered. */
function judging(opts: {
  running?: string[];
  queued?: string[];
  places?: Record<string, ChatPlace>;
  caughtUp?: Record<string, true>;
  stored?: string[];
  answered?: string[];
}) {
  const done: string[] = [];
  const deps: SteerJudging = {
    isRunning: (id) => (opts.running ?? []).includes(id),
    isQueued: (_, messageId) => (opts.queued ?? []).includes(messageId),
    placeOf: async (id) => opts.places?.[id],
    caughtUp: opts.caughtUp ?? {},
    stored: async (ids) => ids.filter((id) => (opts.stored ?? []).includes(id)),
    answered: async (ids) => ids.filter((id) => (opts.answered ?? []).includes(id)),
    clear: (id) => done.push(`clear ${id}`),
    markFailed: (_, id) => done.push(`failed ${id}`),
    giveBack: (d) => done.push(`back ${d.messageId}`),
  };
  return { deps, done };
}

const draft = (dialogId: string, messageId: string, extra: Partial<SteerDraft> = {}) => ({
  dialogId,
  messageId,
  segments: [{ type: 'text' as const, text: messageId }],
  createdAt: '2026-10-21T00:00:00.000Z',
  ...extra,
});

test('a draft no run holds: never read → back; read, no answer → failed and back; answered → gone', async () => {
  const { deps, done } = judging({ stored: ['read', 'answered'], answered: ['answered'] });
  const later = await judgeSteerDrafts(
    [draft('c', 'never'), draft('c', 'read'), draft('c', 'answered')],
    deps
  );
  assert.deepEqual(later, []);
  assert.deepEqual(done, ['back never', 'failed read', 'back read', 'clear answered']);
});

test('a running chat’s drafts, a queued one, and a host chat’s until it caught up, wait', async () => {
  const host: ChatPlace = { kind: 'host', host: 'h1' };
  const { deps, done } = judging({
    running: ['busy'],
    queued: ['shown'],
    places: { remote: host },
  });
  const drafts = [
    draft('busy', 'a'),
    draft('c', 'shown'),
    draft('remote', 'b'),
    draft('c', 'gone', { returned: true }),
  ];
  const later = await judgeSteerDrafts(drafts, deps);
  assert.deepEqual(
    later.map((d) => d.messageId),
    ['a', 'shown', 'b']
  );
  assert.deepEqual(done, [], 'a returned one is the input’s already');
  // Caught up: judged.
  const caught = judging({ places: { remote: host }, caughtUp: { h1: true } });
  await judgeSteerDrafts([draft('remote', 'b')], caught.deps);
  assert.deepEqual(caught.done, ['back b']);
});
