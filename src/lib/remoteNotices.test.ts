// remoteNotices.ts tests (node:test), from a scripted sequence of the app's
// events (U06): what waited as the app started is summed up in one notice
// once its host caught up; on a host that has, an approval or a question
// notifies on its own, as a local question does, and a local approval
// doesn't; when the host comes back, what's told again folds into one
// notice; a window that missed `caughtUp` learns it from the summaries.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initAgentRuns, onShowChat } from './agentRuns';
import { initCloud, refreshHosts } from './cloud';
import { notifications, type AppNotification } from './notifications';
import type { ShowTarget } from './remoteNotices';
import type { ChatPlace } from '../types';

const ON_BUILD: ChatPlace = { kind: 'host', host: 'h1' };
/** Where each chat is, as `chat_get_dialog` says: absent here. */
const places: Record<string, ChatPlace | undefined> = {
  build: ON_BUILD,
  'build-2': ON_BUILD,
  'build-3': ON_BUILD,
};

const approval = (
  dialogId: string,
  toolCallId: string,
  summary: string,
  parentDialogId?: string
) => ({
  dialogId,
  toolCallId,
  toolName: 'bash',
  summary,
  parentDialogId,
});
const question = (dialogId: string, toolCallId: string, text: string) => ({
  dialogId,
  toolCallId,
  questions: [{ question: text, options: [] }],
});

let summaries = [{ id: 'h1', name: 'Build', deployed: true, state: 'ready', caughtUp: false }];
const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    transformCallback: (callback: (event: unknown) => void) => {
      const id = callbacks.size + 1;
      callbacks.set(id, callback);
      return id;
    },
    invoke: async (cmd: string, args: any) => {
      if (cmd === 'plugin:event|listen') {
        listeners.push({ event: args.event, handler: args.handler });
        return args.handler;
      }
      if (cmd === 'agent_status') {
        // What waited as the app started: on the host, and here.
        return {
          runs: [],
          subagents: [],
          approvals: [approval('build', 'a1', 'Run `npm test`')],
          questions: [question('build-2', 'q1', 'Which branch?'), question('local', 'q0', 'Ok?')],
        };
      }
      if (cmd === 'hosts_summaries') return summaries;
      if (cmd === 'chat_get_dialog') {
        const id = args.dialogId as string;
        return { id, title: id, createdAt: 1, updatedAt: 1, messageCount: 1, place: places[id] };
      }
      return null;
    },
  },
  __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
  setTimeout: () => 0,
  clearTimeout: () => undefined,
};

/** The app sends `payload` on `event`. */
function emit(event: 'agent-event' | 'cloud-event', payload: unknown) {
  for (const l of listeners.filter((l) => l.event === event)) {
    callbacks.get(l.handler)!({ event, id: 1, payload });
  }
}

/** Lets the chats' lookups and the notices they lead to finish. */
async function settle() {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setImmediate(resolve));
}

const shown = () => notifications().map((n) => n.id);
const notice = (id: string) => notifications().find((n) => n.id === id);
const showOf = (n: AppNotification | undefined) => n?.actions?.find((a) => a.label === 'Show');
const shows: (ShowTarget | undefined)[] = [];
onShowChat((target) => shows.push(target));

