// conflictMerge.ts
//
// The conflict resolver's choices, and the file they make. Each conflict
// of a file (src-tauri/src/git_sync.rs, git_conflict_file) gets a choice:
// the sides taken whole, in the order they were taken; lines picked one by
// one from either side; or text edited by hand. A choice is never final:
// taking or dropping a side or a line again just changes it, and the
// result is recomputed from the choices every time.
//
// To pick lines, a conflict's two sides are lined up in rows (`rowsOf`):
// a line both sides have is one row; lines that differ but look alike (the
// same line, changed) share a row; any other line has a row of its own.

import type { GitConflictFile, GitMergeSegment } from '../types';

export type Side = 'ours' | 'theirs';

/** Lines picked one by one. */
export interface LinePicks {
  /** The picked lines of each side, by index in that side. */
  ours: number[];
  theirs: number[];
  /** How the result orders them: row by row ("rows": in each row, yours
   *  before incoming), or one side's lines before the other's, as sides
   *  taken whole are (the picks started from both sides taken). */
  order: 'rows' | [Side, Side];
}

export interface HunkChoice {
  /** The sides taken, in order: ['theirs', 'ours'] puts theirs first. */
  sides: Side[];
  /** Edited by hand: used instead of the sides. */
  custom?: string;
  /** Lines picked one by one: used instead of the sides. */
  lines?: LinePicks;
}

type Conflict = Extract<GitMergeSegment, { kind: 'conflict' }>;

export function conflictsOf(segments: GitMergeSegment[]): Conflict[] {
  return segments.filter((s): s is Conflict => s.kind === 'conflict');
}

export function isResolved(choice: HunkChoice | undefined): boolean {
  if (!choice) return false;
  if (choice.custom !== undefined) return true;
  if (choice.lines) return choice.lines.ours.length + choice.lines.theirs.length > 0;
  return choice.sides.length > 0;
}

/** Takes a side if it isn't taken (after the ones that are), drops it if
 *  it is. An edit by hand, or lines picked, give way to the sides. */
export function toggleSide(choice: HunkChoice | undefined, side: Side): HunkChoice {
  const sides = choice?.custom === undefined && !choice?.lines ? (choice?.sides ?? []) : [];
  return sides.includes(side)
    ? { sides: sides.filter((s) => s !== side) }
    : { sides: [...sides, side] };
}

/** A text's lines, each with its line ending ("" has none). */
function linesWithEnds(text: string): string[] {
  return text.match(/[^\n]*\n|[^\n]+$/g) ?? [];
}

/** A text's lines, without their endings. */
function bareLines(text: string): string[] {
  return linesWithEnds(text).map((l) => l.replace(/\r?\n$/, ''));
}

/** Joins two pieces of a file so the second starts on a line of its own. */
function joinLines(a: string, b: string): string {
  if (!a || !b || a.endsWith('\n')) return a + b;
  return `${a}\n${b}`;
}

/** A row of a conflict's sides lined up: each side's line there, by its
 *  index in that side (null: none). `same`: both sides have this line. */
export interface Row {
  ours: number | null;
  theirs: number | null;
  same: boolean;
}

/** Above this many line pairs, a conflict's sides aren't lined up by
 *  their longest run of shared lines; above the second, lines that differ
 *  aren't paired by likeness. Each line then gets a row of its own. */
const MATCH_LIMIT = 1_000_000;
const PAIR_LIMIT = 40_000;
/** How alike two lines must be to share a row (see `likeness`). */
const PAIR_LIKENESS = 0.5;

/** A line's words, numbers and marks, without the space between them. */
function tokens(line: string): string[] {
  return line.match(/[\p{L}\p{N}_]+|[^\s\p{L}\p{N}_]/gu) ?? [];
}

const isWord = (token: string) => /[\p{L}\p{N}_]/u.test(token);

/** How alike two lines are, 0 to 1: the share of their words they have in
 *  common (of their marks too, when one has no words: `}` and `};`). Two
 *  calls with different arguments aren't alike for their brackets and
 *  quotes. Indentation doesn't count. */
export function likeness(a: string, b: string): number {
  let ta = tokens(a);
  let tb = tokens(b);
  const wa = ta.filter(isWord);
  const wb = tb.filter(isWord);
  if (wa.length && wb.length) {
    ta = wa;
    tb = wb;
  }
  if (!ta.length || !tb.length) return !ta.length && !tb.length ? 1 : 0;
  const counts = new Map<string, number>();
  for (const t of ta) counts.set(t, (counts.get(t) ?? 0) + 1);
  let common = 0;
  for (const t of tb) {
    const n = counts.get(t);
    if (n) {
      common++;
      counts.set(t, n - 1);
    }
  }
  return (2 * common) / (ta.length + tb.length);
}

