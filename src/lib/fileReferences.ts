// fileReferences.ts
//
// File references the user adds to a chat message with "@": a whole file,
// or some of its lines (the editor selection), shown as "App.tsx:12-14".
import type { FileSuggestion } from '../types';

/** Identity of a reference: the same file can be mentioned whole and by lines. */
export function referenceKey(ref: FileSuggestion): string {
  return ref.lines ? `${ref.path}#${ref.lines.start}-${ref.lines.end}` : ref.path;
}

/** "12-14", or "12" for one line. */
export function lineRangeLabel(lines: { start: number; end: number }): string {
  return lines.start === lines.end ? `${lines.start}` : `${lines.start}-${lines.end}`;
}

/** How a reference reads in the composer and the sent message: "App.tsx:12-14". */
export function referenceLabel(ref: FileSuggestion): string {
  return ref.lines ? `${ref.name}:${lineRangeLabel(ref.lines)}` : ref.name;
}
