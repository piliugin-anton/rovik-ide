import { onMount, onCleanup, createEffect, untrack } from 'solid-js';
import type { Accessor } from 'solid-js';
import * as monaco from 'monaco-editor';
import type { RevealRequest } from '../types';
import type { CursorInfo } from '../lib/activeContext';
import { selectedLines } from '../lib/activeContext';
import { selectionReference } from '../lib/fileReferences';
import { addToChat } from '../lib/editorBridge';
import { agentChangesFor, type AgentChange } from '../lib/agentChanges';
import { ensureProjectIntelliSense, modelUriFor } from '../lib/intellisense';
import {
  checkForRustAnalyzerUpdate,
  ensureRustSupport,
  restartRustAnalyzer,
} from '../lib/lsp/rustAnalyzer';
import { pendingEditFor } from '../lib/approvals';
import {
  hunkLine,
  renderEditReview,
  renderReviewUnavailable,
  reviewMatchesModel,
} from './editorReview';

interface EditorProps {
  projectDir: Accessor<string | null>;
  currentFilePath: string;
  content: Accessor<string>;
  language: Accessor<string>;
  onChange: (value: string) => void;
  onSave: () => void;
  onCursorChange?: (info: CursorInfo) => void;
  /** Select and center a range (search results); cleared via onRevealed. */
  reveal?: Accessor<RevealRequest | null>;
  onRevealed?: () => void;
}

