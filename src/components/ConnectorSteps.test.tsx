// Component test: what Claude Code did for a reply with its own tools,
// shown with the chat's own parts: its thinking as a reply's is, its tool
// calls as Rovik's of the same kind, in the order it went, with no line
// of its own around them. A call waiting for the user's approval shows the
// request, and the answer goes to the run. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ConnectorSteps from './ConnectorSteps';
import { registerApproval, pendingApproval } from '../lib/approvals';
import type { ConnectorMeta } from '../lib/connectors';

const sent: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: unknown) => {
    sent.push({ cmd, args });
    return true;
  },
  transformCallback: () => 0,
};

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** The lines shown, in order: each thought's and each tool line's header, its words as shown. */
const lines = (root: HTMLElement) =>
  [...root.querySelectorAll('.tool-group-header')].map((header) => {
    const words: string[] = [];
    const walk = (node: Node) => {
      if (node.nodeType === 3) words.push(node.textContent?.trim() ?? '');
      node.childNodes.forEach(walk);
    };
    walk(header);
    return words.filter(Boolean).join(' ');
  });

test('its thinking and its calls show as the chat shows Rovik’s, in the order it went', () => {
  const meta: ConnectorMeta = {
    kind: 'claude-code',
    steps: [
      { id: 'thinking-1', name: '', input: null, thinking: 'Read a.rs first.\n\n', ms: 3_200 },
      { id: 't1', name: 'Read', input: { file_path: 'src/a.rs' }, result: 'fn a() {}', ok: true },
      { id: 't2', name: 'Bash', input: { command: 'npm test' }, result: 'ok', ok: true },
      { id: 'thinking-2', name: '', input: null, thinking: 'It passes.', ms: 900 },
    ],
  };
  const root = container();
  const dispose = render(() => <ConnectorSteps meta={meta} running={false} dialogId="d1" />, root);
  assert.deepEqual(lines(root), ['Thought for 3 s', 'Read a.rs', 'Ran npm', 'Thought for 1 s']);
  // A thought opens to its text, as a reply's does.
  (root.querySelector('.tool-group-header') as HTMLElement).click();
  assert.equal(root.querySelector('.chat-thinking-text')?.textContent, 'Read a.rs first.');
  dispose();
});

test('while it thinks, its last thought says so with its latest line', () => {
  const meta: ConnectorMeta = {
    kind: 'claude-code',
    steps: [
      { id: 't1', name: 'Read', input: { file_path: 'a.rs' }, result: '42', ok: true },
      { id: 'thinking-1', name: '', input: null, thinking: 'It returns 42.\nNow the tests' },
    ],
  };
  const root = container();
  const dispose = render(() => <ConnectorSteps meta={meta} running={true} dialogId="d1" />, root);
  assert.deepEqual(lines(root), ['Read a.rs', 'Thinking… Now the tests']);
  dispose();
  // Cut short: its time not known, not thinking any more.
  const ended = container();
  const gone = render(() => <ConnectorSteps meta={meta} running={false} dialogId="d1" />, ended);
  assert.deepEqual(lines(ended), ['Read a.rs', 'Thought']);
  gone();
});

test('a call waiting for approval shows its request, and Allow answers it', async () => {
  const meta: ConnectorMeta = {
    kind: 'claude-code',
    steps: [{ id: 'toolu_1', name: 'Bash', input: { command: 'npm install' } }],
  };
  registerApproval({
    toolCallId: 'toolu_1',
    dialogId: 'd1',
    toolName: 'bash',
    summary: 'Run `npm install`',
    command: 'npm install',
  } as any);
  // Another chat's request isn't this reply's.
  registerApproval({
    toolCallId: 'toolu_1',
    dialogId: 'd2',
    toolName: 'bash',
    summary: 'Run `ls`',
  } as any);
  const root = container();
  const dispose = render(() => <ConnectorSteps meta={meta} running={true} dialogId="d1" />, root);
  const allow = [...root.querySelectorAll('button')].find((b) => b.textContent === 'Allow');
  assert.ok(allow, root.textContent ?? '');
  assert.doesNotMatch(root.textContent ?? '', /Run `ls`/);
  allow.click();
  await settle();
  const answer = sent.find((s) => s.args?.toolCallId === 'toolu_1');
  assert.ok(answer, JSON.stringify(sent));
  assert.equal(answer.args.dialogId, 'd1');
  assert.equal(pendingApproval('d1', 'toolu_1'), undefined);
  dispose();
});

test('a call waiting on what Rovik installs for it says so on its line', () => {
  const meta: ConnectorMeta = {
    kind: 'claude-code',
    steps: [{ id: 'toolu_1', name: 'Bash', input: { command: 'npm run dev' } }],
  };
  const root = container();
  const notes = { toolu_1: 'Installing Node 22 for this project…' };
  const dispose = render(
    () => <ConnectorSteps meta={meta} running={true} dialogId="d1" notes={notes} />,
    root
  );
  // Its line, open, as a call of Rovik's own says what it waits for.
  (root.querySelector('.tool-group-header') as HTMLElement).click();
  assert.match(root.textContent ?? '', /Installing Node 22 for this project…/);
  dispose();
  // Without a note, the line is the call's own.
  const plain = container();
  const gone = render(() => <ConnectorSteps meta={meta} running={true} dialogId="d1" />, plain);
  (plain.querySelector('.tool-group-header') as HTMLElement).click();
  assert.doesNotMatch(plain.textContent ?? '', /Installing/);
  gone();
});
