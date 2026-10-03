// ContextMeter.tsx
//
// "Context used: N%" above the chat input, with a ring showing the same.
// N is how much of the model's context window the chat's next request
// would take: the history as it would be sent (after compaction and
// trimming), the system prompt, tool schemas and project context. It's an
// estimate from the backend (agent.rs, chat_context_estimate), calibrated
// per model from the token counts providers report.
import { Show } from 'solid-js';

export interface ContextUsage {
  /** The whole next request, estimated. */
  promptTokens: number;
  /** The model's context window. */
  windowTokens: number;
  /** The window is a default, not the model's real one. */
  windowGuessed: boolean;
  /** The history before shortening. */
  fullHistoryTokens: number;
  historyTokens: number;
  /** Messages compacted or turns dropped to fit. */
  shortened: boolean;
}

function compact(n: number): string {
  if (n < 1_000) return `${n}`;
  if (n < 10_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${Math.round(n / 1_000)}k`;
}

/** Ring geometry: a 16-unit box, as the app's other icons. */
const RADIUS = 6;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

export default function ContextMeter(props: { usage: ContextUsage }) {
  const share = () => Math.min(1, props.usage.promptTokens / Math.max(1, props.usage.windowTokens));
  const percent = () => Math.round(share() * 100);
  const level = () => (percent() >= 85 ? 'high' : percent() >= 60 ? 'mid' : 'low');

  const tooltip = () => {
    const u = props.usage;
    const lines = [
      `About ${compact(u.promptTokens)} of ${compact(u.windowTokens)} tokens in the next request (estimated).`,
    ];
    if (u.shortened) {
      lines.push(
        `The full chat is about ${compact(u.fullHistoryTokens)} tokens; older parts are shortened to fit.`
      );
    }
    if (u.windowGuessed) {
      lines.push(
        "This model's context size isn't known, so 32k is assumed. Set it in Model providers."
      );
    }
    return lines.join('\n');
  };

  return (
    <div class={`chat-context-bar ${level()}`} title={tooltip()}>
      <span class="chat-context-label">
        Context used: {percent()}%
        <Show when={props.usage.windowGuessed}>
          <span class="chat-context-note"> (32k assumed)</span>
        </Show>
      </span>
      <svg
        class="chat-context-ring"
        width="14"
        height="14"
        viewBox="0 0 16 16"
        role="img"
        aria-label={`Context used: ${percent()} percent`}
      >
        <circle class="chat-context-track" cx="8" cy="8" r={RADIUS} />
        <circle
          class="chat-context-fill"
          cx="8"
          cy="8"
          r={RADIUS}
          stroke-dasharray={`${CIRCUMFERENCE}`}
          stroke-dashoffset={`${CIRCUMFERENCE * (1 - share())}`}
          // From 12 o'clock, clockwise.
          transform="rotate(-90 8 8)"
        />
      </svg>
    </div>
  );
}
