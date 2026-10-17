// TestsRecord.tsx
//
// An Autopilot task's Tests stage in the chat (S04,
// agent_run/test_writer.rs): the test writer at work, then the tests it
// wrote before the code, those that pass already flagged, its report, and
// its work on demand. Updates live while it runs.
import { For, Show } from 'solid-js';
import { testsHeadline, type TestsMeta } from '../lib/autopilot';
import { liveLabel, liveSubagent } from '../lib/subagents';
import { SubagentTranscript } from './ToolCallBlock';
import { createSubagentWork, ShowWorkButton } from './SubagentWork';

/** "the model · 12 tool calls · 40 s", what's known of it. */
function facts(meta: TestsMeta): string {
  const seconds = meta.ms !== undefined ? `${Math.round(meta.ms / 1000)} s` : '';
  const calls =
    meta.toolCalls !== undefined
      ? `${meta.toolCalls} tool call${meta.toolCalls === 1 ? '' : 's'}`
      : '';
  return [meta.model, calls, seconds, meta.partial ? 'cut short by its budget' : '']
    .filter(Boolean)
    .join(' · ');
}

export default function TestsRecord(props: {
  meta: TestsMeta;
  /** The chat it's in, and this record's id (the writer's events name it). */
  dialogId: string | null;
  recordId: string;
  /** Its run is running right now. */
  live: boolean;
}) {
  const interrupted = () => props.meta.status === 'running' && !props.live;
  const progress = () => liveSubagent(props.dialogId, props.recordId);
  const childId = () => props.meta.childDialogId ?? progress()?.childDialogId ?? null;
  // P5.9: neither a finished writer's live entry nor an unshown transcript stays.
  const work = createSubagentWork({
    dialogId: () => props.dialogId,
    toolCallId: () => props.recordId,
    childId,
    finishedChildId: () => (props.meta.status !== 'running' ? props.meta.childDialogId : null),
  });
  const passes = (file: string) => props.meta.passing?.includes(file) ?? false;
  return (
    <div
      class={`chat-verify chat-tests ${interrupted() ? 'stopped' : props.meta.status}`}
      role="status"
    >
      <div class="chat-verify-head">
        <span class="chat-verify-title">{testsHeadline(props.meta, interrupted())}</span>
      </div>
      <Show when={props.meta.status === 'running' && !interrupted() && progress()}>
        {(p) => <div class="chat-verify-progress">{liveLabel(p())}</div>}
      </Show>
      <Show when={props.meta.files?.length}>
        <ul class="chat-tests-files">
          <For each={props.meta.files}>
            {(file) => (
              <li>
                <code>{file}</code>
                <Show when={passes(file)}>
                  <span class="chat-tests-note"> · passes already</span>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={props.meta.summary}>
        {(text) => <div class="chat-tests-summary">{text()}</div>}
      </Show>
      <Show when={props.meta.status !== 'running'}>
        <div class="chat-verify-files">
          {facts(props.meta)}
          <Show when={childId()}>
            {facts(props.meta) ? ' · ' : ''}
            <ShowWorkButton work={work} />
          </Show>
        </div>
        <Show when={props.meta.assumptions || props.meta.report}>
          <details class="review-details">
            <summary>Details</summary>
            <Show when={props.meta.assumptions}>
              <div class="review-label">What it assumed:</div>
              <div class="chat-tests-assumptions">{props.meta.assumptions}</div>
            </Show>
            <Show when={props.meta.report}>
              <div class="review-label">The report as the test writer wrote it:</div>
              <pre class="review-report">{props.meta.report}</pre>
            </Show>
          </details>
        </Show>
      </Show>
      <Show when={work.shown() && childId()}>
        <SubagentTranscript childDialogId={childId()!} running={props.meta.status === 'running'} />
      </Show>
    </div>
  );
}
