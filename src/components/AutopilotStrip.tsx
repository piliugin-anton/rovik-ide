// AutopilotStrip.tsx
//
// The chat's current Autopilot task above the input (docs/design/
// autopilot-plan.md, 3.3): its stages in order, each with a tick, a
// spinner or a cross, its round when it ran more than once, and its
// summary on hover; a click shows the stage's record in the chat. It reads
// the stage records and keeps no state of its own, so it is right after a
// restart and in a remote chat's mirror.
import { Index, Show } from 'solid-js';
import { shownStatus, stageHover, stripStages, taskLine, type StageRow } from '../lib/autopilot';
import { StageIcon } from './StageRecord';

export default function AutopilotStrip(props: {
  /** The task's records, oldest first (lib/autopilot.ts `taskRecords`). */
  rows: StageRow[];
  /** A run of the chat's is going on. */
  running: boolean;
  /** Show a record in the chat, by its message id. */
  onShow: (recordId: string) => void;
}) {
  const line = () => taskLine(props.rows, props.running);
  return (
    <div class="autopilot-strip" role="group" aria-label={line()}>
      <span class="autopilot-strip-line" aria-live="polite">
        {line()}
      </span>
      <ol class="autopilot-stages">
        {/* By position: a stage's button stays as its record changes. */}
        <Index each={stripStages(props.rows)}>
          {(stage) => {
            const status = () => shownStatus(stage().status, props.running);
            const hover = () => stageHover(stage(), props.running);
            return (
              <li class={`autopilot-stage ${status()}`}>
                <button
                  type="button"
                  class="autopilot-stage-button"
                  title={hover()}
                  aria-label={hover()}
                  disabled={!stage().recordId}
                  onClick={() => {
                    const id = stage().recordId;
                    if (id) props.onShow(id);
                  }}
                >
                  <StageIcon status={status()} />
                  <span class="autopilot-stage-label">{stage().label}</span>
                  <Show when={stage().round}>
                    {(round) => <span class="autopilot-stage-round">×{round()}</span>}
                  </Show>
                </button>
              </li>
            );
          }}
        </Index>
      </ol>
    </div>
  );
}
