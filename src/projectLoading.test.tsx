import { container } from './test/dom';
import { flush, installTauri } from './test/chatPanel';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import App from './App';

function deferred() {
  let resolve!: (value: unknown) => void;
  const promise = new Promise<unknown>((done) => (resolve = done));
  return { promise, resolve };
}

test('project loading covers the window but chat loading stays inside the AI Chat tab', async (t) => {
  for (const name of ['offsetHeight', 'clientHeight', 'offsetWidth', 'clientWidth'] as const) {
    t.mock.getter(HTMLElement.prototype, name, () => 400);
  }
  installTauri([]);
  const tauri = (window as any).__TAURI_INTERNALS__;
  const fallback = tauri.invoke;
  let settingsPause: ReturnType<typeof deferred> | undefined;
  let historyReads = 0;
  const identity = deferred();
  const folders = deferred();
  const chats = deferred();
  const messages = deferred();
  const project = (id: string) => ({ id, path: `/${id}`, name: id, chatCount: 1, lastOpenedAt: 1 });
  tauri.metadata = { currentWindow: { label: 'main' } };
  tauri.unregisterCallback = () => {};
  tauri.invoke = async (cmd: string, args: any) => {
    switch (cmd) {
      case 'load_ai_settings':
        if (settingsPause) return settingsPause.promise;
        break;
      case 'vfs_status':
      case 'sandbox_status':
      case 'browser_status':
        throw new Error('Not supported in this test');
      case 'shared_state_read':
      case 'shared_state_start':
      case 'shared_state_merge':
        return {};
      case 'hosts_list':
        return { hosts: [] };
      case 'load_last_project':
        return '{}';
      case 'startup_info':
        return { folder: '/old', newWindow: false };
      case 'project_check':
      case 'project_claim':
        return { claimed: true, openElsewhere: false };
      case 'chat_open_project':
        if (args.root === '/next') return identity.promise;
        if (args.root === '/failed') throw new Error('Project unavailable');
        return { project: project(args.root.slice(1)), suggestion: null };
      case 'chat_list_projects':
        return ['old', 'next', 'failed', 'history-error'].map(project);
      case 'read_dir_entries':
        return args.path === '/next' ? folders.promise : [];
      case 'get_path_id':
        return args.path;
      case 'chat_list_dialogs':
        historyReads++;
        if (args.projectId === 'next') return chats.promise;
        if (args.projectId === 'history-error') throw new Error('Chats unavailable');
        break;
      case 'chat_get_messages':
        historyReads++;
        return messages.promise;
      case 'read_editorconfigs':
      case 'hosts_summaries':
        return [];
      case 'cloud_forwarded_ports':
        return {};
      case 'update_info':
        return { version: '0.1.0', installable: false, periodic: false };
    }
    return fallback(cmd, args);
  };
  const root = container();
  const dispose = render(() => <App />, root);
  const overlay = () => root.querySelector('.project-loading-overlay');
  const open = async (id: string) => {
    root.querySelector<HTMLButtonElement>('.project-switcher-trigger')!.click();
    await flush();
    root.querySelector<HTMLButtonElement>(`.project-row-main[title="/${id}"]`)!.click();
    await flush();
  };
  try {
    await flush();
    assert.equal(overlay(), null, 'the first project is ready');
    [...root.querySelectorAll<HTMLButtonElement>('.sidebar-tabs button')]
      .find((button) => button.textContent?.includes('AI Chat'))!
      .click();
    await flush();
    assert.equal(overlay(), null, 'the original chat panel has finished loading');

    await open('next');
    assert.equal(overlay()?.textContent, 'Loading...');
    assert.equal(overlay()?.getAttribute('role'), 'status');
    assert.ok(overlay()?.querySelector('.sidebar-tab-spinner'));
    identity.resolve({ project: project('next'), suggestion: null });
    await flush();
    assert.ok(overlay(), 'the project identity alone is not enough');
    folders.resolve([]);
    await flush();
    assert.equal(overlay(), null, 'chat loading does not cover the window');
    assert.equal(root.querySelector('.chat-loading')?.textContent, 'Loading...');
    assert.ok(root.querySelector('.chat-loading .sidebar-tab-spinner'));
    assert.ok(root.querySelector('.sidebar-tabs [aria-label="Loading chats"]'));
    assert.ok(root.querySelector('.chat-panel.loading-history'), 'only the loader is shown');
    chats.resolve({
      items: [
        { id: 'saved-chat', title: 'Existing chat', createdAt: 1, updatedAt: 1, messageCount: 1 },
      ],
      nextCursor: null,
    });
    await flush();
    assert.equal(overlay(), null);
    assert.ok(root.querySelector('.chat-loading'), 'the selected transcript is still loading');
    messages.resolve({
      messages: [
        {
          id: 'saved-message',
          dialogId: 'saved-chat',
          seq: 1,
          role: 'assistant',
          content: 'Cached conversation',
          attachments: [],
          createdAt: 1,
          finalized: true,
        },
      ],
      hasOlder: false,
      hasNewer: false,
    });
    await flush();
    assert.equal(overlay(), null);
    assert.equal(root.querySelector('.chat-loading'), null);
    assert.equal(root.querySelector('.chat-panel.loading-history'), null);
    assert.equal(root.querySelector('.sidebar-tabs [aria-label="Loading chats"]'), null);

    await t.test(
      'returning to cached chats shows them immediately while settings initialize',
      async () => {
        const selectTab = (label: string) =>
          [...root.querySelectorAll<HTMLButtonElement>('.sidebar-tabs button')]
            .find((button) => button.textContent?.includes(label))!
            .click();
        selectTab('Files');
        await flush();
        const readsBefore = historyReads;
        settingsPause = deferred();
        selectTab('AI Chat');
        assert.equal(root.querySelector('.chat-loading'), null, 'no initial loading flash');
        await flush();
        assert.equal(root.querySelector('.chat-loading'), null, 'settings are still pending');
        assert.equal(root.querySelector('.chat-panel.loading-history'), null);
        assert.equal(root.querySelector('.sidebar-tabs [aria-label="Loading chats"]'), null);
        assert.equal(overlay(), null);
        assert.match(root.textContent ?? '', /Cached conversation/);
        assert.equal(root.querySelector('.chat-title')?.textContent, 'Existing chat');
        assert.equal(historyReads, readsBefore, 'history stays cached');
        settingsPause.resolve(await fallback('load_ai_settings', {}));
        settingsPause = undefined;
        await flush();
        assert.equal(historyReads, readsBefore, 'initialization does not refetch history');
        assert.equal(root.querySelector('.chat-loading'), null);
      }
    );

    await open('history-error');
    assert.equal(overlay(), null, 'a chat load failure releases the overlay');
    assert.match(root.textContent ?? '', /Chat history couldn't be loaded/);
    await open('failed');
    assert.equal(overlay(), null, 'a project load failure releases the overlay');
    assert.match(root.textContent ?? '', /Project unavailable/);
  } finally {
    dispose();
    root.remove();
  }
});
