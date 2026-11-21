import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createDiagnosticReporter,
  currentEditorDiagnostics,
  type OwnedEditorDiagnostic,
} from './diagnosticReporting';
const marker = (owner: string): OwnedEditorDiagnostic => ({
  owner,
  startLine: 1,
  startCol: 1,
  endLine: 1,
  endCol: 1,
  severity: 'warning',
  message: owner,
});
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
test('an inactive TypeScript program cannot hide architecture or Rust diagnostics', () => {
  assert.deepEqual(
    currentEditorDiagnostics(
      ['typescript', 'javascript', 'rust-analyzer', 'rovik-polyglot-architecture'].map(marker),
      false
    ).map((value) => value.message),
    ['rust-analyzer', 'rovik-polyglot-architecture']
  );
  assert.equal(currentEditorDiagnostics([marker('typescript')], true).length, 1);
});
test('closing or switching projects discards queued reports and clears the old project identity', async () => {
  const sent: string[] = [];
  const reporter = createDiagnosticReporter({
    path: (uri) => uri,
    markers: () => [marker('rust-analyzer')],
    typesCurrent: () => false,
    send: async (root, path) => {
      sent.push(`${root}:${path}`);
    },
    delayMs: 0,
  });
  reporter.project('/old', ['/old/file.rs']);
  reporter.project(null, []);
  await tick();
  assert.deepEqual(sent, []);
  reporter.project('/new', ['/old/file.rs', '/new/file.rs']);
  await tick();
  assert.deepEqual(sent, ['/new:/new/file.rs']);
  reporter.dispose();
});
test('dropping stale TypeScript markers forwards an empty report instead of retaining backend errors', async () => {
  let current = true;
  const sent: number[] = [];
  const reporter = createDiagnosticReporter({
    path: (uri) => uri,
    markers: () => [marker('typescript')],
    typesCurrent: () => current,
    send: async (_root, _path, values) => {
      sent.push(values.length);
    },
    delayMs: 0,
  });
  reporter.project('/project', ['/project/file.ts']);
  await tick();
  current = false;
  reporter.changed(['/project/file.ts']);
  await tick();
  assert.deepEqual(sent, [1, 0]);
  reporter.dispose();
});

test('a rejected report from an earlier project cannot invalidate the new project report cache', async () => {
  let rejectOld!: (error: Error) => void;
  const sent: string[] = [];
  const reporter = createDiagnosticReporter({
    path: (uri) => uri,
    markers: () => [marker('rovik-polyglot-architecture')],
    typesCurrent: () => true,
    send(root) {
      sent.push(root);
      return root === '/old'
        ? new Promise((_resolve, reject) => {
            rejectOld = reject;
          })
        : Promise.resolve();
    },
    delayMs: 0,
  });
  reporter.project('/old', ['/old/file.rs']);
  await tick();
  reporter.project('/new', ['/new/file.rs']);
  await tick();
  rejectOld(new Error('Old backend request failed'));
  await tick();
  reporter.changed(['/new/file.rs']);
  await tick();
  assert.deepEqual(sent, ['/old', '/new']);
  reporter.dispose();
});
