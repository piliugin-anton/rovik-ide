// Component test: the chat list's badges (U03) follow a scripted sequence
// of the app's events: a question pending at start-up (`agent_status`),
// approvals asked and settled in a remote chat and in a subagent, a host
// going offline and coming back, and a chat moving. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ChatList, { type ChatListRow } from './ChatList';
import { initAgentRuns } from '../lib/agentRuns';
import { waitingChats } from '../lib/chatBadges';
import { cloud, initCloud } from '../lib/cloud';
import type { DialogSummary } from '../types';

const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];
(window as any).__TAURI_INTERNALS__ = {
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
      return {
        runs: [],
        subagents: [],
        approvals: [],
        questions: [{ dialogId: 'local', toolCallId: 'q1', questions: [] }],
      };
    }
    if (cmd === 'hosts_summaries') {
      return [
        { id: 'h1', name: 'Build', deployed: true, state: 'ready' },
        { id: 'h2', name: 'Spare', deployed: true, state: 'ready' },
      ];
    }
    return null;
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

/** The app sends `payload` on `event`. */
function emit(event: 'agent-event' | 'cloud-event', payload: unknown) {
  for (const l of listeners.filter((l) => l.event === event)) {
    callbacks.get(l.handler)!({ event, id: 1, payload });
  }
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const dialog = (id: string, place?: DialogSummary['place']): DialogSummary => ({
  id,
  title: id,
  createdAt: 1,
  updatedAt: 1,
  messageCount: 1,
  place,
});

/** Each row's badges, by the chat's title. */
function badges(root: HTMLElement): Record<string, string[]> {
  const out: Record<string, string[]> = {};
  for (const item of root.querySelectorAll('.chat-history-item')) {
    const title = item.querySelector('.chat-history-item-title')!.textContent!;
    out[title] = [...item.querySelectorAll('.chat-badge')].map((b) => b.textContent!);
  }
  return out;
}

const approval = (dialogId: string, toolCallId: string, parentDialogId?: string) => ({
  type: 'approvalRequested',
  request: { dialogId, toolCallId, toolName: 'bash', summary: 'Run `npm test`', parentDialogId },
});

test('the badges follow the events: waiting, a host offline and back, a move', async () => {
  await initAgentRuns();
  await initCloud();
  const dialogs = [
    dialog('local'),
    dialog('lead'),
    dialog('on-build', { kind: 'host', host: 'h1' }),
    dialog('on-spare', { kind: 'host', host: 'h2' }),
    dialog('moving', { kind: 'moving', host: 'h1' }),
  ];
  const rows: ChatListRow[] = dialogs.map((d) => ({
    dialog: d,
    title: { text: d.title, ranges: [] },
  }));
  const root = container();
  const dispose = render(
    () => (
      <ChatList
        rows={rows}
        activeId={null}
        query=""
        onQueryChange={() => {}}
        noMatches={false}
        loading={false}
        onSelect={() => {}}
        onDelete={() => {}}
        onNearEnd={() => {}}
        waitingIds={waitingChats()}
        hosts={cloud.hosts}
      />
    ),
    root
  );
  await flush();
  // At start-up: a question waits in the local chat.
  assert.deepEqual(badges(root), {
    local: ['Waiting for you'],
    lead: [],
    'on-build': ['Build'],
    'on-spare': ['Spare'],
    moving: ['Moving…'],
  });

  // An approval in a remote chat, and one in a subagent: its lead waits.
  emit('agent-event', approval('on-build', 't1'));
  emit('agent-event', approval('kid', 't2', 'lead'));
  // The question is answered.
  emit('agent-event', { type: 'questionsSettled', dialogId: 'local', toolCallId: 'q1' });
  await flush();
  assert.deepEqual(badges(root).local, []);
  assert.deepEqual(badges(root).lead, ['Waiting for you']);
  assert.deepEqual(badges(root)['on-build'], ['Waiting for you', 'Build']);

  // A host goes offline: its chats say so.
  emit('cloud-event', { type: 'host', host: 'h2', state: 'offline' });
  await flush();
  assert.deepEqual(badges(root)['on-spare'], ['Spare · offline']);
  const offline = root.querySelector('.chat-badge.offline') as HTMLElement;
  assert.match(offline.title, /can’t be reached now/);

  // The approvals are settled, and the host comes back.
  emit('agent-event', { type: 'approvalSettled', dialogId: 'on-build', toolCallId: 't1' });
  emit('agent-event', { type: 'approvalSettled', dialogId: 'kid', toolCallId: 't2' });
  emit('cloud-event', { type: 'host', host: 'h2', state: 'ready' });
  await flush();
  assert.deepEqual(badges(root), {
    local: [],
    lead: [],
    'on-build': ['Build'],
    'on-spare': ['Spare'],
    moving: ['Moving…'],
  });
  dispose();
});
