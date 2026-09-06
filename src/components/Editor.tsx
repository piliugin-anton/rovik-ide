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

    // 1. Set up standard compiler baseline defaults first
    monaco.typescript.typescriptDefaults.setCompilerOptions(compilerOptions);
    monaco.typescript.javascriptDefaults.setCompilerOptions(compilerOptions);

    try {
      // 2. Safely call Rust. If node_modules is missing, it returns instantly with []
      const typeLibraries = await invoke<MonacoLibEntry[]>('load_nodejs_project_typings', {
        projectRoot,
      });

      if (typeLibraries.length > 0) {
        // node_modules exists and has valid typings
        typeLibraries.forEach((lib) => {
          monaco.typescript.typescriptDefaults.addExtraLib(lib.content, lib.name);
          monaco.typescript.javascriptDefaults.addExtraLib(lib.content, lib.name);
        });
        console.log(`IntelliSense Loaded: Registered ${typeLibraries.length} local typings.`);
      } else {
        // node_modules is missing or completely empty
        console.warn('IntelliSense Limited: No node_modules folder detected for this project.');
      }
    } catch (err) {
      console.error('Failed to load types:', err);
    }
  });

  // 2. Handle Editor Synchronization in synchronous Effects at the root level
  createEffect(() => {
    if (!containerRef) return;

    // Only instantiate the editor once
    if (!editor) {
      let normalizedPath = props.currentFilePath.replace(/\\/g, '/');
      while (normalizedPath.startsWith('/')) {
        normalizedPath = normalizedPath.substring(1);
      }
      const currentFileUri = monaco.Uri.parse(`file:///${normalizedPath}`);

      model =
        monaco.editor.getModel(currentFileUri) ||
        monaco.editor.createModel(props.content(), props.language(), currentFileUri);

      editor = monaco.editor.create(containerRef, {
        model: model,
        theme: 'vs-dark',
        automaticLayout: true,
        minimap: { enabled: false },
      });

      // Bind basic editor event listeners
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

      // Register cleanups synchronously inside the correct root scope
      onCleanup(() => {
        contentDisposable.dispose();
        selectionDisposable.dispose();
        editor?.dispose();
        model?.dispose();
        editor = undefined;
        model = undefined;
      });
    }
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
    const nextLang = props.language();
    const currentModel = editor?.getModel();
    if (currentModel) {
      monaco.editor.setModelLanguage(currentModel, nextLang);
    }
  });

  return <div ref={containerRef} class="editor" style={{ height: '100%', width: '100%' }} />;
}
