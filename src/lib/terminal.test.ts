// terminal.ts tests (node:test): which cloud hosts a terminal can open on,
// and what the terminal panel calls its tabs, here and on hosts.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { remoteLabel, terminalHosts, terminalLabel, type LabeledTab } from './terminal';
import type { HostSummary } from './cloud';

test('a terminal opens on a host that is connected, and that SSH reaches', () => {
  const hosts: HostSummary[] = [
    { id: 'a', name: 'Ready', deployed: true, state: 'ready', terminal: true },
    { id: 'b', name: 'Offline', deployed: true, state: 'offline', terminal: true },
    { id: 'c', name: 'Connecting', deployed: true, state: 'connecting', terminal: true },
    { id: 'd', name: 'Not deployed', deployed: false, terminal: true },
    { id: '/run/agentd.sock', name: 'agentd.sock', deployed: true, state: 'ready' },
  ];
  assert.deepEqual(
    terminalHosts(hosts).map((h) => h.name),
    ['Ready']
  );
});

test("a tab's label: its shell, numbered in its place, then the place when told", () => {
  const build = { id: 'h1', name: 'Build' };
  const tabs: LabeledTab[] = [
    { key: 1, shell: 'bash', host: null },
    { key: 2, shell: 'zsh', host: build },
    { key: 3, shell: 'bash', host: null },
    { key: 4, shell: 'bash', host: build },
  ];
  const all = (placed: boolean) => tabs.map((tab) => terminalLabel(tab, tabs, placed));
  assert.deepEqual(all(true), [
    'bash 1 (local)',
    'zsh (Build (remote))',
    'bash 2 (local)',
    'bash (Build (remote))',
  ]);
  assert.deepEqual(all(false), ['bash 1', 'zsh', 'bash 2', 'bash']);
  assert.equal(remoteLabel('Build'), 'Build (remote)');
});
