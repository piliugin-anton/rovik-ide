import { createMemo, For } from 'solid-js';

/** Text with the given [start, end) ranges (UTF-16 indexes) wrapped in <mark>. */
export default function HighlightedText(props: {
  text: string;
  ranges: readonly (readonly [number, number])[];
}) {
  const parts = createMemo(() => {
    const { text, ranges } = props;
    const out: { text: string; mark: boolean }[] = [];
    let cursor = 0;
    for (const [start, end] of ranges) {
      if (start < cursor || end <= start) continue;
      if (start > cursor) out.push({ text: text.slice(cursor, start), mark: false });
      out.push({ text: text.slice(start, end), mark: true });
      cursor = end;
    }
    if (cursor < text.length) out.push({ text: text.slice(cursor), mark: false });
    return out;
  });
  return <For each={parts()}>{(part) => (part.mark ? <mark>{part.text}</mark> : part.text)}</For>;
}
