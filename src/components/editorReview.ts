// editorReview.ts
//
// Shows a pending agent edit (lib/approvals.ts) inside a Monaco editor:
//
//   - the file stays as it is on disk; lines the edit deletes are red,
//   - the lines it adds appear right below them in green (a view zone, with
//     syntax highlighting),
//   - each change has Accept / Reject buttons, and a bar at the top shows
//     what's left with Accept all / Reject all,
//   - the editor is read-only while the review is open, so line numbers
//     can't shift under the hunks.
//
// `renderEditReview` draws the current state; callers re-run it when the
// state changes (it's cheap: a handful of decorations and zones).
import * as monaco from 'monaco-editor';
import {
  decideRemaining,
  hunkCounts,
  setHunkState,
  type PendingApproval,
  type ReviewHunk,
} from '../lib/approvals';

export interface ReviewHandle {
  dispose: () => void;
}

const STRIP_HEIGHT = 26;

function button(label: string, className: string, onClick: () => void, title?: string) {
  const el = document.createElement('button');
  el.type = 'button';
  el.className = `review-button ${className}`;
  el.textContent = label;
  if (title) el.title = title;
  el.addEventListener('click', (e) => {
    e.preventDefault();
    e.stopPropagation();
    onClick();
  });
  return el;
}

/** Keeps Monaco from treating clicks on our controls as editor clicks. */
function isolate(el: HTMLElement) {
  for (const type of ['mousedown', 'mouseup', 'pointerdown', 'pointerup']) {
    el.addEventListener(type, (e) => e.stopPropagation());
  }
}

/** The line a change sits at, for scrolling. */
export function hunkLine(hunk: ReviewHunk): number {
  return Math.max(1, hunk.origStart);
}

/**
 * The file in the editor no longer matches what the review was computed
 * against (unsaved edits): line numbers would be wrong.
 */
export function reviewMatchesModel(model: monaco.editor.ITextModel, approval: PendingApproval) {
  const edit = approval.edit!;
  const current = model
    .getValue()
    .split('\n')
    .map((l) => l.replace(/\r$/, ''));
  return (
    current.length === edit.baseLines.length && current.every((l, i) => l === edit.baseLines[i])
  );
}

