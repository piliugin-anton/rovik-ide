import { onMount, onCleanup, createEffect } from 'solid-js';
import type { Accessor } from 'solid-js';
import * as monaco from 'monaco-editor';
import { MonacoLibEntry } from '../types';
import { invoke } from '@tauri-apps/api/core';

interface EditorProps {
  projectDir: Accessor<string | null>;
  currentFilePath: string;
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

export default function Editor(props: EditorProps) {
  let containerRef!: HTMLDivElement;

  let editor: monaco.editor.IStandaloneCodeEditor | undefined;
  let model: monaco.editor.ITextModel | undefined;

  onMount(async () => {
    const projectRoot = props.projectDir();

    if (!projectRoot) return;

    const compilerOptions = {
      target: monaco.typescript.ScriptTarget.ESNext,
      moduleResolution: monaco.typescript.ModuleResolutionKind.NodeJs,
      jsx: monaco.typescript.JsxEmit.Preserve,
      allowNonTsExtensions: true,
      baseUrl: 'file:///',
    };

    monaco.typescript.typescriptDefaults.setCompilerOptions(compilerOptions);
    monaco.typescript.javascriptDefaults.setCompilerOptions(compilerOptions);

    try {
      const typeLibraries = await invoke<MonacoLibEntry[]>('load_nodejs_project_typings', {
        projectRoot,
      });

      for (const lib of typeLibraries) {
        monaco.typescript.typescriptDefaults.addExtraLib(lib.content, lib.name);

        monaco.typescript.javascriptDefaults.addExtraLib(lib.content, lib.name);
      }
    } catch (err) {
      console.error('Failed to load types:', err);
    }

    // ------------------------------------------
    // Create Monaco ONCE
    // ------------------------------------------

    let normalizedPath = props.currentFilePath.replace(/\\/g, '/');

    while (normalizedPath.startsWith('/')) {
      normalizedPath = normalizedPath.substring(1);
    }

    const currentFileUri = monaco.Uri.parse(`file:///${normalizedPath}`);

    model =
      monaco.editor.getModel(currentFileUri) ??
      monaco.editor.createModel(props.content(), props.language(), currentFileUri);

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

    const selectionDisposable = editor.onDidChangeCursorSelection((e) => {
      const sel = e.selection;

      props.onCursorChange?.({
        line: sel.positionLineNumber,
        column: sel.positionColumn,
        selectionStartLine: sel.isEmpty() ? null : sel.startLineNumber,
        selectionEndLine: sel.isEmpty() ? null : sel.endLineNumber,
      });
    });

    // This cleanup belongs to the COMPONENT,
    // not the creation effect.
    onCleanup(() => {
      contentDisposable.dispose();
      selectionDisposable.dispose();

      editor?.dispose();
      model?.dispose();

      editor = undefined;
      model = undefined;
    });
  });

  // ------------------------------------------
  // External content -> Monaco
  // ------------------------------------------

  createEffect(() => {
    const next = props.content();

    if (!editor) return;

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
