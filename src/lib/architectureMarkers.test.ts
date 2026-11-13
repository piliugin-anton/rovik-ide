import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createArchitectureFeedbackController } from './architectureFeedback';
import {
  ARCHITECTURE_MARKER_OWNER,
  architectureContentDigest,
  installArchitectureMarkers,
  type ArchitectureMarker,
  type ArchitectureModel,
} from './architectureMarkers';
import {
  architectureContext,
  architectureEvidence,
  architectureSource,
} from '../test/architectureFixtures';

test('the dedicated owner clears on edits and repopulates newly opened models without touching other owners', async () => {
  const controller = createArchitectureFeedbackController(async () => architectureEvidence());
  controller.updateContext(architectureContext);
  const writes: { model: ArchitectureModel; owner: string; markers: ArchitectureMarker[] }[] = [];
  const edits = new Map<ArchitectureModel, () => void>();
  const make = (path: string): ArchitectureModel => ({
    path,
    disposed: () => false,
    content: () => architectureSource,
    version: () => 1,
    changed(listener) {
      edits.set(this, listener);
      return () => {
        edits.delete(this);
      };
    },
  });
  const first = make('/project/src/lib.rs');
  const models = [first];
  let created!: (model: ArchitectureModel) => void;
  let disposing!: (model: ArchitectureModel) => void;
  const stop = installArchitectureMarkers(
    controller,
    {
      models: () => models,
      created(listener) {
        created = listener;
        return () => {};
      },
      disposing(listener) {
        disposing = listener;
        return () => {};
      },
      publish(model, owner, markers) {
        writes.push({ model, owner, markers });
      },
    },
    async () => architectureEvidence().observations[0].findings[0].locations[0].contentDigest
  );
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  await new Promise((resolve) => setTimeout(resolve, 0));
  const last = () => writes[writes.length - 1];
  assert.equal(last().markers.length, 1);
  assert.match(last().markers[0].message, /File-level.*precise source location unavailable/);
  assert.equal(last().markers[0].startLineNumber, 1);
  const opened = make('/project/src/lib.rs');
  models.push(opened);
  created(opened);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(last().markers.length, 1);
  edits.get(opened)!();
  assert.equal(last().markers.length, 0);
  assert.equal(
    writes.every((write) => write.owner === ARCHITECTURE_MARKER_OWNER),
    true
  );
  disposing(opened);
  assert.equal(edits.has(opened), false);
  stop();
  assert.equal(edits.size, 0);
  assert.equal(last().markers.length, 0);
});

test('a clean cached model with different bytes cannot receive saved-source markers', async () => {
  const controller = createArchitectureFeedbackController(async () => architectureEvidence());
  controller.updateContext(architectureContext);
  const writes: ArchitectureMarker[][] = [];
  const model: ArchitectureModel = {
    path: '/project/src/lib.rs',
    disposed: () => false,
    content: () => 'old cached content',
    version: () => 1,
    changed: () => () => {},
  };
  const stop = installArchitectureMarkers(
    controller,
    {
      models: () => [model],
      created: () => () => {},
      disposing: () => () => {},
      publish(_model, _owner, markers) {
        writes.push(markers);
      },
    },
    async () => 'sha256:different-displayed-bytes'
  );
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(
    writes.every((markers) => markers.length === 0),
    true
  );
  assert.match(controller.view()!.reason!, /displayed buffer.*differs/);
  stop();
});

