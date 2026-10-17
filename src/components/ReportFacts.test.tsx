// Component tests: the facts under an Autopilot task's report (S09), with
// where a server that pushes its results put it (H04). Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ReportFacts from './ReportFacts';
import type { ReportMeta } from '../lib/autopilot';

/** The commands the card sent, in order. */
const calls: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 1,
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    return null;
  },
};

const META: ReportMeta = {
  outcome: 'verified',
  ending: 'Its checks pass',
  delivered: true,
  partial: false,
  delivery:
    'Delivered: pushed to `rovik/make-the-test-pass-u1`, pull request https://github.com/acme/app/pull/7',
  branch: 'rovik/make-the-test-pass-u1',
  pr: 'https://github.com/acme/app/pull/7',
  assumptions: [],
  criteria: [],
  verdicts: [],
  tests: [],
  checks: [],
  spent: { ms: 61_000, cost: 0, uncosted: 0 },
};

test('a pushed result: its branch, and its pull request, which opens in the browser', async () => {
  const root = container();
  const dispose = render(() => <ReportFacts meta={META} />, root);
  try {
    const facts = [...root.querySelectorAll('dt')].map((dt) => dt.textContent);
    assert.deepEqual(facts, ['Result', 'Branch', 'Pull request', 'Spent']);
    const result = root.querySelector('dd')!;
    assert.equal(result.querySelector('code')?.textContent, 'rovik/make-the-test-pass-u1');
    assert.doesNotMatch(result.textContent!, /`/, 'its code shown as code');
    const link = root.querySelector('a')!;
    assert.equal(link.getAttribute('href'), META.pr);
    link.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(calls.at(-1), { cmd: 'open_url', args: { url: META.pr } });
  } finally {
    dispose();
  }
});

test('a result kept on the server: no branch, no pull request', () => {
  const root = container();
  const kept = {
    ...META,
    delivery: 'Delivered: the result waits on the server for its computer',
    branch: undefined,
    pr: undefined,
  };
  const dispose = render(() => <ReportFacts meta={kept} />, root);
  try {
    const facts = [...root.querySelectorAll('dt')].map((dt) => dt.textContent);
    assert.deepEqual(facts, ['Result', 'Spent']);
    assert.equal(root.querySelector('a'), null);
  } finally {
    dispose();
  }
});