export default function Editor(props: EditorProps) {
  let containerRef!: HTMLDivElement;

  let editor: monaco.editor.IStandaloneCodeEditor | undefined;
  let disposed = false;

  onMount(() => {
    // Register cleanup FIRST, synchronously - it must run even if the
    // component unmounts while typings are still loading.
    onCleanup(() => {
      disposed = true;
      editor?.dispose();
      editor = undefined;
      // NOTE: we intentionally do NOT dispose the Monaco model here.
      // Models are cached by URI and shared; disposing on tab close is
      // what caused the "reopen -> empty editor" bug.
    });

    // Compiler options and the project's files: lib/intellisense.ts.
    const uri = modelUriFor(props.currentFilePath);

    // Monaco caches models by URI. Reuse a live cached model if there is
    // one, but never a disposed one - this cache is a known race area
    // (see microsoft/monaco-editor issue #1715, "Race when model is
    // disposed": getModel can hand back a model around disposal time).
    let model = monaco.editor.getModel(uri);
    if (model && model.isDisposed()) {
      model = null;
    }

    const content = props.content();
    if (model) {
      // A cached model may hold stale content/language (e.g. the file
      // changed on disk while the tab was closed). Bring it in sync with
      // the content we just read from disk.
      monaco.editor.setModelLanguage(model, props.language());
      if (model.getValue() !== content) {
        model.setValue(content);
      }
    } else {
      model = monaco.editor.createModel(content, props.language(), uri);
    }

    // Create the editor SYNCHRONOUSLY during mount. The previous version
    // awaited `load_nodejs_project_typings` first, so the editor did not
    // exist when the content-sync effect below ran - and since `editor`
    // is a plain (non-reactive) variable, that effect never re-ran after
    // creation. The editor got exactly one chance to receive content.
    editor = monaco.editor.create(containerRef, {
      model,
      theme: 'vs-dark',
      automaticLayout: true,
      minimap: {
        enabled: false,
      },
    });

    const contentDisposable = editor.onDidChangeModelContent(() => {
      const value = editor!.getValue();

      if (value !== props.content()) {
        props.onChange(value);
      }
    });

    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
      props.onSave();
    });

    const reportCursor = (sel: monaco.Selection) => {
      props.onCursorChange?.({
        path: props.currentFilePath,
        line: sel.positionLineNumber,
        column: sel.positionColumn,
        selection: sel.isEmpty()
          ? null
          : {
              startLine: sel.startLineNumber,
              startColumn: sel.startColumn,
              endLine: sel.endLineNumber,
              endColumn: sel.endColumn,
            },
      });
    };
    const selectionDisposable = editor.onDidChangeCursorSelection((e) => reportCursor(e.selection));
    // Report right away too: without it, the previous tab's cursor and
    // selection stayed in the chat context until the cursor first moved.
    const initial = editor.getSelection();
    if (initial) reportCursor(initial);

    const addToChatWidget = mountAddToChat(editor, () => ({
      path: props.currentFilePath,
      root: props.projectDir(),
    }));

    onCleanup(() => {
      contentDisposable.dispose();
      selectionDisposable.dispose();
      addToChatWidget.dispose();
    });

    // Runs for an already-open file and for one that just opened (this
    // effect is created after the editor exists).
    createEffect(() => {
      const request = props.reveal?.();
      if (!request || !editor || request.path !== props.currentFilePath) return;
      const model = editor.getModel();
      if (!model) return;
      const end = request.column + Math.max(0, request.length);
      // Clamped to the file: a line from an older read may be past its end now.
      const range = model.validateRange(
        new monaco.Range(request.line, request.column, request.line, end)
      );
      const selection = new monaco.Selection(
        range.startLineNumber,
        range.startColumn,
        range.endLineNumber,
        range.endColumn
      );
      editor.setSelection(selection);
      editor.revealRangeInCenter(selection, monaco.editor.ScrollType.Smooth);
      editor.focus();
      props.onRevealed?.();
    });

    /*
     * A pending agent edit of this file (lib/approvals.ts): drawn over the
     * file, redrawn whenever a change is accepted or rejected. Read-only
     * meanwhile, so the changes' line numbers stay valid.
     */
    createEffect(() => {
      const approval = pendingEditFor(props.currentFilePath);
      const ed = editor;
      if (!approval?.edit || !ed) return;
      // Re-checked when the content changes (e.g. the user saves or undoes).
      props.content();
      const model = ed.getModel();
      if (!model) return;

      if (!reviewMatchesModel(model, approval)) {
        const handle = renderReviewUnavailable(ed, approval);
        onCleanup(() => handle.dispose());
        return;
      }

      // Track every change's state so the drawing follows decisions.
      approval.edit.hunks.forEach((h) => h.state);
      const handle = renderEditReview(ed, approval);
      ed.updateOptions({
        readOnly: true,
        readOnlyMessage: { value: 'Accept or reject the agent’s edit first.' },
      });
      onCleanup(() => {
        handle.dispose();
        if (!disposed) ed.updateOptions({ readOnly: false });
      });
    });

    // Scroll to the next undecided change when the review asks for it.
    createEffect(() => {
      const approval = pendingEditFor(props.currentFilePath);
      const nonce = approval?.edit?.revealNonce;
      if (!approval?.edit || !nonce || !editor) return;
      // Only the nonce triggers a scroll, not each decision.
      const edit = approval.edit;
      const next = untrack(() => edit.hunks.find((h) => h.state === 'pending') ?? edit.hunks[0]);
      if (next) {
        // After the zones are in, so the scroll accounts for them.
        requestAnimationFrame(() =>
          editor?.revealLineInCenter(hunkLine(next), monaco.editor.ScrollType.Smooth)
        );
      }
    });

    /*
     * The agent's unreviewed edits (lib/agentChanges.ts): added and changed
     * lines green, removed lines a red mark where they were. Drawn once
     * the editor shows exactly what the agent wrote; after that Monaco keeps
     * them on their lines as the user types. Cleared when the file is saved.
     */
    let agentMarks: monaco.editor.IEditorDecorationsCollection | undefined;
    let drawnFor: AgentChange | undefined;
    createEffect(() => {
      const change = agentChangesFor(props.currentFilePath);
      props.content(); // re-checked once the new content is in the editor
      const ed = editor;
      const model = ed?.getModel();
      if (!ed || !model) return;
      if (!change) {
        agentMarks?.clear();
        drawnFor = undefined;
        return;
      }
      if (change === drawnFor) return;
      const sameText = (a: string) => a.replace(/\r\n/g, '\n');
      if (sameText(model.getValue()) !== sameText(change.current)) return;
      agentMarks?.clear();
      agentMarks = ed.createDecorationsCollection(agentChangeDecorations(change, model));
      drawnFor = change;
    });
    onCleanup(() => agentMarks?.clear());

    // Load typings in the background - never block editor creation on it.
    // Once per project (later mounts reuse it).
    void ensureProjectIntelliSense(props.projectDir());

    // Rust: rust-analyzer, started with the project's first Rust file.
    if (props.language() === 'rust') {
      ensureRustSupport(props.projectDir());
      const rustActions = [
        editor.addAction({
          id: 'rovik.rustAnalyzer.restart',
          label: 'Restart rust-analyzer',
          contextMenuGroupId: 'z_rust',
          contextMenuOrder: 1,
          run: () => restartRustAnalyzer(),
        }),
        editor.addAction({
          id: 'rovik.rustAnalyzer.checkUpdate',
          label: 'Check for rust-analyzer Updates',
          contextMenuGroupId: 'z_rust',
          contextMenuOrder: 2,
          run: () => checkForRustAnalyzerUpdate(),
        }),
      ];
      onCleanup(() => rustActions.forEach((a) => a.dispose()));
    }
  });

  // ------------------------------------------
  // External content -> Monaco
  // ------------------------------------------

  createEffect(() => {
    const next = props.content();

    if (!editor || disposed) return;

    if (editor.getValue() !== next) {
      editor.setValue(next);
    }
  });

  return (
    <div
      ref={containerRef}
      class="editor"
      style={{
        height: '100%',
        width: '100%',
      }}
    />
  );
}

