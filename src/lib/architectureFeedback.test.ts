import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  architectureFilePath,
  architectureFindings,
  architectureMarkersCurrent,
  createArchitectureFeedbackController,
} from './architectureFeedback';
import { architectureContext, architectureEvidence } from '../test/architectureFixtures';

test('architecture markers require approved policy and the exact current candidate bytes', async () => {
  let evidence = architectureEvidence();
  const controller = createArchitectureFeedbackController(async () => evidence);
  controller.updateContext(architectureContext);
  await controller.show({ root: '/project' }, 'run-1', 'This computer');
  assert.equal(architectureMarkersCurrent(controller.view()), true);
  evidence = { ...evidence, evidenceCandidateDigest: 'sha256:private-candidate' };
  await controller.refresh();
  assert.equal(architectureMarkersCurrent(controller.view()), false);
  evidence = { ...architectureEvidence(), policyApproved: false };
  await controller.refresh();
  assert.equal(architectureMarkersCurrent(controller.view()), false);
});
test('read failure retains visible historical findings while clearing their current authority', async () => {
  let fail = false;
  const controller = createArchitectureFeedbackController(async () => {
    if (fail) throw new Error('Host offline');
    return architectureEvidence();
  });
  controller.updateContext(architectureContext);
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  fail = true;
  await controller.refresh();
  assert.equal(controller.view()?.state, 'unavailable');
  assert.equal(architectureMarkersCurrent(controller.view()), false);
  assert.equal(architectureFindings(controller.view()!.feedback!).length, 1);
  assert.match(controller.view()!.reason!, /Host offline/);
});
test('unsaved edits, disk writes and configuration changes invalidate source-bound feedback', async () => {
  let requests = 0;
  const controller = createArchitectureFeedbackController(async () => {
    requests++;
    return architectureEvidence();
  });
  controller.updateContext(architectureContext);
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  controller.updateContext({ ...architectureContext, unsavedFiles: ['src/lib.rs'] });
  assert.equal(architectureMarkersCurrent(controller.view()), false);
  assert.equal(controller.canRefresh(), false);
  await controller.refresh();
  assert.equal(requests, 1);
  controller.updateContext(architectureContext);
  assert.equal(controller.canRefresh(), true);
  await controller.refresh();
  controller.sourceChanged('/another/file.rs');
  assert.equal(architectureMarkersCurrent(controller.view()), true);
  controller.sourceChanged('/project/src/lib.rs');
  assert.equal(architectureMarkersCurrent(controller.view()), false);
  await controller.refresh();
  controller.inputsChanged({ root: '/project' });
  assert.equal(controller.view()!.state, 'stale');
});
test('project close clears evidence and changing chat owners never retargets the selected run', async () => {
  let requests = 0;
  const controller = createArchitectureFeedbackController(async () => {
    requests++;
    return architectureEvidence();
  });
  controller.updateContext(architectureContext);
  await controller.show({ root: '/project', dialogId: 'chat' }, 'run-1', 'Host');
  controller.updateContext({ ...architectureContext, hostId: 'different-host' });
  assert.equal(controller.canRefresh(), false);
  await controller.refresh();
  assert.equal(requests, 1);
  assert.match(controller.view()!.reason!, /execution host changed/);
  controller.updateContext({ ...architectureContext, root: null, projectId: null });
  assert.equal(controller.view(), undefined);
});
test('cancelled and superseded feedback responses cannot publish old markers', async () => {
  const pending: ((value: ReturnType<typeof architectureEvidence>) => void)[] = [];
  const controller = createArchitectureFeedbackController(
    () => new Promise((resolve) => pending.push(resolve))
  );
  controller.updateContext(architectureContext);
  const first = controller.show({ root: '/project' }, 'run-1', 'Local');
  const second = controller.show({ root: '/project' }, 'run-2', 'Local');
  pending[1](architectureEvidence('run-2'));
  await second;
  pending[0](architectureEvidence('run-1'));
  await first;
  assert.equal(controller.view()!.feedback!.runId, 'run-2');
  const next = controller.refresh();
  controller.invalidate('Read cancelled');
  pending[2](architectureEvidence('run-2'));
  await next;
  assert.equal(controller.view()!.state, 'stale');
  assert.equal(architectureMarkersCurrent(controller.view()), false);
});
test('native findings retain file precision and cannot navigate outside the selected root', () => {
  const finding = architectureFindings(architectureEvidence())[0];
  assert.equal(finding.path, 'src/lib.rs');
  assert.equal('line' in finding, false);
  assert.equal(architectureFilePath('/project', finding.path!), '/project/src/lib.rs');
  for (const path of ['../secret', '/host/file.rs', 'C:/outside', 'src/../outside', 'src\\file.rs'])
    assert.equal(architectureFilePath('/project', path), undefined);
});

