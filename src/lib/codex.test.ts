// Codex as a connector (lib/codex.ts): its settings and its steps as
// Rovik's calls of the same kind (codexChatCall), for the chat to show
// them as it shows Rovik's.
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { CODEX, CODEX_DEFAULTS, codexChatCall, codexSettings } from './codex';

test('a command reads as what Codex read it as doing, else as bash', () => {
  const shell = (actions: unknown[]) =>
    codexChatCall('shell', { command: 'sed -n 1,20p src/a.rs', actions });
  assert.deepEqual(
    shell([{ type: 'read', command: 'sed -n 1,20p src/a.rs', name: 'a.rs', path: 'src/a.rs' }]),
    {
      name: 'read',
      arguments: { path: 'src/a.rs' },
    }
  );
  assert.deepEqual(shell([{ type: 'listFiles', command: 'ls src', path: 'src' }]), {
    name: 'list_dir',
    arguments: { path: 'src' },
  });
  assert.deepEqual(shell([{ type: 'search', command: 'rg fn', query: 'fn', path: 'src' }]), {
    name: 'search_code',
    arguments: { query: 'fn', path: 'src' },
  });
  // Several things at once, or one Codex can't say: the command itself.
  for (const actions of [
    [],
    [{ type: 'unknown', command: 'npm test' }],
    [{ type: 'read' }, { type: 'read' }],
  ]) {
    assert.deepEqual(shell(actions), {
      name: 'bash',
      arguments: { command: 'sed -n 1,20p src/a.rs' },
    });
  }
});

test('a file a patch adds is a write of it, one it changes a partial write', () => {
  assert.deepEqual(codexChatCall('edit', { path: 'b.txt', kind: 'add', diff: 'bee\n' }), {
    name: 'write',
    arguments: { path: 'b.txt', content: 'bee\n' },
  });
  const diff = '@@ -1,3 +1,3 @@\n keep\n-old\n+new\n';
  assert.deepEqual(codexChatCall('edit', { path: 'a.txt', kind: 'update', diff }), {
    name: 'write',
    arguments: { path: 'a.txt', old_text: 'keep\nold', content: 'keep\nnew' },
  });
  // Moved: where it goes.
  const moved = codexChatCall('edit', { path: 'a.txt', movePath: 'b.txt', kind: 'update', diff });
  assert.equal(moved?.arguments.path, 'b.txt');
  // Deleted: Rovik has no call like it.
  assert.equal(codexChatCall('edit', { path: 'a.txt', kind: 'delete', diff: '' }), undefined);
});

test('its plan is Rovik’s checklist, its web search Rovik’s', () => {
  const plan = [
    { step: 'Read the code', status: 'completed' },
    { step: 'Fix it', status: 'inProgress' },
    { step: 'Test it', status: 'pending' },
  ];
  assert.deepEqual(codexChatCall('update_plan', { plan }), {
    name: 'todo',
    arguments: {
      items: [
        { title: 'Read the code', status: 'done' },
        { title: 'Fix it', status: 'in_progress' },
        { title: 'Test it', status: 'pending' },
      ],
    },
  });
  assert.deepEqual(codexChatCall('web_search', { query: 'solid signals' }), {
    name: 'web_search',
    arguments: { query: 'solid signals' },
  });
});

test('a step Rovik has none like keeps its own name', () => {
  assert.equal(codexChatCall('mcp__github__search', { q: 'x' }), undefined);
  assert.equal(codexChatCall('agent', { tool: 'spawnAgent' }), undefined);
});

test('its settings take the defaults for what isn’t set, and its row says how it runs', () => {
  assert.deepEqual(codexSettings({ settings: {} }), CODEX_DEFAULTS);
  assert.equal(codexSettings({ settings: { program: '  ' } }).program, 'codex');
  assert.equal(CODEX.summary({}), 'codex · Follow Rovik’s permissions');
  assert.equal(
    CODEX.summary({ tools: 'rovik', program: '/opt/codex' }),
    '/opt/codex · Rovik’s agent'
  );
  assert.ok(CODEX.usesRovikTools({ tools: 'rovik' }));
  assert.ok(CODEX.planMode);
});