test('hash completion cannot restore markers after a source change or cancellation', async () => {
  const controller = createArchitectureFeedbackController(async () => architectureEvidence());
  controller.updateContext(architectureContext);
  const writes: ArchitectureMarker[][] = [];
  let finish!: (digest: string) => void;
  const model: ArchitectureModel = {
    path: '/project/src/lib.rs',
    disposed: () => false,
    content: () => architectureSource,
    version: () => 1,
    changed: () => () => {},
  };
  const stop = installArchitectureMarkers(
    controller,
    {
      models: () => [model],
      created: () => () => {},
      disposing: () => () => {},
      publish(_model, _owner, markers) {
        writes.push(markers);
      },
    },
    () =>
      new Promise((resolve) => {
        finish = resolve;
      })
  );
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  controller.invalidate('Read cancelled');
  finish(architectureEvidence().observations[0].findings[0].locations[0].contentDigest);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(
    writes.every((markers) => markers.length === 0),
    true
  );
  stop();
});

test('displayed text identity preserves exact line endings and the optional byte order mark', async () => {
  assert.equal(
    await architectureContentDigest(architectureSource),
    architectureEvidence().observations[0].findings[0].locations[0].contentDigest
  );
  assert.notEqual(
    await architectureContentDigest(architectureSource.replace('\n', '\r\n')),
    await architectureContentDigest(architectureSource)
  );
  assert.notEqual(
    await architectureContentDigest('\ufeff' + architectureSource),
    await architectureContentDigest(architectureSource)
  );
});

test('an application receipt never bypasses the exact displayed-file content check', async () => {
  const evidence = {
    ...architectureEvidence(),
    currentSnapshotDigest: 'sha256:applied-tree',
    applicationBinding: {
      receiptDigest: 'sha256:receipt',
      candidateDigest: 'sha256:source',
      observedSnapshotDigest: 'sha256:applied-tree',
    },
  };
  const controller = createArchitectureFeedbackController(async () => evidence);
  controller.updateContext(architectureContext);
  const writes: ArchitectureMarker[][] = [];
  const model: ArchitectureModel = {
    path: '/project/src/lib.rs',
    disposed: () => false,
    content: () => 'old editor bytes',
    version: () => 1,
    changed: () => () => {},
  };
  const stop = installArchitectureMarkers(
    controller,
    {
      models: () => [model],
      created: () => () => {},
      disposing: () => () => {},
      publish(_model, _owner, markers) {
        writes.push(markers);
      },
    },
    async () => 'sha256:wrong-editor-bytes'
  );
  await controller.show({ root: '/project' }, 'run-1', 'Local');
  await Promise.resolve();
  assert.equal(
    writes.every((markers) => markers.length === 0),
    true
  );
  assert.match(controller.view()!.reason!, /displayed buffer.*differs/);
  stop();
});

test('matching a marked file cannot authorize desktop markers for a different host dependency graph', async () => {
  const evidence = {
    ...architectureEvidence(),
    currentSnapshotDigest: 'sha256:applied-host-tree',
    applicationBinding: {
      receiptDigest: 'sha256:host-receipt',
      candidateDigest: 'sha256:source',
      observedSnapshotDigest: 'sha256:applied-host-tree',
    },
  };
  const controller = createArchitectureFeedbackController(async () => evidence);
  controller.updateContext(architectureContext);
  const writes: ArchitectureMarker[][] = [];
  let hashCalls = 0;
  const model: ArchitectureModel = {
    path: '/project/src/lib.rs',
    disposed: () => false,
    content: () => architectureSource,
    version: () => 1,
    changed: () => () => {},
  };
  const stop = installArchitectureMarkers(
    controller,
    {
      models: () => [model],
      created: () => () => {},
      disposing: () => () => {},
      publish(_model, _owner, markers) {
        writes.push(markers);
      },
    },
    async () => {
      hashCalls++;
      return evidence.observations[0].findings[0].locations[0].contentDigest;
    }
  );
  await controller.show({ root: '/project', dialogId: 'chat' }, 'run-1', 'Build host');
  await Promise.resolve();
  assert.equal(
    writes.every((markers) => markers.length === 0),
    true
  );
  assert.equal(
    hashCalls,
    0,
    'individual matching bytes cannot establish whole-project correspondence'
  );
  assert.equal(controller.view()?.state, 'stale');
  stop();
});
