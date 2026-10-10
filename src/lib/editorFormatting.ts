// editorFormatting.ts
//
// Per-file indentation and what happens to a file's text on save.
//
// Indentation is a property of each model, never the global editor
// options: Monaco copies global tabSize/insertSpaces into every open model
// whenever they change, which would flatten each file's own settings back
// to the user's defaults. A file gets, in order of precedence: its
// .editorconfig, then what its content shows (detectIndentation), then
// Settings → Editor.
//
// On save: the language's formatter (Monaco's Format Document, when one is
// registered and format on save is on), then the .editorconfig's line
// ending, trailing whitespace and final newline rules.

import * as monaco from 'monaco-editor';
import { readEditorConfigs } from './commands';
import { resolveEditorConfig, type EditorConfigProps } from './editorconfig';
import type { EditorSettings } from './editorSettings';

/** A formatter that hangs doesn't hold up the save longer than this. */
const FORMAT_ON_SAVE_TIMEOUT_MS = 2000;

/** The .editorconfig properties for a file; none when it can't be read. */
export async function loadEditorConfig(path: string): Promise<EditorConfigProps> {
  try {
    return resolveEditorConfig(await readEditorConfigs(path), path);
  } catch (err) {
    console.warn(`Couldn't read .editorconfig for ${path}:`, err);
    return {};
  }
}

type IndentSettings = Pick<EditorSettings, 'tabSize' | 'insertSpaces' | 'detectIndentation'>;

export function applyIndentation(
  model: monaco.editor.ITextModel,
  s: IndentSettings,
  ec: EditorConfigProps
): void {
  if (s.detectIndentation) {
    // Falls back to the given defaults where the content says nothing.
    model.detectIndentation(s.insertSpaces, s.tabSize);
  } else {
    model.updateOptions({
      tabSize: s.tabSize,
      indentSize: s.tabSize,
      insertSpaces: s.insertSpaces,
    });
  }
  const fromConfig: monaco.editor.ITextModelUpdateOptions = {};
  if (ec.indentStyle) fromConfig.insertSpaces = ec.indentStyle === 'space';
  if (ec.tabWidth) fromConfig.tabSize = ec.tabWidth;
  if (ec.indentSize) fromConfig.indentSize = ec.indentSize;
  model.updateOptions(fromConfig);
  // Indenting with tabs: one level is one tab, whatever width it shows at.
  if (!model.getOptions().insertSpaces) model.updateOptions({ indentSize: 'tabSize' });
}

/** Whether the file's language has a formatter (Format Document is enabled). */
export function canFormat(editor: monaco.editor.IStandaloneCodeEditor): boolean {
  return editor.getAction('editor.action.formatDocument')?.isSupported() ?? false;
}

/**
 * Brings the text to its saved form, as undoable edits: formats it (with
 * `format`, when the language has a formatter), then applies the
 * .editorconfig's whitespace rules.
 */
export async function prepareForSave(
  editor: monaco.editor.IStandaloneCodeEditor,
  ec: EditorConfigProps,
  format: boolean
): Promise<void> {
  if (editor.getOption(monaco.editor.EditorOption.readOnly)) return;
  if (format && canFormat(editor)) {
    const run = editor.getAction('editor.action.formatDocument')!.run();
    await Promise.race([
      run.catch((err) => console.warn('Format on save failed:', err)),
      new Promise((resolve) => setTimeout(resolve, FORMAT_ON_SAVE_TIMEOUT_MS)),
    ]);
  }
  const model = editor.getModel();
  if (model && !model.isDisposed()) applyEditorConfigOnSave(model, ec);
}

function applyEditorConfigOnSave(model: monaco.editor.ITextModel, ec: EditorConfigProps): void {
  model.pushStackElement();
  if (ec.endOfLine) {
    const eol =
      ec.endOfLine === 'crlf'
        ? monaco.editor.EndOfLineSequence.CRLF
        : monaco.editor.EndOfLineSequence.LF;
    if (model.getEndOfLineSequence() !== eol) model.pushEOL(eol);
  }
  if (ec.trimTrailingWhitespace) {
    const edits: monaco.editor.IIdentifiedSingleEditOperation[] = [];
    for (let line = 1; line <= model.getLineCount(); line++) {
      const text = model.getLineContent(line);
      const kept = text.trimEnd().length;
      if (kept < text.length) {
        edits.push({
          range: new monaco.Range(line, kept + 1, line, text.length + 1),
          text: null,
        });
      }
    }
    if (edits.length) model.pushEditOperations(null, edits, () => null);
  }
  // `false` asks for no final newline; removing one is left to the
  // project's formatter, as dropping it by hand surprises more than it helps.
  if (ec.insertFinalNewline) {
    const last = model.getLineCount();
    if (model.getLineContent(last) !== '') {
      const end = model.getLineMaxColumn(last);
      model.pushEditOperations(
        null,
        [{ range: new monaco.Range(last, end, last, end), text: model.getEOL() }],
        () => null
      );
    }
  }
  model.pushStackElement();
}
