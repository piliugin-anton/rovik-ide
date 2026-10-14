// markdownEditing.ts
//
// Typing help in Markdown files (Editor.tsx adds it to Markdown tabs):
//   - Enter in a list item starts the next item: same bullet, the next
//     number, an empty task box; Enter on an empty item ends the list.
//     Quotes continue the same way. Not inside fenced code.
//   - Ctrl+B and Ctrl+I make the selection (or the word at the cursor)
//     bold or italic, and back.
// The text logic is plain functions (tested in markdownEditing.test.ts);
// addMarkdownEditing wires them to a Monaco editor.
import * as monaco from 'monaco-editor';

export type ListEnter =
  /** Insert this after the cursor's newline: the next item's marker. */
  | { kind: 'continue'; marker: string }
  /** An empty item: the line becomes `keep` (the list ends here). */
  | { kind: 'end'; keep: string };

/*
 * Quote markers, then a list marker: `> - `, `  3. `, `- [ ] `. Groups:
 * 1 indentation and quotes, 2 bullet, 3 number, 4 its delimiter, 5 spacing,
 * 6 task box.
 */
const LIST_ITEM =
  /^((?:[ \t]*>)*[ \t]*)(?:([-*+])|(\d{1,9})([.)]))([ \t]+)(\[[ xX]\](?:[ \t]+|$))?/;
