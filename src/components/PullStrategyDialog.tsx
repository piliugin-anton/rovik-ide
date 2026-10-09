// PullStrategyDialog.tsx
//
// Asked when a pull has to combine diverged branches and git config
// (pull.rebase / branch.<name>.rebase) doesn't say how: merge, or rebase.
// "Remember" stores the answer as this repository's pull.rebase, the way
// `git config pull.rebase true|false` would.
import { createSignal, onMount } from 'solid-js';
import { Portal } from 'solid-js/web';
import type { PullStrategy } from '../types';

export interface PullStrategyPick {
  strategy: Extract<PullStrategy, 'merge' | 'rebase'>;
  remember: boolean;
}

export function PullStrategyDialog(props: {
  reason: string;
  onPick: (pick: PullStrategyPick) => void;
  onCancel: () => void;
}) {
  const [remember, setRemember] = createSignal(false);
  let first: HTMLButtonElement | undefined;
  onMount(() => first?.focus());

  return (
    <Portal>
      <div
        class="modal-overlay"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) props.onCancel();
        }}
      >
        <div
          class="modal confirm-dialog pull-strategy-dialog"
          role="dialog"
          aria-modal="true"
          aria-label="How to pull"
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.preventDefault();
              e.stopPropagation();
              props.onCancel();
            }
          }}
        >
          <div class="confirm-dialog-body">
            <div class="confirm-dialog-title">Merge or rebase?</div>
            <div class="confirm-dialog-message">{props.reason}</div>
            <div class="pull-strategy-options">
              <button
                ref={first}
                type="button"
                class="pull-strategy-option"
                onClick={() => props.onPick({ strategy: 'merge', remember: remember() })}
              >
                <strong>Merge</strong>
                <span>Keep both histories and join them with a merge commit.</span>
              </button>
              <button
                type="button"
                class="pull-strategy-option"
                onClick={() => props.onPick({ strategy: 'rebase', remember: remember() })}
              >
                <strong>Rebase</strong>
                <span>Replay your commits on top of the remote's, for a straight history.</span>
              </button>
            </div>
            <label class="pull-strategy-remember">
              <input
                type="checkbox"
                checked={remember()}
                onChange={(e) => setRemember(e.currentTarget.checked)}
              />
              Remember for this repository (sets pull.rebase)
            </label>
          </div>
          <div class="confirm-dialog-actions">
            <button type="button" onClick={() => props.onCancel()}>
              Cancel
            </button>
          </div>
        </div>
      </div>
    </Portal>
  );
}