test('Windows editor path casing still invalidates a locally verified saved source', async () => {
  const controller = createArchitectureFeedbackController(async () => architectureEvidence());
  controller.updateContext({ ...architectureContext, root: 'C:\\Project' });
  await controller.show({ root: 'C:\\Project' }, 'run-1', 'This computer');
  assert.equal(architectureMarkersCurrent(controller.view()), true);
  controller.sourceChanged('c:\\Project\\src\\lib.rs');
  assert.equal(architectureMarkersCurrent(controller.view()), false);
});

test('an authenticated application binding connects applied files without bypassing freshness or policy', async () => {
  let evidence = {
    ...architectureEvidence(),
    currentSnapshotDigest: 'sha256:applied-tree',
    applicationBinding: {
      receiptDigest: 'sha256:application-receipt',
      candidateDigest: 'sha256:source',
      observedSnapshotDigest: 'sha256:applied-tree',
    },
  };
  const controller = createArchitectureFeedbackController(async () => evidence);
  controller.updateContext(architectureContext);
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  assert.equal(architectureMarkersCurrent(controller.view()), true);
  for (const binding of [
    { ...evidence.applicationBinding, receiptDigest: '' },
    { ...evidence.applicationBinding, candidateDigest: 'sha256:unrelated-candidate' },
    { ...evidence.applicationBinding, observedSnapshotDigest: 'sha256:previous-tree' },
  ]) {
    const original = evidence;
    evidence = { ...evidence, applicationBinding: binding };
    await controller.refresh();
    assert.equal(architectureMarkersCurrent(controller.view()), false);
    evidence = original;
  }
  evidence = { ...evidence, policyApproved: false };
  await controller.refresh();
  assert.equal(architectureMarkersCurrent(controller.view()), false);
  evidence = { ...evidence, policyApproved: true, freshness: 'stale' };
  await controller.refresh();
  assert.equal(architectureMarkersCurrent(controller.view()), false);
});

test('a legacy response without an application binding cannot match a different saved tree', async () => {
  const controller = createArchitectureFeedbackController(async () => ({
    ...architectureEvidence(),
    currentSnapshotDigest: 'sha256:another-tree',
  }));
  controller.updateContext(architectureContext);
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  assert.equal(controller.view()?.state, 'unavailable');
  assert.equal(architectureMarkersCurrent(controller.view()), false);
});

test('host-current evidence remains historical in the desktop with direct or application-receipt identity', async () => {
  for (const evidence of [
    architectureEvidence(),
    {
      ...architectureEvidence(),
      currentSnapshotDigest: 'sha256:applied-host-tree',
      applicationBinding: {
        receiptDigest: 'sha256:host-receipt',
        candidateDigest: 'sha256:source',
        observedSnapshotDigest: 'sha256:applied-host-tree',
      },
    },
  ]) {
    const controller = createArchitectureFeedbackController(async () => evidence);
    controller.updateContext(architectureContext);
    await controller.show({ root: '/project', dialogId: 'chat' }, 'run-1', 'Build host');
    assert.equal(controller.view()?.state, 'stale');
    assert.match(
      controller.view()!.reason!,
      /whole-project correspondence with the desktop has not been established/
    );
    assert.equal(architectureMarkersCurrent(controller.view()), false);
    assert.equal(
      architectureMarkersCurrent({ ...controller.view()!, state: 'current' }),
      false,
      'marker guard must independently reject remote evidence'
    );
    await controller.show({ root: '/project' }, 'run-1', 'This computer');
    assert.equal(
      architectureMarkersCurrent(controller.view()),
      true,
      'local direct and authenticated application identities remain valid'
    );
  }
});

test('unloaded dependency, directory and unknown-root watcher changes invalidate pending graph reads', async () => {
  for (const watcherPath of [
    '/project/src/unloaded-dependency.ts',
    '/project/src/dependencies',
    '/project',
  ]) {
    let finish!: (feedback: ReturnType<typeof architectureEvidence>) => void;
    let pending = false;
    const controller = createArchitectureFeedbackController(() =>
      pending
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : Promise.resolve(architectureEvidence())
    );
    controller.updateContext(architectureContext);
    await controller.show({ root: '/project' }, 'run-1', 'This computer');
    assert.equal(architectureMarkersCurrent(controller.view()), true);
    // App forwards raw fs-changes paths (or the project root for an unknown event)
    // before looking up tabs, cached models, source kinds or file metadata.
    pending = true;
    const reading = controller.refresh();
    controller.sourceChanged(watcherPath);
    assert.equal(controller.view()?.state, 'stale');
    assert.equal(architectureMarkersCurrent(controller.view()), false);
    finish(architectureEvidence());
    await reading;
    assert.equal(controller.view()?.state, 'stale', watcherPath);
    assert.match(controller.view()!.reason!, /Source files changed/);
  }
});
