// fileFormatters.ts
//
// Format Document through the project's command-line formatter (Prettier,
// rustfmt, Ruff, gofmt, … — the table is src-tauri/src/formatter.rs). The
// backend picks the formatter per file; this registers it with Monaco for
// that one file, so it wins over Monaco's built-in TS/JSON/CSS/HTML
// formatters there, and those stay for files with no formatter of their own.
//
// The provider returns the whole new text as one edit; Monaco cuts it down
// to the lines that changed, so the cursor and undo history stay sensible.

import * as monaco from 'monaco-editor';
import { formatFile, formatterForFile, type FormatterInfo } from './commands';
import { notify } from './notifications';

const NOTICE_ID = 'file-formatter';

function failed(title: string, message: string) {
  notify({ id: NOTICE_ID, kind: 'error', title, message, timeout: 10_000 });
}

/**
 * Looks up the formatter for the model's file and registers it. Resolves to
 * what was registered (dispose it when the file closes), or null when the
 * file has no formatter of its own.
 */
export async function registerFileFormatter(
  model: monaco.editor.ITextModel,
  path: string,
  language: string,
  root: string | null
): Promise<{ formatter: FormatterInfo | null; disposable: monaco.IDisposable } | null> {
  const { formatter, missing } = await formatterForFile(path, language, root);
  if (!formatter && !missing) return null;
  const name = formatter?.name ?? missing!;
  // An exact path matches the file and nothing else (score() compares a
  // pattern with the file's fsPath before treating it as a glob).
  const selector: monaco.languages.LanguageFilter = { language, pattern: model.uri.fsPath };
  const disposable = monaco.languages.registerDocumentFormattingEditProvider(selector, {
    displayName: name,
    provideDocumentFormattingEdits: async (m, _options, token) => {
      if (!formatter) {
        // The project names a formatter that isn't installed: formatting
        // with another one would only make a noisy diff.
        failed(`${name} isn't installed`, `This project formats its files with ${name}.`);
        return [];
      }
      const version = m.getVersionId();
      const text = m.getValue();
      let out: string;
      try {
        out = await formatFile(path, language, root, text);
      } catch (err) {
        failed(`Couldn't format ${path.split(/[\\/]/).pop()}`, String(err));
        return [];
      }
      // Typed into while it ran: the result is for older text.
      if (token.isCancellationRequested || m.isDisposed() || m.getVersionId() !== version) {
        return [];
      }
      // The file keeps its line endings: tools print LF (gofmt) or follow
      // their own config (Prettier's endOfLine), and a CRLF file on Windows
      // would otherwise come back with mixed or changed ones. An
      // .editorconfig end_of_line still applies on save.
      out = out.replace(/\r\n|\r|\n/g, m.getEOL());
      if (out === text) return [];
      return [{ range: m.getFullModelRange(), text: out }];
    },
  });
  return { formatter, disposable };
}
