// A connector's slash commands (lib/connectorCommands.ts): what the picker
// offers for what's typed, which messages are commands, and how listings
// are kept. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { ModelProfile } from '../types';
import {
  clearCommandsCache,
  commandQuery,
  COMMANDS_FRESH_MS,
  connectorCommands,
  matchCommands,
  startsWithCommand,
  profileConnectorArgs,
  type CommandsKey,
  type ConnectorCommand,
} from './connectorCommands';

const COMMANDS: ConnectorCommand[] = [
  { name: 'compact', description: 'Clear the history but keep a summary', builtin: true },
  { name: 'context', description: 'Show context usage', builtin: true },
  {
    name: 'clear',
    description: 'Start a new conversation',
    aliases: ['reset', 'new'],
    builtin: true,
  },
  { name: 'commit', description: 'Commit the staged changes (project)', argumentHint: '<message>' },
  { name: 'deploy:staging', description: 'Ship to staging (project)' },
];

test('a picked command stays one only while the message starts with it', () => {
  assert.equal(startsWithCommand('/compact', 'compact'), true);
  assert.equal(startsWithCommand('/compact keep the API', 'compact'), true);
  assert.equal(startsWithCommand('  /compact\nkeep the API', 'compact'), true);
  assert.equal(startsWithCommand('/deploy:staging now', 'deploy:staging'), true);
  // Edited into something else: a message.
  assert.equal(startsWithCommand('/compactly', 'compact'), false);
  assert.equal(startsWithCommand('/comp', 'compact'), false);
  assert.equal(startsWithCommand('run /compact', 'compact'), false);
  assert.equal(startsWithCommand('/ compact', 'compact'), false);
});

test('the picker opens while the message is only a command name being typed', () => {
  assert.deepEqual(commandQuery('/'), { query: '', start: 0 });
  assert.deepEqual(commandQuery('/com'), { query: 'com', start: 0 });
  assert.deepEqual(commandQuery('  /co'), { query: 'co', start: 2 });
  assert.equal(commandQuery('/compact '), null);
  assert.equal(commandQuery('/etc/pa'), null);
  assert.equal(commandQuery('see /com'), null);
  assert.equal(commandQuery(''), null);
});

test('names starting with the query come first, the project’s before the built-in', () => {
  const names = (q: string) => matchCommands(q, COMMANDS).map((c) => c.name);
  assert.deepEqual(names('co'), ['commit', 'compact', 'context']);
  // An alias's match, after the names'; then a match inside a name.
  assert.deepEqual(names('re'), ['clear']);
  assert.deepEqual(names('stag'), ['deploy:staging']);
  assert.deepEqual(names('CON'), ['context']);
  assert.deepEqual(names(''), ['commit', 'deploy:staging', 'clear', 'compact', 'context']);
  assert.deepEqual(names('zzz'), []);
});

test('a connector’s model gives the connector to list commands of; a provider’s none', () => {
  const profile = {
    id: 'c::sonnet',
    label: 'Claude Code/sonnet',
    model: 'sonnet',
    baseUrl: 'connector://claude-code',
    apiKey: 'k',
    connector: { bare: true },
  } as unknown as ModelProfile;
  assert.deepEqual(profileConnectorArgs(profile), {
    kind: 'claude-code',
    settings: { bare: true },
    apiKey: 'k',
    request: false,
  });
  assert.equal(profileConnectorArgs({ ...profile, baseUrl: 'https://api.example.com' }), null);
  assert.equal(profileConnectorArgs(null), null);
});

test('a listing is reused for a minute, and a failed one is asked again', async () => {
  clearCommandsCache();
  const key: CommandsKey = {
    args: { kind: 'claude-code', settings: {}, apiKey: '', request: false },
    cwd: '/p',
    host: null,
  };
  let asked = 0;
  const fetch = async () => {
    asked += 1;
    return COMMANDS;
  };
  assert.equal((await connectorCommands(key, fetch, 1000)).length, COMMANDS.length);
  await connectorCommands(key, fetch, 1000 + COMMANDS_FRESH_MS - 1);
  assert.equal(asked, 1);
  // Another project folder is another listing; a minute later, a new one.
  await connectorCommands({ ...key, cwd: '/q' }, fetch, 1000);
  await connectorCommands(key, fetch, 1000 + COMMANDS_FRESH_MS);
  assert.equal(asked, 3);

  clearCommandsCache();
  let fail = true;
  const flaky = async () => {
    if (fail) throw new Error('claude: not found');
    return COMMANDS;
  };
  await assert.rejects(connectorCommands(key, flaky, 0), /not found/);
  fail = false;
  assert.equal((await connectorCommands(key, flaky, 1)).length, COMMANDS.length);
});
