import { createResource, createSignal, onCleanup, Show } from 'solid-js';
import { chatApi } from '../lib/chatApi';
import { chatStore } from '../lib/chatStore';
import { activeRunId, isRunning, showChat } from '../lib/agentRuns';
import { notify } from '../lib/notifications';
import { readPlan } from '../lib/plans';
import { onFileWritten } from '../lib/approvals';
import { currentProject } from '../lib/projects';
import {
  getPlanImplementation,
  planImplementationRevision,
  type PlanImplementation,
} from '../lib/planImplementations';

/** A saved plan's implementation state, with navigation to its owning chat. */
export default function PlanImplementationAction(props: {
  path: string;
  dialogId?: string | null;
  /** Editor text, including unsaved edits; otherwise read the saved plan. */
  content?: string;
  canImplement?: boolean;
  onImplement?: (requireVerified?: boolean) => void;
}) {
  const [requireVerified, setRequireVerified] = createSignal(false);
  const [implementation, { refetch }] = createResource(
    () => ({
      path: props.path,
      content: props.content,
      dialogId: props.dialogId,
      revision: planImplementationRevision(),
      projectId:
        currentProject() === undefined ? chatStore.projectId() : (currentProject()?.id ?? null),
    }),
    async ({ path, content, dialogId, projectId }) => {
      const text = content ?? (dialogId ? await readPlan(dialogId, path) : undefined);
      if (text === undefined) return null;
      return getPlanImplementation(projectId, path, text);
    }
  );
  onCleanup(
    onFileWritten((path) => {
      if (path === props.path) void refetch();
    })
  );
  const status = () => {
    const record = implementation.error ? undefined : implementation();
    if (
      record?.status === 'implementing' &&
      (!isRunning(record.dialogId) ||
        !record.runId ||
        activeRunId(record.dialogId) !== record.runId)
    )
      return 'interrupted';
    return record?.status;
  };
  const label = () => {
    switch (status()) {
      case 'implemented':
        return 'Implemented in';
      case 'implementing':
        return 'Implementing in';
      case 'review-needed':
        return 'Implementation finished; review needed in';
      default:
        return 'Implementation interrupted in';
    }
  };

  async function openChat(record: PlanImplementation) {
    try {
      const dialog = await chatApi.getDialog(record.dialogId);
      if (!dialog) throw new Error('The implementation chat has been deleted.');
      if (chatStore.state.chatVisible) {
        // The mounted panel saves/restores composer drafts when selecting a chat.
        showChat({ dialogId: record.dialogId });
        return;
      }
      const project = currentProject();
      if (project !== undefined && chatStore.projectId() !== (project?.id ?? null)) {
        await chatStore.switchProject(project?.id ?? null);
      }
      // Set the active chat before showing the tab: its panel may not be mounted yet.
      await chatStore.openChat(dialog);
      showChat({ dialogId: record.dialogId });
    } catch (err) {
      notify({
        kind: 'error',
        title: 'The implementation chat couldn’t be opened',
        message: String(err),
      });
    }
  }

  return (
    <div class="chat-plan-actions">
      <Show
        when={!implementation.error}
        fallback={
          <span class="chat-plan-hint">
            Implementation status unavailable.{' '}
            <button type="button" class="link-button" onClick={() => void refetch()}>
              Retry
            </button>
          </span>
        }
      >
        <Show when={implementation()}>
          {(record) => (
            <span class="chat-plan-hint">
              {label()}{' '}
              <button
                type="button"
                class="link-button"
                disabled={implementation.loading}
                onClick={() => void openChat(record())}
              >
                chat
              </button>
            </span>
          )}
        </Show>
      </Show>
      <Show when={props.canImplement && status() !== 'implemented' && status() !== 'implementing'}>
        <label class="settings-hint">
          <input
            type="checkbox"
            checked={requireVerified()}
            onChange={(event) => setRequireVerified(event.currentTarget.checked)}
          />{' '}
          Require verified result
        </label>
        <button
          type="button"
          class="chat-action-button primary"
          disabled={implementation.loading}
          onClick={() => props.onImplement?.(requireVerified())}
          title={
            requireVerified()
              ? 'Review the exact plan and acceptance criteria before verified implementation'
              : 'Switch to Agent mode and carry out this plan'
          }
        >
          Implement this plan
        </button>
      </Show>
    </div>
  );
}
