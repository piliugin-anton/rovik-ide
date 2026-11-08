import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import PlanImplementationAction from './PlanImplementationAction';
import { resolveProject } from '../lib/projects';
import { chatStore } from '../lib/chatStore';
import { onShowChat } from '../lib/agentRuns';

test('an editor opened before AI Chat reads the project status and keeps the linked chat active on mount', async () => {
  const calls: { cmd: string; args: any }[] = [];
  const dialog = {
    id: 'implemented-here',
    title: 'Contact form',
    createdAt: 1,
    updatedAt: 1,
    messageCount: 1,
  };
  (window as any).__TAURI_INTERNALS__ = {
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'chat_open_project')
        return {
          project: { id: 'project-one', name: 'Project', path: '/p', idFile: true },
          suggestion: null,
        };
      if (cmd === 'chat_list_projects') return [];
      if (cmd === 'chat_list_dialogs') return { items: [dialog], nextCursor: null };
      if (cmd === 'chat_plan_implementation')
        return { dialogId: dialog.id, messageId: 'request', runId: 'run', status: 'implemented' };
      if (cmd === 'chat_get_dialog') return dialog;
      if (cmd === 'chat_get_messages') return { messages: [], hasOlder: false, hasNewer: false };
      return null;
    },
  };
  assert.equal(chatStore.projectId(), null, 'the chat panel has never been mounted');
  await resolveProject('/p');
  const shown: unknown[] = [];
  const stop = onShowChat((target) => shown.push(target));
  const root = container();
  const dispose = render(
    () => (
      <PlanImplementationAction
        path="/p/.rovik/plans/contact.md"
        content={'# Contact form\n'}
        canImplement
      />
    ),
    root
  );
  try {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    assert.equal(
      calls.find((c) => c.cmd === 'chat_plan_implementation')?.args.projectId,
      'project-one'
    );
    assert.match(root.textContent ?? '', /Implemented in chat/);
    root.querySelector<HTMLButtonElement>('.link-button')!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    assert.equal(chatStore.projectId(), 'project-one');
    assert.equal(chatStore.state.activeId, dialog.id);
    // ChatPanel's mount takes this path: the existing active chat must survive it.
    await chatStore.switchProject('project-one');
    assert.equal(chatStore.state.activeId, dialog.id);
    assert.deepEqual(shown, [{ dialogId: dialog.id }]);
  } finally {
    dispose();
    stop();
  }
});
