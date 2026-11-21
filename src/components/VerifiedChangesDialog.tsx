import { For, createSignal, onMount, Show } from 'solid-js';
import { Portal } from 'solid-js/web';
import type { HarnessChangeReview } from '../lib/polyglotPlan';
import EditDiffBlock from './EditDiffBlock';

/** The only checkout write in the automatic flow follows this explicit diff review. */
export default function VerifiedChangesDialog(props: {
  review: HarnessChangeReview;
  preview?: boolean;
  onApproveDesign?: () => void;
  destination: string;
  busy: boolean;
  problem?: string;
  exportPath?: string;
  onExport: () => void;
  onClose: () => void;
  onApply: () => void;
}) {
  const [fileIndex, setFileIndex] = createSignal(0);
  const chooseFile = () => props.preview && (props.review.files?.length ?? 0) > 1;
  const title = () => (props.preview ? 'Review proposed changes' : 'Review verified changes');
  let dialog!: HTMLDialogElement;
  onMount(() => dialog.showModal());
  return (
    <Portal>
      <dialog
        ref={dialog}
        class="modal verified-changes-dialog"
        aria-label={title()}
        onCancel={(event) => {
          event.preventDefault();
          if (!props.busy) props.onClose();
        }}
      >
        <header class="modal-header">
          <div class="confirm-dialog-title">{title()}</div>
        </header>
        <div class="verified-changes-files">
          <p class="settings-hint">{props.destination}</p>
          <Show when={chooseFile()}>
            <label class="verified-file-choice">
              Changed file{' '}
              <select
                class="settings-select"
                value={fileIndex()}
                onChange={(event) => setFileIndex(Number(event.currentTarget.value))}
              >
                <For each={props.review.files}>
                  {(file, index) => <option value={index()}>{file.path}</option>}
                </For>
              </select>
            </label>
          </Show>
          <For
            each={
              props.preview
                ? props.review.files?.slice(fileIndex(), fileIndex() + 1)
                : props.review.files
            }
            fallback={<p>No files changed.</p>}
          >
            {(file) => (
              <section>
                <Show when={!chooseFile()}>
                  <h4>{file.path}</h4>
                </Show>
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
                  when={!file.beforeOmitted && !file.afterOmitted && !file.omitted}
                  fallback={<p>{file.beforeOmitted || file.afterOmitted || file.omitted}</p>}
                >
                  <EditDiffBlock
                    changedLinesOnly={props.preview}
                    startLine={props.preview ? 1 : undefined}
                    oldText={file.before ?? ''}
                    newText={file.after ?? ''}
                  />
                </Show>
              </section>
            )}
          </For>
          <Show
            when={props.review.files?.some(
              (file) => file.beforeOmitted || file.afterOmitted || file.omitted
            )}
          >
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
          <Show when={!props.preview || props.onApproveDesign}>
            <button
              type="button"
              class="primary"
              disabled={props.busy || !!props.problem}
              onClick={() => (props.preview ? props.onApproveDesign?.() : props.onApply())}
            >
              {props.preview ? 'Approve design' : 'Apply changes'}
            </button>
          </Show>
        </footer>
      </dialog>
    </Portal>
  );
}
