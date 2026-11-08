import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import PlanImplementationAction from './PlanImplementationAction';
import { chatStore } from '../lib/chatStore';
import { onShowChat } from '../lib/agentRuns';

test('a mounted chat panel receives navigation before the active chat changes so it can save its draft', async () => {
  const calls: string[] = [];
  (window as any).__TAURI_INTERNALS__ = {
    invoke: async (cmd: string) => {
      calls.push(cmd);
      if (cmd === 'chat_plan_implementation')
        return {
          dialogId: 'implemented-here',
          messageId: 'request',
          runId: 'run',
          status: 'implemented',
        };
      if (cmd === 'chat_get_dialog')
        return {
          id: 'implemented-here',
          title: 'Contact form',
          createdAt: 1,
          updatedAt: 1,
          messageCount: 1,
        };
      return null;
    },
  };
  const previous = chatStore.createDialog('Has an unsent draft');
  chatStore.setChatVisible(true);
  const shown: { target: unknown; previous: string | null }[] = [];
  const stop = onShowChat((target) => shown.push({ target, previous: chatStore.state.activeId }));
  const root = container();
  const dispose = render(
    () => (
      <PlanImplementationAction path="/p/.rovik/plans/contact.md" content={'# Contact form\n'} />
    ),
    root
  );
  try {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    root.querySelector<HTMLButtonElement>('.link-button')!.click();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(shown, [{ target: { dialogId: 'implemented-here' }, previous: previous.id }]);
    assert.equal(chatStore.state.activeId, previous.id, 'the panel owns the draft-aware switch');
    assert.ok(!calls.includes('chat_get_messages'), 'not opened behind the panel’s back');
  } finally {
    dispose();
    stop();
    chatStore.setChatVisible(false);
  }
});