/** The longest run of lines `a` and `b` share, in order: their indices. */
function sharedLines(a: string[], b: string[]): [number, number][] {
  const n = a.length;
  const m = b.length;
  if (!n || !m || n * m > MATCH_LIMIT) return [];
  const width = m + 1;
  const len = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      len[i * width + j] =
        a[i] === b[j]
          ? len[(i + 1) * width + j + 1] + 1
          : Math.max(len[(i + 1) * width + j], len[i * width + j + 1]);
    }
  }
  const out: [number, number][] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) out.push([i++, j++]);
    else if (len[(i + 1) * width + j] >= len[i * width + j + 1]) i++;
    else j++;
  }
  return out;
}

/** Lines that differ, in order, paired where they look alike: the pairs
 *  with the most likeness in all that keep both sides' order. */
function alikePairs(a: string[], b: string[]): [number, number][] {
  const n = a.length;
  const m = b.length;
  if (!n || !m || n * m > PAIR_LIMIT) return [];
  const width = m + 1;
  const best = new Float64Array((n + 1) * width);
  const like = new Float64Array(n * m);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      const l = likeness(a[i], b[j]);
      like[i * m + j] = l;
      let v = Math.max(best[(i + 1) * width + j], best[i * width + j + 1]);
      if (l >= PAIR_LIKENESS) v = Math.max(v, best[(i + 1) * width + j + 1] + l);
      best[i * width + j] = v;
    }
  }
  const out: [number, number][] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    const l = like[i * m + j];
    if (l >= PAIR_LIKENESS && best[i * width + j] === best[(i + 1) * width + j + 1] + l) {
      out.push([i++, j++]);
    } else if (best[(i + 1) * width + j] >= best[i * width + j + 1]) i++;
    else j++;
  }
  return out;
}

const rowsCache = new WeakMap<Conflict, Row[]>();

/** A conflict's two sides lined up in rows, top to bottom: each side's
 *  lines in their order. Lines both sides have are one row; between them,
 *  lines alike share a row, and any other line of yours comes before any
 *  other line of incoming. */
export function rowsOf(conflict: Conflict): Row[] {
  const cached = rowsCache.get(conflict);
  if (cached) return cached;
  const a = bareLines(conflict.ours);
  const b = bareLines(conflict.theirs);
  const rows: Row[] = [];
  /** The rows of a run of lines that differ: a[i0..i1) and b[j0..j1). */
  const differing = (i0: number, i1: number, j0: number, j1: number) => {
    const pairs = alikePairs(a.slice(i0, i1), b.slice(j0, j1)).map(
      ([i, j]) => [i + i0, j + j0] as const
    );
    let i = i0;
    let j = j0;
    for (const [pi, pj] of [...pairs, [i1, j1] as const]) {
      for (; i < pi; i++) rows.push({ ours: i, theirs: null, same: false });
      for (; j < pj; j++) rows.push({ ours: null, theirs: j, same: false });
      if (pi < i1) rows.push({ ours: i++, theirs: j++, same: false });
    }
  };
  let i = 0;
  let j = 0;
  for (const [si, sj] of sharedLines(a, b)) {
    differing(i, si, j, sj);
    rows.push({ ours: si, theirs: sj, same: true });
    i = si + 1;
    j = sj + 1;
  }
  differing(i, a.length, j, b.length);
  rowsCache.set(conflict, rows);
  return rows;
}

/** Where two versions of a line differ: the part of each between what
 *  they start and end with alike, not cutting a word in two. */
export function differingParts(
  a: string,
  b: string
): { ours: [number, number]; theirs: [number, number] } {
  const word = (c: string | undefined) => !!c && /[\p{L}\p{N}_]/u.test(c);
  const max = Math.min(a.length, b.length);
  let start = 0;
  while (start < max && a[start] === b[start]) start++;
  while (start > 0 && word(a[start - 1]) && (word(a[start]) || word(b[start]))) start--;
  let end = 0;
  while (end < max - start && a[a.length - 1 - end] === b[b.length - 1 - end]) end++;
  while (
    end > 0 &&
    word(a[a.length - end]) &&
    (word(a[a.length - end - 1]) || word(b[b.length - end - 1]))
  )
    end--;
  return { ours: [start, a.length - end], theirs: [start, b.length - end] };
}

/** Whether `side`'s line `index` is in the result. */
export function isPicked(choice: HunkChoice | undefined, side: Side, index: number): boolean {
  if (!choice || choice.custom !== undefined) return false;
  if (choice.lines) return choice.lines[side].includes(index);
  return choice.sides.includes(side);
}

