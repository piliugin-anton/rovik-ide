export type MessageSegment =
  | { type: 'text'; content: string }
  /** `open`: the closing fence hasn't arrived yet (still streaming). */
  | { type: 'code'; language?: string; content: string; open?: boolean };

// Matches a fully-closed ```lang\n...\n``` block. Non-greedy so multiple
// blocks in one message are split correctly instead of merged into one.
const CLOSED_FENCE_RE = /```([\w+-]*)\n?([\s\S]*?)```/g;

// Matches a fence that has been opened but not yet closed — the case you'll
// see mid-stream, while the model is still typing the code block out.
const OPEN_FENCE_RE = /```([\w+-]*)\n?([\s\S]*)$/;

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
    const [full, lang, code] = match;

    if (match.index > lastIndex) {
      segments.push({ type: 'text', content: content.slice(lastIndex, match.index) });
    }

    segments.push({
      type: 'code',
      language: lang || undefined,
      // Drop the single newline that sits right before the closing fence.
      content: code.replace(/\n$/, ''),
    });

    lastIndex = match.index + full.length;
  }

  const remainder = content.slice(lastIndex);
  const openMatch = remainder.match(OPEN_FENCE_RE);

  if (openMatch) {
    const [, lang, code] = openMatch;
    const before = remainder.slice(0, openMatch.index);

    if (before) {
      segments.push({ type: 'text', content: before });
    }

    segments.push({ type: 'code', language: lang || undefined, content: code, open: true });
  } else if (remainder) {
    segments.push({ type: 'text', content: remainder });
  }

  return segments;
}
