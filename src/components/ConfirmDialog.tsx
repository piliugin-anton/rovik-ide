// ConfirmDialog.tsx
//
// Reusable confirmation dialog. Tauri's webview doesn't support the native
// window.confirm(), so this replaces it.
//
// Usage (anywhere, no props to thread through):
//
//   if (await confirmDialog({ title: 'Delete chat?', confirmLabel: 'Delete', danger: true })) {
//     ...
//   }
//
// Mount <ConfirmDialogHost /> once near the root of the app.
// <ConfirmDialog> can also be used directly when you need it controlled.
import { createSignal, onCleanup, onMount, Show, type JSX } from 'solid-js';
import { Portal } from 'solid-js/web';

export interface ConfirmOptions {
  title: string;
  /** Detail text under the title. Strings keep their line breaks. */
  message?: JSX.Element;
  confirmLabel?: string;
  cancelLabel?: string;
  /** A third way out, at the left (chooseDialog): "Don't save". */
  alternateLabel?: string;
  /** Destructive action: red confirm button, and Cancel gets initial focus. */
  danger?: boolean;
}

export interface ConfirmDialogProps extends ConfirmOptions {
  onConfirm: () => void;
  onCancel: () => void;
  onAlternate?: () => void;
}

/** What chooseDialog() resolves to. */
export type DialogChoice = 'confirm' | 'alternate' | 'cancel';

let nextId = 0;

export function ConfirmDialog(props: ConfirmDialogProps) {
  const id = `confirm-dialog-${++nextId}`;
  let dialogRef: HTMLDivElement | undefined;
  let confirmRef: HTMLButtonElement | undefined;
  let cancelRef: HTMLButtonElement | undefined;

  // Give focus back to whatever had it (e.g. the delete button) on close.
  const previouslyFocused = document.activeElement as HTMLElement | null;

  onMount(() => {
    // The safe choice gets focus for destructive actions, so a reflexive
    // Enter doesn't delete anything.
    (props.danger ? cancelRef : confirmRef)?.focus();
  });

  onCleanup(() => {
    if (previouslyFocused?.isConnected) previouslyFocused.focus();
  });

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      props.onCancel();
      return;
    }

    // Keep Tab inside the dialog.
    if (e.key === 'Tab' && dialogRef) {
      const focusable = [...dialogRef.querySelectorAll<HTMLElement>('button:not(:disabled)')];
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  return (
    <Portal>
      <div
        class="modal-overlay"
        onMouseDown={(e) => {
          // Only a press that starts on the backdrop itself cancels, so a
          // text selection dragged out of the dialog doesn't.
          if (e.target === e.currentTarget) props.onCancel();
        }}
      >
        <div
          ref={dialogRef}
          class="modal confirm-dialog"
          role="alertdialog"
          aria-modal="true"
          aria-labelledby={`${id}-title`}
          aria-describedby={props.message ? `${id}-message` : undefined}
          onKeyDown={onKeyDown}
        >
          <div class="confirm-dialog-body">
            <div id={`${id}-title`} class="confirm-dialog-title">
              {props.title}
            </div>
            <Show when={props.message}>
              <div id={`${id}-message`} class="confirm-dialog-message">
                {props.message}
              </div>
            </Show>
          </div>

          <div class="confirm-dialog-actions">
            <Show when={props.alternateLabel}>
              <button
                type="button"
                class="confirm-dialog-alternate"
                onClick={() => props.onAlternate?.()}
              >
                {props.alternateLabel}
              </button>
            </Show>
            <button ref={cancelRef} type="button" onClick={() => props.onCancel()}>
              {props.cancelLabel ?? 'Cancel'}
            </button>
            <button
              ref={confirmRef}
              type="button"
              class={props.danger ? 'danger-filled' : 'primary'}
              onClick={() => props.onConfirm()}
            >
              {props.confirmLabel ?? 'OK'}
            </button>
          </div>
        </div>
      </div>
    </Portal>
  );
}

/*
 * ---------------------------------------------------------------------------
 * Imperative API
 * ---------------------------------------------------------------------------
 */

interface PendingConfirm extends ConfirmOptions {
  resolve: (choice: DialogChoice) => void;
}

// Requests made while a dialog is open wait their turn.
const [queue, setQueue] = createSignal<PendingConfirm[]>([]);

/** Shows a confirmation dialog; resolves true on confirm, false otherwise. */
export function confirmDialog(options: ConfirmOptions): Promise<boolean> {
  return chooseDialog(options).then((choice) => choice === 'confirm');
}

/** The same, with its third button (`alternateLabel`) told apart. */
export function chooseDialog(options: ConfirmOptions): Promise<DialogChoice> {
  return new Promise((resolve) => {
    setQueue((q) => [...q, { ...options, resolve }]);
  });
}

function settle(choice: DialogChoice) {
  const [current, ...rest] = queue();
  if (!current) return;
  setQueue(rest);
  current.resolve(choice);
}

/** Renders the dialog for confirmDialog(). Mount once near the app root. */
export function ConfirmDialogHost() {
  // Anything still pending when the host unmounts counts as cancelled.
  onCleanup(() => {
    for (const pending of queue()) pending.resolve('cancel');
    setQueue([]);
  });

  return (
    <Show when={queue()[0]} keyed>
      {(current) => (
        <ConfirmDialog
          {...current}
          onConfirm={() => settle('confirm')}
          onCancel={() => settle('cancel')}
          onAlternate={() => settle('alternate')}
        />
      )}
    </Show>
  );
}
