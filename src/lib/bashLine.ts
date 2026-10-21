// Bash's input line, as a terminal sees it. Before 5.3, bash garbles a
// line taller than the terminal: holding Backspace through a long paste
// blanked the row the caret moved up to. Its start file in a terminal here,
// on a cloud host or in WSL (src-tauri/src/terminal/bash.sh), binds two
// keys: one puts the line on one
// row that scrolls sideways, the other puts it there if it's too tall and
// lets it wrap if it fits. The terminal sends the first before a paste that
// may not fit and before it gets smaller, the second after those and after
// deleting, and says nothing else to bash. Bash says where it is (an OSC):
// at a prompt, its line on one row (and how many characters must go before
// it may fit), or wrapping.
//
// Keys go only to bash's line editor at a prompt the file set up, and only
// while every key since has kept it editing the line: a search (Ctrl+R),
// a completion's question, or a program a key starts (fzf, atuin) would take
// them as typed. After any other key, nothing more until the next prompt.

/** The line on one row (bash.sh's `__rovik_one_row`). */
export const ONE_ROW = '\x1b[9001~';
/** On one row if the line is too tall, wrapping if it fits (`__rovik_check`). */
export const CHECK = '\x1b[9003~';
/** The OSC bash says how its line is with: "rovik;prompt;emacs", "rovik;one-row;250", "rovik;wrap". */
export const BASH_LINE_OSC = 7777;

export interface BashLine {
  /** At a prompt of bash.sh's, its line editor reading keys. */
  prompt: boolean;
  /** A key went since the prompt that may have taken the editor elsewhere. */
  away: boolean;
  /** The line is on one row. */
  oneRow: boolean;
  /** How many characters must go, at least, before the line may fit. */
  need: number;
  /**
   * A check sent after a deletion, not answered yet: deleting sends no other
   * meanwhile. A host's bash answers a network's round trip later, and each
   * deletion in between had sent one.
   */
  checking: boolean;
  /** Bash edits in vi mode. */
  vi: boolean;
}

export const NO_LINE: BashLine = {
  prompt: false,
  away: false,
  oneRow: false,
  need: 0,
  checking: false,
  vi: false,
};

/** What bash said, from its OSC's text; null when it isn't bash.sh's. */
export type BashSaid =
  { kind: 'prompt'; vi: boolean } | { kind: 'one-row'; need: number } | { kind: 'wrap' };

export function parseBashSaid(text: string): BashSaid | null {
  const [who, kind, arg] = text.split(';');
  if (who !== 'rovik') return null;
  if (kind === 'prompt') return { kind, vi: arg === 'vi' };
  if (kind === 'one-row') return { kind, need: Number.parseInt(arg ?? '', 10) || 0 };
  if (kind === 'wrap') return { kind };
  return null;
}

export function lineAfter(line: BashLine, said: BashSaid): BashLine {
  switch (said.kind) {
    case 'prompt':
      return { prompt: true, away: false, oneRow: false, need: 0, checking: false, vi: said.vi };
    case 'one-row':
      return { ...line, oneRow: true, need: said.need, checking: false };
    case 'wrap':
      return { ...line, oneRow: false, need: 0, checking: false };
  }
}

/** Bash's line editor is done with the line: it turned bracketed paste off (CSI ? 2004 l). */
export function lineDone(line: BashLine): BashLine {
  return line.prompt ? { ...line, prompt: false } : line;
}

const PASTE_START = '\x1b[200~';
const PASTE_END = '\x1b[201~';

/** Backspace, Ctrl+Backspace (^H), Delete: a character each. */
const DELETES = new Set(['\x7f', '\x08', '\x1b[3~']);
/** Ctrl+W, Ctrl+U, Ctrl+K: a word, or the line's start or end. */
const KILLS = new Set(['\x17', '\x15', '\x0b']);
/** Alt+Backspace, Alt+Ctrl+H, Alt+D: a word. Emacs mode's: in vi mode Escape leaves insert mode. */
const EMACS_KILLS = new Set(['\x1b\x7f', '\x1b\x08', '\x1bd']);
/** Arrows left and right (cursor and application mode), Home, End, by word, Ctrl+A/E/B/F; Ctrl+L, which clears the screen. */
const MOVES = new Set([
  '\x1b[C',
  '\x1b[D',
  '\x1bOC',
  '\x1bOD',
  '\x1b[H',
  '\x1b[F',
  '\x1bOH',
  '\x1bOF',
  '\x1b[1~',
  '\x1b[4~',
  '\x1b[1;5C',
  '\x1b[1;5D',
  '\x01',
  '\x05',
  '\x02',
  '\x06',
  '\x0c',
]);
/** Alt+B, Alt+F, Alt+arrows: by word. */
const EMACS_MOVES = new Set(['\x1bb', '\x1bf', '\x1b[1;3C', '\x1b[1;3D']);

