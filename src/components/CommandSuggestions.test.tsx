// Component test: the composer's `/` picker of a connector's commands shows
// each command's name, what it takes, its aliases and what it does, a line
// while listing or when listing failed, and picks a row on a click without
// taking focus from the input. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import CommandSuggestions from './CommandSuggestions';
import type { ConnectorCommand } from '../lib/connectorCommands';

const COMMANDS: ConnectorCommand[] = [
  { name: 'commit', description: 'Commit the staged changes (project)', argumentHint: '<message>' },
  { name: 'clear', description: 'Start a new conversation', aliases: ['reset', 'new'] },
];

test('each command shows its name, what it takes, its aliases and what it does', () => {
  const picked: string[] = [];
  const root = container();
  const dispose = render(
    () => (
      <CommandSuggestions commands={COMMANDS} selected={1} onPick={(c) => picked.push(c.name)} />
    ),
    root
  );
  const rows = [...root.querySelectorAll<HTMLButtonElement>('[role="option"]')];
  assert.equal(rows.length, 2);
  assert.match(rows[0].textContent ?? '', /\/commit<message>Commit the staged changes \(project\)/);
  assert.match(rows[1].textContent ?? '', /\/clear\/reset \/newStart a new conversation/);
  assert.equal(rows[1].getAttribute('aria-selected'), 'true');
  assert.equal(rows[0].getAttribute('aria-selected'), 'false');
  // Rows never take focus: the input keeps its caret.
  assert.equal(rows[0].tabIndex, -1);
  rows[0].click();
  assert.deepEqual(picked, ['commit']);
  assert.equal(root.querySelector('[role="status"]'), null);
  dispose();
});

test('a line says the commands are being listed, or why they couldn’t be', () => {
  const root = container();
  const dispose = render(
    () => (
      <CommandSuggestions commands={[]} selected={0} status="Listing commands…" onPick={() => {}} />
    ),
    root
  );
  assert.equal(root.querySelector('[role="status"]')?.textContent, 'Listing commands…');
  assert.equal(root.querySelectorAll('[role="option"]').length, 0);
  dispose();
});
