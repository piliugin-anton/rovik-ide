// Component test: a connector's reply in the chat shows one line of what
// it's doing, the chat's own: before its harness has said anything,
// "Starting Claude Code…"; then the run's status, as for any reply. Its
// steps show as Rovik's tool calls do, with no line of the connector's
// own. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import MessageList from './MessageList';
import type { ChatMessage } from '../types';

(window as any).__TAURI_INTERNALS__ = { invoke: async () => null, transformCallback: () => 0 };

const noop = async () => 0;
function list(messages: ChatMessage[], connectorTitle?: string) {
  const root = container();
  const dispose = render(
    () => (
      <MessageList
        dialogId="d1"
        chat={{ messages, hasOlder: false, hasNewer: false, loadingOlder: false, lastUsed: 0 }}
        sending={true}
        connectorTitle={connectorTitle}
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
  return { root, dispose };
}

const user: ChatMessage = { id: 'u1', role: 'user', content: 'Add a test', finalized: true };
const working = (root: HTMLElement) =>
  [...root.querySelectorAll('[role="status"]')].map((e) => e.textContent);

test('a connector’s reply not started yet says the connector is starting', () => {
  const reply: ChatMessage = { id: 'a1', role: 'assistant', content: '', pending: true };
  const { root, dispose } = list([user, reply], 'Claude Code');
  assert.deepEqual(working(root), ['Starting Claude Code…']);
  dispose();
  // A model's reply: the IDE's own word.
  const model = list([user, reply]);
  assert.deepEqual(working(model.root), ['Roviking…']);
  model.dispose();
});

test('once the harness has started, the run’s status is the chat’s own line', () => {
  const reply: ChatMessage = {
    id: 'a1',
    role: 'assistant',
    content: '',
    pending: true,
    status: { text: 'Still waiting for a response' },
    meta: { connector: { kind: 'claude-code', model: 'claude-sonnet-5', steps: [] } },
  };
  const { root, dispose } = list([user, reply], 'Claude Code');
  assert.deepEqual(working(root), ['Still waiting for a response']);
  dispose();
  // Text arriving, then a pause: still one line.
  const writing = list(
    [user, { ...reply, content: 'Added a test that', status: { text: 'Still generating' } }],
    'Claude Code'
  );
  assert.deepEqual(working(writing.root), ['Still generating']);
  writing.dispose();
  // Its steps so far, then a pause: they show as Rovik's calls do, the status after them.
  const stepped = list(
    [
      user,
      {
        ...reply,
        status: { text: 'Stopping' },
        meta: {
          connector: {
            kind: 'claude-code',
            steps: [
              { id: 't1', name: 'Read', input: { file_path: 'a.rs' }, result: 'x', ok: true },
            ],
          },
        },
      },
    ],
    'Claude Code'
  );
  assert.deepEqual(working(stepped.root), ['Stopping']);
  assert.match(stepped.root.querySelector('.tool-group-header')?.textContent ?? '', /Read/);
  stepped.dispose();
});

test('a failed reply that ran nothing shows the error alone', () => {
  const failed: ChatMessage = {
    id: 'a1',
    role: 'assistant',
    content: '',
    finalized: true,
    error: 'Claude Code isn’t signed in.',
    meta: { connector: { kind: 'claude-code', steps: [] } },
  };
  const { root, dispose } = list([user, failed], 'Claude Code');
  assert.equal(root.querySelector('.tool-group'), null);
  assert.match(root.textContent ?? '', /isn’t signed in/);
  dispose();
});