test('what waits on a host is told as the U06 notices say, through a reconnection', async () => {
  await initAgentRuns();
  await initCloud();
  await settle();
  // Started: nothing on its own, the host not caught up yet, and the
  // local question from before isn't told again.
  assert.deepEqual(shown(), []);

  emit('cloud-event', { type: 'caughtUp', host: 'h1' });
  await settle();
  assert.deepEqual(shown(), ['waiting-on-h1']);
  const summary = notice('waiting-on-h1')!;
  assert.equal(summary.title, 'Waiting for you on Build');
  assert.equal(summary.message, '1 approval and 1 question, in 2 chats.');
  showOf(summary)!.run();
  assert.deepEqual(shows, [{ list: true }], 'several chats: the list');

  // Caught up: each new one on its own, Show opening its chat (a
  // subagent's lead's); a local approval stays in its chat, a local
  // question notifies as before.
  emit('agent-event', {
    type: 'approvalRequested',
    request: approval('build', 'a2', 'Run `cargo build`'),
  });
  emit('agent-event', {
    type: 'approvalRequested',
    request: approval('kid', 'a3', 'Run `ls`', 'build-3'),
  });
  emit('agent-event', { type: 'approvalRequested', request: approval('local', 'a4', 'Run `rm`') });
  emit('agent-event', { type: 'questionsAsked', request: question('local', 'q2', 'Which file?') });
  await settle();
  const a2 = notice('approval-build-a2')!;
  assert.equal(a2.title, 'The agent needs your approval');
  assert.equal(a2.message, 'Run `cargo build` (on Build)');
  showOf(a2)!.run();
  showOf(notice('approval-kid-a3'))!.run();
  assert.equal(notice('approval-local-a4'), undefined);
  const local = notice('question-local-q2')!;
  assert.equal(local.title, 'The agent has a question');
  assert.equal(local.message, 'Which file?');
  showOf(local)!.run();
  assert.deepEqual(shows.slice(1), [
    { dialogId: 'build' },
    { dialogId: 'build-3' },
    { dialogId: 'local' },
  ]);

  // Settled: its notice goes, and the summary counts again.
  emit('agent-event', { type: 'approvalSettled', dialogId: 'build', toolCallId: 'a2' });
  emit('agent-event', { type: 'approvalSettled', dialogId: 'build', toolCallId: 'a1' });
  await settle();
  assert.equal(notice('approval-build-a2'), undefined);
  assert.equal(notice('waiting-on-h1')!.message, '1 question.');
  emit('agent-event', { type: 'questionsSettled', dialogId: 'build-2', toolCallId: 'q1' });
  await settle();
  assert.equal(notice('waiting-on-h1'), undefined, 'none of it waits');

  // The host goes away and comes back: what it tells again, and what was
  // asked meanwhile, fold into one notice once it caught up.
  emit('cloud-event', { type: 'host', host: 'h1', state: 'offline' });
  emit('cloud-event', { type: 'host', host: 'h1', state: 'ready' });
  emit('agent-event', {
    type: 'approvalRequested',
    request: approval('kid', 'a3', 'Run `ls`', 'build-3'),
  });
  emit('agent-event', { type: 'questionsAsked', request: question('build', 'q3', 'Deploy?') });
  await settle();
  assert.equal(notice('question-build-q3'), undefined, 'not on its own while it connects');
  emit('cloud-event', { type: 'caughtUp', host: 'h1' });
  await settle();
  const back = notice('waiting-on-h1')!;
  assert.equal(back.message, '1 approval and 1 question, in 2 chats.');
  assert.equal(notice('approval-kid-a3'), undefined, 'told before, it folds into the notice');
  assert.equal(notice('question-local-q2')?.message, 'Which file?', 'a local one stays');

  // Away again, and the window missed `caughtUp`: the summaries say it.
  emit('cloud-event', { type: 'host', host: 'h1', state: 'connecting' });
  emit('agent-event', { type: 'approvalSettled', dialogId: 'kid', toolCallId: 'a3' });
  emit('cloud-event', { type: 'host', host: 'h1', state: 'ready' });
  await settle();
  summaries = [{ ...summaries[0], caughtUp: true }];
  await refreshHosts();
  await settle();
  const missed = notice('waiting-on-h1')!;
  assert.equal(missed.message, '1 question.');
  showOf(missed)!.run();
  assert.deepEqual(shows[shows.length - 1], { dialogId: 'build' }, 'one chat: that chat');
});

test('a host that doesn’t catch up gets its notice a while after it connected', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  places.spare = { kind: 'host', host: 'h2' };
  summaries = [
    ...summaries,
    { id: 'h2', name: 'Spare', deployed: true, state: 'ready', caughtUp: false },
  ];
  await refreshHosts();
  emit('agent-event', { type: 'questionsAsked', request: question('spare', 'q9', 'Proceed?') });
  await settle();
  assert.equal(notice('question-spare-q9'), undefined, 'its pulls may still be going');
  t.mock.timers.tick(29_000);
  await settle();
  assert.equal(notice('waiting-on-h2'), undefined);

  t.mock.timers.tick(1_000);
  await settle();
  assert.equal(notice('waiting-on-h2')?.message, '1 question.');
  // What comes after is told on its own.
  emit('agent-event', {
    type: 'approvalRequested',
    request: approval('spare', 'a9', 'Run `make`'),
  });
  await settle();
  assert.equal(notice('approval-spare-a9')?.message, 'Run `make` (on Spare)');
});
