// Component test: a connector's reply in the chat shows one line of what
// it's doing, the chat's own: before its harness has said anything,
// "Starting Claude Code…"; then the run's status, as for any reply. Its
// steps show as Rovik's tool calls do, with no line of the connector's
// own. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import MessageList from './MessageList';
import type { ChatMessage } from '../types';

(window as any).__TAURI_INTERNALS__ = { invoke: async () => null, transformCallback: () => 0 };

const noop = async () => 0;
function list(messages: ChatMessage[], connectorTitle?: string, starting = false) {
  const root = container();
  const [rows, setRows] = createSignal(messages);
  const [isStarting, setStarting] = createSignal(starting);
  const [hasNewer, setHasNewer] = createSignal(false);
  const dispose = render(
    () => (
      <MessageList
        dialogId="d1"
        chat={{
          messages: rows(),
          hasOlder: false,
          hasNewer: hasNewer(),
          loadingOlder: false,
          lastUsed: 0,
        }}
        sending={true}
        starting={isStarting()}
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
  return { root, dispose, setRows, setStarting, setHasNewer };
}

const user: ChatMessage = { id: 'u1', role: 'user', content: 'Add a test', finalized: true };
const working = (root: HTMLElement) =>
  [...root.querySelectorAll('[role="status"]')].map((e) => e.textContent);

test('an API chat shows progress during preparation and hands over to the reply', () => {
  const model = list([user], undefined, true);
  try {
    assert.deepEqual(working(model.root), ['Roviking…']);
    model.setRows([
      user,
      {
        id: 'a1',
        role: 'assistant',
        content: '',
        pending: true,
        status: { text: 'Waiting for a response' },
      },
    ]);
    assert.deepEqual(working(model.root), ['Waiting for a response']);
    model.setStarting(false);
    assert.deepEqual(working(model.root), ['Waiting for a response']);
  } finally {
    model.dispose();
  }
});

test('startup progress disappears when starting fails and stays off older pages', () => {
  const model = list([user], undefined, true);
  try {
    model.setHasNewer(true);
    assert.deepEqual(working(model.root), []);
    model.setHasNewer(false);
    assert.deepEqual(working(model.root), ['Roviking…']);
    model.setStarting(false);
    assert.deepEqual(working(model.root), []);
  } finally {
    model.dispose();
  }
});

test('a connector shows its startup label during preparation', () => {
  const model = list([user], 'Codex', true);
  try {
    assert.deepEqual(working(model.root), ['Starting Codex…']);
  } finally {
    model.dispose();
  }
});

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

test('a connector’s text shows in its place among its steps, as it went', () => {
  const reply: ChatMessage = {
    id: 'a1',
    role: 'assistant',
    content: 'I’ll read a.txt first.\n\nI’ll read b.txt next.\n\nThey say alpha and beta.',
    finalized: true,
    meta: {
      connector: {
        kind: 'claude-code',
        steps: [
          { id: 't1', name: 'Read', input: { file_path: 'a.txt' }, result: 'a', ok: true, at: 22 },
          { id: 't2', name: 'Read', input: { file_path: 'b.txt' }, result: 'b', ok: true, at: 45 },
        ],
      },
    },
  };
  const { root, dispose } = list([user, reply], 'Claude Code');
  // Its words and its steps' lines, top to bottom.
  const words = (e: Element) => {
    const found: string[] = [];
    const walk = (node: Node) => {
      if (node.nodeType === 3) found.push(node.textContent?.trim() ?? '');
      node.childNodes.forEach(walk);
    };
    walk(e);
    return found.filter(Boolean).join(' ');
  };
  try {
    assert.deepEqual(
      [...root.querySelectorAll('.chat-message.assistant, .tool-group-header')].map(words),
      [
        'I’ll read a.txt first.',
        'Read a.txt',
        'I’ll read b.txt next.',
        'Read b.txt',
        'They say alpha and beta.',
      ]
    );
  } finally {
    dispose();
  }
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
