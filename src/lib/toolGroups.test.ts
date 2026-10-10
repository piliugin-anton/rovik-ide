// @ts-nocheck
// toolGroups.ts tests (node:test). Run with: npm test
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { commandName, describeGroup, domainName, groupToolCalls } from './toolGroups';

let next = 0;
function call(name, args = {}) {
  return { id: `c${next++}`, name, arguments: JSON.stringify(args) };
}

describe('groupToolCalls', () => {
  it('groups consecutive calls to one tool, whatever their arguments', () => {
    const runs = groupToolCalls([
      call('read', { path: 'a.ts' }),
      call('read', { path: 'b.ts' }),
      call('bash', { command: 'ls' }),
      call('read', { path: 'c.ts' }),
    ]);
    assert.deepEqual(
      runs.map((r) => [r.name, r.calls.length]),
      [
        ['read', 2],
        ['bash', 1],
        ['read', 1],
      ]
    );
  });

  it('keeps questions, checklists and context edits apart', () => {
    const runs = groupToolCalls([
      call('todo', { items: [] }),
      call('todo', { items: [] }),
      call('write', { path: 'a.ts', content: '' }),
      call('write', { path: '@context', content: '' }),
      call('write', { path: 'b.ts', content: '' }),
    ]);
    assert.deepEqual(
      runs.map((r) => [r.calls.length, !!r.alone]),
      [
        [1, true],
        [1, true],
        [1, false],
        [1, true],
        [1, false],
      ]
    );
  });
});

describe('describeGroup', () => {
  it('names files once each, with where to open them', () => {
    const label = describeGroup(
      'read',
      [{ path: 'src/App.tsx', start_line: 10 }, { path: 'src/main.tsx' }, { path: 'src/App.tsx' }],
      false
    );
    assert.equal(label.verb, 'Read');
    assert.deepEqual(
      label.items.map((i) => [i.text, i.path, i.line]),
      [
        ['App.tsx', 'src/App.tsx', 10],
        ['main.tsx', 'src/main.tsx', undefined],
      ]
    );
  });

  it('says editing for partial writes and caps the list', () => {
    const args = ['a', 'b', 'c', 'd', 'e', 'f'].map((n) => ({ path: `${n}.ts`, content: '' }));
    args[2].old_text = 'x';
    const label = describeGroup('write', args, true);
    assert.equal(label.verb, 'Editing');
    assert.equal(label.items.length, 4);
    assert.equal(label.more, 2);
    assert.equal(describeGroup('write', [{ path: 'a.ts' }], false).verb, 'Wrote');
  });

  it('names commands by their program and pages by their site', () => {
    const ran = describeGroup(
      'bash',
      [{ command: 'npm test -- --watch' }, { command: 'git status' }, { command: 'npm run build' }],
      false
    );
    assert.equal(ran.verb, 'Ran');
    assert.deepEqual(
      ran.items.map((i) => i.text),
      ['npm', 'git']
    );
    const browsed = describeGroup(
      'browse_url',
      [{ url: 'https://www.docs.rs/tokio' }, { url: 'docs.rs/serde' }, { url: 'github.com/x' }],
      true
    );
    assert.equal(browsed.verb, 'Browsing');
    assert.deepEqual(
      browsed.items.map((i) => i.text),
      ['docs.rs', 'github.com']
    );
  });
});

describe('commandName', () => {
  it('skips variable assignments and paths', () => {
    assert.equal(commandName('FOO=1 BAR=2 /usr/bin/cargo build'), 'cargo');
    assert.equal(commandName('  "./run.sh" --fast'), 'run.sh');
    assert.equal(commandName(''), undefined);
  });
});

describe('domainName', () => {
  it('takes the host from a URL with or without a scheme', () => {
    assert.equal(domainName('http://localhost:5173/app'), 'localhost');
    assert.equal(domainName('example.org/path'), 'example.org');
    assert.equal(domainName(undefined), undefined);
  });
});
