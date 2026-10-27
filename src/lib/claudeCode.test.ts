// Claude Code's tool calls as Rovik's of the same kind (lib/claudeCode.ts
// claudeCodeChatCall), for the chat to show them as it shows Rovik's.
// Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { claudeCodeChatCall } from './claudeCode';

test('its file tools are Rovik’s read and write, an edit a partial write', () => {
  assert.deepEqual(claudeCodeChatCall('Read', { file_path: 'src/a.rs' }), {
    name: 'read',
    arguments: { path: 'src/a.rs' },
  });
  assert.deepEqual(claudeCodeChatCall('Read', { file_path: 'a.rs', offset: 10, limit: 5 }), {
    name: 'read',
    arguments: { path: 'a.rs', start_line: 10, end_line: 14 },
  });
  assert.deepEqual(claudeCodeChatCall('Write', { file_path: 'a.rs', content: 'x' }), {
    name: 'write',
    arguments: { path: 'a.rs', content: 'x' },
  });
  assert.deepEqual(
    claudeCodeChatCall('Edit', { file_path: 'a.rs', old_string: 'x', new_string: 'y' }),
    { name: 'write', arguments: { path: 'a.rs', old_text: 'x', content: 'y' } }
  );
});

test('its shell, searches and web tools are Rovik’s of the same kind', () => {
  assert.deepEqual(claudeCodeChatCall('Bash', { command: 'npm test', description: 'Test' }), {
    name: 'bash',
    arguments: { command: 'npm test' },
  });
  for (const tool of ['Grep', 'Glob']) {
    assert.deepEqual(claudeCodeChatCall(tool, { pattern: 'fn a', path: 'src' }), {
      name: 'search_code',
      arguments: { query: 'fn a', path: 'src' },
    });
  }
  assert.deepEqual(claudeCodeChatCall('LS', { path: 'src' }), {
    name: 'list_dir',
    arguments: { path: 'src' },
  });
  assert.deepEqual(claudeCodeChatCall('WebFetch', { url: 'https://a.dev', prompt: 'Sum up' }), {
    name: 'browse_url',
    arguments: { url: 'https://a.dev' },
  });
  assert.deepEqual(claudeCodeChatCall('WebSearch', { query: 'solid' }), {
    name: 'web_search',
    arguments: { query: 'solid' },
  });
});

test('its checklist is Rovik’s, its finished items done', () => {
  const todos = [
    { content: 'Read the code', status: 'completed', activeForm: 'Reading the code' },
    { content: 'Fix it', status: 'in_progress', activeForm: 'Fixing it' },
    { content: 'Test it', status: 'pending', activeForm: 'Testing it' },
  ];
  assert.deepEqual(claudeCodeChatCall('TodoWrite', { todos }), {
    name: 'todo',
    arguments: {
      items: [
        { title: 'Read the code', status: 'done' },
        { title: 'Fix it', status: 'in_progress' },
        { title: 'Test it', status: 'pending' },
      ],
    },
  });
});

test('a skill it uses is a skill call, by the skill’s name', () => {
  assert.deepEqual(claudeCodeChatCall('Skill', { skill: 'pdf', args: 'report.pdf' }), {
    name: 'skill',
    arguments: { skill: 'pdf', args: 'report.pdf' },
  });
  assert.deepEqual(claudeCodeChatCall('Skill', { skill: 'slow-census' }), {
    name: 'skill',
    arguments: { skill: 'slow-census' },
  });
});

test('a tool Rovik has none like keeps its own name', () => {
  assert.equal(claudeCodeChatCall('Task', { description: 'Look around' }), undefined);
  assert.equal(claudeCodeChatCall('mcp__github__search', { q: 'x' }), undefined);
});
