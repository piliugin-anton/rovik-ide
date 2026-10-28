// Unit tests: Settings → MCP servers' logic (lib/mcpServers.ts). Stored
// servers read back as saved, never with a secret's value; a server checked
// before it's saved; its secrets taken out to the keychain when it's saved,
// and forgotten when it no longer has them; JSON from other clients' setups
// made into servers. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  forgettingServer,
  importServers,
  mcpServersSummary,
  normalizeMcpServers,
  savingServer,
  serverProblem,
  serverSummary,
  transportProblem,
  unsavedSecrets,
  type McpServer,
} from './mcpServers';

const github: McpServer = {
  id: 'gh',
  name: 'github',
  enabled: true,
  transport: {
    kind: 'command',
    command: 'npx',
    args: ['-y', '@modelcontextprotocol/server-github'],
    env: [
      { name: 'GITHUB_TOKEN', value: '', secret: true, saved: true },
      { name: 'GITHUB_HOST', value: 'github.com', secret: false },
    ],
  },
  disabledTools: [],
};

test('stored servers read back as saved, and a secret’s value never', () => {
  const read = normalizeMcpServers([
    {
      id: 'gh',
      name: 'github',
      transport: {
        kind: 'command',
        command: 'npx',
        args: ['-y', 7, '@modelcontextprotocol/server-github'],
        env: [
          { name: ' GITHUB_TOKEN ', value: 'leaked', secret: true, saved: true },
          { name: 'GITHUB_HOST', value: 'github.com', saved: true },
          { name: '', value: 'nameless' },
        ],
      },
    },
    { id: 'docs', name: 'docs', enabled: false, transport: { kind: 'url', url: 'https://x' } },
    { id: '', name: 'no-id', transport: { kind: 'url', url: 'https://x' } },
    { id: 'bad', name: 'bad', transport: { kind: 'ftp' } },
    'junk',
  ]);
  assert.deepEqual(read, [
    github,
    {
      id: 'docs',
      name: 'docs',
      enabled: false,
      transport: { kind: 'url', url: 'https://x', headers: [] },
      disabledTools: [],
    },
  ]);
  assert.deepEqual(normalizeMcpServers(undefined), []);
});

test('a server is checked before it’s saved: its name, then how it’s reached', () => {
  const others = [github];
  const named = (name: string) => serverProblem({ ...github, id: 'new', name }, others);
  assert.equal(named(' '), 'Give the server a name.');
  assert.match(named('GitHub')!, /lower-case letters, digits/);
  assert.equal(named('github'), 'Another server is named github.');
  assert.equal(serverProblem(github, others), null, 'its own name is no clash');
  assert.equal(named('gh-2'), null);

  assert.equal(
    transportProblem({ kind: 'command', command: ' ', args: [], env: [] }),
    'Give the command that starts the server.'
  );
  assert.equal(
    transportProblem({ kind: 'url', url: 'mcp.example.com', headers: [] }),
    'The URL starts with http:// or https://.'
  );
  const header = (name: string) => ({ name, value: 'x', secret: false });
  const url = (...names: string[]) =>
    transportProblem({ kind: 'url', url: 'https://mcp.example.com', headers: names.map(header) });
  assert.equal(url(''), 'Each value needs a name.');
  assert.match(url('X Token')!, /X Token isn’t a name/);
  assert.equal(url('Authorization', 'authorization'), 'Two headers have the same name.');
  assert.equal(url('Authorization', 'X-Team'), null);
});

test('saving a server takes out each secret typed, and forgets those it no longer has', () => {
  const draft: McpServer = {
    ...github,
    name: ' github ',
    transport: {
      kind: 'command',
      command: ' npx ',
      args: ['-y', '', '@modelcontextprotocol/server-github'],
      env: [
        // Typed again: saved over the old value.
        { name: 'GITHUB_TOKEN', value: 'ghp_new', secret: true, saved: true },
        // New, with nothing typed: not saved yet.
        { name: 'GITHUB_APP_KEY', value: '', secret: true },
        { name: 'GITHUB_HOST', value: 'github.com', secret: false },
      ],
    },
  };
  const saved: McpServer = {
    ...github,
    transport: {
      ...github.transport,
      env: [
        { name: 'GITHUB_TOKEN', value: '', secret: true, saved: true },
        { name: 'OLD_SECRET', value: '', secret: true, saved: true },
        { name: 'GITHUB_HOST', value: 'github.com', secret: false },
      ],
    } as McpServer['transport'],
  };
  const { server, secrets } = savingServer(draft, saved);
  assert.deepEqual(secrets, { 'env/GITHUB_TOKEN': 'ghp_new', 'env/OLD_SECRET': '' });
  assert.deepEqual(server, {
    ...github,
    transport: {
      kind: 'command',
      command: 'npx',
      args: ['-y', '@modelcontextprotocol/server-github'],
      env: [
        { name: 'GITHUB_TOKEN', value: '', secret: true, saved: true },
        { name: 'GITHUB_APP_KEY', value: '', secret: true },
        { name: 'GITHUB_HOST', value: 'github.com', secret: false },
      ],
    },
  });
  assert.deepEqual(unsavedSecrets(server), ['GITHUB_APP_KEY']);

  // Left as it was: nothing typed, the saved secret kept, nothing to change.
  assert.deepEqual(savingServer(github, github), { server: github, secrets: {} });

  // Made plain, or moved to a URL: the saved secret is forgotten.
  const plain = savingServer(
    {
      ...github,
      transport: {
        ...github.transport,
        env: [{ name: 'GITHUB_TOKEN', value: 'shown', secret: false }],
      } as McpServer['transport'],
    },
    github
  );
  assert.deepEqual(plain.secrets, { 'env/GITHUB_TOKEN': '' });
  const moved = savingServer(
    {
      ...github,
      transport: {
        kind: 'url',
        url: 'https://api.githubcopilot.com/mcp/',
        headers: [{ name: 'GITHUB_TOKEN', value: '', secret: true, saved: true }],
      },
    },
    github
  );
  assert.deepEqual(moved.secrets, { 'env/GITHUB_TOKEN': '' });
  assert.deepEqual(unsavedSecrets(moved.server), ['GITHUB_TOKEN'], 'not saved as a header');
});

