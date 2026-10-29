// projectMcpServers.ts tests (node:test): what the chats ask of a
// project's own MCP servers and when they stop asking, and the approvals
// and values allowing and revoking keep (mcp-plan-2.md, Q12), an approval
// saying whether its server may ask for a sign-in, and a changed server's
// old sign-in forgotten (Q14). The backend
// is answered here, with the project settings it keeps by folder and the
// values it keeps in the keychain. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  allowServers,
  approvalsIn,
  awaitingServersIn,
  noteMcpServersAwaiting,
  projectApprovals,
  revokeServer,
  settleAwaitingServers,
  variablesOf,
  withApproved,
  type McpApproval,
  type ProjectServer,
} from './projectMcpServers';

/** The project settings the backend keeps, by folder, and the keychain's values. */
const settings = new Map<string, unknown>();
const keychain = new Map<string, string>();
const calls: { cmd: string; args: any }[] = [];
let projects: { id: string; name: string; path: string }[] = [];
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'load_project_settings') return settings.get(args.root) ?? null;
      if (cmd === 'save_project_settings') {
        if (args.settings === null) settings.delete(args.root);
        else settings.set(args.root, args.settings);
        return;
      }
      if (cmd === 'mcp_save_project_values') {
        for (const [name, value] of Object.entries(args.values as Record<string, string>)) {
          const key = `${args.project}/${name}`;
          if (value === '') keychain.delete(key);
          else keychain.set(key, value);
        }
        return;
      }
      if (cmd === 'chat_list_projects') return projects;
      if (cmd === 'mcp_sign_out') return;
      throw new Error(`no app: ${cmd}`);
    },
  },
};

const server = (name: string, hash: string, variables: string[] = []): ProjectServer => ({
  name,
  file: '.mcp.json',
  hash,
  transport: { kind: 'command', command: 'npx', args: [name], env: [] },
  variables: variables.map((v) => ({ name: v })),
});

test('allowing in one chat stops every chat of the project asking for those servers', () => {
  const github = server('github', 'h-gh');
  const time = server('time', 'h-time');
  noteMcpServersAwaiting('chat-a', { projectId: 'p1', servers: [github, time] });
  noteMcpServersAwaiting('chat-b', { projectId: 'p1', servers: [github] });
  noteMcpServersAwaiting('chat-c', { projectId: 'p2', servers: [github] });
  settleAwaitingServers('chat-b', false);
  assert.equal(awaitingServersIn('chat-b'), undefined);
  assert.deepEqual(
    awaitingServersIn('chat-a')?.servers.map((s) => s.name),
    ['time'],
    'what the other chat still asks'
  );
  assert.deepEqual(
    awaitingServersIn('chat-c')?.servers.map((s) => s.name),
    ['github'],
    'another project’s are its own'
  );
  // Allowed, not put off: a run that finds it waiting again asks again.
  noteMcpServersAwaiting('chat-b', { projectId: 'p1', servers: [github] });
  assert.deepEqual(
    awaitingServersIn('chat-b')?.servers.map((s) => s.name),
    ['github']
  );
});

test('“Not now” puts the servers off, and a note of what isn’t used is seen once', () => {
  const docs = server('docs', 'h-docs');
  const note = 'time (.mcp.json) isn’t used: one of the user’s MCP servers has its name.';
  noteMcpServersAwaiting('chat-d', { projectId: 'p3', servers: [docs], leftOut: [note] });
  settleAwaitingServers('chat-d', true);
  assert.equal(awaitingServersIn('chat-d'), undefined);
  noteMcpServersAwaiting('chat-d', { projectId: 'p3', servers: [docs], leftOut: [note] });
  assert.equal(awaitingServersIn('chat-d'), undefined, 'nothing asked again');
  const changed = server('docs', 'h-docs-2');
  noteMcpServersAwaiting('chat-d', { projectId: 'p3', servers: [changed], leftOut: [note] });
  assert.deepEqual(awaitingServersIn('chat-d'), {
    projectId: 'p3',
    servers: [changed],
    leftOut: [],
  });
});

test('a server’s variables are each asked for once', () => {
  const both = [server('a', 'h1', ['TOKEN', 'URL']), server('b', 'h2', ['TOKEN', 'KEY'])];
  assert.deepEqual(
    variablesOf(both).map((v) => v.name),
    ['TOKEN', 'URL', 'KEY']
  );
});

test('an approval replaces the one of its file and name, and malformed ones are passed over', () => {
  const kept: McpApproval = { hash: 'old-time', name: 'time', file: '.vscode/mcp.json' };
  const before: McpApproval[] = [
    { hash: 'old-gh', name: 'github', file: '.mcp.json', variables: ['TOKEN'] },
    kept,
  ];
  assert.deepEqual(withApproved(before, [server('github', 'new-gh', ['GH_TOKEN'])]), [
    kept,
    { hash: 'new-gh', name: 'github', file: '.mcp.json', variables: ['GH_TOKEN'] },
  ]);
  assert.deepEqual(approvalsIn({ mcpApprovals: [...before, { hash: 1 }, null, 'x'] }), before);
  assert.deepEqual(approvalsIn(null), []);
  assert.deepEqual(approvalsIn({ mcpApprovals: 'all' }), []);
});

