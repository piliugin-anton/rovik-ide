import {
  onMount,
  onCleanup,
  createEffect,
  untrack,
  createRoot,
  createSignal,
  createMemo,
  on,
  Show,
} from 'solid-js';
import type { Accessor } from 'solid-js';
import * as monaco from 'monaco-editor';
import type { RevealRequest } from '../types';
import type { CursorInfo } from '../lib/activeContext';
import { selectedLines } from '../lib/activeContext';
import { selectionReference } from '../lib/fileReferences';
import { addToChat } from '../lib/editorBridge';
import { agentChangesFor, type AgentChange } from '../lib/agentChanges';
import { gitBaseFor } from '../lib/gitMarks';
import { diffLines, splitLines } from '../lib/lineDiff';
import { plural } from '../lib/textUtils';
import {
  ensureProjectIntelliSense,
  isIntelliSenseReady,
  modelUriFor,
  setActiveTypesFile,
} from '../lib/intellisense';
import { touchModel } from '../lib/models';
import { ensureOutlineIntelliSense } from '../lib/outlineIntelliSense';
import { ensureDiagnosticsReporting } from '../lib/diagnostics';
import {
  checkForRustAnalyzerUpdate,
  ensureRustSupport,
  restartRustAnalyzer,
} from '../lib/lsp/rustAnalyzer';
import { pendingEditFor } from '../lib/approvals';
import { editorSettings } from '../lib/editorSettings';
import type { EditorConfigProps } from '../lib/editorconfig';
import { applyIndentation, loadEditorConfig, prepareForSave } from '../lib/editorFormatting';
import { registerFileFormatter } from '../lib/fileFormatters';
import { applyTheme } from '../lib/monacoThemes';
import {
  hunkLine,
  renderEditReview,
  renderReviewUnavailable,
  reviewMatchesModel,
} from './editorReview';
import MarkdownPreview from './MarkdownPreview';
import { headingLine } from '../lib/markdownPreview';
import { addMarkdownEditing } from '../lib/markdownEditing';
import {
  clearHeadingRequest,
  headingRequest,
  isMarkdownLanguage,
  markdownSplit,
  markdownView,
  setMarkdownSplit,
  setMarkdownView,
  settleMarkdownView,
  togglePreview,
  toggleSplit,
  type MarkdownView,
} from '../lib/markdownView';

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
  /** Read-only preview (e.g. stashed content): no edits, with a message. */
  readOnly?: Accessor<boolean>;
  readOnlyMessage?: string;
}

/**
 * Puts outside text (an agent's write, a revert, a reload from disk) into
 * the model as one undoable edit. `setValue` would also clear the undo
 * history, so Ctrl+Z could no longer reach what was typed before.
 */
function replaceKeepingUndo(model: monaco.editor.ITextModel, text: string) {
  model.pushStackElement();
  // Line endings come with the text, as setValue would take them.
  const crlf = text.includes('\r\n');
  if (model.getEOL() !== (crlf ? '\r\n' : '\n')) {
    model.pushEOL(crlf ? monaco.editor.EndOfLineSequence.CRLF : monaco.editor.EndOfLineSequence.LF);
  }
  model.pushEditOperations([], [{ range: model.getFullModelRange(), text }], () => null);
  model.pushStackElement();
}