/**
 * "Add to chat" under a non-empty selection, plus the same command in the
 * editor's context menu. Adds a reference chip ("App.tsx:12-14") to the
 * chat composer, exactly as mentioning the selection with "@" does.
 */
function mountAddToChat(
  editor: monaco.editor.IStandaloneCodeEditor,
  file: () => { path: string; root: string | null }
): monaco.IDisposable {
  const node = document.createElement('div');
  node.className = 'editor-add-to-chat';
  const button = document.createElement('button');
  button.type = 'button';
  button.title = 'Reference the selected lines in the chat';
  button.innerHTML = `<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor"
    stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M4 2.5h8A1.5 1.5 0 0 1 13.5 4v5.5A1.5 1.5 0 0 1 12 11H7.5l-3 2.5V11H4a1.5 1.5 0 0 1-1.5-1.5V4A1.5 1.5 0 0 1 4 2.5Z"/>
    <path d="M8 4.75v3.5M6.25 6.5h3.5"/></svg><span>Add to chat</span>`;
  node.append(button);

  let position: monaco.IPosition | null = null;
  /** Mouse held down while selecting: the button waits until it's released. */
  let selecting = false;
  /** Used for this selection already; shows again once the selection changes. */
  let used = false;

  const widget: monaco.editor.IContentWidget = {
    getId: () => 'rovik.addToChat',
    getDomNode: () => node,
    // null hides it.
    getPosition: () =>
      position && {
        position,
        preference: [
          monaco.editor.ContentWidgetPositionPreference.BELOW,
          monaco.editor.ContentWidgetPositionPreference.ABOVE,
        ],
      },
  };
  editor.addContentWidget(widget);

  function update() {
    const selection = editor.getSelection();
    const model = editor.getModel();
    if (!selection || selection.isEmpty() || !model || selecting || used) {
      position = null;
    } else {
      // Under the selection's last line, lined up with that line's code.
      const { end } = selectedLines({
        startLine: selection.startLineNumber,
        startColumn: selection.startColumn,
        endLine: selection.endLineNumber,
        endColumn: selection.endColumn,
      });
      position = { lineNumber: end, column: model.getLineFirstNonWhitespaceColumn(end) || 1 };
    }
    editor.layoutContentWidget(widget);
  }

  function add() {
    const selection = editor.getSelection();
    if (!selection || selection.isEmpty()) return;
    const { path, root } = file();
    addToChat(
      selectionReference(path, root, {
        startLine: selection.startLineNumber,
        startColumn: selection.startColumn,
        endLine: selection.endLineNumber,
        endColumn: selection.endColumn,
      })
    );
    used = true;
    update();
  }

  // Keep the press away from the editor, or it would move the cursor and
  // drop the selection before the click lands.
  const swallow = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
  };
  button.addEventListener('pointerdown', swallow);
  button.addEventListener('mousedown', swallow);
  button.addEventListener('click', (e) => {
    swallow(e);
    add();
  });

  const stopSelecting = () => {
    if (!selecting) return;
    selecting = false;
    update();
  };
  window.addEventListener('mouseup', stopSelecting);

  const disposables = [
    editor.onMouseDown((e) => {
      if (e.event.leftButton) selecting = true;
    }),
    editor.onDidChangeCursorSelection(() => {
      used = false;
      update();
    }),
    editor.addAction({
      id: 'rovik.addSelectionToChat',
      label: 'Add Selection to Chat',
      precondition: 'editorHasSelection',
      contextMenuGroupId: 'navigation',
      contextMenuOrder: 0,
      run: add,
    }),
  ];

  return {
    dispose() {
      window.removeEventListener('mouseup', stopSelecting);
      disposables.forEach((d) => d.dispose());
      editor.removeContentWidget(widget);
    },
  };
}

