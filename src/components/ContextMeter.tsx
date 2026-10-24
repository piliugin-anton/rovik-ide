// ContextMeter.tsx
//
// "Context used: N%" above the chat input, with a ring showing the same.
// For a model's chat (ContextMeter), N is how much of the model's context window the chat's next request
// would take: the history as it would be sent (after trimming), the
// system prompt, tool schemas and project context. It's an
// estimate from the backend (agent.rs, chat_context_estimate), calibrated
// per model from the token counts providers report. A lighter arc after it
// is the room kept for the reply, its reasoning included (agent.rs,
// reply_reserve): the history only gets what that leaves.
//
// A connector's chat (ConnectorContextMeter) shows the harness's own
// context instead, as its last reply reported it: the harness keeps the
// conversation and compacts it itself, so Rovik's estimate and context
// modes don't apply to it.
import { Show } from 'solid-js';
import { connectorTitle, type ConnectorContext } from '../lib/connectors';
import type { ConnectorKind } from '../types';

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
  /** Kept free for the reply, its reasoning included. */
  replyReserveTokens: number;
  /** Out-of-date messages replaced or turns dropped to fit. */
  shortened: boolean;
}

function compact(n: number): string {
  if (n < 1_000) return `${n}`;
  if (n < 10_000) return `${(n / 1_000).toFixed(1)}k`;
  if (n < 999_500) return `${Math.round(n / 1_000)}k`;
  // 1M, 1.2M: a million-token window isn't "1000k".
  return `${Number((n / 1_000_000).toFixed(1))}M`;
}

/** Ring geometry: a 16-unit box, as the app's other icons. */
const RADIUS = 6;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/** The bar itself: a label, and a ring of `used` (and `reserve` after it) of `window`. */
function ContextGauge(props: {
  used: number;
  window: number;
  reserve?: number;
  label: string;
  note?: string;
  tooltip: string;
}) {
  const share = () => Math.min(1, props.used / Math.max(1, props.window));
  const percent = () => Math.round(share() * 100);
  /** The reply's share, drawn after the prompt's, within what's left. */
  const replyShare = () => Math.min(1 - share(), (props.reserve ?? 0) / Math.max(1, props.window));
  const level = () => (percent() >= 85 ? 'high' : percent() >= 60 ? 'mid' : 'low');

  return (
    <div class={`chat-context-bar ${level()}`} title={props.tooltip}>
      <span class="chat-context-label">
        {props.label}
        <Show when={props.note}>
          <span class="chat-context-note"> {props.note}</span>
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
          class="chat-context-reserve"
          cx="8"
          cy="8"
          r={RADIUS}
          stroke-dasharray={`${CIRCUMFERENCE * replyShare()} ${CIRCUMFERENCE}`}
          // Starts where the prompt's arc ends.
          transform={`rotate(${-90 + share() * 360} 8 8)`}
        />
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

/** A model's chat: its next request, as the backend estimates it. */
export default function ContextMeter(props: { usage: ContextUsage }) {
  const percent = () =>
    Math.round(Math.min(1, props.usage.promptTokens / Math.max(1, props.usage.windowTokens)) * 100);
  const tooltip = () => {
    const u = props.usage;
    const lines = [
      `About ${compact(u.promptTokens)} of ${compact(u.windowTokens)} tokens in the next request (estimated).`,
      `${compact(u.replyReserveTokens)} more are kept for the reply and the model's reasoning.`,
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
    <ContextGauge
      used={props.usage.promptTokens}
      window={props.usage.windowTokens}
      reserve={props.usage.replyReserveTokens}
      label={`Context used: ${percent()}%`}
      note={props.usage.windowGuessed ? '(32k assumed)' : undefined}
      tooltip={tooltip()}
    />
  );
}

/** A connector's chat: "Context used: 35k / 1M (4%)", as its last reply left it. */
export function ConnectorContextMeter(props: { kind?: string; context: ConnectorContext }) {
  const c = () => props.context;
  const percent = () => Math.round(Math.min(1, c().used / Math.max(1, c().window)) * 100);
  const tooltip = () => {
    const title = connectorTitle(props.kind as ConnectorKind | undefined);
    const lines = [
      `${title}’s context after its last reply: ${compact(c().used)} of ${compact(c().window)} tokens.`,
      `${title} keeps this chat’s conversation itself${
        c().compactsAt ? ` and compacts it at about ${compact(c().compactsAt!)} tokens` : ''
      }.`,
    ];
    return lines.join('\n');
  };
  return (
    <ContextGauge
      used={c().used}
      window={c().window}
      label={`Context used: ${compact(c().used)} / ${compact(c().window)} (${percent()}%)`}
      tooltip={tooltip()}
    />
  );
}
