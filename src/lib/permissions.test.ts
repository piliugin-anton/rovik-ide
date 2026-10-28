import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  checkCommand,
  DEFAULT_PERMISSIONS,
  describeToolCall,
  normalizeHostEntry,
  normalizePermissions,
  parseCommand,
  permissionsSummary,
  usesAiReview,
} from './permissions';

/*
 * The cases are shared with the backend's tests (agent_run/permissions.rs):
 * the backend enforces these rules, this file only hints at them in the
 * UI, and both must agree on every case.
 */
interface Case {
  command: string;
  allowed?: string[];
  allowAny?: boolean;
  expect: {
    segments?: string[][];
    writesViaRedirect?: boolean;
    opaque?: boolean;
    permitted?: boolean;
    modifiesFiles?: boolean;
  };
}
const fixture = JSON.parse(
  readFileSync(join(process.cwd(), 'test-fixtures/command-permissions.json'), 'utf8')
) as { defaultAllowed: string[]; cases: Case[] };

test('commands parse and are judged as the backend judges them', () => {
  for (const c of fixture.cases) {
    const at = JSON.stringify(c.command);
    const parsed = parseCommand(c.command);
    if (c.expect.segments)
      assert.deepEqual(parsed.segments, c.expect.segments, `segments of ${at}`);
    if (c.expect.writesViaRedirect !== undefined)
      assert.equal(parsed.writesViaRedirect, c.expect.writesViaRedirect, `redirect in ${at}`);
    if (c.expect.opaque !== undefined) assert.equal(parsed.opaque, c.expect.opaque, `opaque ${at}`);

    const verdict = checkCommand(c.command, {
      ...DEFAULT_PERMISSIONS,
      allowAnyCommand: c.allowAny ?? false,
      allowedCommands: c.allowed ?? fixture.defaultAllowed,
    });
    if (c.expect.permitted !== undefined)
      assert.equal(verdict.permitted, c.expect.permitted, `permitted ${at}`);
    if (c.expect.modifiesFiles !== undefined) {
      assert.ok(verdict.permitted, `${at} must be permitted to say whether it modifies files`);
      if (verdict.permitted)
        assert.equal(verdict.modifiesFiles, c.expect.modifiesFiles, `modifies files ${at}`);
    }
    if (!verdict.permitted) assert.ok(verdict.reason.length > 0, `a reason for ${at}`);
  }
});

test('a command left running, and the tool that reads and stops it, are described as the backend does', () => {
  assert.equal(
    describeToolCall('bash', JSON.stringify({ command: 'npm run dev', background: true })),
    'Run `npm run dev` in the background'
  );
  assert.equal(describeToolCall('bash', JSON.stringify({ command: 'npm test' })), 'Run `npm test`');
  assert.equal(
    describeToolCall('background', JSON.stringify({ action: 'stop', id: 'bg2' })),
    'Stop bg2'
  );
  assert.equal(
    describeToolCall('background', JSON.stringify({ action: 'output', id: 'bg2' })),
    'Read what bg2 printed'
  );
  assert.equal(
    describeToolCall('background', JSON.stringify({ action: 'list' })),
    'List the commands running in the background'
  );
});

/*
 * The allowed hosts' entries, as rovik-core's allowed_hosts.rs reads them:
 * both check every case in the shared file.
 */
test('allowed-host entries are kept as the backend reads them', () => {
  const hosts = JSON.parse(
    readFileSync(join(process.cwd(), 'test-fixtures/allowed-hosts.json'), 'utf8')
  ) as { normalize: [string, string | null][] };
  for (const [raw, want] of hosts.normalize) {
    assert.equal(normalizeHostEntry(raw), want, JSON.stringify(raw));
  }
});

test('permissions saved before the allowed hosts reach any host', () => {
  const before = normalizePermissions({ allowAnyCommand: true });
  assert.equal(before.allowAnyHost, true);
  assert.deepEqual(before.allowedHosts, []);
});

test('saved allowed hosts are read again as entries, each once', () => {
  const saved = normalizePermissions({
    allowAnyHost: false,
    allowedHosts: ['Docs.RS', 'https://docs.rs/tokio', '*.github.com', 'exa mple.com', 7, ''],
  });
  assert.equal(saved.allowAnyHost, false);
  assert.deepEqual(saved.allowedHosts, ['docs.rs', '*.github.com']);
});

// docs/design/mcp-plan.md, Q07: MCP calls can change things outside the
// project, so they ask unless the user says otherwise.
test('MCP tool calls ask by default, also with permissions saved before their rule', () => {
  const before = normalizePermissions({
    commandApproval: { required: false, by: 'user' },
    toolApproval: { required: false, by: 'user' },
  });
  assert.deepEqual(before.mcpApproval, { required: true, by: 'user' });
  assert.match(permissionsSummary(before), / · MCP tools: your approval$/);
  const free = normalizePermissions({ mcpApproval: { required: false, by: 'user' } });
  assert.deepEqual(free.mcpApproval, { required: false, by: 'user' });
  const reviewed = normalizePermissions({ mcpApproval: { required: true, by: 'ai' } });
  assert.equal(usesAiReview(reviewed), true, 'the AI reviewer is set up for it');
});

test('an MCP call is described as the backend describes it: the server’s tool and its arguments', () => {
  assert.equal(
    describeToolCall('mcp', JSON.stringify({ tool: 'docs.read', arguments: { path: 'a' } })),
    'Call docs.read with {"path":"a"}'
  );
  assert.equal(describeToolCall('mcp', JSON.stringify({ tool: 'docs.list' })), 'Call docs.list');
  assert.equal(
    describeToolCall('mcp', JSON.stringify({ tool: 'docs.read', describe: true })),
    'Read about docs.read'
  );
});
