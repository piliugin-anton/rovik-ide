// AutopilotStrip.tsx
//
// The chat's current Autopilot task above the input (docs/design/
// autopilot-plan.md, 3.3): its stages in order, each with a tick, a
// spinner or a cross, its round when it ran more than once, and its
// summary on hover; a click shows the stage's record in the chat. It reads
// the stage records and keeps no state of its own, so it is right after a
// restart and in a remote chat's mirror.
import { Index, Show } from 'solid-js';
import {
  shownStatus,
  stageHover,
  stripStages,
  taskLine,
  strictAutopilotRun,
  strictAutopilotTask,
  unresolvedStrictTask,
  type StageRow,
} from '../lib/autopilot';
import { StageIcon } from './StageRecord';

export default function AutopilotStrip(props: {
  /** The task's records, oldest first (lib/autopilot.ts `taskRecords`). */
  rows: StageRow[];
  /** A run of the chat's is going on, or its task's driver is at work. */
  running: boolean;
  /** Show a record in the chat, by its message id. */
  onShow: (recordId: string) => void;
  /** Open the existing strict controller's approvals, results and delivery review. */
  onOpenVerified?: (runId: string) => void;
  /** Cancel frozen task ownership, including paused preparation before a controller exists. */
  onCancelVerified?: () => void;
}) {
  const line = () => taskLine(props.rows, props.running);
  const strictRun = () => strictAutopilotRun(props.rows);
  const strictSummary = () =>
    strictAutopilotTask(props.rows)
      ? [...props.rows].reverse().find((row) => row.stage.summary)?.stage.summary
      : undefined;
  return (
    <div class="autopilot-strip" role="group" aria-label={line()}>
      <span class="autopilot-strip-line" aria-live="polite">
        {line()}
      </span>
      <Show when={strictAutopilotTask(props.rows)}>
        <span class="settings-hint">Verified delivery required</span>
      </Show>
      <Show when={strictRun()}>
        {(run) => (
          <button
            type="button"
            class="link-button"
            disabled={!props.onOpenVerified}
            onClick={() => props.onOpenVerified?.(run().runId)}
          >
            {run().stopped
              ? 'Review stopped verified task'
              : 'Open verified task decisions and result'}
          </button>
        )}
      </Show>
      <Show when={unresolvedStrictTask(props.rows)}>
        <button
          type="button"
          class="link-button"
          disabled={!props.onCancelVerified}
          onClick={() => props.onCancelVerified?.()}
        >
          Cancel verified task
        </button>
      </Show>
      <Show when={strictSummary()}>
        {(summary) => (
          <span class="settings-hint" aria-live="polite">
            {summary()}
          </span>
        )}
      </Show>
      <ol class="autopilot-stages">
        {/* By position: a stage's button stays as its record changes. */}
        <Index each={stripStages(props.rows)}>
          {(stage) => {
            const status = () =>
              shownStatus(stage().status, props.running, stage().waiting, stage().failures);
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
