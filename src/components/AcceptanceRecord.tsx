// AcceptanceRecord.tsx
//
// An Autopilot task's acceptance check in the chat (S08,
// agent_run/acceptance.rs): each of the plan's criteria with its verdict
// and what shows it, what the checker's work took once it's done, and its
// work on demand, while it runs too, as a task call's. Updates live while
// it runs.
import { For, Show } from 'solid-js';
import { acceptanceHeadline, type AcceptanceMeta } from '../lib/autopilot';
import { liveLabel, liveSubagent, workFacts } from '../lib/subagents';
import { SubagentTranscript } from './ToolCallBlock';
import { createSubagentWork, ShowWorkButton } from './SubagentWork';

export default function AcceptanceRecord(props: {
  meta: AcceptanceMeta;
  /** The chat it's in, and this record's id (the checker's events name it). */
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
  const verdictOf = (i: number) => props.meta.verdicts?.find((v) => v.criterion === i + 1);
  const status = () => (interrupted() ? 'stopped' : props.meta.unmet ? 'unmet' : props.meta.status);
  return (
    <div class={`chat-verify chat-acceptance ${status()}`} role="status">
      <div class="chat-verify-head">
        <span class="chat-verify-title">{acceptanceHeadline(props.meta, interrupted())}</span>
      </div>
      <Show when={props.meta.status === 'running' && !interrupted() && progress()}>
        {(p) => <div class="chat-verify-progress">{liveLabel(p())}</div>}
      </Show>
      <ol class="chat-acceptance-criteria">
        <For each={props.meta.criteria}>
          {(criterion, i) => (
            <li
              classList={{
                met: verdictOf(i())?.satisfied === true,
                unmet: verdictOf(i())?.satisfied === false,
              }}
            >
              <span class="chat-acceptance-criterion">{criterion}</span>
              <Show when={verdictOf(i())}>
                {(v) => (
                  <span class="chat-acceptance-verdict">
                    {v().satisfied ? 'Met' : 'Unmet'}: {v().note}
                  </span>
                )}
              </Show>
            </li>
          )}
        </For>
      </ol>
      <Show when={(props.meta.status !== 'running' && workFacts(props.meta)) || childId()}>
        <div class="chat-verify-files">
          <Show when={props.meta.status !== 'running'}>{workFacts(props.meta)}</Show>
          <Show when={childId()}>
            {props.meta.status !== 'running' && workFacts(props.meta) ? ' · ' : ''}
            <ShowWorkButton work={work} />
          </Show>
        </div>
      </Show>
      <Show when={work.shown() && childId()}>
        <SubagentTranscript
          childDialogId={childId()!}
          running={props.meta.status === 'running' && !interrupted()}
        />
      </Show>
    </div>
  );
}
