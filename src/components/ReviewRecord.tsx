// ReviewRecord.tsx
//
// One change review in the chat (lib/changeReview.ts): what the reviewer
// concluded, each finding with where it is (a click opens the file there),
// and its work on demand. Updates live while the reviewer reads the change.
import { createEffect, createSignal, For, onCleanup, Show } from 'solid-js';
import {
  reviewDetails,
  reviewHeadline,
  type ReviewFinding,
  type ReviewMeta,
} from '../lib/changeReview';
import { forgetLive, liveSubagent, transcriptHold, workLabel } from '../lib/subagents';
import { requestOpenFile } from '../lib/editorBridge';
import { stopRun } from '../lib/agentRuns';
import { SubagentTranscript } from './ToolCallBlock';

function FindingItem(props: { finding: ReviewFinding; dropped?: boolean }) {
  const f = () => props.finding;
  const open = () => {
    const path = f().path;
    if (path) requestOpenFile({ path, line: f().line ?? 1 });
  };
  return (
    <li class={`review-finding ${f().severity}`} classList={{ dropped: !!props.dropped }}>
      <div class="review-finding-head">
        <span class={`review-severity ${f().severity}`}>{f().severity}</span>
        <Show when={f().path} fallback={<span class="review-location">{f().location}</span>}>
          <button type="button" class="link-button review-location" onClick={open}>
            {f().location}
          </button>
        </Show>
        <span class="review-title">{f().title}</span>
        <span class="review-confidence">{f().confidence}</span>
      </div>
      <div class="review-finding-body">
        <div>
          <span class="review-label">Scenario:</span> {f().scenario}
        </div>
        <div>
          <span class="review-label">Fix:</span> {f().fix}
        </div>
        <Show when={f().newEvidence}>
          <div>
            <span class="review-label">New evidence:</span> {f().newEvidence}
          </div>
        </Show>
      </div>
    </li>
  );
}

export default function ReviewRecord(props: {
  meta: ReviewMeta;
  /** The chat it's in, and this record's id (the reviewer's events name it). */
  dialogId: string | null;
  recordId: string;
  /** The reviewer is running right now. */
  live: boolean;
}) {
  const interrupted = () => props.meta.status === 'running' && !props.live;
  const progress = () => liveSubagent(props.dialogId, props.recordId);
  const childId = () => props.meta.childDialogId ?? progress()?.childDialogId ?? null;
  const [showWork, setShowWork] = createSignal(false);
  // P5.9: neither a finished review's live entry nor an unshown transcript stays.
  createEffect(() => {
    const id = props.meta.childDialogId;
    if (props.meta.status !== 'running' && id) forgetLive(props.dialogId, props.recordId, id);
  });
  const hold = transcriptHold();
  onCleanup(hold.release);
  const toggleWork = () => {
    const id = childId();
    if (!id) return;
    if (!showWork() && props.dialogId) void hold.open(id, props.dialogId, props.recordId);
    setShowWork(!showWork());
  };
  return (
    <div
      class={`chat-verify chat-review ${interrupted() ? 'stopped' : props.meta.status}`}
      role="status"
    >
      <div class="chat-verify-head">
        <span class="chat-verify-title">{reviewHeadline(props.meta, interrupted())}</span>
        <Show when={props.meta.status === 'running' && props.live && childId()}>
          <button
            type="button"
            class="link-button"
            title="Stop the review; the checks still run"
            onClick={() => void stopRun(childId()!)}
          >
            Stop review
          </button>
        </Show>
      </div>
      <Show when={props.meta.status === 'running' && !interrupted() && progress()}>
        {(p) => (
          <div class="chat-verify-progress">
            {workLabel(p().toolCalls)}
            {p().currentTool ? ` · ${p().currentTool}` : ''} · {p().model}
          </div>
        )}
      </Show>
      <Show when={props.meta.findings?.length}>
        <ul class="review-findings">
          <For each={props.meta.findings}>{(f) => <FindingItem finding={f} />}</For>
        </ul>
      </Show>
      <Show when={props.meta.dropped?.length}>
        <details class="review-dropped">
          <summary>
            {props.meta.dropped!.length} repeated without new evidence, not sent again
          </summary>
          <ul class="review-findings">
            <For each={props.meta.dropped}>{(f) => <FindingItem finding={f} dropped />}</For>
          </ul>
        </details>
      </Show>
      <Show when={props.meta.status !== 'running'}>
        <div class="chat-verify-files">
          {reviewDetails(props.meta)}
          <Show when={childId()}>
            {' · '}
            <button
              type="button"
              class="link-button"
              aria-expanded={showWork()}
              onClick={toggleWork}
            >
              {showWork() ? 'Hide its work' : 'Show its work'}
            </button>
          </Show>
        </div>
      </Show>
      <Show when={showWork() && childId()}>
        <SubagentTranscript childDialogId={childId()!} running={props.meta.status === 'running'} />
      </Show>
    </div>
  );
}
