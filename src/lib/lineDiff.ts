// lineDiff.ts
//
// Line-level diff for reviewing the agent's file edits: Myers' algorithm on
// the lines that differ (common prefix and suffix trimmed first), grouped
// into hunks that can be accepted or rejected one by one.

export interface DiffHunk {
  /**
   * 1-based line in the original where the hunk starts: the first deleted
   * line, or for a pure insertion, the line the new lines go before
   * (original line count + 1 for an insertion at the end).
   */
  origStart: number;
  deleted: string[];
  added: string[];
}

/** Above this many differing lines the diff is shown as one big hunk. */
const MAX_EDIT_DISTANCE = 2_000;

type Op = 'eq' | 'del' | 'ins';

function myers(a: string[], b: string[]): Op[] | null {
  const n = a.length;
  const m = b.length;
  // At least the difference in length apart: past the limit, no search.
  if (Math.abs(n - m) > MAX_EDIT_DISTANCE) return null;
  // The search stops at the limit, so the diagonals it visits, and the copy
  // of them each step keeps, are bounded by it, not by the texts.
  const max = Math.min(n + m, MAX_EDIT_DISTANCE);
  const off = max + 1;
  const v = new Int32Array(2 * max + 3);
  const trace: Int32Array[] = [];

  for (let d = 0; d <= max; d++) {
    trace.push(v.slice());
    for (let k = -d; k <= d; k += 2) {
      let x =
        k === -d || (k !== d && v[off + k - 1] < v[off + k + 1])
          ? v[off + k + 1]
          : v[off + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[off + k] = x;
      if (x >= n && y >= m) return backtrack(trace, n, m, off);
    }
  }
  return null;
}

function backtrack(trace: Int32Array[], n: number, m: number, off: number): Op[] {
  const ops: Op[] = [];
  let x = n;
  let y = m;
  for (let d = trace.length - 1; d >= 0; d--) {
    const v = trace[d];
    const k = x - y;
    const prevK = k === -d || (k !== d && v[off + k - 1] < v[off + k + 1]) ? k + 1 : k - 1;
    const prevX = v[off + prevK];
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      ops.push('eq');
      x--;
      y--;
    }
    if (d > 0) ops.push(x === prevX ? 'ins' : 'del');
    x = prevX;
    y = prevY;
  }
  return ops.reverse();
}

/** Hunks turning `a` into `b`, in order. */
export function diffLines(a: string[], b: string[]): DiffHunk[] {
  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  let endA = a.length;
  let endB = b.length;
  while (endA > start && endB > start && a[endA - 1] === b[endB - 1]) {
    endA--;
    endB--;
  }

  const midA = a.slice(start, endA);
  const midB = b.slice(start, endB);
  if (midA.length === 0 && midB.length === 0) return [];

  const ops = myers(midA, midB);
  if (!ops) return [{ origStart: start + 1, deleted: midA, added: midB }];

  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;
  let ai = 0;
  let bi = 0;
  for (const op of ops) {
    if (op === 'eq') {
      current = null;
      ai++;
      bi++;
      continue;
    }
    if (!current) {
      current = { origStart: start + ai + 1, deleted: [], added: [] };
      hunks.push(current);
    }
    if (op === 'del') current.deleted.push(midA[ai++]);
    else current.added.push(midB[bi++]);
  }
  return hunks;
}

/** The original with only the hunks `keep` says yes to applied. */
export function applyHunks(
  original: string[],
  hunks: DiffHunk[],
  keep: (hunk: DiffHunk, index: number) => boolean
): string[] {
  const out: string[] = [];
  let i = 0;
  hunks.forEach((hunk, index) => {
    const at = hunk.origStart - 1;
    for (; i < at; i++) out.push(original[i]);
    // A line at a time: spreading a hunk of a big file overflows the stack.
    for (const line of keep(hunk, index) ? hunk.added : hunk.deleted) out.push(line);
    i = at + hunk.deleted.length;
  });
  for (; i < original.length; i++) out.push(original[i]);
  return out;
}

/** Lines without their \r, and the line ending to write them back with. */
export function splitLines(text: string): { lines: string[]; eol: '\n' | '\r\n' } {
  return {
    lines: text.split('\n').map((line) => line.replace(/\r$/, '')),
    eol: text.includes('\r\n') ? '\r\n' : '\n',
  };
}

/** One line of a diff as shown: unchanged, removed or added. */
export interface DiffRow {
  kind: 'same' | 'removed' | 'added';
  /** Index into the old lines (same, removed). */
  oldIndex?: number;
  /** Index into the new lines (same, added). */
  newIndex?: number;
}

/** All lines of `a` and `b` in diff order, each marked same, removed or added. */
export function diffRows(a: string[], b: string[]): DiffRow[] {
  const out: DiffRow[] = [];
  let ai = 0;
  let bi = 0;
  for (const hunk of diffLines(a, b)) {
    for (; ai < hunk.origStart - 1; ai++, bi++)
      out.push({ kind: 'same', oldIndex: ai, newIndex: bi });
    for (let i = 0; i < hunk.deleted.length; i++) out.push({ kind: 'removed', oldIndex: ai++ });
    for (let i = 0; i < hunk.added.length; i++) out.push({ kind: 'added', newIndex: bi++ });
  }
  for (; ai < a.length; ai++, bi++) out.push({ kind: 'same', oldIndex: ai, newIndex: bi });
  return out;
}

/** Lines of a snippet; a trailing line break doesn't add an empty line. */
export function snippetLines(text: string): string[] {
  if (text === '') return [];
  const { lines } = splitLines(text);
  if (text.endsWith('\n')) lines.pop();
  return lines;
}
