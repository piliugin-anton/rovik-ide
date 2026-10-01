import { onMount, onCleanup, createEffect } from 'solid-js';
import type { Accessor } from 'solid-js';
import * as monaco from 'monaco-editor';
import type { MonacoLibEntry, RevealRequest } from '../types';
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
  /** Select and center a range (search results); cleared via onRevealed. */
  reveal?: Accessor<RevealRequest | null>;
  onRevealed?: () => void;
}

function modelUriFor(filePath: string): monaco.Uri {
  let normalizedPath = filePath.replace(/\\/g, '/');
  while (normalizedPath.startsWith('/')) {
    normalizedPath = normalizedPath.substring(1);
  }
  return monaco.Uri.parse(`file:///${normalizedPath}`);
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

    const compilerOptions = {
      target: monaco.typescript.ScriptTarget.ESNext,
      moduleResolution: monaco.typescript.ModuleResolutionKind.NodeJs,
      jsx: monaco.typescript.JsxEmit.Preserve,
      allowNonTsExtensions: true,
      baseUrl: 'file:///',
    };

    monaco.typescript.typescriptDefaults.setCompilerOptions(compilerOptions);
    monaco.typescript.javascriptDefaults.setCompilerOptions(compilerOptions);

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

    const selectionDisposable = editor.onDidChangeCursorSelection((e) => {
      const sel = e.selection;

      props.onCursorChange?.({
        line: sel.positionLineNumber,
        column: sel.positionColumn,
        selectionStartLine: sel.isEmpty() ? null : sel.startLineNumber,
        selectionEndLine: sel.isEmpty() ? null : sel.endLineNumber,
      });
    });

    onCleanup(() => {
      contentDisposable.dispose();
      selectionDisposable.dispose();
    });

    // Runs for an already-open file and for one that just opened (this
    // effect is created after the editor exists).
    createEffect(() => {
      const request = props.reveal?.();
      if (!request || !editor || request.path !== props.currentFilePath) return;
      const end = request.column + Math.max(0, request.length);
      const selection = new monaco.Selection(request.line, request.column, request.line, end);
      editor.setSelection(selection);
      editor.revealRangeInCenter(selection, monaco.editor.ScrollType.Smooth);
      editor.focus();
      props.onRevealed?.();
    });

    // Load typings in the background - never block editor creation on it.
    void loadProjectTypings(props.projectDir());
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

async function loadProjectTypings(projectRoot: string | null): Promise<void> {
  if (!projectRoot) return;

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
}