/** Decorations for the agent's changes, on the lines of the file as it is now. */
function agentChangeDecorations(
  change: AgentChange,
  model: monaco.editor.ITextModel
): monaco.editor.IModelDeltaDecoration[] {
  const lineCount = model.getLineCount();
  const out: monaco.editor.IModelDeltaDecoration[] = [];
  const plural = (n: number) => `${n} line${n === 1 ? '' : 's'}`;
  // Hunks count lines of the old text; `shift` maps them onto the new one.
  let shift = 0;
  for (const hunk of change.hunks) {
    const start = hunk.origStart + shift;
    if (hunk.added.length > 0) {
      const first = Math.min(start, lineCount);
      const last = Math.min(start + hunk.added.length - 1, lineCount);
      out.push({
        range: new monaco.Range(first, 1, last, 1),
        options: {
          isWholeLine: true,
          className: 'agent-change-line',
          linesDecorationsClassName: 'agent-change-gutter',
          stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
          overviewRuler: {
            color: '#9bb955',
            position: monaco.editor.OverviewRulerLane.Left,
          },
          hoverMessage: {
            // A new file diffs as "one empty line replaced": that's an addition.
            value:
              hunk.deleted.length > 0 && hunk.deleted.join('') !== ''
                ? `Changed by the agent (was ${plural(hunk.deleted.length)})`
                : `Added by the agent`,
          },
        },
      });
    } else if (hunk.deleted.length > 0) {
      // Nothing left to color: a red line where the text was.
      const atEnd = start > lineCount;
      const line = Math.max(1, Math.min(start, lineCount));
      out.push({
        range: new monaco.Range(line, 1, line, 1),
        options: {
          isWholeLine: true,
          className: atEnd ? 'agent-change-removed-below' : 'agent-change-removed-above',
          stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
          overviewRuler: {
            color: '#e06c75',
            position: monaco.editor.OverviewRulerLane.Left,
          },
          hoverMessage: { value: `${plural(hunk.deleted.length)} removed by the agent` },
        },
      });
    }
    shift += hunk.added.length - hunk.deleted.length;
  }
  return out;
}
