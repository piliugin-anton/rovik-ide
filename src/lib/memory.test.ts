// memory.ts tests (node:test): a project's long-term memory as Settings says
// it, in one line. The backend is answered here: `memory_stats` returns the
// counts the test sets, and every command asked for is kept.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { memoryStats, memorySummary } from './memory';

let counts: unknown = { total: 0, verified: 0, provisional: 0, deprecated: 0 };
const asked: { cmd: string; args: any }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: any) => {
      asked.push({ cmd, args });
      if (cmd === 'memory_stats') return counts;
      throw new Error(`no app: ${cmd}`);
    },
  },
};

test('a project’s memories, said in one line: every state it has, and none', () => {
  // All three states: the deprecated ones read "invalidated".
  assert.equal(
    memorySummary({ total: 12, verified: 9, provisional: 2, deprecated: 1 }),
    '12 memories: 9 verified, 2 provisional, 1 invalidated'
  );
  // One is said in the singular.
  assert.equal(
    memorySummary({ total: 1, verified: 1, provisional: 0, deprecated: 0 }),
    '1 memory: 1 verified'
  );
  // A state with none isn't mentioned.
  assert.equal(
    memorySummary({ total: 2, verified: 0, provisional: 2, deprecated: 0 }),
    '2 memories: 2 provisional'
  );
  assert.equal(
    memorySummary({ total: 1, verified: 0, provisional: 0, deprecated: 1 }),
    '1 memory: 1 invalidated'
  );
  // None yet says what fills it.
  assert.equal(
    memorySummary({ total: 0, verified: 0, provisional: 0, deprecated: 0 }),
    'No memories yet: the agent adds them as it works'
  );
});

test('the count is asked of the app for the project whose page it is', async () => {
  counts = { total: 3, verified: 2, provisional: 1, deprecated: 0 };
  asked.length = 0;
  assert.deepEqual(await memoryStats('/home/me/demo'), {
    total: 3,
    verified: 2,
    provisional: 1,
    deprecated: 0,
  });
  assert.deepEqual(asked, [{ cmd: 'memory_stats', args: { root: '/home/me/demo' } }]);
});
