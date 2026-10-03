import { fenceInfo } from './codeLocation';
import { languageForFile } from './language';

/** ```src/main.rs names no language: the file's extension does. */
function languageOfFile(file?: string): string | undefined {
  if (!file) return undefined;
  const language = languageForFile(file);
  return language === 'plaintext' ? undefined : language;
}

export type MessageSegment =
  | { type: 'text'; content: string }
  /**
   * `open`: the closing fence hasn't arrived yet (still streaming).
   * `file`: the file the fence names (```rust src/main.rs).
   */
  | { type: 'code'; language?: string; file?: string; content: string; open?: boolean };

// Matches a fully-closed ```lang\n...\n``` block. Non-greedy so multiple
// blocks in one message are split correctly instead of merged into one.
// A fence's info line (```rust src/main.rs) can name a file, so it's taken
// whole when a line break follows it. The second form is the plain one, for
// fences without a line break (```js x```).
const CLOSED_FENCE_RE = /```([^\s`]*)([ \t][^\n`]*)?\n([\s\S]*?)```|```([\w+-]*)\n?([\s\S]*?)```/g;

// Matches a fence that has been opened but not yet closed — the case you'll
// see mid-stream, while the model is still typing the code block out.
const OPEN_FENCE_RE = /```([^\s`]*)([ \t][^\n`]*)?\n([\s\S]*)$|```([\w+-]*)\n?([\s\S]*)$/;

/**
 * Splits raw assistant content into an ordered list of text/code segments.
 *
 * Safe to call on every render of a streaming message: an unterminated
 * fence at the end of the string is treated as an in-progress code block
 * (using whatever content has arrived so far) rather than being left as
 * literal backticks in the text.
 */
export function parseMessageSegments(content: string): MessageSegment[] {
  const segments: MessageSegment[] = [];
  let lastIndex = 0;

  CLOSED_FENCE_RE.lastIndex = 0;

  let match: RegExpExecArray | null;

  while ((match = CLOSED_FENCE_RE.exec(content)) !== null) {
    const full = match[0];
    const lang = match[3] !== undefined ? `${match[1]}${match[2] ?? ''}` : match[4];
    const code = match[3] ?? match[5] ?? '';

    if (match.index > lastIndex) {
      segments.push({ type: 'text', content: content.slice(lastIndex, match.index) });
    }

    const info = fenceInfo(lang);
    segments.push({
      type: 'code',
      language: info.language ?? languageOfFile(info.file),
      file: info.file,
      // Drop the single newline that sits right before the closing fence.
      content: code.replace(/\n$/, ''),
    });

    lastIndex = match.index + full.length;
  }

  const remainder = content.slice(lastIndex);
  const openMatch = remainder.match(OPEN_FENCE_RE);

  if (openMatch) {
    const lang = openMatch[3] !== undefined ? `${openMatch[1]}${openMatch[2] ?? ''}` : openMatch[4];
    const code = openMatch[3] ?? openMatch[5] ?? '';
    const before = remainder.slice(0, openMatch.index);

    if (before) {
      segments.push({ type: 'text', content: before });
    }

    const info = fenceInfo(lang);
    segments.push({
      type: 'code',
      language: info.language ?? languageOfFile(info.file),
      file: info.file,
      content: code,
      open: true,
    });
  } else if (remainder) {
    segments.push({ type: 'text', content: remainder });
  }

  return segments;
}
