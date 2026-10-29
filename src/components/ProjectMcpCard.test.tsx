// Component test: the chat's card for the project's own MCP servers
// (ProjectMcpCard.tsx, mcp-plan-2.md Q12). Its fields start with this
// computer's environment, and a value already saved isn't asked for again;
// Allow waits for the values with no default, then keeps them in the
// keychain and the servers in the project's settings; Not now puts them
// off; a card of notes only is dismissed with OK; and a project not open
// here can't be allowed. The app is answered here. Run with: npm test
import { container } from '../test/dom';
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ProjectMcpCard from './ProjectMcpCard';
import {
  awaitingServersIn,
  noteMcpServersAwaiting,
  type AwaitingServers,
} from '../lib/projectMcpServers';

let backend: Record<string, (args: any) => unknown> = {};
let calls: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

let dispose = () => {};
afterEach(() => {
  dispose();
  document.body.innerHTML = '';
});

const awaiting: AwaitingServers = {
  projectId: 'p1',
  servers: [
    {
      name: 'github',
      file: '.mcp.json',
      hash: 'h-gh',
      transport: {
        kind: 'command',
        command: 'npx',
        args: ['-y', '@modelcontextprotocol/server-github'],
        env: [{ name: 'GITHUB_TOKEN', value: '${GITHUB_TOKEN}', secret: true }],
      },
      variables: [{ name: 'GITHUB_TOKEN' }, { name: 'LEVEL', default: 'info' }],
    },
    {
      name: 'db',
      file: '.vscode/mcp.json',
      hash: 'h-db',
      transport: {
        kind: 'url',
        url: 'https://db.example.com/mcp',
        headers: [{ name: 'Authorization', value: '${input:db}', secret: true }],
      },
      variables: [{ name: 'input:db', description: 'The database’s password', password: true }],
    },
  ],
  leftOut: ['time (.cursor/mcp.json) isn’t used: one of the user’s MCP servers has its name'],
};

/** The card for `chat`, the project open at `root`, the app answering as given. */
async function card(
  chat: string,
  shown: AwaitingServers,
  root: string | null,
  prefill: { values: Record<string, string>; saved: string[] } = { values: {}, saved: [] }
) {
  calls = [];
  const saved: { settings: unknown } = { settings: null };
  backend = {
    mcp_project_prefill: () => prefill,
    mcp_save_project_values: () => null,
    load_project_settings: () => saved.settings,
    save_project_settings: (args) => {
      saved.settings = args.settings;
    },
  };
  noteMcpServersAwaiting(chat, shown);
  dispose = render(
    () => <ProjectMcpCard dialogId={chat} awaiting={awaitingServersIn(chat)!} root={root} />,
    container()
  );
  await flush();
  return saved;
}

const text = (el: Element | null | undefined) => el?.textContent?.replace(/\s+/g, ' ').trim() ?? '';
const button = (label: string) =>
  [...document.querySelectorAll<HTMLButtonElement>('button')].find((b) => text(b) === label);
const field = (label: string) =>
  document.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;
const sent = (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args);

async function type(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await flush();
}

test('the card shows each server and its file, with a field for each variable, filled from the environment', async () => {
  await card('card-1', awaiting, '/work/app', {
    values: { GITHUB_TOKEN: 'ghp_env' },
    saved: [],
  });
  assert.deepEqual(sent('mcp_project_prefill'), [
    { project: 'p1', names: ['GITHUB_TOKEN', 'LEVEL', 'input:db'] },
  ]);
  const servers = [...document.querySelectorAll('.mcp-awaiting-server')].map((s) =>
    [...s.querySelectorAll('span')].map(text)
  );
  assert.deepEqual(servers, [
    ['github', 'npx -y @modelcontextprotocol/server-github', '.mcp.json'],
    ['db', 'https://db.example.com/mcp', '.vscode/mcp.json'],
  ]);
  assert.equal(field('GITHUB_TOKEN').value, 'ghp_env');
  assert.equal(field('GITHUB_TOKEN').type, 'password');
  assert.equal(field('LEVEL').placeholder, 'info unless given');
  assert.match(text(document.querySelector('.mcp-awaiting')), /input:db · The database’s password/);
  assert.match(
    text(document.querySelector('.mcp-awaiting-left-out')),
    /time \(.cursor\/mcp.json\) isn’t used/
  );
});

test('Allow waits for each value without a default, then keeps them and the servers', async () => {
  const saved = await card('card-2', awaiting, '/work/app');
  const allow = button('Allow')!;
  assert.ok(allow.disabled, 'GITHUB_TOKEN has no value yet');
  assert.equal(allow.title, 'Give GITHUB_TOKEN a value first');
  await type(field('GITHUB_TOKEN'), 'ghp_typed');
  assert.ok(allow.disabled, 'nor has the database’s password');
  await type(field('input:db'), 'hunter2');
  assert.ok(!allow.disabled, 'LEVEL has its default');
  allow.click();
  await flush();
  assert.deepEqual(sent('mcp_save_project_values'), [
    { project: 'p1', values: { GITHUB_TOKEN: 'ghp_typed', 'input:db': 'hunter2' } },
  ]);
  assert.deepEqual(saved.settings, {
    mcpApprovals: [
      { hash: 'h-gh', name: 'github', file: '.mcp.json', variables: ['GITHUB_TOKEN', 'LEVEL'] },
      { hash: 'h-db', name: 'db', file: '.vscode/mcp.json', variables: ['input:db'] },
    ],
  });
  assert.equal(awaitingServersIn('card-2'), undefined, 'the chat no longer asks');
});

test('a value already saved isn’t asked for again', async () => {
  const one = { ...awaiting, servers: [awaiting.servers[0]], leftOut: [] };
  await card('card-3', one, '/work/app', { values: {}, saved: ['GITHUB_TOKEN'] });
  assert.equal(field('GITHUB_TOKEN').value, '', 'the saved value isn’t shown');
  assert.equal(field('GITHUB_TOKEN').placeholder, 'Saved: type to replace');
  button('Allow')!.click();
  await flush();
  assert.deepEqual(sent('mcp_save_project_values'), [], 'nothing typed to save');
  assert.equal(awaitingServersIn('card-3'), undefined);
});

test('Not now puts the servers off: the chat no longer asks', async () => {
  await card('card-4', { ...awaiting, projectId: 'p-off' }, '/work/app');
  button('Not now')!.click();
  await flush();
  assert.equal(awaitingServersIn('card-4'), undefined);
  assert.deepEqual(sent('save_project_settings'), [], 'nothing allowed');
  noteMcpServersAwaiting('card-4', { ...awaiting, projectId: 'p-off' });
  assert.equal(awaitingServersIn('card-4'), undefined, 'put off until Rovik is closed');
});

test('a card of notes only is dismissed with OK', async () => {
  await card('card-5', { projectId: 'p-notes', servers: [], leftOut: awaiting.leftOut }, '/w');
  assert.equal(button('Allow'), undefined);
  button('OK')!.click();
  await flush();
  assert.equal(awaitingServersIn('card-5'), undefined);
});

test('the servers of a project not open here can’t be allowed from its chat', async () => {
  await card('card-6', { ...awaiting, projectId: 'p-closed' }, null, {
    values: { GITHUB_TOKEN: 'ghp_env' },
    saved: ['input:db'],
  });
  assert.ok(button('Allow')!.disabled);
  assert.match(text(document.body), /Open the project to allow its servers\./);
});
