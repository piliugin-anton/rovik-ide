import { container } from './test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import App from './App';
import { reconnectBackend } from './lib/backendSession';
import { chatStore } from './lib/chatStore';
import { openInEditor } from './lib/openInEditor';

test('startup keeps the splash until the project and expanded folders have loaded', async (t) => {
  // happy-dom has no layout; give the virtual file tree a visible viewport.
  for (const name of ['offsetHeight', 'clientHeight', 'offsetWidth', 'clientWidth'] as const) {
    t.mock.getter(HTMLElement.prototype, name, () => 400);
  }
  let openProject!: (value: unknown) => void;
  let readFolder!: (value: unknown) => void;
  const project = new Promise<unknown>((resolve) => (openProject = resolve));
  const folder = new Promise<unknown>((resolve) => (readFolder = resolve));
  const commands: string[] = [];
  let recoveryProject: unknown;
  let contextRequest: unknown;
  const fileReads: string[] = [];
  (window as any).__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' } },
    transformCallback: () => 0,
    unregisterCallback: () => {},
    invoke: async (cmd: string, args: any) => {
      commands.push(cmd);
      switch (cmd) {
        case 'chat_get_dialog':
          return { id: args.dialogId };
        case 'clm_status':
          return {
            realPath:
              args.dialogId === 'mounted-chat'
                ? '/virtual/context/mounted-chat/LIVE_CONTEXT.md'
                : null,
          };
        case 'read_editorconfigs':
          return [];
        case 'path_exists':
          return true;
        case 'read_file_content':
          fileReads.push(args.path);
          return '# Mounted task';
        case 'clm_read_file':
          contextRequest = args;
          return '# Task for the source chat';
        case 'desktop_reconnect':
          recoveryProject = args;
          throw new Error('native reconnect boundary');
        case 'plugin:event|listen':
          return 0;
        case 'plugin:event|unlisten':
        case 'save_last_project':
        case 'start_file_watch':
          return;
        case 'startup_info':
          return { folder: null, newWindow: false };
        case 'load_last_project':
          return JSON.stringify({
            path: '/p',
            openedFiles: [],
            expandedPaths: ['/p/src'],
            terminalCollapsed: true,
          });
        case 'load_ai_settings':
          return JSON.stringify({ providers: [] });
        case 'project_claim':
          return { claimed: true, openElsewhere: false };
        case 'chat_open_project':
          return project;
        case 'chat_list_projects':
        case 'hosts_summaries':
          return [];
        case 'cloud_forwarded_ports':
          return {};
        case 'agent_status':
          return { runs: [], approvals: [], questions: [] };
        case 'get_path_id':
          return args.path;
        case 'read_dir_entries':
          if (args.path === '/p/src') return folder;
          return [{ id: 'src', path: '/p/src', name: 'src', is_dir: true, ignored: false }];
        case 'update_info':
          return { version: '0.1.0', installable: false, periodic: false };
        default:
          throw new Error(`no app: ${cmd}`);
      }
    },
  };
  (window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  const splash = document.createElement('div');
  splash.id = 'boot-splash';
  document.body.appendChild(splash);
  const root = container();
  const dispose = render(() => <App />, root);
  const flush = async () => {
    for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
  };
  try {
    await flush();
    assert.ok(commands.includes('chat_open_project'));
    assert.ok(root.querySelector('[data-file-id="src"]'), 'the file list is already visible');
    assert.equal(splash.classList.contains('done'), false, 'project identity is still loading');
    assert.equal(root.querySelector('.project-switcher-name')?.textContent, 'Loading…');

    openProject({ project: { id: 'p', path: '/p', name: 'Demo' }, suggestion: null });
    await flush();
    assert.equal(splash.classList.contains('done'), false, 'an expanded folder is still loading');
    readFolder([]);
    await flush();
    assert.equal(splash.classList.contains('done'), true);
    assert.equal(root.querySelector('.project-switcher-name')?.textContent, 'Demo');
    await t.test('a TASK.md link opens from its source chat without a mounted volume', async () => {
      await openInEditor({ path: '@context/TASK.md', dialogId: 'source-chat', focus: true });
      await flush();
      assert.deepEqual(contextRequest, { dialogId: 'source-chat', path: '@context/TASK.md' });
      assert.ok(root.textContent?.includes('TASK.md'));
    });
    await t.test('a local mounted task card still opens as a normal file', async () => {
      const previous = contextRequest;
      await openInEditor({
        path: '@context/TASK.md',
        dialogId: 'mounted-chat',
        marks: 'none',
        activate: false,
      });
      assert.deepEqual(fileReads, ['/virtual/context/mounted-chat/TASK.md']);
      assert.equal(contextRequest, previous, 'no read-only snapshot was requested');
    });
    await t.test('recovery uses the open folder before the chat panel has mounted', async () => {
      assert.equal(chatStore.projectId(), null, 'no chat project was selected');
      // Stop at the native boundary: this regression verifies the UI's identity;
      // the platform acceptance tests exercise the real replacement and restore.
      await assert.rejects(reconnectBackend(), /native reconnect boundary/);
      assert.deepEqual(recoveryProject, { root: '/p', projectId: 'p' });
    });
  } finally {
    dispose();
    root.remove();
    splash.remove();
  }
});
