// StageRecord.tsx
//
// One stage of an Autopilot task in the chat (lib/autopilot.ts): where the
// stage began, and, once it ended, how, in one line. The stage's own work
// (the plan, the implementation, their reviews and checks) follows it as
// the chat's messages and records.
import { Match, Show, Switch } from 'solid-js';
import {
  shownStatus,
  stageTitle,
  statusWord,
  type StageMeta,
  type StageShown,
} from '../lib/autopilot';

/** A stage's state as an icon: a tick, a cross, a spinner, a pause, a dash or a ring. */
export function StageIcon(props: { status: StageShown }) {
  const common = {
    width: 12,
    height: 12,
    viewBox: '0 0 16 16',
    fill: 'none',
    stroke: 'currentColor',
    'stroke-width': 1.8,
    'stroke-linecap': 'round' as const,
    'stroke-linejoin': 'round' as const,
    'aria-hidden': true,
  };
  return (
    <Switch
      fallback={
        <svg {...common}>
          <path d="M4.5 8h7" />
        </svg>
      }
    >
      <Match when={props.status === 'done'}>
        <svg {...common}>
          <path d="M3.5 8.5 6.5 11.5 12.5 4.5" />
        </svg>
      </Match>
      <Match when={props.status === 'failed'}>
        <svg {...common}>
          <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" />
        </svg>
      </Match>
      <Match when={props.status === 'running'}>
        <svg {...common} class="verify-spin">
          <path d="M13 8a5 5 0 1 1-5-5" />
        </svg>
      </Match>
      <Match when={props.status === 'stopped'}>
        <svg {...common}>
          <path d="M6 4.5v7M10 4.5v7" />
        </svg>
      </Match>
      <Match when={props.status === 'pending'}>
        <svg {...common}>
          <circle cx="8" cy="8" r="4" />
        </svg>
      </Match>
    </Switch>
  );
}

export default function StageRecord(props: {
  stage: StageMeta;
  /** A run of the chat's is going on: a stage recorded running is at work. */
  live: boolean;
}) {
  const status = () => shownStatus(props.stage.status, props.live);
  return (
    <div class={`chat-stage ${status()}`} role="status">
      <StageIcon status={status()} />
      <span class="chat-stage-name">Autopilot · {stageTitle(props.stage)}</span>
      <span class="chat-stage-status">{statusWord(status())}</span>
      <Show when={props.stage.summary}>
        {(summary) => <span class="chat-stage-summary">{summary()}</span>}
      </Show>
    </div>
  );
}
