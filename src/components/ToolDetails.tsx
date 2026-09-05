// ToolDetails.tsx
import { createSignal, Show } from 'solid-js';
import type { ChatToolCall } from '../types';

export type ToolDetailsVariant = 'call' | 'result';

export default function ToolDetails(props: {
  variant: ToolDetailsVariant;
  toolCall?: ChatToolCall; // for variant === 'call'
  content?: string; // for variant === 'result'
}) {
  const [expanded, setExpanded] = createSignal(false);

  const formattedContent = () => {
    if (props.variant === 'call' && props.toolCall) {
      try {
        return JSON.stringify(JSON.parse(props.toolCall.arguments), null, 2);
      } catch {
        return props.toolCall.arguments;
      }
    }

    // variant === 'result'
    return props.content ?? '';
  };

  const summary = () => {
    if (props.variant === 'call' && props.toolCall) {
      const args = props.toolCall.arguments.trim();

      if (!args) {
        return `Tool: ${props.toolCall.name}`;
      }

      try {
        const parsed = JSON.parse(args);

        if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
          const keys = Object.keys(parsed);
          if (keys.length > 0) {
            return `Tool call: ${props.toolCall.name} · ${keys.join(', ')}`;
          }
        }
      } catch {
        // Streaming / invalid JSON
      }

      return `Tool call: ${props.toolCall.name}`;
    }

    // variant === 'result'
    const content = (props.content ?? '').trim();

    if (!content) {
      return 'Tool result · empty';
    }

    const firstLine = content.split(/\r?\n/, 1)[0].trim();

    if (firstLine.length <= 120) {
      return `Tool result · ${firstLine}`;
    }

    return `Tool result · ${firstLine.slice(0, 117)}…`;
  };

  return (
    <div class="tool-details" classList={{ expanded: expanded() }}>
      <button
        type="button"
        class="tool-summary"
        onClick={() => setExpanded((value) => !value)}
        aria-expanded={expanded()}
      >
        <span class="tool-summary-icon">{expanded() ? '⌄' : '›'}</span>

        <span class="tool-summary-text" title={summary()}>
          {summary()}
        </span>
      </button>

      <Show when={expanded()}>
        <pre class="tool-args">{formattedContent()}</pre>
      </Show>
    </div>
  );
}
