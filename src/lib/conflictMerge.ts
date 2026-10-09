// conflictMerge.ts
//
// The conflict resolver's choices, and the file they make. Each conflict
// of a file (src-tauri/src/git_sync.rs, git_conflict_file) gets a choice:
// the sides taken, in the order they were taken, or text edited by hand.
// A choice is never final: taking or dropping a side again just changes it,
// and the result is recomputed from the choices every time.

import type { GitConflictFile, GitMergeSegment } from '../types';

export type Side = 'ours' | 'theirs';

export interface HunkChoice {
  /** The sides taken, in order: ['theirs', 'ours'] puts theirs first. */
  sides: Side[];
  /** Edited by hand: used instead of the sides. */
  custom?: string;
}

type Conflict = Extract<GitMergeSegment, { kind: 'conflict' }>;

export function conflictsOf(segments: GitMergeSegment[]): Conflict[] {
  return segments.filter((s): s is Conflict => s.kind === 'conflict');
}

export function isResolved(choice: HunkChoice | undefined): boolean {
  return !!choice && (choice.custom !== undefined || choice.sides.length > 0);
}

/** Takes a side if it isn't taken (after the ones that are), drops it if
 *  it is. An edit by hand gives way to the sides. */
export function toggleSide(choice: HunkChoice | undefined, side: Side): HunkChoice {
  const sides = choice?.custom === undefined ? (choice?.sides ?? []) : [];
  return sides.includes(side)
    ? { sides: sides.filter((s) => s !== side) }
    : { sides: [...sides, side] };
}

/** Joins two pieces of a file so the second starts on a line of its own. */
function joinLines(a: string, b: string): string {
  if (!a || !b || a.endsWith('\n')) return a + b;
  return `${a}\n${b}`;
}

/** The text a conflict resolves to ('' while unresolved). */
export function hunkText(conflict: Conflict, choice: HunkChoice | undefined): string {
  if (!choice) return '';
  if (choice.custom !== undefined) return choice.custom;
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
      choice!.sides.every((side) => file[side] === null);
  }
  return { text, unresolved, deleted };
}

/** Every conflict of the file to one side. */
export function allTo(segments: GitMergeSegment[], side: Side): HunkChoice[] {
  return conflictsOf(segments).map(() => ({ sides: [side] }));
}
