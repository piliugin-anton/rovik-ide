// thinking.ts
//
// How a model's thinking reads in the chat, wherever it shows: a reply's
// own (MessageList's ThinkingBlock), or a connector's among its steps
// (ConnectorRecord): its line's label, and its latest line while it thinks.

/** "Thinking…" while it thinks; then "Thought for 12 s", or "Thought" when its time isn't known. */
export function thoughtLabel(thinking: boolean, ms?: number | null): string {
  if (thinking) return 'Thinking…';
  return ms ? `Thought for ${Math.max(1, Math.round(ms / 1000))} s` : 'Thought';
}

/** Its latest line, the end of a long one: what it's on now. */
export function latestLine(text: string): string {
  const lines = text.trimEnd().split('\n');
  const line = lines[lines.length - 1] ?? '';
  return line.length > 160 ? `…${line.slice(-160)}` : line;
}
