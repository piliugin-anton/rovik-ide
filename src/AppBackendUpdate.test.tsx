import { container } from './test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import App from './App';
import { BACKEND_UPDATE_NOTE } from './lib/backendUpdates';
import { notifications } from './lib/notifications';

test('an incompatible startup backend can restart and restore the saved project and its history', async (t) => {
  for (const name of ['offsetHeight', 'clientHeight', 'offsetWidth', 'clientWidth'] as const)
    t.mock.getter(HTMLElement.prototype, name, () => 400);
  let updated = false;
  const reconnects: unknown[] = [];
  const restarts: unknown[] = [];
  const historyProjects: unknown[] = [];
  let historyReads = 0;
  const startupErrors: unknown[][] = [];
  t.mock.method(console, 'error', (...args: unknown[]) => startupErrors.push(args));
  const project = { id: 'saved-project', path: '/saved', name: 'Saved project' };
  const status = {
    restartRequired: true,
    backendBoot: 'old',
    backendVersion: '0.2.16',
    backendProtocol: 1,
    backendBuild: 'old-build',
    appVersion: '0.2.17',
    appProtocol: 2,
    appBuild: 'new-build',
  };
  (window as any).__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' } },
    transformCallback: () => 0,
    unregisterCallback: () => {},
    invoke: async (command: string, args: any) => {
      switch (command) {
        case 'startup_info':
          return { folder: null, newWindow: false };
        case 'desktop_backend_update_status':
          return { ...status, restartRequired: !updated };
        case 'desktop_restart_backend':
          restarts.push(args);
          updated = true;
          return;
        case 'desktop_reconnect':
          reconnects.push(args);
          return;
        case 'project_claim':
          return { claimed: true, openElsewhere: false };
        case 'plugin:event|listen':
          return 0;
        case 'plugin:event|unlisten':
          return;
        case 'update_info':
          return { version: status.appVersion, installable: false, periodic: false };
      }
      if (command === 'load_last_project') historyReads++;
      if (!updated) throw new Error('The running backend uses an incompatible protocol');
      switch (command) {
        case 'load_last_project':
          return JSON.stringify({
            path: '/saved',
            openedFiles: [],
            expandedPaths: [],
            terminalCollapsed: true,
          });
        case 'chat_open_project':
          assert.equal(args.root, '/saved');
          return { project, suggestion: null };
        case 'chat_list_projects':
          return [project];
        case 'chat_list_dialogs':
          historyProjects.push(args.projectId);
          return {
            items: [
              {
                id: 'history',
                title: 'Saved conversation',
                createdAt: 1,
                updatedAt: 1,
                messageCount: 2,
              },
            ],
            nextCursor: null,
          };
        case 'chat_get_dialog':
          return { id: 'history', title: 'Saved conversation', projectId: project.id };
        case 'chat_get_messages':
          return { messages: [], hasOlder: false };
        case 'chat_task_stages':
        case 'chat_get_composer_draft':
          return null;
        case 'clm_status':
          return { realPath: null };
        case 'chat_checklist':
        case 'chat_unsettled_sends':
        case 'background_tasks':
        case 'read_dir_entries':
        case 'read_editorconfigs':
        case 'hosts_summaries':
          return [];
        case 'get_path_id':
          return args.path;
        case 'desktop_event_status':
        case 'agent_status':
          return { runs: [], approvals: [], questions: [] };
        case 'cloud_forwarded_ports':
          return {};
        case 'desktop_session_usable':
          return true;
        case 'desktop_poll_events':
          return new Promise(() => {});
        case 'desktop_pages_subscribe':
          return 'pages';
        case 'load_ai_settings':
          return JSON.stringify({ providers: [] });
        case 'vfs_status':
          return { driver: { installed: false, canInstall: false }, mountpoint: null };
        case 'start_file_watch':
        case 'save_last_project':
        case 'desktop_pages_unsubscribe':
        case 'workload_focus':
        case 'agent_configure':
          return;
        default:
          throw new Error(`no app: ${command}`);
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
    for (let i = 0; i < 8; i++) await new Promise((resolve) => setTimeout(resolve, 0));
  };
  try {
    await flush();
    assert.equal(historyReads, 1, 'startup attempted the unavailable saved-project read');
    assert.equal(splash.classList.contains('done'), true, 'the restart action is reachable');
    const button = Array.from(
      root.querySelectorAll<HTMLButtonElement>('.notification-action')
    ).find((item) => item.textContent === 'Restart backend');
    assert.ok(button);
    button.click();
    await flush();
    assert.equal(restarts.length, 1);
    assert.deepEqual(restarts[0], {
      expectedBoot: 'old',
      expectedVersion: '0.2.16',
      expectedProtocol: 1,
      expectedBuild: 'old-build',
      root: null,
      projectId: null,
    });
    assert.equal(historyReads, 2, 'startup history is retried on the replacement backend');
    assert.deepEqual(reconnects, [{ root: '/saved', projectId: 'saved-project' }]);
    assert.equal(root.querySelector('.project-switcher-name')?.textContent, 'Saved project');
    assert.equal(notifications().find((note) => note.id === BACKEND_UPDATE_NOTE)?.kind, 'success');
    const chat = Array.from(root.querySelectorAll<HTMLButtonElement>('.sidebar-tabs button')).find(
      (item) => item.textContent?.includes('AI Chat')
    );
    assert.ok(chat);
    chat.click();
    await flush();
    assert.ok(historyProjects.includes('saved-project'));
    assert.ok(root.textContent?.includes('Saved conversation'));
    assert.ok(startupErrors.length > 0);
    assert.ok(
      startupErrors.every((error) => String(error).includes('incompatible protocol')),
      startupErrors.map(String).join('\n')
    );
  } finally {
    dispose();
    root.remove();
    splash.remove();
  }
});
