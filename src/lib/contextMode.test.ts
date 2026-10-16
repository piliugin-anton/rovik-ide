// contextMode.ts tests (node:test): a remote chat's context file, whose
// folder is on the server, is read from its latest revision (U04).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { remoteContextText } from './contextMode';

let revision: number | null = 7;
const calls: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'clm_status') return { revision, realPath: null };
      if (cmd === 'clm_read_revision') return `[[LIVE_CONTEXT revision=${args.number} turns=2]]`;
      return null;
    },
  },
};

test('a remote chat’s context file is read from its latest revision', async () => {
  assert.equal(await remoteContextText('c1'), '[[LIVE_CONTEXT revision=7 turns=2]]');
  assert.deepEqual(
    calls.map((c) => c.cmd),
    ['clm_status', 'clm_read_revision']
  );
  revision = null;
  assert.equal(await remoteContextText('c1'), null, 'none yet: nothing to show');
});
