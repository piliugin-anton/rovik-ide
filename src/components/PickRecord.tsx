// PickRecord.tsx
//
// A Thorough Autopilot task's Pick in the chat (T06,
// agent_run/candidates.rs): each candidate's score (D14: the tests written
// first that pass, the checks that pass, the findings left open, the
// lines changed), the winner and how it won, the judge's when the scores
// tied, and the loser's diff, kept.
import { For, Show } from 'solid-js';
import { pickHeadline, scoreLine, type PickMeta } from '../lib/autopilot';

export default function PickRecord(props: {
  meta: PickMeta;
  /** The task chat's run is running right now. */
  live: boolean;
}) {
  const interrupted = () => props.meta.status === 'running' && !props.live;
  const status = () => (interrupted() ? 'stopped' : props.meta.status);
  return (
    <div class={`chat-verify chat-pick ${status()}`} role="status">
      <div class="chat-verify-head">
        <span class="chat-verify-title">{pickHeadline(props.meta, interrupted())}</span>
      </div>
      <Show when={props.meta.candidates.length}>
        <ul class="chat-pick-scores">
          <For each={props.meta.candidates}>
            {(c) => (
              <li classList={{ 'chat-pick-winner': c.letter === props.meta.winner }}>
                {c.letter.toUpperCase()}: {scoreLine(c.score)}
                <Show when={c.diff}>
                  {(diff) => (
                    <details class="review-details">
                      <summary>
                        Candidate {c.letter.toUpperCase()}’s change, not taken
                        {c.reduced ? ' (shortened)' : ''}
                      </summary>
                      <pre class="review-report">{diff()}</pre>
                    </details>
                  )}
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}
