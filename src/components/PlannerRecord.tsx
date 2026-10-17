// PlannerRecord.tsx
//
// One of a Thorough Autopilot task's planners in the chat (T01,
// agent_run/planning.rs): at work, then the plan it gave (whether the
// task took it, what the plan's checks found in it), or why it gave none,
// and its work on demand. Updates live while it runs.
import { For, Show } from 'solid-js';
import { plannerHeadline, type PlannerMeta } from '../lib/autopilot';
import { liveLabel, liveSubagent } from '../lib/subagents';
import { SubagentTranscript } from './ToolCallBlock';
import { createSubagentWork, ShowWorkButton } from './SubagentWork';

/** "the model · 12 tool calls · 40 s", what's known of it. */
function facts(meta: PlannerMeta): string {
  const seconds = meta.ms !== undefined ? `${Math.round(meta.ms / 1000)} s` : '';
  const calls =
    meta.toolCalls !== undefined
      ? `${meta.toolCalls} tool call${meta.toolCalls === 1 ? '' : 's'}`
      : '';
  return [meta.model, calls, seconds, meta.partial ? 'cut short by its budget' : '']
    .filter(Boolean)
    .join(' · ');
}

export default function PlannerRecord(props: {
  meta: PlannerMeta;
  /** The chat it's in, and this record's id (the planner's events name it). */
  dialogId: string | null;
  recordId: string;
  /** Its run is running right now. */
  live: boolean;
}) {
  const interrupted = () => props.meta.status === 'running' && !props.live;
  const progress = () => liveSubagent(props.dialogId, props.recordId);
  const childId = () => props.meta.childDialogId ?? progress()?.childDialogId ?? null;
  const work = createSubagentWork({
    dialogId: () => props.dialogId,
    toolCallId: () => props.recordId,
    childId,
    finishedChildId: () => (props.meta.status !== 'running' ? props.meta.childDialogId : null),
  });
  const status = () => {
    if (interrupted()) return 'stopped';
    if (props.meta.status === 'done' && props.meta.problems?.length) return 'flagged';
    return props.meta.status;
  };
  return (
    <div
      class={`chat-verify chat-planner ${status()}`}
      classList={{ chosen: !!props.meta.chosen }}
      role="status"
    >
      <div class="chat-verify-head">
        <span class="chat-verify-title">{plannerHeadline(props.meta, interrupted())}</span>
      </div>
      <Show when={props.meta.status === 'running' && !interrupted() && progress()}>
        {(p) => <div class="chat-verify-progress">{liveLabel(p())}</div>}
      </Show>
      <Show when={props.meta.problems?.length}>
        <ul class="chat-verify-flags">
          <For each={props.meta.problems}>{(p) => <li>{p}</li>}</For>
        </ul>
      </Show>
      <Show when={props.meta.status !== 'running'}>
        <div class="chat-verify-files">
          {facts(props.meta)}
          <Show when={childId()}>
            {facts(props.meta) ? ' · ' : ''}
            <ShowWorkButton work={work} />
          </Show>
        </div>
        <Show when={props.meta.plan}>
          <details class="review-details">
            <summary>The plan</summary>
            <pre class="review-report">{props.meta.plan}</pre>
          </details>
        </Show>
      </Show>
      <Show when={work.shown() && childId()}>
        <SubagentTranscript childDialogId={childId()!} running={props.meta.status === 'running'} />
      </Show>
    </div>
  );
}
