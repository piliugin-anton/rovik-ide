import { test } from 'node:test';
import assert from 'node:assert/strict';
import { canRetry, describeFailure } from './chatErrors';

test('a reply cut by a server restart says so, and can be taken again', () => {
  const failure = {
    kind: 'restarted' as const,
    message: 'The server restarted while the reply was being written.',
    attempts: 0,
  };
  // The same words as the core's (agent_run/failure.rs).
  assert.equal(describeFailure(failure), failure.message);
  assert.ok(canRetry(failure));
});

test('a request the model declined says why, as the core does', () => {
  const failure = {
    kind: 'refused' as const,
    message: 'The model declined to answer (cyber). This request was declined.',
    attempts: 1,
  };
  assert.equal(describeFailure(failure), failure.message);
  assert.equal(describeFailure({ ...failure, message: '' }), 'The model declined this request.');
});
