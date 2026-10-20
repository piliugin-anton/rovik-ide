// ReviewRecord.tsx
//
// One change review in the chat (lib/changeReview.ts): what the reviewer
// concluded, each finding with where it is (a click opens the file there),
// and its work on demand. Updates live while the reviewer reads the change.
import { For, Show } from 'solid-js';
import {
  earlierVerdict,
  reviewDetails,
  reviewFacts,
  reviewHeadline,
  type EarlierCheck,
  type ReviewFinding,
  type ReviewMeta,
} from '../lib/changeReview';
import { liveLabel, liveSubagent } from '../lib/subagents';
import { openInEditor } from '../lib/openInEditor';
import { stopRun } from '../lib/agentRuns';
import { SubagentTranscript } from './ToolCallBlock';
import { createSubagentWork, ShowWorkButton } from './SubagentWork';

function FindingItem(props: { finding: ReviewFinding; dropped?: boolean }) {
  const f = () => props.finding;
  const open = () => {
    const { path, line } = f();
    if (path) void openInEditor({ path, focus: true, ...(line ? { at: { line } } : {}) });
  };
  return (
    <li class={`review-finding ${f().severity}`} classList={{ dropped: !!props.dropped }}>
      <div class="review-finding-head">
        <span class={`review-severity ${f().severity}`}>{f().severity}</span>
        {/* No location: the finding is about the change, or the plan, as a whole. */}
        <Show
          when={f().path}
          fallback={
            <Show when={f().location}>
              <span class="review-location">{f().location}</span>
            </Show>
          }
        >
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

/** A follow-up plan review's verdict on an earlier finding, with the agent's answer. */
function EarlierItem(props: { check: EarlierCheck }) {
  const c = () => props.check;
  const verdict = () => earlierVerdict(c());
  return (
    <li
      class="review-earlier-item"
      classList={{ resolved: verdict() === 'resolved', unchecked: verdict() === 'not checked' }}
    >
      <span class="review-earlier-verdict">{verdict()}</span>
      <span class="review-location">
        Earlier {c().finding}
        {c().location ? ` · ${c().location}` : ''}
      </span>
      <span class="review-title">{c().title}</span>
      <Show when={c().response}>
        {(r) => (
          <div class="review-finding-body">
            <span class="review-label">Agent:</span>{' '}
            {r().action === 'fixed' ? 'fixed' : 'not a problem'}
            {r().note ? ` — ${r().note}` : ''}
          </div>
        )}
      </Show>
      <Show when={c().reason}>
        <div class="review-finding-body">
          <span class="review-label">Why:</span> {c().reason}
        </div>
      </Show>
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
  // P5.9: neither a finished review's live entry nor an unshown transcript stays.
  const work = createSubagentWork({
    dialogId: () => props.dialogId,
    toolCallId: () => props.recordId,
    childId,
    finishedChildId: () => {
      const id = props.meta.childDialogId;
      return props.meta.status !== 'running' ? id : null;
    },
  });
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
        {(p) => <div class="chat-verify-progress">{liveLabel(p())}</div>}
      </Show>
      <Show when={props.meta.earlier?.length}>
        <ul class="review-findings review-earlier">
          <For each={props.meta.earlier}>{(c) => <EarlierItem check={c} />}</For>
        </ul>
      </Show>
      <Show when={props.meta.findings?.length}>
        <ul class="review-findings">
          <For each={props.meta.findings}>{(f) => <FindingItem finding={f} />}</For>
        </ul>
      </Show>
      <Show when={props.meta.notHeld?.length}>
        <details class="review-dropped">
          <summary>
            {props.meta.notHeld!.length} about what the revision didn’t change, not held
          </summary>
          <ul class="review-findings">
            <For each={props.meta.notHeld}>{(f) => <FindingItem finding={f} dropped />}</For>
          </ul>
        </details>
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
            <ShowWorkButton work={work} />
          </Show>
        </div>
        <details class="review-details">
          <summary>Details</summary>
          <dl class="review-facts">
            <For each={reviewFacts(props.meta)}>
              {([label, value]) => (
                <>
                  <dt>{label}</dt>
                  <dd>{value}</dd>
                </>
              )}
            </For>
          </dl>
          <Show when={props.meta.report}>
            <div class="review-label">The report as the reviewer wrote it:</div>
            <pre class="review-report">{props.meta.report}</pre>
          </Show>
        </details>
      </Show>
      <Show when={work.shown() && childId()}>
        <SubagentTranscript childDialogId={childId()!} running={props.meta.status === 'running'} />
      </Show>
    </div>
  );
}