test('removing a server forgets each of its secrets', () => {
  assert.deepEqual(forgettingServer(github), { 'env/GITHUB_TOKEN': '' });
  assert.deepEqual(
    forgettingServer({ ...github, transport: { kind: 'url', url: 'https://x', headers: [] } }),
    {}
  );
});

test('a server’s row shows its command line or its URL', () => {
  assert.equal(serverSummary(github), 'npx -y @modelcontextprotocol/server-github');
  assert.equal(
    serverSummary({
      ...github,
      transport: {
        kind: 'command',
        command: 'uvx',
        args: ['mcp-server-fetch', 'a b', ''],
        env: [],
      },
    }),
    'uvx mcp-server-fetch "a b" ""'
  );
  assert.equal(
    serverSummary({ ...github, transport: { kind: 'url', url: 'https://x/mcp', headers: [] } }),
    'https://x/mcp'
  );
});

test('the menu says which servers are on', () => {
  assert.equal(mcpServersSummary([]), 'None yet: add one for its tools');
  const docs = { ...github, id: 'docs', name: 'docs' };
  assert.equal(mcpServersSummary([github, docs]), 'github, docs');
  assert.equal(mcpServersSummary([github, { ...docs, enabled: false }]), 'github (1 off)');
  assert.equal(mcpServersSummary([{ ...github, enabled: false }]), 'All turned off');
});

test('Claude Desktop’s, Cursor’s and VS Code’s JSON become servers, their secrets typed in', () => {
  const desktop = importServers(
    JSON.stringify({
      mcpServers: {
        GitHub: {
          command: 'npx',
          args: ['-y', '@modelcontextprotocol/server-github'],
          env: { GITHUB_PERSONAL_ACCESS_TOKEN: 'ghp_123', LOG_LEVEL: 3 },
        },
        'my.docs': {
          url: 'https://docs.example.com/mcp',
          headers: { Authorization: 'Bearer <YOUR_TOKEN>' },
          disabled: true,
        },
      },
    }),
    [github]
  );
  assert.ok('servers' in desktop, JSON.stringify(desktop));
  const [gh, docs] = desktop.servers;
  assert.equal(gh.name, 'github-2', 'made valid, and not a name already taken');
  assert.deepEqual(gh.transport, {
    kind: 'command',
    command: 'npx',
    args: ['-y', '@modelcontextprotocol/server-github'],
    env: [
      { name: 'GITHUB_PERSONAL_ACCESS_TOKEN', value: 'ghp_123', secret: true },
      { name: 'LOG_LEVEL', value: '3', secret: false },
    ],
  });
  assert.equal(gh.enabled, true);
  assert.equal(docs.name, 'my-docs');
  assert.equal(docs.enabled, false, 'turned off as it was');
  assert.deepEqual(docs.transport, {
    kind: 'url',
    url: 'https://docs.example.com/mcp',
    // The placeholder is left for the user to enter.
    headers: [{ name: 'Authorization', value: '', secret: true }],
  });
  // Saved as a form's would be: the token to the keychain.
  assert.deepEqual(savingServer(gh, undefined).secrets, {
    'env/GITHUB_PERSONAL_ACCESS_TOKEN': 'ghp_123',
  });

  const vscode = importServers(
    JSON.stringify({
      servers: { fetch: { type: 'stdio', command: 'uvx', args: ['mcp-server-fetch'] } },
    }),
    []
  );
  assert.deepEqual('servers' in vscode && vscode.servers.map((s) => s.name), ['fetch']);
  const alone = importServers(JSON.stringify({ command: 'uvx', args: ['mcp-server-time'] }), []);
  assert.deepEqual('servers' in alone && alone.servers.map((s) => s.name), ['server']);
  const named = importServers(JSON.stringify({ time: { command: 'uvx' } }), []);
  assert.deepEqual('servers' in named && named.servers.map((s) => s.name), ['time']);
});

test('JSON that has no server, or an SSE one, says what to do instead', () => {
  assert.match((importServers('{', []) as { error: string }).error, /That isn’t JSON/);
  assert.match((importServers('[]', []) as { error: string }).error, /Paste an object/);
  assert.match(
    (importServers('{"mcpServers": {"x": {"args": []}}}', []) as { error: string }).error,
    /No server found/
  );
  assert.equal(
    (
      importServers(
        JSON.stringify({
          mcpServers: { old: { type: 'sse', url: 'https://old.example.com/sse' } },
        }),
        []
      ) as { error: string }
    ).error,
    'old uses the old SSE transport: add it as a command, through a bridge: npx mcp-remote https://old.example.com/sse'
  );
});
