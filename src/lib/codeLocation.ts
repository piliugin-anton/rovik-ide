// codeLocation.ts
//
// Tying code in the chat to project files, for "Open in editor":
//   - which file a code block in a reply belongs to (fenceInfo, pathBefore);
//   - where in that file a piece of code starts (findSnippetLine).
// Pure functions, so they're tested on their own.

/** "src/main.rs", "./a.ts", "Cargo.toml": has a folder or an extension. */
function looksLikePath(s: string): boolean {
  if (!s || s.length > 260 || /^https?:/i.test(s)) return false;
  return (
    /^[\w@.+~-]+(?:[\\/][\w@.+~ -]+)*\.[A-Za-z0-9]{1,10}$/.test(s) ||
    /^[\w@.+~-]+(?:[\\/][\w@.+~-]+)+$/.test(s)
  );
}

/**
 * The language and file a fence's info string names. Covers the usual forms:
 * ```rust src/main.rs   ```rust:src/main.rs   ```ts title="src/a.ts"   ```src/main.rs
 */
export function fenceInfo(info: string): { language?: string; file?: string } {
  const text = info.trim();
  if (!text) return {};
  const attr = /(?:^|\s)(?:title|file|filename|path)=["']?([^"'\s]+)["']?/i.exec(text)?.[1];
  const tokens = text.split(/\s+/);
  const first = tokens[0];
  let language: string | undefined;
  let file = attr;
  const colon = first.indexOf(':');
  if (colon > 0 && !/^[a-z]:[\\/]/i.test(first)) {
    language = first.slice(0, colon);
    file ??= first.slice(colon + 1) || undefined;
  } else if (looksLikePath(first) && first.includes('.')) {
    file ??= first;
  } else if (!first.includes('=')) {
    language = first;
  }
  file ??= tokens.slice(1).find((t) => looksLikePath(t) && /\.[A-Za-z0-9]+$/.test(t));
  return { language, file: file && looksLikePath(file) ? file.replace(/^\.\//, '') : undefined };
}

/**
 * A file named on the last line of text right before a code block, as in
 * "In `src/main.rs`:" or "Update shapes.rs as follows:". The last such
 * name on that line wins; it needs an extension (a folder isn't a file).
 */
export function pathBefore(text: string): string | undefined {
  const line = text.trimEnd().split('\n').pop() ?? '';
  const candidates = [
    ...[...line.matchAll(/`([^`\n]+)`/g)].map((m) => m[1]),
    ...[...line.matchAll(/\*\*([^*\n]+)\*\*/g)].map((m) => m[1]),
    ...line.split(/[\s`*(),;:]+/),
  ]
    .map((c) =>
      c
        .trim()
        .replace(/[.:]+$/, '')
        .replace(/^\.\//, '')
    )
    .filter((c) => looksLikePath(c) && /\.[A-Za-z0-9]+$/.test(c));
  // In order of position on the line: the one closest to the code.
  return candidates.sort((a, b) => line.lastIndexOf(a) - line.lastIndexOf(b)).pop();
}

/**
 * The 1-based line where `snippet` starts in `fileText`, or null.
 *   1. The whole snippet, line by line (trailing spaces ignored).
 *   2. The same, ignoring indentation.
 *   3. Its first distinctive line that occurs exactly once in the file
 *      (snippets often leave parts out with "// ..."), minus its offset.
 */
export function findSnippetLine(fileText: string, snippet: string): number | null {
  const file = fileText.replace(/\r\n?/g, '\n').split('\n');
  const lines = snippet.replace(/\r\n?/g, '\n').split('\n');
  while (lines.length && !lines[0].trim()) lines.shift();
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  if (lines.length === 0) return null;

  for (const norm of [(s: string) => s.trimEnd(), (s: string) => s.trim()]) {
    const want = lines.map(norm);
    const have = file.map(norm);
    for (let i = 0; i + want.length <= have.length; i++) {
      if (want.every((w, j) => w === have[i + j])) return i + 1;
    }
  }

  const fileTrimmed = file.map((l) => l.trim());
  for (let k = 0; k < lines.length; k++) {
    const line = lines[k].trim();
    if (line.replace(/\s/g, '').length < 8 || /^(\/\/|#|\/\*|\*)\s*\.\.\./.test(line)) continue;
    const first = fileTrimmed.indexOf(line);
    if (first >= 0 && fileTrimmed.indexOf(line, first + 1) < 0) return Math.max(1, first + 1 - k);
  }
  return null;
}
