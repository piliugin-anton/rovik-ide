// ToolCallGroup.tsx
//
// Consecutive calls to one tool as a single line, like the model's thinking:
// "Read App.tsx, main.tsx · 3 calls". The file names open in the editor; the
// line opens to each call's own block. A call waiting for the user's
// approval keeps the group open, so its request is never hidden.
import { createMemo, createSignal, For, Show, type JSX } from 'solid-js';
import type { ChatMessage, ChatToolCall } from '../types';
import { describeGroup } from '../lib/toolGroups';
import { isErrorResult, parseArgsLenient } from '../lib/toolArgs';
import { pendingApproval } from '../lib/approvals';
import { requestOpenFile } from '../lib/editorBridge';

export default function ToolCallGroup(props: {
  name: string;
  calls: ChatToolCall[];
  dialogId: string | null;
  result: (id: string) => ChatMessage | undefined;
  /** True while the agent loop is still running. */
  running: boolean;
  /** One call's own block, as it renders outside a group. */
  renderCall: (call: ChatToolCall) => JSX.Element;
}) {
  const [userOpen, setUserOpen] = createSignal(false);

  const states = createMemo(() =>
    props.calls.map((call) => {
      const result = props.result(call.id);
      if (!result) return props.running ? 'running' : 'idle';
      return isErrorResult(result.content) ? 'error' : 'done';
    })
  );
  const active = () => states().includes('running');
  const failed = () => states().filter((s) => s === 'error').length;
  const awaiting = () =>
    !!props.dialogId && props.calls.some((c) => pendingApproval(props.dialogId!, c.id));
  const open = () => userOpen() || awaiting();

  const label = createMemo(() =>
    describeGroup(
      props.name,
      props.calls.map((c) => parseArgsLenient(c.arguments)),
      active()
    )
  );

  const toggle = (e: MouseEvent) => {
    // A file name opens the file, not the group.
    if ((e.target as HTMLElement).closest('.tool-group-file')) return;
    setUserOpen(!open());
  };

  return (
    <div class="tool-group" classList={{ open: open(), active: active() }}>
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
                      onClick={() => requestOpenFile({ path: path(), line: item.line ?? 1 })}
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
        <span class="tool-group-meta" classList={{ error: failed() > 0 }}>
          {props.calls.length} calls{failed() > 0 ? ` · ${failed()} failed` : ''}
        </span>
        <Show when={active() && !awaiting()}>
          <span class="tool-call-spinner" aria-label="Running" />
        </Show>
      </div>
      <Show when={open()}>
        <div class="tool-group-calls">
          <For each={props.calls}>{(call) => props.renderCall(call)}</For>
        </div>
      </Show>
    </div>
  );
}