/**
 * What a key the terminal sends does to bash's line, while it edits it:
 * a paste, a deletion of a character, of more (a kill), a move, typing.
 * "other" is any other key: it may take the editor elsewhere.
 */
export type KeyKind = 'paste' | 'delete' | 'kill' | 'move' | 'text' | 'other';

export function keyKind(data: string, vi: boolean): KeyKind {
  if (data.startsWith(PASTE_START) && data.endsWith(PASTE_END)) return 'paste';
  if (DELETES.has(data)) return 'delete';
  if (KILLS.has(data) || (!vi && EMACS_KILLS.has(data))) return 'kill';
  if (MOVES.has(data) || (!vi && EMACS_MOVES.has(data))) return 'move';
  if (data.length > 0 && !/[\u0000-\u001f\u007f-\u009f]/.test(data)) return 'text';
  return 'other';
}

/** What the terminal sends for `data`, and the line once bash has it. */
export interface Sent {
  data: string;
  line: BashLine;
  /** Bash's keys in it: bash answers each, and draws its line anew. */
  keys: number;
}

/**
 * `data` (what the terminal sends: a key, typed text, a paste), with bash's
 * keys around it where they go. `mayOverflow` says whether a paste, its
 * text, may make the line too tall to fit.
 */
export function withBashKeys(
  line: BashLine,
  data: string,
  mayOverflow: (text: string) => boolean
): Sent {
  const as = (line: BashLine, sent = data, keys = 0): Sent => ({ data: sent, line, keys });
  if (!line.prompt || line.away) return as(line);
  switch (keyKind(data, line.vi)) {
    case 'paste': {
      if (line.oneRow) return as(line, data + CHECK, 1);
      const text = data.slice(PASTE_START.length, data.length - PASTE_END.length);
      if (mayOverflow(text)) return as(line, ONE_ROW + data + CHECK, 2);
      return as(line);
    }
    case 'delete': {
      if (!line.oneRow) return as(line);
      const need = line.need - 1;
      if (need > 0 || line.checking) return as({ ...line, need });
      return as({ ...line, need, checking: true }, data + CHECK, 1);
    }
    case 'kill':
      return line.oneRow ? as(line, data + CHECK, 1) : as(line);
    case 'text':
      // A character typed is one more to delete.
      return line.oneRow ? as({ ...line, need: line.need + [...data].length }) : as(line);
    case 'move':
      return as(line);
    case 'other':
      return as({ ...line, away: true });
  }
}

/** The most columns `char` takes: two for one that may be wide (CJK, emoji), eight for a tab, two for ^X. */
function columns(char: string): number {
  const code = char.codePointAt(0) ?? 0;
  if (code === 9) return 8;
  if (code < 0x20 || (code >= 0x7f && code < 0xa0)) return 2;
  return code >= 0x1100 ? 2 : 1;
}

/**
 * Whether pasting `text` (as a terminal sends it: line breaks as CR) at
 * column `x` of a line that takes `lineRows` rows of a `cols`×`rows`
 * terminal may leave it too tall: more rows than all but one. Generous:
 * it may say so of a paste that fits, which then goes on one row and back.
 */
export function pasteMayOverflow(
  text: string,
  cols: number,
  rows: number,
  lineRows: number,
  x: number
): boolean {
  // What follows the caret may move to a row of its own.
  const most = rows - 1 - lineRows - 1;
  let added = 0;
  let col = x;
  for (const char of text) {
    if (char === '\r' || char === '\n') {
      added++;
      col = 0;
    } else {
      col += columns(char);
      if (col >= cols) {
        added += Math.floor(col / cols);
        col %= cols;
      }
    }
    if (added > most) return true;
  }
  return false;
}
