import { For, onMount, Show } from 'solid-js';
import { Portal } from 'solid-js/web';
import type { HarnessChangeReview } from '../lib/polyglotPlan';
import EditDiffBlock from './EditDiffBlock';

/** The only checkout write in the automatic flow follows this explicit diff review. */
export default function VerifiedChangesDialog(props: {
  review: HarnessChangeReview;
  destination: string;
  busy: boolean;
  problem?: string;
  exportPath?: string;
  onExport: () => void;
  onClose: () => void;
  onApply: () => void;
}) {
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  return (
    <Portal>
      <dialog
        ref={dialog}
        class="modal verified-changes-dialog"
        aria-label="Review verified changes"
        onCancel={(event) => {
          event.preventDefault();
          if (!props.busy) props.onClose();
        }}
      >
        <header class="modal-header">
          <div class="confirm-dialog-title">Review verified changes</div>
        </header>
        <div class="verified-changes-files">
          <p class="settings-hint">{props.destination}</p>
          <For each={props.review.files} fallback={<p>No files changed.</p>}>
            {(file) => (
              <section>
                <h4>{file.path}</h4>
                <Show when={file.beforeMode !== file.afterMode}>
                  <p class="settings-hint">
                    {file.beforeMode == null
                      ? 'Added file'
                      : file.afterMode == null
                        ? 'Deleted file'
                        : `File mode ${file.beforeMode.toString(8)} → ${file.afterMode.toString(8)}`}
                  </p>
                </Show>
                <Show
                  when={!file.beforeOmitted && !file.afterOmitted}
                  fallback={<p>{file.beforeOmitted || file.afterOmitted}</p>}
                >
                  <EditDiffBlock oldText={file.before ?? ''} newText={file.after ?? ''} />
                </Show>
              </section>
            )}
          </For>
          <Show when={props.review.files?.some((file) => file.beforeOmitted || file.afterOmitted)}>
            <button
              type="button"
              class="link-button"
              disabled={props.busy}
              onClick={props.onExport}
            >
              Export full review
            </button>
            <Show when={props.exportPath}>
              <p role="status">Review saved to {props.exportPath}</p>
            </Show>
          </Show>
        </div>
        <Show when={props.problem}>
          <p class="settings-status warning" role="alert">
            {props.problem}
          </p>
        </Show>
        <footer class="confirm-dialog-actions">
          <button type="button" disabled={props.busy} onClick={props.onClose}>
            Close
          </button>
          <button
            type="button"
            class="primary"
            disabled={props.busy || !!props.problem}
            onClick={props.onApply}
          >
            Apply changes
          </button>
        </footer>
      </dialog>
    </Portal>
  );
}
