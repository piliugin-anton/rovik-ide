import { onMount, onCleanup, createEffect } from 'solid-js';
import type { Accessor } from 'solid-js';
import * as monaco from 'monaco-editor';

interface EditorProps {
  content: Accessor<string>;
  language: Accessor<string>;
  onChange: (value: string) => void;
  onSave: () => void;
  onCursorChange?: (info: {
    line: number;
    column: number;
    selectionStartLine: number | null;
    selectionEndLine: number | null;
  }) => void;
}

/**
 * Monaco-based editor with syntax highlighting.
 *
 * Since <App> remounts this component per file (via <Show keyed>), a fresh
 * Monaco instance is created per open file — no need to juggle multiple
 * models here. The createEffect below only exists to guard against the
 * editor's own onChange feedback loop, not to handle file switching.
 */
export default function Editor(props: EditorProps) {
  let containerRef: HTMLDivElement | undefined;
  let editor: monaco.editor.IStandaloneCodeEditor | undefined;

  onMount(() => {
    if (!containerRef) return;

    editor = monaco.editor.create(containerRef, {
      value: props.content(),
      language: props.language(), // TODO: pick based on file extension
      theme: 'vs-dark',
      automaticLayout: true,
      minimap: { enabled: false },
    });

    const disposable = editor.onDidChangeModelContent(() => {
      const value = editor!.getValue();
      if (value !== props.content()) {
        props.onChange(value);
      }
    });

    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
      props.onSave();
    });

    const selectionDisposable = editor.onDidChangeCursorSelection((e) => {
      const sel = e.selection;
      props.onCursorChange?.({
        line: sel.positionLineNumber,
        column: sel.positionColumn,
        selectionStartLine: sel.isEmpty() ? null : sel.startLineNumber,
        selectionEndLine: sel.isEmpty() ? null : sel.endLineNumber,
      });
    });

    // Only push external content changes into the editor — never on the
    // user's own keystrokes, or every character typed would reset the
    // cursor position and blow away undo history.
    createEffect(() => {
      const next = props.content();
      if (editor && editor.getValue() !== next) {
        editor.setValue(next);
      }
    });

    createEffect(() => {
      const model = editor?.getModel();
      if (model) monaco.editor.setModelLanguage(model, props.language());
    });

    onCleanup(() => {
      disposable.dispose();
      selectionDisposable.dispose();
      editor?.dispose();
    });
  });

  return <div ref={containerRef} class="editor" style={{ height: '100%', width: '100%' }} />;
}
