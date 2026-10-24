// Component test: a Claude Code step that waits for the user's approval
// (its runs follow Rovik's permissions) shows the request in the reply's
// record, open or not, and the answer goes to the run. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ConnectorRecord from './ConnectorRecord';
import { registerApproval, pendingApproval } from '../lib/approvals';
import type { ConnectorMeta } from '../lib/connectors';

const sent: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: unknown) => {
    sent.push({ cmd, args });
    return true;
  },
};

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('a step waiting for approval shows its request, and Allow answers it', async () => {
  const meta = {
    harness: 'claude-code',
    steps: [{ id: 'toolu_1', name: 'Bash', input: { command: 'npm install' } }],
  } as unknown as ConnectorMeta;
  registerApproval({
    toolCallId: 'toolu_1',
    dialogId: 'd1',
    toolName: 'bash',
    summary: 'Run `npm install`',
    command: 'npm install',
  } as any);
  // Another chat's request isn't this record's.
  registerApproval({
    toolCallId: 'toolu_1',
    dialogId: 'd2',
    toolName: 'bash',
    summary: 'Run `ls`',
  } as any);
  const root = container();
  const dispose = render(() => <ConnectorRecord meta={meta} running={true} dialogId="d1" />, root);
  assert.match(root.textContent ?? '', /Run `npm install`/);
  assert.doesNotMatch(root.textContent ?? '', /Run `ls`/);
  assert.match(root.textContent ?? '', /Waiting for your approval/);

  const allow = [...root.querySelectorAll('button')].find((b) => b.textContent === 'Allow')!;
  allow.click();
  await settle();
  const answer = sent.find((s) => s.cmd !== 'plugin:event|listen');
  assert.ok(answer, JSON.stringify(sent));
  assert.equal(answer.args.dialogId, 'd1');
  assert.equal(answer.args.toolCallId, 'toolu_1');
  assert.equal(pendingApproval('d1', 'toolu_1'), undefined);
  dispose();
});

test('the run’s status shows in the record’s line, with its spinner', () => {
  const meta = { kind: 'claude-code', steps: [] } as unknown as ConnectorMeta;
  const root = container();
  const dispose = render(
    () => <ConnectorRecord meta={meta} running={true} status="Stopping… 6 s" dialogId="d3" />,
    root
  );
  assert.equal(
    root.querySelector('.chat-connector-headline')?.textContent,
    'Claude Code · Stopping… 6 s'
  );
  assert.ok(root.querySelector('.tool-call-spinner'));
  dispose();
});

test('a step waiting for approval stops the spinner: the request under the line says why', () => {
  const meta = {
    kind: 'claude-code',
    steps: [{ id: 'toolu_7', name: 'Bash', input: { command: 'rm -rf dist' } }],
  } as unknown as ConnectorMeta;
  registerApproval({
    toolCallId: 'toolu_7',
    dialogId: 'd4',
    toolName: 'bash',
    summary: 'Run `rm -rf dist`',
    command: 'rm -rf dist',
  } as any);
  const root = container();
  const dispose = render(() => <ConnectorRecord meta={meta} running={true} dialogId="d4" />, root);
  assert.equal(root.querySelector('.chat-connector-headline')?.textContent, 'Claude Code · 1 step');
  assert.equal(root.querySelector('.tool-call-spinner'), null);
  assert.match(root.textContent ?? '', /Waiting for your approval/);
  dispose();
});