export default function Editor(props: EditorProps) {
  let containerRef!: HTMLDivElement;
  let shellRef!: HTMLDivElement;
  let previewRef: HTMLDivElement | undefined;

  let editor: monaco.editor.IStandaloneCodeEditor | undefined;
  let disposed = false;
  /** The editor for the Markdown preview, once created. */
  const [instance, setInstance] = createSignal<monaco.editor.IStandaloneCodeEditor>();
  /** Ctrl+S, also from the Markdown preview (set once the editor exists). */
  let saveFile = () => {};

  /*
   * A Markdown file shows its source, the rendered preview beside it, or
   * only the preview (lib/markdownView.ts). An agent's edit waiting for
   * review is reviewed in the source, so a preview-only tab shows the
   * source beside the preview meanwhile.
   */
  const view = createMemo<MarkdownView>(() => {
    if (!isMarkdownLanguage(props.language())) return 'edit';
    const chosen = markdownView(props.currentFilePath);
    return chosen === 'preview' && pendingEditFor(props.currentFilePath)?.edit ? 'split' : chosen;
  });

  // While the TS worker has no project libs, every import looks like an
  // error (TS2307). Cover the editor with a centered "loading" note until
  // definitions arrive instead of flashing false squiggles.
  const needsDefs = (lang: string) => lang === 'typescript' || lang === 'javascript';
  const [defsLoading, setDefsLoading] = createSignal(
    needsDefs(props.language()) && !isIntelliSenseReady(props.projectDir())
  );

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
        replaceKeepingUndo(model, content);
      }
    } else {
      model = monaco.editor.createModel(content, props.language(), uri);
    }

    // Shown again: the most recently used (idle clean-up, lib/models.ts).
    touchModel(uri);

    // From here on the TS worker serves this file from its model (mirror
    // sync). Its tsconfig's `types` decide which global @types the worker sees.
    setActiveTypesFile(props.currentFilePath);

    // Create the editor SYNCHRONOUSLY during mount. The previous version
    // awaited the project's typings first, so the editor did not
    // exist when the content-sync effect below ran - and since `editor`
    // is a plain (non-reactive) variable, that effect never re-ran after
    // creation. The editor got exactly one chance to receive content.
    // Community themes are defineTheme'd lazily (lib/monacoThemes.ts):
    // create with a safe theme, then apply the real one once defined.
    const initialTheme = editorSettings().theme;
    void applyTheme(initialTheme);
    editor = monaco.editor.create(containerRef, {
      model,
      theme: initialTheme === 'hc-black' ? 'hc-black' : 'vs-dark',
      automaticLayout: true,
      fontSize: editorSettings().fontSize,
      wordWrap: editorSettings().wordWrap,
      minimap: {
        enabled: editorSettings().minimap,
      },
      renderWhitespace: editorSettings().renderWhitespace,
      readOnly: props.readOnly?.() ?? false,
      readOnlyMessage: { value: props.readOnlyMessage ?? 'This preview is read-only.' },
    });
    setInstance(editor);

    // Read-only preview tabs (e.g. stashed content): lock the editor.
    // The component remounts per tab (keyed), so the creation flag above
    // covers it; this effect keeps it correct if the flag ever flips.
    createEffect(() => {
      const ro = props.readOnly?.() ?? false;
      if (!editor || disposed) return;
      // Agent-review tabs manage readOnly themselves (pendingEditFor), and
      // put the tab's own setting back when the review ends.
      if (ro) {
        editor.updateOptions({
          readOnly: true,
          readOnlyMessage: { value: props.readOnlyMessage ?? 'This preview is read-only.' },
        });
      }
    });

    // Settings → Editor (theme, font size, display): applied live.
    // applyTheme is global (defineTheme + setTheme); updateOptions targets this instance.
    // Indentation is set on the model instead (lib/editorFormatting.ts).
    createEffect(() => {
      const s = editorSettings();
      if (!editor || disposed) return;
      void applyTheme(s.theme);
      editor.updateOptions({
        fontSize: s.fontSize,
        wordWrap: s.wordWrap,
        minimap: { enabled: s.minimap },
        renderWhitespace: s.renderWhitespace,
      });
    });

    const contentDisposable = editor.onDidChangeModelContent(() => {
      const value = editor!.getValue();

      if (value !== props.content()) {
        props.onChange(value);
      }
    });

    // The file's .editorconfig (null until read): its indentation beats
    // the settings', and its whitespace rules apply on save.
    const [editorConfig, setEditorConfig] = createSignal<EditorConfigProps | null>(null);
    void loadEditorConfig(props.currentFilePath).then((ec) => {
      if (!disposed) setEditorConfig(ec);
    });
    // Only the indentation settings: re-detecting on every theme change
    // would undo indentation the user set for this file meanwhile.
    const indentSettings = createMemo(
      () => {
        const s = editorSettings();
        return {
          tabSize: s.tabSize,
          insertSpaces: s.insertSpaces,
          detectIndentation: s.detectIndentation,
        };
      },
      undefined,
      {
        equals: (a, b) =>
          a.tabSize === b.tabSize &&
          a.insertSpaces === b.insertSpaces &&
          a.detectIndentation === b.detectIndentation,
      }
    );
    createEffect(() => {
      const s = indentSettings();
      const ec = editorConfig() ?? {};
      const m = editor?.getModel();
      if (!m || disposed) return;
      applyIndentation(m, s, ec);
    });

    // The project's command-line formatter for this file, if it has one.
    // Looked up again when the language, the project or the setting changes.
    const useProjectFormatters = createMemo(() => editorSettings().projectFormatters);
    createEffect(() => {
      const language = props.language();
      const root = props.projectDir();
      const m = editor?.getModel();
      if (!useProjectFormatters() || !m) return;
      let registered: monaco.IDisposable | null = null;
      let stale = false;
      onCleanup(() => {
        stale = true;
        registered?.dispose();
      });
      registerFileFormatter(m, props.currentFilePath, language, root)
        .then((r) => {
          if (stale || disposed) r?.disposable.dispose();
          else registered = r?.disposable ?? null;
        })
        .catch((err) => console.warn(`No formatter lookup for ${props.currentFilePath}:`, err));
    });

    let saving = false;
    const save = async () => {
      // A second Ctrl+S while the formatter runs would save the text twice.
      if (saving || !editor) return;
      saving = true;
      try {
        await prepareForSave(editor, editorConfig() ?? {}, editorSettings().formatOnSave);
      } finally {
        saving = false;
      }
      if (!disposed) props.onSave();
    };
    saveFile = () => void save();
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, saveFile);

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

    // Not on a read-only tab (a commit's or a stash's version, a
    // comparison): its path ("commit@…:…") is nothing the agent can read.
    const addToChatWidget = untrack(() => props.readOnly?.() ?? false)
      ? null
      : mountAddToChat(editor, () => ({
          path: props.currentFilePath,
          root: props.projectDir(),
        }));

    onCleanup(() => {
      contentDisposable.dispose();
      selectionDisposable.dispose();
      addToChatWidget?.dispose();
    });

    // Runs for an already-open file and for one that just opened (this
    // effect is created after the editor exists).
    createEffect(() => {
      const request = props.reveal?.();
      if (!request || !editor || request.path !== props.currentFilePath) return;
      const ed = editor;
      const model = ed.getModel();
      if (!model) return;
      const show = () => {
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
        ed.setSelection(selection);
        ed.revealRangeInCenter(selection, monaco.editor.ScrollType.Smooth);
        ed.focus();
        props.onRevealed?.();
      };
      // A Markdown file showing only its preview: the match is in the
      // source, so show the source beside it, once it has its size.
      if (untrack(view) !== 'preview') return show();
      setMarkdownView(props.currentFilePath, 'split', false);
      requestAnimationFrame(() => {
        if (disposed) return;
        ed.layout();
        show();
      });
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
        // Back to the tab's own setting: a read-only preview stays locked.
        if (!disposed) {
          ed.updateOptions({
            readOnly: untrack(() => props.readOnly?.() ?? false),
            readOnlyMessage: { value: props.readOnlyMessage ?? 'This preview is read-only.' },
          });
        }
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

    // Git change marks (lib/gitMarks.ts): recomputed against the base as the
    // text changes, a moment after typing stops.
    const gitMarks = createGitMarks(editor, () => gitBaseFor(props.currentFilePath));
    onCleanup(() => gitMarks.dispose());

    // Definitions load in the background - never block editor creation on
    // them. Once per project (later mounts reuse them). While they load,
    // TS/JS files show a centered loading note (see defsLoading below) and
    // semantic diagnostics stay off (lib/intellisense.ts), so imports don't
    // flash as errors. Tracked on projectDir: a project switch without a
    // remount re-arms the overlay for the new root.
    createEffect(() => {
      const root = props.projectDir();
      if (!needsDefs(props.language()) || isIntelliSenseReady(root)) {
        setDefsLoading(false);
        void ensureProjectIntelliSense(root);
        return;
      }
      setDefsLoading(true);
      void ensureProjectIntelliSense(root).then(
        () => {
          if (!disposed) setDefsLoading(false);
        },
        () => {
          if (!disposed) setDefsLoading(false);
        }
      );
    });

    // Worker-free fallback from the Rust backend's tree-sitter outlines
    // (lib/outlineIntelliSense.ts): document symbols, hover signatures and
    // same-file go-to-definition for every language. Registered once; the
    // TypeScript worker and rust-analyzer keep providing typed results
    // alongside these.
    ensureOutlineIntelliSense();

    // Editor markers (TS worker, rust-analyzer) to the backend, so the
    // agent's `diagnostics` tool sees what the editor shows.
    ensureDiagnosticsReporting(props.projectDir());

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

    // Markdown: the preview's shortcuts (as in VS Code), and typing help.
    if (isMarkdownLanguage(props.language())) {
      settleMarkdownView(props.currentFilePath);
      const markdownActions = [
        editor.addAction({
          id: 'rovik.markdown.togglePreview',
          label: 'Markdown: Toggle Preview',
          keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyV],
          run: () => togglePreview(props.currentFilePath),
        }),
        editor.addAction({
          id: 'rovik.markdown.toggleSplit',
          label: 'Markdown: Open Preview to the Side',
          keybindings: [
            monaco.KeyMod.chord(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyK, monaco.KeyCode.KeyV),
          ],
          run: () => toggleSplit(props.currentFilePath),
        }),
        addMarkdownEditing(editor),
      ];
      onCleanup(() => markdownActions.forEach((a) => a.dispose()));

      // A view picked with the switch or a shortcut: what's shown takes the
      // keys (the source was hidden, or the preview went away).
      const chosenView = createMemo(() => markdownView(props.currentFilePath));
      createEffect(
        on(
          chosenView,
          (chosen) =>
            requestAnimationFrame(() => {
              if (disposed) return;
              if (chosen === 'preview') previewRef?.focus({ preventScroll: true });
              else editor?.focus();
            }),
          { defer: true }
        )
      );

      // A link to a heading of this file, while only the source shows: the
      // preview, when shown, scrolls to it itself.
      createEffect(() => {
        const anchor = headingRequest(props.currentFilePath);
        if (anchor === undefined || view() !== 'edit' || !editor) return;
        clearHeadingRequest(props.currentFilePath);
        const line = headingLine(untrack(props.content), anchor);
        if (line === null) return;
        editor.setPosition({ lineNumber: line + 1, column: 1 });
        editor.revealLineNearTop(line + 1);
      });
    }
  });

  /** Dragging the line between the source and the preview. */
  function resizeSplit(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    const box = shellRef.getBoundingClientRect();
    handle.setPointerCapture(e.pointerId);
    document.body.classList.add('is-resizing');
    const move = (ev: PointerEvent) => setMarkdownSplit((ev.clientX - box.left) / box.width, false);
    const end = () => {
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', end);
      handle.removeEventListener('pointercancel', end);
      document.body.classList.remove('is-resizing');
      setMarkdownSplit(markdownSplit(), true);
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', end);
    handle.addEventListener('pointercancel', end);
  }

  // ------------------------------------------
  // External content -> Monaco
  // ------------------------------------------

  createEffect(() => {
    const next = props.content();

    if (!editor || disposed) return;

    const model = editor.getModel();
    if (model && model.getValue() !== next) {
      replaceKeepingUndo(model, next);
    }
  });

  return (
    <div
      ref={shellRef}
      style={{
        position: 'relative',
        height: '100%',
        width: '100%',
        flex: '1',
        'min-height': '0',
        display: 'flex',
      }}
    >
      <div
        ref={containerRef}
        class="editor"
        style={{
          height: '100%',
          width: '100%',
          // Hidden, not removed: the source keeps its undo, review and marks.
          display: view() === 'preview' ? 'none' : undefined,
          flex: view() === 'split' ? `0 0 ${markdownSplit() * 100}%` : undefined,
        }}
      />
      <Show when={view() !== 'edit'}>
        <Show when={view() === 'split'}>
          <div
            class="md-split-divider"
            role="separator"
            aria-orientation="vertical"
            title="Drag to resize; double-click to split evenly"
            onPointerDown={resizeSplit}
            onDblClick={() => setMarkdownSplit(0.5, true)}
          />
        </Show>
        <MarkdownPreview
          path={props.currentFilePath}
          root={props.projectDir}
          content={props.content}
          editor={instance}
          beside={() => view() === 'split'}
          readOnly={() => props.readOnly?.() ?? false}
          onSave={() => saveFile()}
          ref={(el) => (previewRef = el)}
        />
      </Show>
      <Show when={defsLoading()}>
        <div class="editor-loading-overlay">
          <span>Loading definitions…</span>
        </div>
      </Show>
    </div>
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

/** Removed lines shown in full up to this many per place; the rest are counted. */
const MAX_REMOVED_LINES_SHOWN = 200;

/**
 * Marks for differences from the git base: added lines tinted green and
 * changed lines yellow, each with a bar in the gutter, and the lines git
 * has that the file no longer does shown in red where they were (a view
 * zone with the old text), so an edit reads as what it replaced over what
 * it became. Tinted, not just gutter bars: the bars alone were easy to miss.
 */
export function createGitMarks(
  editor: monaco.editor.IStandaloneCodeEditor,
  base: () => string | undefined
): monaco.IDisposable {
  const marks = editor.createDecorationsCollection();
  let zoneIds: string[] = [];
  let timer: number | undefined;
  let disposed = false;
  const update = () => {
    if (disposed) return;
    const model = editor.getModel();
    const original = base();
    const decorations: monaco.editor.IModelDeltaDecoration[] = [];
    const zones: monaco.editor.IViewZone[] = [];
    if (model && original !== undefined) {
      const hunks = diffLines(splitLines(original).lines, splitLines(model.getValue()).lines);
      const lineCount = model.getLineCount();
      // Hunks count lines of the base; `shift` maps them onto the file as it is.
      let shift = 0;
      for (const hunk of hunks) {
        const start = hunk.origStart + shift;
        // A new file diffs as "one empty line replaced": all added.
        const removed =
          hunk.deleted.join('') === '' && hunk.deleted.length === 1 && original === ''
            ? 0
            : hunk.deleted.length;
        if (removed > 0) {
          // The old lines, above the ones that replaced them (or where they were).
          const after = Math.max(0, Math.min(start - 1, lineCount));
          zones.push(removedLinesZone(editor, model, hunk.deleted, after));
        }
        if (hunk.added.length > 0) {
          const kind = removed > 0 ? 'modified' : 'added';
          decorations.push({
            range: new monaco.Range(
              Math.min(start, lineCount),
              1,
              Math.min(start + hunk.added.length - 1, lineCount),
              1
            ),
            options: {
              isWholeLine: true,
              className: `git-line-${kind}`,
              linesDecorationsClassName: `git-mark git-mark-${kind}`,
              stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
              hoverMessage: {
                value:
                  kind === 'added'
                    ? 'Added since the version in git'
                    : `Changed since the version in git: the ${plural(removed, 'line')} it replaced ${removed === 1 ? 'is' : 'are'} shown above in red`,
              },
              overviewRuler: {
                color: kind === 'added' ? '#587c0c' : '#dcb26a',
                position: monaco.editor.OverviewRulerLane.Left,
              },
            },
          });
        } else if (removed > 0) {
          // Only in the overview ruler: the zone shows the lines themselves.
          const line = Math.max(1, Math.min(start, lineCount));
          decorations.push({
            range: new monaco.Range(line, 1, line, 1),
            options: {
              isWholeLine: true,
              stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
              overviewRuler: { color: '#94151b', position: monaco.editor.OverviewRulerLane.Left },
            },
          });
        }
        shift += hunk.added.length - hunk.deleted.length;
      }
    }
    marks.set(decorations);
    editor.changeViewZones((accessor) => {
      zoneIds.forEach((id) => accessor.removeZone(id));
      zoneIds = zones.map((zone) => accessor.addZone(zone));
    });
  };
  const sub = editor.onDidChangeModelContent(() => {
    window.clearTimeout(timer);
    timer = window.setTimeout(update, 250);
  });
  // The base arriving or changing (open from the Git tab, a commit): redraw.
  const stop = createRoot((dispose) => {
    createEffect(() => {
      base();
      update();
    });
    return dispose;
  });
  return {
    dispose() {
      disposed = true;
      window.clearTimeout(timer);
      sub.dispose();
      stop();
      marks.clear();
      editor.changeViewZones((accessor) => zoneIds.forEach((id) => accessor.removeZone(id)));
      zoneIds = [];
    },
  };
}

/**
 * Lines the file had in git and no longer has, in red below `afterLineNumber`
 * (0: at the top), syntax highlighted like the editor's own lines, with a
 * red bar in the gutter where the changed lines have theirs.
 */
function removedLinesZone(
  editor: monaco.editor.IStandaloneCodeEditor,
  model: monaco.editor.ITextModel,
  lines: string[],
  afterLineNumber: number
): monaco.editor.IViewZone {
  const lineHeight = editor.getOption(monaco.editor.EditorOption.lineHeight);
  const fontInfo = editor.getOption(monaco.editor.EditorOption.fontInfo);
  const shown = lines.slice(0, MAX_REMOVED_LINES_SHOWN);
  const hidden = lines.length - shown.length;

  const dom = document.createElement('div');
  dom.className = 'git-removed-zone';
  dom.style.fontFamily = fontInfo.fontFamily;
  dom.style.fontSize = `${fontInfo.fontSize}px`;
  dom.style.lineHeight = `${lineHeight}px`;
  dom.title = `${plural(lines.length, 'line')} removed since the version in git`;
  // Plain text first, highlighted once colorize finishes.
  const rows = shown.map((line) => {
    const row = document.createElement('div');
    row.className = 'git-removed-line';
    row.textContent = line || ' ';
    return row;
  });
  dom.append(...rows);
  if (hidden > 0) {
    const more = document.createElement('div');
    more.className = 'git-removed-more';
    more.textContent = `… ${plural(hidden, 'more removed line')}`;
    dom.append(more);
  }
  void monaco.editor
    .colorize(shown.join('\n'), model.getLanguageId(), { tabSize: model.getOptions().tabSize })
    .then((html) => {
      const parts = html.split(/<br\s*\/?>/);
      rows.forEach((row, i) => {
        if (parts[i]) row.innerHTML = parts[i];
      });
    });

  const margin = document.createElement('div');
  margin.className = 'git-removed-margin';
  const bar = document.createElement('div');
  bar.className = 'git-mark git-mark-removed';
  bar.style.left = `${editor.getLayoutInfo().decorationsLeft}px`;
  margin.append(bar);

  return {
    afterLineNumber,
    heightInPx: (shown.length + (hidden > 0 ? 1 : 0)) * lineHeight,
    domNode: dom,
    marginDomNode: margin,
  };
}

/** Decorations for the agent's changes, on the lines of the file as it is now. */
function agentChangeDecorations(
  change: AgentChange,
  model: monaco.editor.ITextModel
): monaco.editor.IModelDeltaDecoration[] {
  const lineCount = model.getLineCount();
  const out: monaco.editor.IModelDeltaDecoration[] = [];
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
                ? `Changed by the agent (was ${plural(hunk.deleted.length, 'line')})`
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
          hoverMessage: { value: `${plural(hunk.deleted.length, 'line')} removed by the agent` },
        },
      });
    }
    shift += hunk.added.length - hunk.deleted.length;
  }
  return out;
}
