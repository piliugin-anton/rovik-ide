// textBlocks.ts
//
// Text a user message carries as one block: a long paste, or a text file
// attached to it. The chat input shows each as a chip (ChatPanel), the model
// gets it in the message, wrapped in a tag on lines of its own, and the chat
// shows it collapsed (MessageContent):
//
//   <pasted_text lines="2">
//   …
//   </pasted_text>
//
//   <attached_file name="notes.md" lines="12">
//   …
//   </attached_file>
//
// `lines` counts the text's lines, so a block ends where it should even when
// its text holds a closing tag of its own (a file of this app's own code).

export interface TextBlock {
  text: string;
  /** The attached file's name; none for a paste. */
  name?: string;
}

/** A message's text: plain runs, and blocks with their line counts. */
export type ContentPart = string | (TextBlock & { lines: number });

const PASTE_TAG = 'pasted_text';
const FILE_TAG = 'attached_file';
const OPENING_RE = /<(pasted_text|attached_file)((?:\s+[a-z]+="[^"]*")*)\s*>\n?/g;

const ESCAPES: [string, string][] = [
  ['&', '&amp;'],
  ['"', '&quot;'],
  ['<', '&lt;'],
  ['>', '&gt;'],
];

function escapeAttribute(value: string): string {
  return ESCAPES.reduce((s, [raw, escaped]) => s.split(raw).join(escaped), value).replace(
    /[\r\n\t]/g,
    ' '
  );
}

function unescapeAttribute(value: string): string {
  return [...ESCAPES].reverse().reduce((s, [raw, escaped]) => s.split(escaped).join(raw), value);
}

function attributes(raw: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [, key, value] of raw.matchAll(/([a-z]+)="([^"]*)"/g)) {
    out[key] = unescapeAttribute(value);
  }
  return out;
}

/** The block as a message carries it, on lines of its own. */
export function blockMarkup(block: TextBlock): string {
  const lines = block.text.split('\n').length;
  const [tag, attrs] =
    block.name === undefined
      ? [PASTE_TAG, `lines="${lines}"`]
      : [FILE_TAG, `name="${escapeAttribute(block.name)}" lines="${lines}"`];
  return `\n<${tag} ${attrs}>\n${block.text}\n</${tag}>\n`;
}

/**
 * Where the block whose text starts at `start` ends: after its `lines`
 * lines when its closing tag is there, else at the first closing tag. Null
 * when it's never closed.
 */
function blockEnd(
  content: string,
  start: number,
  tag: string,
  lines: number
): { text: string; end: number } | null {
  const closing = `</${tag}>`;
  if (lines > 0) {
    let at = start;
    for (let i = 1; i < lines && at > 0; i++) at = content.indexOf('\n', at) + 1;
    const lineEnd = at > 0 ? content.indexOf('\n', at) : -1;
    if (lineEnd >= 0 && content.startsWith(closing, lineEnd + 1)) {
      return { text: content.slice(start, lineEnd), end: lineEnd + 1 + closing.length };
    }
  }
  const at = content.indexOf(closing, start);
  if (at < 0) return null;
  return { text: content.slice(start, at).replace(/\n$/, ''), end: at + closing.length };
}

/** A message's text split into plain runs and blocks, in order. */
export function splitBlocks(content: string): ContentPart[] {
  const parts: ContentPart[] = [];
  let last = 0;
  OPENING_RE.lastIndex = 0;
  for (let m = OPENING_RE.exec(content); m; m = OPENING_RE.exec(content)) {
    const [opening, tag, raw] = m;
    const attrs = attributes(raw);
    const block = blockEnd(content, m.index + opening.length, tag, Number(attrs.lines) || 0);
    if (!block) continue;
    if (m.index > last) parts.push(content.slice(last, m.index));
    parts.push({
      text: block.text,
      lines: block.text.split('\n').length,
      ...(tag === FILE_TAG ? { name: attrs.name ?? '' } : {}),
    });
    last = block.end;
    OPENING_RE.lastIndex = block.end;
  }
  if (last < content.length) parts.push(content.slice(last));
  return parts;
}

/**
 * A block for people rather than the model: a paste's text, a file's name
 * in brackets.
 */
export function blockText(block: TextBlock): string {
  return block.name === undefined ? block.text : `[${block.name}]`;
}

/** The text with its blocks as people read them (blockText), not as tags. */
export function withoutBlockMarkup(content: string): string {
  return splitBlocks(content)
    .map((part) => (typeof part === 'string' ? part : blockText(part)))
    .join('');
}
