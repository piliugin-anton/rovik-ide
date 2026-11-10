import { test } from 'node:test';
import assert from 'node:assert/strict';
import { pageCallbacks, type PageRequest } from './pageCallbacks';
import type { EventScope } from './eventReplay';

const page: PageRequest = {
  dialogId: 'child',
  leadId: 'lead',
  projectId: 'one',
  address: 'http://localhost:3000/',
  inTab: true,
  ports: [{ port: 3000, localPort: 3000, tabPort: 4000 }],
};
function fixture() {
  let scope: EventScope = { projectId: 'one', chats: [] };
  const pending: { handler(page: PageRequest): void; resolve(id: string): void }[] = [];
  const released: string[] = [];
  const actions: unknown[] = [];
  const callbacks = pageCallbacks({
    scope: () => scope,
    subscribe: (_, handler) => new Promise<string>((resolve) => pending.push({ handler, resolve })),
    unsubscribe: async (id) => {
      released.push(id);
    },
    applyPorts: (request) => actions.push(request.ports),
    openTab: (address) => actions.push(address),
    openExternal: async () => {
      throw new Error('OS opener failed');
    },
    failed: (error) => actions.push(String(error)),
  });
  return {
    callbacks,
    pending,
    released,
    actions,
    switchProject: () => {
      scope = { projectId: 'two', chats: [] };
    },
  };
}
const settle = () => new Promise((resolve) => setImmediate(resolve));

test('a private page applies its ports before opening and reports external opener failure', async () => {
  const f = fixture();
  f.callbacks.refresh();
  f.pending[0].handler(page);
  assert.deepEqual(f.actions, [page.ports, page.address]);
  f.pending[0].handler({ ...page, inTab: false });
  await settle();
  assert.equal(f.actions[f.actions.length - 1], 'Error: OS opener failed');
  f.callbacks.stop();
});

test('project changes reject queued pages before the replacement subscription starts', async () => {
  const f = fixture();
  f.callbacks.refresh();
  f.switchProject();
  f.pending[0].handler(page);
  f.callbacks.refresh();
  f.pending[0].resolve('old');
  f.pending[1].resolve('new');
  await settle();
  assert.deepEqual(f.actions, []);
  assert.deepEqual(f.released, ['old']);
  f.callbacks.stop();
  f.pending[1].handler(page);
  assert.deepEqual(f.released, ['old', 'new']);
  assert.deepEqual(f.actions, []);
});

test('unmount releases a subscription that finishes after disposal without replaying pages', async () => {
  const f = fixture();
  f.callbacks.refresh();
  f.callbacks.stop();
  f.pending[0].resolve('late');
  f.pending[0].handler(page);
  await settle();
  assert.deepEqual(f.released, ['late']);
  assert.deepEqual(f.actions, []);
});
