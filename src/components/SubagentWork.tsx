// SubagentWork.tsx
//
// A subagent's work as a block shows it on demand ("Show its work"): the
// review, triage and task blocks share the plumbing (lib/subagents.ts).
import { createEffect, createSignal, onCleanup } from 'solid-js';
import { forgetLive, transcriptHold } from '../lib/subagents';

export interface SubagentWork {
  /** Its transcript is shown. */
  shown: () => boolean;
  toggle: () => void;
}

/**
 * A block's hold on a subagent's work: its transcript shown and loaded on
 * demand and let go when the block goes, and its live entry forgotten once
 * it has finished (P5.9). Called while the block is set up.
 */
export function createSubagentWork(opts: {
  dialogId: () => string | null;
  /** The call (or record) the subagent's events name. */
  toolCallId: () => string;
  /** The subagent's chat, if known. */
  childId: () => string | null;
  /** Its chat once it has finished, whose live entry goes. */
  finishedChildId: () => string | null | undefined;
  /** More to forget alongside, in the same effect (a group's workers). */
  alsoForget?: () => void;
}): SubagentWork {
  const [shown, setShown] = createSignal(false);
  createEffect(() => {
    const id = opts.finishedChildId();
    if (id) forgetLive(opts.dialogId(), opts.toolCallId(), id);
    opts.alsoForget?.();
  });
  const hold = transcriptHold();
  onCleanup(hold.release);
  const toggle = () => {
    const id = opts.childId();
    if (!id) return;
    const dialogId = opts.dialogId();
    if (!shown() && dialogId) void hold.open(id, dialogId, opts.toolCallId());
    setShown(!shown());
  };
  return { shown, toggle };
}

/** "Show its work" / "Hide its work". */
export function ShowWorkButton(props: { work: SubagentWork }) {
  return (
    <button
      type="button"
      class="link-button"
      aria-expanded={props.work.shown()}
      onClick={props.work.toggle}
    >
      {props.work.shown() ? 'Hide its work' : 'Show its work'}
    </button>
  );
}