test('the approval of a server reached at its URL without a token says it may ask for a sign-in', () => {
  const at = (name: string, headers: { name: string; value: string; secret: boolean }[]) => ({
    name,
    file: '.mcp.json',
    hash: `h-${name}`,
    transport: { kind: 'url' as const, url: `https://${name}.example.com/mcp`, headers },
  });
  const approved = withApproved(
    [],
    [
      at('linear', []),
      at('keyed', [{ name: 'Authorization', value: 'Bearer ${TOKEN}', secret: true }]),
      server('local', 'h-local'),
    ]
  );
  assert.deepEqual(
    approved.map((a) => [a.name, a.signsIn]),
    [
      ['linear', true],
      ['keyed', undefined],
      ['local', undefined],
    ]
  );
});

test('allowing keeps the values given in the keychain and the servers in the project’s settings', async () => {
  settings.set('/work/app', { checks: [{ kind: 'test' }] });
  calls.length = 0;
  const github = server('github', 'h-gh', ['GITHUB_TOKEN', 'LEVEL']);
  await allowServers('/work/app', 'p1', [github], { GITHUB_TOKEN: 'ghp_1', LEVEL: '' });
  assert.deepEqual(
    calls.map((c) => c.cmd),
    ['mcp_save_project_values', 'load_project_settings', 'save_project_settings'],
    'the values first: a run that has the approval has its values'
  );
  assert.deepEqual(calls[0].args, { project: 'p1', values: { GITHUB_TOKEN: 'ghp_1' } });
  assert.deepEqual(settings.get('/work/app'), {
    checks: [{ kind: 'test' }],
    mcpApprovals: [
      { hash: 'h-gh', name: 'github', file: '.mcp.json', variables: ['GITHUB_TOKEN', 'LEVEL'] },
    ],
  });
  assert.ok(!JSON.stringify(settings.get('/work/app')).includes('ghp_1'), 'never in the settings');
});

test('allowing a server whose entry changed forgets the sign-in made for it as it was', async () => {
  const root = '/work/changed';
  settings.set(root, {
    mcpApprovals: [
      { hash: 'h-linear-1', name: 'linear', file: '.mcp.json', signsIn: true },
      { hash: 'h-local-1', name: 'local', file: '.mcp.json' },
      { hash: 'h-docs', name: 'docs', file: '.mcp.json', signsIn: true },
    ],
  });
  calls.length = 0;
  const linear: ProjectServer = {
    name: 'linear',
    file: '.mcp.json',
    hash: 'h-linear-2',
    transport: { kind: 'url', url: 'https://mcp.linear.app/mcp', headers: [] },
  };
  await allowServers(root, 'p7', [linear, server('local', 'h-local-2')], {});
  assert.deepEqual(
    calls.filter((c) => c.cmd === 'mcp_sign_out').map((c) => c.args),
    [{ server: 'project/p7/h-linear-1' }],
    'local never signed in, and docs is unchanged'
  );
  assert.deepEqual(
    approvalsIn(settings.get(root)).map((a) => a.hash),
    ['h-docs', 'h-linear-2', 'h-local-2']
  );
});

test('revoking forgets the values no other allowed server takes, and the list once empty', async () => {
  const root = '/work/revoked';
  settings.set(root, {
    mcpApprovals: [
      { hash: 'h-a', name: 'a', file: '.mcp.json', variables: ['TOKEN', 'A_ONLY'] },
      { hash: 'h-b', name: 'b', file: '.mcp.json', variables: ['TOKEN'] },
    ],
  });
  for (const name of ['TOKEN', 'A_ONLY']) keychain.set(`p9/${name}`, 'secret');
  await revokeServer(root, 'p9', 'h-a');
  assert.deepEqual(approvalsIn(settings.get(root)), [
    { hash: 'h-b', name: 'b', file: '.mcp.json', variables: ['TOKEN'] },
  ]);
  assert.deepEqual(
    [...keychain.keys()].filter((k) => k.startsWith('p9/')),
    ['p9/TOKEN']
  );
  await revokeServer(root, 'p9', 'h-b');
  assert.equal(settings.get(root), undefined, 'settings left empty are removed');
  assert.deepEqual(
    [...keychain.keys()].filter((k) => k.startsWith('p9/')),
    []
  );
});

test('revoking a server without variables still stops the project’s servers that run', async () => {
  const root = '/work/plain';
  settings.set(root, { mcpApprovals: [{ hash: 'h-t', name: 'time', file: '.mcp.json' }] });
  calls.length = 0;
  await revokeServer(root, 'p8', 'h-t');
  assert.deepEqual(
    calls.filter((c) => c.cmd === 'mcp_save_project_values').map((c) => c.args),
    [{ project: 'p8', values: {} }],
    'nothing to forget, but its server stops'
  );
});

test('the settings page lists the projects that have servers allowed, each with them', async () => {
  projects = [
    { id: 'p1', name: 'app', path: '/work/app' },
    { id: 'p2', name: 'site', path: '/work/site' },
  ];
  settings.set('/work/site', { checks: [] });
  const listed = await projectApprovals();
  assert.deepEqual(
    listed.map((l) => [l.project.name, l.approvals.map((a) => a.name)]),
    [['app', ['github']]]
  );
});
