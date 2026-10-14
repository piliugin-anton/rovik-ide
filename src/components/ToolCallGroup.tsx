// ToolCallGroup.tsx
//
// A tool call, or consecutive calls to one tool, as a single line like the
// model's thinking: "Read App.tsx", "Read App.tsx, main.tsx · 3 calls". The
// file names open in the editor; the line opens to each call's own block
// (a single call's already expanded). A call waiting for the user (an
// approval, a question) keeps the line open, so the request is never hidden.
import { createMemo, createSignal, For, Show, type JSX } from 'solid-js';
import type { ChatMessage, ChatToolCall } from '../types';
import { describeGroup, editCounts } from '../lib/toolGroups';
import { callStatus, parseArgsLenient } from '../lib/toolArgs';
import LineCounts from './LineCounts';
import { pendingApproval, pendingApprovalsOfTask } from '../lib/approvals';
import { childOf } from '../lib/subagents';
import { pendingQuestions } from '../lib/questions';
import { requestOpenFile } from '../lib/editorBridge';

export default function ToolCallGroup(props: {
  name: string;
  calls: ChatToolCall[];
  dialogId: string | null;
  /** A call's result (P5.8: by the call, as ids can repeat across replies). */
  result: (call: ChatToolCall) => ChatMessage | undefined;
  /** True while the agent loop is still running. */
  running: boolean;
  /** Earlier checklist snapshots: history, dimmed. */
  superseded?: boolean;
  /** One call's own block; expanded when it's the only one. */
  renderCall: (call: ChatToolCall, expanded: boolean) => JSX.Element;
}) {
  const [userOpen, setUserOpen] = createSignal(false);

  const states = createMemo(() =>
    props.calls.map((call) => callStatus(props.result(call), props.running))
  );
  const active = () => states().includes('running');
  const failed = () => states().filter((s) => s === 'error').length;
  const notRun = () => (awaiting() ? 0 : states().filter((s) => s === 'idle').length);
  const awaiting = () =>
    !!props.dialogId &&
    props.calls.some(
      (c) =>
        pendingApproval(props.dialogId!, c.id) ||
        pendingQuestions(props.dialogId!, c.id) ||
        // A subagent's approvals are its own chat's (P1.6).
        (c.name === 'task' &&
          !props.result(c) &&
          pendingApprovalsOfTask(props.dialogId!, c.id, childOf(props.dialogId, c.id)).length > 0)
    );
  const open = () => userOpen() || awaiting();
  const meta = () =>
    [
      props.calls.length > 1 ? `${props.calls.length} calls` : '',
      failed() ? `${failed()} failed` : '',
      // Never ran (the app closed while it waited, say).
      notRun() ? (props.calls.length > 1 ? `${notRun()} not run` : 'not run') : '',
    ]
      .filter(Boolean)
      .join(' · ');

  // Edits that went through, totalled like an edit's own header.
  const counts = createMemo(() => {
    if (props.name !== 'write') return undefined;
    const state = states();
    return editCounts(
      props.calls
        .filter(
          (c, i) => state[i] === 'done' && props.result(c)?.meta?.approval?.approved !== false
        )
        .map((c) => ({ args: parseArgsLenient(c.arguments), result: props.result(c)?.meta }))
    );
  });

  const label = createMemo(() =>
    describeGroup(
      props.name,
      props.calls.map((c) => parseArgsLenient(c.arguments)),
      active(),
      props.calls.map((c) => props.result(c)?.meta)
    )
  );

  const toggle = (e: MouseEvent) => {
    // A file name opens the file, not the group.
    if ((e.target as HTMLElement).closest('.tool-group-file')) return;
    setUserOpen(!open());
  };

  return (
    <div
      class="tool-group"
      classList={{ open: open(), active: active(), superseded: !!props.superseded }}
    >
      <div class="tool-group-header" onClick={toggle}>
        <button type="button" class="tool-group-toggle" aria-expanded={open()}>
          <svg
            class="chat-pasted-caret"
            classList={{ open: open() }}
            width="10"
            height="10"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M6 4l4 4-4 4" />
          </svg>
          {label().verb}
        </button>
        <span class="tool-group-items">
          <For each={label().items}>
            {(item, i) => (
              <>
                {i() > 0 ? ', ' : ''}
                <Show
                  when={item.path}
                  fallback={
                    <span class="tool-group-item" title={item.title}>
                      {item.text}
                    </span>
                  }
                >
                  {(path) => (
                    <button
                      type="button"
                      class="tool-group-file"
                      title={`${item.title ?? path()}: open in the editor`}
                      onClick={() =>
                        requestOpenFile(
                          // A written file: marked against git, centered on
                          // the first change unless the call named a line.
                          item.written
                            ? { path: path(), line: item.line, changes: true }
                            : { path: path(), line: item.line ?? 1 }
                        )
                      }
                    >
                      {item.text}
                    </button>
                  )}
                </Show>
              </>
            )}
          </For>
          <Show when={label().more > 0}>
            <span class="tool-group-item"> and {label().more} more</span>
          </Show>
        </span>
        <Show when={counts()}>{(c) => <LineCounts counts={c()} />}</Show>
        <Show when={meta()}>
          <span class="tool-group-meta" classList={{ error: failed() > 0 }}>
            {meta()}
          </span>
        </Show>
        <Show when={active() && !awaiting()}>
          <span class="tool-call-spinner" aria-label="Running" />
        </Show>
      </div>
      <Show when={open()}>
        <div class="tool-group-calls">
          <For each={props.calls}>{(call) => props.renderCall(call, props.calls.length === 1)}</For>
        </div>
      </Show>
    </div>
  );
}