/** Picks one line of a side, or drops it. Starting from sides taken whole,
 *  their lines stay picked: one side's then go row by row, both sides'
 *  stay in the order the sides were taken. An edit by hand gives way.
 *  Every line of one side and none of the other is that side taken whole;
 *  every line of both, both sides in their order. */
export function toggleLine(
  conflict: Conflict,
  choice: HunkChoice | undefined,
  side: Side,
  index: number
): HunkChoice {
  const count = {
    ours: linesWithEnds(conflict.ours).length,
    theirs: linesWithEnds(conflict.theirs).length,
  };
  const every = (s: Side) => Array.from({ length: count[s] }, (_, i) => i);
  let picks: LinePicks;
  if (choice?.custom === undefined && choice?.lines) {
    picks = { ...choice.lines, ours: [...choice.lines.ours], theirs: [...choice.lines.theirs] };
  } else if (choice?.custom === undefined && choice?.sides.length) {
    const sides = choice.sides;
    picks = {
      ours: sides.includes('ours') ? every('ours') : [],
      theirs: sides.includes('theirs') ? every('theirs') : [],
      order: sides.length === 2 ? [sides[0], sides[1]] : 'rows',
    };
  } else {
    picks = { ours: [], theirs: [], order: 'rows' };
  }
  const list = picks[side];
  const at = list.indexOf(index);
  if (at >= 0) list.splice(at, 1);
  else {
    list.push(index);
    list.sort((x, y) => x - y);
  }
  const whole = (s: Side) => picks[s].length === count[s];
  if (!picks.ours.length && !picks.theirs.length) return { sides: [] };
  if (!picks.theirs.length && whole('ours')) return { sides: ['ours'] };
  if (!picks.ours.length && whole('theirs')) return { sides: ['theirs'] };
  if (picks.order !== 'rows' && whole('ours') && whole('theirs'))
    return { sides: [...picks.order] };
  return { sides: [], lines: picks };
}

/** The text lines picked one by one make. */
function pickedText(conflict: Conflict, picks: LinePicks): string {
  const lines = { ours: linesWithEnds(conflict.ours), theirs: linesWithEnds(conflict.theirs) };
  const has = (side: Side, i: number | null) => i !== null && picks[side].includes(i);
  const out: string[] = [];
  if (picks.order === 'rows') {
    for (const row of rowsOf(conflict)) {
      if (row.same) {
        if (has('ours', row.ours) || has('theirs', row.theirs)) out.push(lines.ours[row.ours!]);
        continue;
      }
      if (has('ours', row.ours)) out.push(lines.ours[row.ours!]);
      if (has('theirs', row.theirs)) out.push(lines.theirs[row.theirs!]);
    }
  } else {
    for (const side of picks.order) for (const i of picks[side]) out.push(lines[side][i]);
  }
  return out.reduce(joinLines, '');
}

/** The text a conflict resolves to ('' while unresolved). */
export function hunkText(conflict: Conflict, choice: HunkChoice | undefined): string {
  if (!choice) return '';
  if (choice.custom !== undefined) return choice.custom;
  if (choice.lines) return pickedText(conflict, choice.lines);
  return choice.sides.map((side) => conflict[side]).reduce(joinLines, '');
}

export interface ComposedFile {
  /** The file as resolved so far (unresolved conflicts contribute nothing). */
  text: string;
  /** Conflicts without a choice yet. */
  unresolved: number;
  /** The resolution is to delete the file: only a side that deleted it was taken. */
  deleted: boolean;
}

/** The file the choices make. `choices[i]` is the i-th conflict's. */
export function composeResult(
  file: Pick<GitConflictFile, 'segments' | 'ours' | 'theirs'>,
  choices: (HunkChoice | undefined)[]
): ComposedFile {
  let text = '';
  let unresolved = 0;
  let index = 0;
  for (const seg of file.segments) {
    if (seg.kind === 'common') {
      text = joinLines(text, seg.text);
      continue;
    }
    const choice = choices[index++];
    if (!isResolved(choice)) unresolved++;
    text = joinLines(text, hunkText(seg, choice));
  }
  // Delete/modify: the one conflict is the whole file.
  let deleted = false;
  if (file.ours === null || file.theirs === null) {
    const choice = choices[0];
    deleted =
      isResolved(choice) &&
      choice!.custom === undefined &&
      !choice!.lines &&
      choice!.sides.every((side) => file[side] === null);
  }
  return { text, unresolved, deleted };
}

/** Every conflict of the file to one side. */
export function allTo(segments: GitMergeSegment[], side: Side): HunkChoice[] {
  return conflictsOf(segments).map(() => ({ sides: [side] }));
}
