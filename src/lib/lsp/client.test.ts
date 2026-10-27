// client.ts tests (node:test): the workspace a server is given — a
// project's one folder, or a workspace's several (C07) — in `initialize`,
// and in the answer to `workspace/workspaceFolders` later. The app is
// answered here, as terminalPanel.test.tsx does; Monaco is a stub whose
// URIs are their text, so a URI is compared with the one sent before.
import '../../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { LspClient } from './client';

/** Every message the client sent, in order. */
let sent: any[] = [];
/** The id of the client's event channel, and the callbacks made for it. */
let channelId = 0;
const callbacks = new Map<number, (event: unknown) => void>();
let nextCallback = 1;
/** The next index a message to the channel must carry (Channel's order). */
let messageIndex = 0;

(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = nextCallback++;
    callbacks.set(id, callback);
    return id;
  },
  unregisterCallback: (id: number) => callbacks.delete(id),
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'lsp_start') {
      channelId = args.onEvent.id;
      return 's1';
    }
    if (cmd === 'lsp_send') {
      const message = JSON.parse(args.message);
      sent.push(message);
      // The server answers each request at once: initialize's capabilities,
      // and nothing else.
      if (message.id !== undefined) {
        reply({
          jsonrpc: '2.0',
          id: message.id,
          result: message.method === 'initialize' ? { capabilities: {} } : null,
        });
      }
      return null;
    }
    if (cmd === 'lsp_stop') return null;
    throw new Error(`unexpected ${cmd}`);
  },
};

const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** The server sends `message` to the client (its index in the order sent). */
function reply(message: unknown) {
  callbacks.get(channelId)!({
    index: messageIndex++,
    message: { type: 'message', data: JSON.stringify(message) },
  });
}

async function started(folders: string[]) {
  sent = [];
  messageIndex = 0;
  const client = new LspClient({
    name: 'test server',
    command: 'rust-analyzer',
    folders,
    languageId: 'rust',
    extensions: ['.rs'],
  });
  const opening = client.start();
  await flush();
  return { client, opening };
}

const initializeOf = () => sent.find((m) => m.method === 'initialize');

/** The client's answers to the server's requests (the messages without a method). */
const answers = () => sent.filter((m) => m.method === undefined);

test('initialize lists both folders, the first one as the workspace root', async () => {
  const { client, opening } = await started(['/w/api', '/w/web']);
  await opening;
  const init = initializeOf();
  assert.ok(init, 'the client initialized');
  assert.deepEqual(
    init.params.workspaceFolders.map((folder: { name: string }) => folder.name),
    ['api', 'web']
  );
  assert.equal(init.params.rootPath, '/w/api');
  // A URI is its text here (the stub), so it's the first folder's own.
  assert.equal(init.params.rootUri, init.params.workspaceFolders[0].uri);
  await client.stop();
});

test('workspace/workspaceFolders is answered with the folders sent in initialize', async () => {
  const { client, opening } = await started(['/w/api', '/w/web']);
  await opening;
  const listed = initializeOf().params.workspaceFolders;

  // The server asks again later (a reload): the same list comes back.
  reply({ jsonrpc: '2.0', id: 'ask-1', method: 'workspace/workspaceFolders' });
  await flush();
  assert.deepEqual(
    answers().find((m) => m.id === 'ask-1')?.result,
    listed,
    'the answer is the list initialize was given'
  );
  await client.stop();
});
