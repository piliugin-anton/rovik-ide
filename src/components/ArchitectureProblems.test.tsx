import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ArchitectureProblems from './ArchitectureProblems';
import { createArchitectureFeedbackController } from '../lib/architectureFeedback';
import { architectureContext, architectureEvidence } from '../test/architectureFixtures';
import { setEditorOpener, type OpenInEditor } from '../lib/openInEditor';
const cleanups: (() => void)[] = [];
afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
  setEditorOpener(null);
});
const button = (root: HTMLElement, text: string) => {
  const value = [...root.querySelectorAll('button')].find(
    (entry) => entry.textContent?.trim() === text
  );
  assert.ok(value, text);
  return value;
};
test('advisory Problems shows partial diagnostics and opens file-level findings without invented lines', async () => {
  const evidence = architectureEvidence();
  evidence.diagnostics = ['Python graph unavailable: protected check did not finish.'];
  const controller = createArchitectureFeedbackController(async () => evidence);
  controller.updateContext(architectureContext);
  const opened: OpenInEditor[] = [];
  setEditorOpener(async (value) => {
    opened.push(value);
  });
  const root = container();
  cleanups.push(render(() => <ArchitectureProblems controller={controller} />, root));
  await controller.show({ root: '/project' }, 'run-1', 'Build host');
  assert.match(root.textContent!, /Architecture Problems · advisory/);
  assert.match(root.textContent!, /Python graph unavailable/);
  assert.match(root.textContent!, /precise source location unavailable/i);
  assert.doesNotMatch(root.textContent!, /Verified|Accepted|clean/);
  button(root, 'src/lib.rs (file-level)').click();
  assert.deepEqual(opened, [{ path: '/project/src/lib.rs', focus: true, marks: 'none' }]);
  controller.sourceChanged('/project/src/lib.rs');
  assert.equal(button(root, 'src/lib.rs (file-level)').disabled, true);
  assert.match(root.textContent!, /Source files changed/);
});
test('zero observations and a failed refresh remain visibly unavailable instead of claiming clean architecture', async () => {
  let failed = false;
  const evidence = {
    ...architectureEvidence(),
    observations: [],
    diagnostics: ['No authenticated architecture observation exists.'],
    freshness: 'unavailable' as const,
  };
  const controller = createArchitectureFeedbackController(async () => {
    if (failed) throw new Error('Host offline');
    return evidence;
  });
  controller.updateContext(architectureContext);
  const root = container();
  cleanups.push(render(() => <ArchitectureProblems controller={controller} />, root));
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  assert.match(root.textContent!, /does not establish complete coverage or strict acceptance/);
  assert.match(root.textContent!, /No authenticated architecture observation/);
  failed = true;
  await controller.refresh();
  assert.match(root.textContent!, /Host offline/);
  assert.doesNotMatch(root.textContent!, /matches the saved source/);
});

test('remote host evidence stays visible without claiming current desktop source or enabling navigation', async () => {
  const evidence = {
    ...architectureEvidence(),
    currentSnapshotDigest: 'sha256:applied-host-tree',
    applicationBinding: {
      receiptDigest: 'sha256:host-receipt',
      candidateDigest: 'sha256:source',
      observedSnapshotDigest: 'sha256:applied-host-tree',
    },
    diagnostics: ['Another host collector was unavailable.'],
  };
  const controller = createArchitectureFeedbackController(async () => evidence);
  controller.updateContext(architectureContext);
  const opened: OpenInEditor[] = [];
  setEditorOpener(async (value) => {
    opened.push(value);
  });
  const root = container();
  cleanups.push(render(() => <ArchitectureProblems controller={controller} />, root));
  await controller.show({ root: '/project', dialogId: 'chat' }, 'run-1', 'Build host');
  assert.match(root.textContent!, /historical in this editor/);
  assert.match(
    root.textContent!,
    /whole-project correspondence with the desktop has not been established/
  );
  assert.match(root.textContent!, /Domain depends on infrastructure/);
  assert.match(root.textContent!, /Another host collector was unavailable/);
  assert.doesNotMatch(root.textContent!, /matches the saved source/);
  const location = button(root, 'src/lib.rs (file-level)');
  assert.equal(location.disabled, true);
  location.click();
  assert.deepEqual(opened, []);
  assert.equal(button(root, 'Refresh architecture feedback').disabled, false);
});
