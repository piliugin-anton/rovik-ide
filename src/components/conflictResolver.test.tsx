// Component tests: the conflict resolver, picking lines. A conflict git
// can't split (yours changed lines 2 and 4, incoming line 3) is settled by
// ticking each changed line on the side that changed it, and that's the
// file written; whole sides and ticks combine. The app is answered here,
// command by command. Run with: npm test
import { container } from '../test/dom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import { ConflictResolver } from './ConflictResolver';
import type { GitConflictFile, GitOperation } from '../types';

let calls: { cmd: string; args: any }[] = [];
let file: GitConflictFile;
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: () => 0,
  unregisterCallback: () => {},
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    if (cmd === 'git_conflict_file') return file;
    if (cmd === 'git_conflict_resolve') return null;
    throw new Error(`no app: ${cmd}`);
  },
};

const OURS = [
  'function total(items) {',
  '  let sum = 0; // running total',
  '  for (const item of items) sum += item.price;',
  '  return Math.round(sum);',
  '}',
];
const THEIRS = [
  'function total(items) {',
  '  let sum = 0;',
  '  for (const item of items) sum += item.price * item.qty;',
  '  return sum;',
  '}',
];
const text = (lines: string[]) => lines.map((l) => `${l}\n`).join('');

file = {
  path: 'total.ts',
  ours: text(OURS),
  theirs: text(THEIRS),
  base: null,
  binary: false,
  segments: [
    { kind: 'common', text: text(OURS.slice(0, 1)) },
    { kind: 'conflict', ours: text(OURS.slice(1, 4)), base: '', theirs: text(THEIRS.slice(1, 4)) },
    { kind: 'common', text: text(OURS.slice(4)) },
  ],
  resolved: false,
  current: null,
  oursLabel: 'Yours (main)',
  theirsLabel: 'Incoming (origin/main)',
};

const operation: GitOperation = {
  kind: 'merge',
  branch: 'main',
  incoming: 'origin/main',
  step: null,
  total: null,
  commit: null,
  conflicts: ['total.ts'],
  canContinue: false,
};

let dispose = () => {};
after(() => dispose());

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

async function open() {
  dispose();
  calls = [];
  const root = container();
  dispose = render(
    () => (
      <ConflictResolver
        projectPath={`/work/project-${Math.random()}`}
        operation={operation}
        onFileWritten={() => {}}
        onContinue={async () => {}}
        onAbort={async () => {}}
        onClose={() => {}}
      />
    ),
    root
  );
  await flush();
}

const q = <T extends Element = HTMLElement>(css: string) => [
  ...document.querySelectorAll<T>(`.conflict-resolver ${css}`),
];
/** The box of `side`'s line whose code contains `code`. */
function box(side: 'ours' | 'theirs', code: string): HTMLInputElement {
  const line = q(`.cr-${side} .cr-line`).find((l) => l.textContent?.includes(code));
  assert.ok(line, `${side} has a line with ${code}`);
  return line.querySelector<HTMLInputElement>('.cr-pick')!;
}
const result = () => q('.cr-result .cr-line .cr-code').map((e) => e.textContent);
const state = () => q('.cr-result .cr-state')[0]?.textContent;

test("each side's lines line up, with what differs marked", async () => {
  await open();
  assert.equal(q('.cr-ours .cr-pick').length, 3);
  assert.equal(q('.cr-theirs .cr-pick').length, 3);
  // Each changed line's changed part: yours added a comment and a round,
  // incoming a quantity.
  assert.deepEqual(
    q('.cr-ours .cr-change').map((m) => m.textContent),
    [' // running total', 'Math.round(sum)']
  );
  assert.deepEqual(
    q('.cr-theirs .cr-change').map((m) => m.textContent),
    [' * item.qty', 'sum']
  );
  assert.equal(state(), 'Unresolved');
});

test('three ticks take each line from the side that changed it, and that is written', async () => {
  await open();
  box('ours', 'running total').click();
  box('theirs', 'item.qty').click();
  box('ours', 'Math.round').click();
  await flush();
  assert.deepEqual(result(), [
    '  let sum = 0; // running total',
    '  for (const item of items) sum += item.price * item.qty;',
    '  return Math.round(sum);',
  ]);
  assert.equal(state(), 'Lines picked: 2 yours, 1 incoming');
  assert.equal(box('theirs', 'item.qty').checked, true);
  assert.equal(box('theirs', 'return sum').checked, false);

  q<HTMLButtonElement>('.cr-save')[0].click();
  await flush();
  const saved = calls.find((c) => c.cmd === 'git_conflict_resolve');
  assert.equal(
    saved?.args.content,
    text([
      'function total(items) {',
      '  let sum = 0; // running total',
      '  for (const item of items) sum += item.price * item.qty;',
      '  return Math.round(sum);',
      '}',
    ])
  );
});

test('a whole side, then one line swapped for the other side’s', async () => {
  await open();
  q<HTMLButtonElement>('.cr-ours .cr-accept')[0].click();
  await flush();
  // The side's boxes show what it took.
  assert.deepEqual(
    q<HTMLInputElement>('.cr-ours .cr-pick').map((b) => b.checked),
    [true, true, true]
  );
  box('ours', 'item.price;').click();
  box('theirs', 'item.qty').click();
  await flush();
  assert.deepEqual(result(), [
    '  let sum = 0; // running total',
    '  for (const item of items) sum += item.price * item.qty;',
    '  return Math.round(sum);',
  ]);
  // Taking the side whole again replaces the picks.
  q<HTMLButtonElement>('.cr-theirs .cr-only')[0].click();
  await flush();
  assert.deepEqual(result(), THEIRS.slice(1, 4));
  assert.equal(state(), 'Incoming');
});
