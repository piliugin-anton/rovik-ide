// openInEditor.ts
//
// The one way a file opens in the editor, from anywhere: the file tree, a
// search result, a link or a file button in the chat, go to definition,
// the Git tab, the agent's own writes. A request says what to show:
//
//   - where to go (`at`): a place, the line where a snippet of the file
//     is found, or the first change the marks show;
//   - how to show it (`highlight`): a flash of the line that fades, the
//     selection a search result keeps, or nothing but the scroll;
//   - which changes to mark (`marks`): the agent's unreviewed ones, git's,
//     a comparison of two versions, or whichever there is;
//   - whether the editor takes the keyboard (`focus`): the user's own
//     actions do, the agent's opens leave it where it is.
//
// App carries a request out (setEditorOpener). The reveal waits here, by
// path, for the tab's editor, which takes it once it has laid out: a tab
// opened in the background scrolls when it's first shown, and two opens
// in a row don't lose each other.
import { createSignal } from 'solid-js';
import { findSnippetLine } from './codeLocation';
import type { DiffHunk } from './lineDiff';
import type { OpenFile } from '../types';

/** A place in a file: its 1-based line and column, and how many characters. */
export interface Place {
  line: number;
  column?: number;
  length?: number;
}

/** What to show at the place: a flash that fades, a selection, or only the scroll. */
export type Highlight = 'flash' | 'selection' | 'none';

/**
 * A read-only tab comparing two versions of a file: `after` shown, with
 * the lines that differ from `before` marked.
 */
export interface Compare {
  title: string;
  /** Relative to the project, for the language and for refreshing the marks. */
  fileName: string;
  before: string;
  after: string;
  stash?: OpenFile['stash'];
  commit?: OpenFile['commit'];
}

/**
 * Which changes the tab marks: the agent's unreviewed ones, git's, both
 * tried in that order (`auto`, the default), a comparison, or none.
 */
export type Marks = 'auto' | 'agent' | 'git' | 'none' | { compare: Compare };

export interface OpenInEditor {
  /**
   * As the caller has it: absolute, relative to the project root, or in
   * the chat's context folder (`@context/…`); a comparison's id with
   * `marks: { compare }`.
   */
  path: string;
  /** Show the tab (the default), or open it behind the active one. */
  activate?: boolean;
  /**
   * The user asked for it: the editor comes into view and takes the
   * keyboard. The agent's opens leave both where they are.
   */
  focus?: boolean;
  /**
   * Where to go. Without it, a tab opened with marks centers its first
   * change; a tab without changes opens where it was left.
   */
  at?: Place | { snippet: string } | 'first-change';
  /** How to show the place (a flash unless said). */
  highlight?: Highlight;
  marks?: Marks;
  /**
   * With marks, the chat that wrote the file: an isolated chat's write
   * isn't in the project's file yet (a remote chat's is on its host), so
   * the file opens as the chat has it, compared with the project's.
   */
  dialogId?: string | null;
}

type Opener = (request: OpenInEditor) => Promise<void>;

let opener: Opener | null = null;

/** App's: it has the tabs and the project. */
export function setEditorOpener(next: Opener | null): void {
  opener = next;
}

export function openInEditor(request: OpenInEditor): Promise<void> {
  return opener ? opener(request) : Promise.resolve();
}

/** What a file's editor shows once it has the file. */
export interface Reveal {
  line: number;
  column: number;
  length: number;
  highlight: Highlight;
  focus: boolean;
  /** Tells a reveal of the same place from the one before. */
  nonce: number;
}

const [reveals, setReveals] = createSignal<Record<string, Reveal>>({});
let nonces = 0;

/** Asks the editor of `path` to show `at`, now or when its tab is shown. */
export function requestReveal(
  path: string,
  at: Place,
  how: { highlight?: Highlight; focus?: boolean } = {}
): void {
  const reveal: Reveal = {
    line: Math.max(1, Math.floor(at.line)),
    column: Math.max(1, at.column ?? 1),
    length: Math.max(0, at.length ?? 0),
    highlight: how.highlight ?? 'flash',
    focus: how.focus ?? false,
    nonce: ++nonces,
  };
  setReveals((all) => ({ ...all, [path]: reveal }));
}

/** The reveal waiting for `path`'s editor (reactive). */
export function pendingReveal(path: string): Reveal | undefined {
  return reveals()[path];
}

/** The editor showed it: a newer one for the path stays. */
export function takeReveal(path: string, reveal: Reveal): void {
  setReveals(({ [path]: current, ...rest }) =>
    current === reveal || current === undefined ? rest : { ...rest, [path]: current }
  );
}

/** The tab closed: nothing to show any more. */
export function dropReveal(path: string): void {
  if (!reveals()[path]) return;
  setReveals(({ [path]: _gone, ...rest }) => rest);
}

/**
 * The line of the first change in the text the hunks lead to. Hunks count
 * lines of the text before, which the first one shares with the text
 * after: nothing has shifted yet.
 */
export function firstChangedLine(hunks: readonly DiffHunk[]): number | undefined {
  const first = hunks[0];
  return first ? Math.max(1, first.origStart) : undefined;
}

/**
 * The place `at` names in `text`: the given one, where the snippet is
 * found, or the first change of `hunks`. Undefined when there's none.
 */
export function placeOf(
  at: OpenInEditor['at'],
  text: string,
  hunks: readonly DiffHunk[] | undefined
): Place | undefined {
  if (at === undefined) return undefined;
  if (at === 'first-change') {
    const line = firstChangedLine(hunks ?? []);
    return line === undefined ? undefined : { line };
  }
  if ('snippet' in at) {
    const line = findSnippetLine(text, at.snippet);
    return line === null ? undefined : { line };
  }
  return at;
}
