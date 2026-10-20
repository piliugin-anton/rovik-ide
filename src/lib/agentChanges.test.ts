// agentChanges.ts tests (node:test): the agent's unreviewed changes are
// found however the file's path is spelled.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { agentChangesFor, clearAgentChanges, recordAgentEdit } from './agentChanges';

test('changes are found however the path is spelled', () => {
  recordAgentEdit('C:\\p\\a.ts', 'a\n', 'b\n');
  const change = agentChangesFor('c:/p/a.ts');
  assert.equal(change?.baseline, 'a\n');
  assert.equal(change?.current, 'b\n');
  // A second edit: the baseline stays the text before the first.
  recordAgentEdit('c:/p//a.ts', 'b\n', 'c\n');
  assert.equal(agentChangesFor('C:/p/a.ts')?.baseline, 'a\n');
  clearAgentChanges('C:/p//a.ts');
  assert.equal(agentChangesFor('C:\\p\\a.ts'), undefined);
  // Back to how it was: nothing to show.
  recordAgentEdit('/p/b.ts', 'same\n', 'same\n');
  assert.equal(agentChangesFor('/p/b.ts'), undefined);
});
