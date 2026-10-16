// Component test: a remote send's upload shows above the input while its
// files go up (U04, 6.11), from `cloud-event`. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import UploadProgress from './UploadProgress';
import { clearUpload, initCloud } from '../lib/cloud';

const callbacks = new Map<number, (event: unknown) => void>();
const listeners: { event: string; handler: number }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  transformCallback: (callback: (event: unknown) => void) => {
    const id = callbacks.size + 1;
    callbacks.set(id, callback);
    return id;
  },
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'plugin:event|listen') {
      listeners.push({ event: args.event, handler: args.handler });
      return args.handler;
    }
    return cmd === 'hosts_summaries' ? [] : null;
  },
};
(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };

function emit(payload: unknown) {
  for (const l of listeners.filter((l) => l.event === 'cloud-event')) {
    callbacks.get(l.handler)!({ event: 'cloud-event', id: 1, payload });
  }
}

const MB = 1024 * 1024;
const uploading = (dialogId: string, sent: number) => ({
  type: 'uploading',
  dialogId,
  messageId: 'm1',
  sent,
  total: 10 * MB,
});

test('a remote send shows its upload’s progress until its files are up', async () => {
  await initCloud();
  const root = container();
  const dispose = render(() => <UploadProgress dialogId="c1" />, root);
  assert.equal(root.querySelector('.chat-upload-progress'), null, 'nothing before');
  emit(uploading('c1', 3 * MB));
  emit(uploading('other', 7 * MB));
  const text = () => root.querySelector('.chat-upload-text')?.textContent;
  assert.equal(text(), 'Sending files to the server… 3 MB of 10 MB');
  const bar = root.querySelector('.chat-upload-bar > span') as HTMLElement;
  assert.equal(bar.style.width, '30%');
  emit(uploading('c1', 6.5 * MB));
  assert.equal(text(), 'Sending files to the server… 6.5 MB of 10 MB');
  emit(uploading('c1', 10 * MB));
  assert.equal(root.querySelector('.chat-upload-progress'), null, 'gone once they’re up');
  // A send that ends without its upload (Stop, a failure) clears it too.
  emit(uploading('c1', 1 * MB));
  clearUpload('c1');
  assert.equal(root.querySelector('.chat-upload-progress'), null);
  dispose();
});
