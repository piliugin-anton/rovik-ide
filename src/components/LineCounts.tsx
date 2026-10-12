// LineCounts.tsx
//
// "+3 −1": the lines a write (or a group of writes) added and removed, on a
// tool call's header line (ToolCallBlock, ToolCallGroup).
export default function LineCounts(props: { counts: { added: number; removed: number } }) {
  return (
    <span
      class="tool-call-counts"
      title={`${props.counts.added} lines added, ${props.counts.removed} removed`}
    >
      <span class="added">+{props.counts.added}</span>
      <span class="removed">−{props.counts.removed}</span>
    </span>
  );
}
