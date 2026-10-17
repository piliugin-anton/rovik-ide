// JudgeRecord.tsx
//
// A Thorough Autopilot task's judge in the chat (T02,
// agent_run/judge.rs): at work on the planners' plans, then what it chose
// (a plan as it is, revised, or the plans merged) and why, or why it gave
// no choice and what the task took instead, and its work on demand.
// Updates live while it runs. Also its merge of the criteria sets (T03):
// the merged set, where the sets disagreed under what it assumed.
import { Show } from 'solid-js';
import { judgeHeadline, type JudgeMeta } from '../lib/autopilot';
import { liveLabel, liveSubagent } from '../lib/subagents';
import { SubagentTranscript } from './ToolCallBlock';
import { createSubagentWork, ShowWorkButton } from './SubagentWork';
import { CriteriaParts } from './PlannerRecord';

/** "the model · 12 tool calls · 40 s", what's known of it. */
function facts(meta: JudgeMeta): string {
  const seconds = meta.ms !== undefined ? `${Math.round(meta.ms / 1000)} s` : '';
  const calls =
    meta.toolCalls !== undefined
      ? `${meta.toolCalls} tool call${meta.toolCalls === 1 ? '' : 's'}`
      : '';
  return [meta.model, calls, seconds, meta.partial ? 'cut short by its budget' : '']
    .filter(Boolean)
    .join(' · ');
}

export default function JudgeRecord(props: {
  meta: JudgeMeta;
  /** The chat it's in, and this record's id (the judge's events name it). */
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
  return (
    <div
      class={`chat-verify chat-judge ${interrupted() ? 'stopped' : props.meta.status}`}
      role="status"
    >
      <div class="chat-verify-head">
        <span class="chat-verify-title">{judgeHeadline(props.meta, interrupted())}</span>
      </div>
      <Show when={props.meta.status === 'running' && !interrupted() && progress()}>
        {(p) => <div class="chat-verify-progress">{liveLabel(p())}</div>}
      </Show>
      <Show when={props.meta.why}>{(why) => <div class="chat-judge-why">{why()}</div>}</Show>
      <CriteriaParts meta={props.meta} />
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
            <summary>
              {props.meta.choice === 'merged' ? 'The merged plan' : 'The revised plan'}
            </summary>
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