export function renderEditReview(
  editor: monaco.editor.IStandaloneCodeEditor,
  approval: PendingApproval
): ReviewHandle {
  const edit = approval.edit!;
  const model = editor.getModel();
  if (!model) return { dispose: () => {} };

  const { dialogId, toolCallId: id } = approval;
  const lineHeight = editor.getOption(monaco.editor.EditorOption.lineHeight);
  const fontInfo = editor.getOption(monaco.editor.EditorOption.fontInfo);
  const language = model.getLanguageId();
  const total = edit.hunks.length;

  // Deleted lines: red while pending, struck through once accepted.
  const decorations: monaco.editor.IModelDeltaDecoration[] = [];
  edit.hunks.forEach((hunk) => {
    if (hunk.state === 'rejected' || hunk.deleted.length === 0) return;
    const first = hunk.origStart;
    const last = hunk.origStart + hunk.deleted.length - 1;
    decorations.push({
      range: new monaco.Range(first, 1, last, model.getLineMaxColumn(last)),
      options: {
        isWholeLine: true,
        className: hunk.state === 'pending' ? 'review-line-deleted' : 'review-line-deleted done',
        linesDecorationsClassName: 'review-gutter-deleted',
        overviewRuler: {
          color: 'rgba(224, 108, 117, 0.8)',
          position: monaco.editor.OverviewRulerLane.Left,
        },
      },
    });
  });
  const collection = editor.createDecorationsCollection(decorations);

  // Added lines + the change's buttons, in a zone under the deleted lines.
  const zoneIds: string[] = [];
  editor.changeViewZones((accessor) => {
    edit.hunks.forEach((hunk, index) => {
      const showAdded = hunk.state !== 'rejected' && hunk.added.length > 0;
      const dom = document.createElement('div');
      dom.className = `review-zone review-${hunk.state}`;

      if (showAdded) {
        const code = document.createElement('div');
        code.className = 'review-added';
        code.style.fontFamily = fontInfo.fontFamily;
        code.style.fontSize = `${fontInfo.fontSize}px`;
        code.style.lineHeight = `${lineHeight}px`;
        // Plain text first, highlighted once colorize finishes.
        for (const line of hunk.added) {
          const row = document.createElement('div');
          row.className = 'review-added-line';
          row.textContent = line || ' ';
          code.append(row);
        }
        void monaco.editor.colorize(hunk.added.join('\n'), language, {}).then((html) => {
          const lines = html.split(/<br\s*\/?>/);
          code.querySelectorAll<HTMLDivElement>('.review-added-line').forEach((row, i) => {
            if (lines[i] !== undefined && lines[i] !== '') row.innerHTML = lines[i];
          });
        });
        dom.append(code);
      }

      const strip = document.createElement('div');
      strip.className = 'review-strip';
      strip.style.height = `${STRIP_HEIGHT}px`;
      isolate(strip);

      const label = document.createElement('span');
      label.className = 'review-strip-label';
      if (hunk.state === 'pending') {
        label.textContent = `Change ${index + 1} of ${total}`;
        strip.append(
          label,
          button(
            'Accept',
            'accept',
            () => setHunkState(dialogId, id, index, 'accepted'),
            'Keep this change'
          ),
          button(
            'Reject',
            'reject',
            () => setHunkState(dialogId, id, index, 'rejected'),
            'Discard this change'
          )
        );
      } else {
        label.textContent = hunk.state === 'accepted' ? 'Accepted' : 'Rejected';
        strip.append(
          label,
          button('Undo', 'undo', () => setHunkState(dialogId, id, index, 'pending'), 'Decide again')
        );
      }
      dom.append(strip);

      const addedHeight = showAdded ? hunk.added.length * lineHeight : 0;
      zoneIds.push(
        accessor.addZone({
          afterLineNumber: hunk.origStart - 1 + hunk.deleted.length,
          heightInPx: addedHeight + STRIP_HEIGHT,
          domNode: dom,
        })
      );
    });
  });

  // Summary bar in the top-right corner.
  const bar = document.createElement('div');
  bar.className = 'review-bar';
  isolate(bar);
  const { pending, accepted, rejected } = hunkCounts(edit);
  const text = document.createElement('span');
  text.className = 'review-bar-text';
  text.textContent =
    pending === total
      ? `Agent edit · ${total} change${total === 1 ? '' : 's'}`
      : `${pending} of ${total} left · ${accepted} accepted · ${rejected} rejected`;
  bar.append(
    text,
    button(
      'Accept all',
      'accept',
      () => decideRemaining(dialogId, id, 'accepted'),
      'Accept every remaining change'
    ),
    button(
      'Reject all',
      'reject',
      () => decideRemaining(dialogId, id, 'rejected'),
      'Reject every remaining change'
    )
  );
  const widget: monaco.editor.IOverlayWidget = {
    getId: () => `edit-review-bar-${id}`,
    getDomNode: () => bar,
    getPosition: () => ({
      preference: monaco.editor.OverlayWidgetPositionPreference.TOP_RIGHT_CORNER,
    }),
  };
  editor.addOverlayWidget(widget);

  return {
    dispose: () => {
      collection.clear();
      editor.changeViewZones((accessor) => zoneIds.forEach((zone) => accessor.removeZone(zone)));
      editor.removeOverlayWidget(widget);
    },
  };
}

/** Shown instead of the review when the open file has unsaved changes. */
export function renderReviewUnavailable(
  editor: monaco.editor.IStandaloneCodeEditor,
  approval: PendingApproval
): ReviewHandle {
  const bar = document.createElement('div');
  bar.className = 'review-bar warning';
  isolate(bar);
  const text = document.createElement('span');
  text.className = 'review-bar-text';
  text.textContent =
    'This file has unsaved changes, so the agent’s edit can’t be shown here. Save or undo them, or decide from the chat.';
  bar.append(text);
  const widget: monaco.editor.IOverlayWidget = {
    getId: () => `edit-review-bar-${approval.toolCallId}`,
    getDomNode: () => bar,
    getPosition: () => ({
      preference: monaco.editor.OverlayWidgetPositionPreference.TOP_RIGHT_CORNER,
    }),
  };
  editor.addOverlayWidget(widget);
  return { dispose: () => editor.removeOverlayWidget(widget) };
}
