// CandidatesRecord.tsx
//
// A Thorough Autopilot task's two implementers in the chat (T05,
// agent_run/candidates.rs): one after the other, each implements the plan
// in a working copy of its own, in a chat of its own; how each went, and
// each one's work on demand. With one implementer, why there was one.
// Updates live while they run.
import { For, Show } from 'solid-js';
import {
  candidateLine,
  candidatesHeadline,
  type CandidateMeta,
  type CandidatesMeta,
} from '../lib/autopilot';
import { liveLabel, liveSubagent } from '../lib/subagents';
import { SubagentTranscript } from './ToolCallBlock';
import { createSubagentWork, ShowWorkButton } from './SubagentWork';

/** One candidate: its line, its live progress, its work on demand. */
function Candidate(props: {
  candidate: CandidateMeta;
  dialogId: string | null;
  /** The call id its events name: the record's id and its letter. */
  callId: string;
  live: boolean;
}) {
  const running = () => props.candidate.status === 'running' && props.live;
  const progress = () => liveSubagent(props.dialogId, props.callId);
  const work = createSubagentWork({
    dialogId: () => props.dialogId,
    toolCallId: () => props.callId,
    childId: () => props.candidate.chat,
    finishedChildId: () =>
      !['waiting', 'running'].includes(props.candidate.status) ? props.candidate.chat : null,
  });
  return (
    <li class={`chat-candidate ${props.candidate.status}`}>
      <span>{candidateLine(props.candidate)}</span>
      <Show when={running() && progress()}>{(p) => <span> · {liveLabel(p(), false)}</span>}</Show>
      <Show when={props.candidate.status !== 'waiting' && props.candidate.status !== 'skipped'}>
        {' · '}
        <ShowWorkButton work={work} />
      </Show>
      <Show when={work.shown()}>
        <SubagentTranscript childDialogId={props.candidate.chat} running={running()} />
      </Show>
    </li>
  );
}

export default function CandidatesRecord(props: {
  meta: CandidatesMeta;
  /** The chat it's in, and this record's id (its candidates' events name it). */
  dialogId: string | null;
  recordId: string;
  /** The task chat's run is running right now. */
  live: boolean;
}) {
  const interrupted = () => props.meta.status === 'running' && !props.live;
  const status = () => (interrupted() ? 'stopped' : props.meta.status);
  return (
    <div class={`chat-verify chat-candidates ${status()}`} role="status">
      <div class="chat-verify-head">
        <span class="chat-verify-title">{candidatesHeadline(props.meta, interrupted())}</span>
      </div>
      <Show when={props.meta.candidates?.length}>
        <ul class="chat-candidates-list">
          <For each={props.meta.candidates}>
            {(c) => (
              <Candidate
                candidate={c}
                dialogId={props.dialogId}
                callId={`${props.recordId}:${c.letter}`}
                live={props.live}
              />
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}