const QUOTE = /^((?:[ \t]*>)+[ \t]?)/;
/** `* * *`, `- - -`: a thematic break, not a list. */
const BREAK = /^(?:[ \t]*>)*[ \t]{0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;

/**
 * What Enter does at `column` (0-based) of `line`, a line of a Markdown
 * list or quote; null for an ordinary newline.
 */
export function listEnter(line: string, column: number): ListEnter | null {
  const item = BREAK.test(line) ? null : LIST_ITEM.exec(line);
  if (item) {
    // Before the marker ends: an ordinary newline pushes the item down.
    if (column < item[0].length) return null;
    const [, lead, bullet, number, delimiter, spacing, task] = item;
    if (line.slice(item[0].length).trim() === '') return { kind: 'end', keep: lead.trimEnd() };
    const next = bullet ?? `${Number(number) + 1}${delimiter}`;
    return { kind: 'continue', marker: `${lead}${next}${spacing}${task ? '[ ] ' : ''}` };
  }
  const quote = QUOTE.exec(line);
  if (quote) {
    if (column < quote[0].length) return null;
    if (line.slice(quote[0].length).trim() === '') {
      // `> > ` ends the inner quote; `> ` the quote.
      return { kind: 'end', keep: quote[1].replace(/[ \t]*>[ \t]*$/, '') };
    }
    return { kind: 'continue', marker: quote[1].endsWith(' ') ? quote[1] : `${quote[1]} ` };
  }
  return null;
}

/** Whether 0-based line `index` of `lines` is inside a fenced code block. */
export function inFencedCode(lines: string[], index: number): boolean {
  let fence: string | null = null;
  for (let i = 0; i < index; i++) {
    const m = /^[ \t]{0,3}(`{3,}|~{3,})/.exec(lines[i]);
    if (!m) continue;
    if (fence === null) fence = m[1];
    // Closed by the same character, at least as many, nothing after.
    else if (m[1][0] === fence[0] && m[1].length >= fence.length && lines[i].trim() === m[1])
      fence = null;
  }
  return fence !== null;
}

export interface EmphasisEdit {
  /** Markers removed from just before and after the selection. */
  removeBefore: number;
  removeAfter: number;
  /** What the selection becomes. */
  text: string;
  /** The selection afterwards, as offsets into `text`. */
  start: number;
  end: number;
}

/** How many `*` end `text` (before) or start it (after), up to 3. */
function stars(text: string, fromEnd: boolean): number {
  let n = 0;
  while (n < 3 && text[fromEnd ? text.length - 1 - n : n] === '*') n++;
  return n;
}

/** Whether a run of `n` stars includes the marker: ** in 2 or 3, * in 1 or 3. */
function marks(n: number, marker: '*' | '**'): boolean {
  return marker === '**' ? n === 2 || n === 3 : n === 1 || n === 3;
}

/**
 * Bold (`**`) or italic (`*`) on or off for `selected`, with the text just
 * before and after it: markers around the selection or at its ends are
 * taken off, else added.
 */
export function toggleEmphasis(
  before: string,
  selected: string,
  after: string,
  marker: '*' | '**'
): EmphasisEdit {
  const m = marker.length;
  const outside = Math.min(stars(before, true), stars(after, false));
  if (marks(outside, marker)) {
    return { removeBefore: m, removeAfter: m, text: selected, start: 0, end: selected.length };
  }
  const inside = Math.min(stars(selected, false), stars(selected, true));
  if (selected.length >= 2 * m + 1 && marks(inside, marker)) {
    const text = selected.slice(m, selected.length - m);
    return { removeBefore: 0, removeAfter: 0, text, start: 0, end: text.length };
  }
  return {
    removeBefore: 0,
    removeAfter: 0,
    text: marker + selected + marker,
    start: m,
    end: m + selected.length,
  };
}

/** Bold or italic on each selection (an empty one takes the word at the cursor). */
function emphasize(editor: monaco.editor.IStandaloneCodeEditor, marker: '*' | '**') {
  const model = editor.getModel();
  const selections = editor.getSelections();
  if (!model || !selections || editor.getOption(monaco.editor.EditorOption.readOnly)) return;
  const plans = selections.map((selection) => {
    let range: monaco.Range = selection;
    // In a word: the whole word, with the cursor kept where it is in it.
    let cursor: number | null = null;
    if (selection.isEmpty()) {
      const word = model.getWordAtPosition(selection.getPosition());
      if (word) {
        cursor = selection.startColumn - word.startColumn;
        range = new monaco.Range(
          selection.startLineNumber,
          word.startColumn,
          selection.startLineNumber,
          word.endColumn
        );
      }
    }
    const start = range.getStartPosition();
    const end = range.getEndPosition();
    const lineStart = model.getLineContent(start.lineNumber).slice(0, start.column - 1);
    const lineEnd = model.getLineContent(end.lineNumber).slice(end.column - 1);
    const edit = toggleEmphasis(lineStart, model.getValueInRange(range), lineEnd, marker);
    const replaced = new monaco.Range(
      start.lineNumber,
      start.column - edit.removeBefore,
      end.lineNumber,
      end.column + edit.removeAfter
    );
    return { range: replaced, edit, cursor };
  });
  editor.pushUndoStop();
  try {
    editor.executeEdits(
      'markdown',
      plans.map((p) => ({ range: p.range, text: p.edit.text })),
      (inverse) =>
        inverse.map((op, i) => {
          const { edit, cursor } = plans[i];
          const at = (offset: number) => offsetIn(op.range, edit.text, offset);
          if (cursor !== null) {
            const p = at(Math.min(edit.text.length, edit.start + cursor));
            return new monaco.Selection(p.lineNumber, p.column, p.lineNumber, p.column);
          }
          const a = at(edit.start);
          const b = at(edit.end);
          return new monaco.Selection(a.lineNumber, a.column, b.lineNumber, b.column);
        })
    );
  } catch (err) {
    // Two cursors in the same word: overlapping edits.
    console.warn('Markdown emphasis:', err);
  }
  editor.pushUndoStop();
}

/** The position `offset` characters into `text`, inserted at `range`'s start. */
function offsetIn(range: monaco.IRange, text: string, offset: number): monaco.Position {
  const head = text.slice(0, offset).split(/\r\n|\r|\n/);
  const lineNumber = range.startLineNumber + head.length - 1;
  const column =
    head.length === 1 ? range.startColumn + head[0].length : head[head.length - 1].length + 1;
  return new monaco.Position(lineNumber, column);
}

/** Enter in a list or quote (listEnter); otherwise the ordinary newline. */
function enter(editor: monaco.editor.IStandaloneCodeEditor) {
  const model = editor.getModel();
  const selection = editor.getSelection();
  const plain = () => editor.trigger('keyboard', 'type', { text: '\n' });
  if (!model || !selection || !selection.isEmpty()) return plain();
  const lineNumber = selection.startLineNumber;
  const line = model.getLineContent(lineNumber);
  const action = listEnter(line, selection.startColumn - 1);
  if (!action) return plain();
  // Lists in code samples are code.
  const above = lineNumber > 1 ? model.getLinesContent().slice(0, lineNumber - 1) : [];
  if (inFencedCode(above, above.length)) return plain();

  editor.pushUndoStop();
  if (action.kind === 'end') {
    const range = new monaco.Range(lineNumber, 1, lineNumber, line.length + 1);
    const column = action.keep.length + 1;
    editor.executeEdits(
      'markdown',
      [{ range, text: action.keep }],
      [new monaco.Selection(lineNumber, column, lineNumber, column)]
    );
  } else {
    const at = selection.getPosition();
    const range = new monaco.Range(at.lineNumber, at.column, at.lineNumber, at.column);
    const text = model.getEOL() + action.marker;
    const column = action.marker.length + 1;
    editor.executeEdits(
      'markdown',
      [{ range, text }],
      [new monaco.Selection(lineNumber + 1, column, lineNumber + 1, column)]
    );
  }
  editor.pushUndoStop();
}

/** Lists, quotes and emphasis for a Markdown file's editor. */
export function addMarkdownEditing(
  editor: monaco.editor.IStandaloneCodeEditor
): monaco.IDisposable {
  const disposables = [
    editor.addAction({
      id: 'rovik.markdown.bold',
      label: 'Markdown: Toggle Bold',
      keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyB],
      precondition: '!editorReadonly',
      run: (ed) => emphasize(ed as monaco.editor.IStandaloneCodeEditor, '**'),
    }),
    editor.addAction({
      id: 'rovik.markdown.italic',
      label: 'Markdown: Toggle Italic',
      keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyI],
      precondition: '!editorReadonly',
      run: (ed) => emphasize(ed as monaco.editor.IStandaloneCodeEditor, '*'),
    }),
    // An action, not addCommand: that one's key would work in every editor.
    editor.addAction({
      id: 'rovik.markdown.enter',
      label: 'Markdown: New Line, Continuing the List',
      keybindings: [monaco.KeyCode.Enter],
      precondition: '!editorReadonly',
      // Not when the suggestions or a snippet take Enter, or with several cursors.
      keybindingContext:
        'editorTextFocus && !suggestWidgetVisible && !inSnippetMode && !editorHasMultipleSelections',
      run: (ed) => enter(ed as monaco.editor.IStandaloneCodeEditor),
    }),
  ];
  return { dispose: () => disposables.forEach((d) => d.dispose()) };
}
