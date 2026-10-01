import { createEffect, on, onCleanup, onMount } from 'solid-js';
import * as monaco from 'monaco-editor';

// Map common fence tags (```js, ```py, ...) to the language ids Monaco knows.
const LANGUAGE_ALIASES: Record<string, string> = {
  js: 'javascript',
  jsx: 'javascript',
  ts: 'typescript',
  tsx: 'typescript',
  py: 'python',
  rs: 'rust',
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  yml: 'yaml',
  md: 'markdown',
  html: 'html',
};

function resolveLanguage(lang: string | undefined): string {
  if (!lang) return 'plaintext';

  const normalized = lang.trim().toLowerCase();

  return LANGUAGE_ALIASES[normalized] ?? normalized;
}

const LINE_HEIGHT = 19;
const PADDING = 16;
const MIN_HEIGHT = LINE_HEIGHT + PADDING;
const MAX_HEIGHT = 480;

function heightForLines(lineCount: number): number {
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, lineCount * LINE_HEIGHT + PADDING));
}

export default function CodeBlock(props: { code: string; language?: string }) {
  let containerRef: HTMLDivElement | undefined;
  let editor: monaco.editor.IStandaloneCodeEditor | undefined;
  let resizeObserver: ResizeObserver | undefined;

  onMount(() => {
    if (!containerRef) return;

    containerRef.style.height = `${heightForLines(props.code.split('\n').length)}px`;

    editor = monaco.editor.create(containerRef, {
      value: props.code,
      language: resolveLanguage(props.language),
      readOnly: true,
      domReadOnly: true,
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      automaticLayout: true,
      folding: true,
      lineNumbers: 'off',
      lineNumbersMinChars: 0,
      lineDecorationsWidth: 0,
      glyphMargin: false,
      links: false,
      quickSuggestions: false,
      parameterHints: { enabled: false },
      occurrencesHighlight: 'off',
      selectionHighlight: false,
      matchBrackets: 'never',
      renderLineHighlight: 'none',
      fontSize: 13,
      padding: { top: 8, bottom: 8 },
      theme: 'vs-dark',
      wordWrap: 'off',
      contextmenu: false,
      overviewRulerLanes: 0,
      hideCursorInOverviewRuler: true,
      scrollbar: { alwaysConsumeMouseWheel: false },
      hover: { enabled: 'off' },
    });

    // Covers layout thrash from panel resizes / splitter drags, on top of
    // the automaticLayout option above (which only reacts to its own div).
    resizeObserver = new ResizeObserver(() => editor?.layout());
    resizeObserver.observe(containerRef);
  });

  // Handles the case where this same component instance is reused for a
  // different segment (e.g. Solid recycling <For> items), and also lets a
  // streaming code block grow live as more tokens arrive.
  createEffect(
    on(
      () => [props.code, props.language] as const,
      ([code, language]) => {
        if (!editor) return;

        const model = editor.getModel();

        if (model) {
          const current = model.getValue();
          if (current !== code) {
            if (code.startsWith(current)) {
              /*
               * Streaming only ever appends. Inserting just the new tail
               * keeps Monaco's tokenization of earlier lines; setValue()
               * would re-tokenize the whole block every frame.
               */
              const end = model.getFullModelRange().getEndPosition();
              model.applyEdits([
                {
                  range: new monaco.Range(end.lineNumber, end.column, end.lineNumber, end.column),
                  text: code.slice(current.length),
                },
              ]);
            } else {
              model.setValue(code);
            }
          }

          const languageId = resolveLanguage(language);
          if (model.getLanguageId() !== languageId) {
            monaco.editor.setModelLanguage(model, languageId);
          }
        }

        if (containerRef) {
          containerRef.style.height = `${heightForLines(code.split('\n').length)}px`;
        }
      },
      { defer: true }
    )
  );

  onCleanup(() => {
    resizeObserver?.disconnect();
    editor?.dispose();
  });

  return <div class="code-block" ref={containerRef} />;
}
