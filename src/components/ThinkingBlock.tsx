// ThinkingBlock.tsx
//
// A model's thinking in the chat, as a line like a tool call's: a reply's
// reasoning (MessageList), or a stretch of a connector's among its steps
// (ConnectorSteps).
import { createSignal, Show } from 'solid-js';
import { latestLine, thoughtLabel } from '../lib/thinking';

/**
 * The model's reasoning, as a line like a tool call's: "Thinking…" with its
 * latest line while it thinks, "Thought for 12 s" after; opens to the whole
 * text (plain, it can be long).
 */
export default function ThinkingBlock(props: {
  text: string;
  ms?: number | null;
  thinking: boolean;
}) {
  const [open, setOpen] = createSignal(false);
  return (
    <div class="tool-group" classList={{ open: open(), active: props.thinking }}>
      <div class="tool-group-header" onClick={() => setOpen((o) => !o)}>
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
          {thoughtLabel(props.thinking, props.ms)}
        </button>
        <Show when={props.thinking && !open()}>
          <span class="tool-group-items chat-thinking-preview">{latestLine(props.text)}</span>
        </Show>
        <Show when={props.thinking}>
          <span class="tool-call-spinner" aria-label="Thinking" />
        </Show>
      </div>
      <Show when={open()}>
        <div class="tool-group-calls">
          <div class="chat-thinking-text">{props.text}</div>
        </div>
      </Show>
    </div>
  );
}
