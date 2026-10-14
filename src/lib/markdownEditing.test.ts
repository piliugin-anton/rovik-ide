// Typing help in Markdown files (markdownEditing.ts): what Enter does in
// lists and quotes, and bold/italic on and off. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inFencedCode, listEnter, toggleEmphasis } from './markdownEditing';

/** Enter at the end of the line. */
const atEnd = (line: string) => listEnter(line, line.length);

test('Enter in a list item starts the next one', () => {
  assert.deepEqual(atEnd('- milk'), { kind: 'continue', marker: '- ' });
  assert.deepEqual(atEnd('* eggs'), { kind: 'continue', marker: '* ' });
  assert.deepEqual(atEnd('  + nested'), { kind: 'continue', marker: '  + ' });
  assert.deepEqual(atEnd('1. first'), { kind: 'continue', marker: '2. ' });
  assert.deepEqual(atEnd('9) ninth'), { kind: 'continue', marker: '10) ' });
  assert.deepEqual(atEnd('-   wide'), { kind: 'continue', marker: '-   ' });
  // A task's next item is an open task.
  assert.deepEqual(atEnd('- [x] done'), { kind: 'continue', marker: '- [ ] ' });
  assert.deepEqual(atEnd('> - quoted'), { kind: 'continue', marker: '> - ' });
  // In the middle of the item: the rest moves to the new one.
  assert.deepEqual(listEnter('- split here', 7), { kind: 'continue', marker: '- ' });
});

test('Enter on an empty item ends the list', () => {
  assert.deepEqual(atEnd('- '), { kind: 'end', keep: '' });
  assert.deepEqual(atEnd('3. '), { kind: 'end', keep: '' });
  assert.deepEqual(atEnd('- [ ] '), { kind: 'end', keep: '' });
  assert.deepEqual(atEnd('- [ ]'), { kind: 'end', keep: '' });
  assert.deepEqual(atEnd('    - '), { kind: 'end', keep: '' });
  // In a quote, the quote goes on.
  assert.deepEqual(atEnd('> - '), { kind: 'end', keep: '>' });
});

test('quotes continue, and an empty quote line ends the quote', () => {
  assert.deepEqual(atEnd('> said'), { kind: 'continue', marker: '> ' });
  assert.deepEqual(atEnd('>said'), { kind: 'continue', marker: '> ' });
  assert.deepEqual(atEnd('> > deeper'), { kind: 'continue', marker: '> > ' });
  assert.deepEqual(atEnd('> '), { kind: 'end', keep: '' });
  assert.deepEqual(atEnd('> > '), { kind: 'end', keep: '>' });
});

test('an ordinary newline elsewhere', () => {
  assert.equal(atEnd('plain text'), null);
  assert.equal(atEnd('-not a list'), null);
  assert.equal(atEnd('* * *'), null);
  assert.equal(atEnd('- - -'), null);
  assert.equal(atEnd('# Heading'), null);
  // Before the marker ends: the item just moves down.
  assert.equal(listEnter('- item', 0), null);
  assert.equal(listEnter('1. item', 2), null);
});

test('lines inside fenced code are code', () => {
  const lines = ['text', '```yaml', '- a', '```', '- b', '~~~~', '```', '- c', '~~~~', '- d'];
  assert.equal(inFencedCode(lines, 2), true);
  assert.equal(inFencedCode(lines, 4), false);
  // A ``` inside a ~~~~ block doesn't close it.
  assert.equal(inFencedCode(lines, 7), true);
  assert.equal(inFencedCode(lines, 9), false);
});

test('bold and italic go on, and come off again', () => {
  // On: around the selection, which stays on the text.
  assert.deepEqual(toggleEmphasis('a ', 'word', ' b', '**'), {
    removeBefore: 0,
    removeAfter: 0,
    text: '**word**',
    start: 2,
    end: 6,
  });
  // Off: markers just outside the selection.
  assert.deepEqual(toggleEmphasis('a **', 'word', '** b', '**'), {
    removeBefore: 2,
    removeAfter: 2,
    text: 'word',
    start: 0,
    end: 4,
  });
  // Off: markers selected with the text.
  assert.deepEqual(toggleEmphasis('', '*word*', '', '*'), {
    removeBefore: 0,
    removeAfter: 0,
    text: 'word',
    start: 0,
    end: 4,
  });
  // Italic on bold text makes it both; then off again leaves it bold.
  assert.equal(toggleEmphasis('**', 'x', '**', '*').text, '*x*');
  assert.equal(toggleEmphasis('**', 'x', '**', '*').removeBefore, 0);
  assert.deepEqual(toggleEmphasis('***', 'x', '***', '*'), {
    removeBefore: 1,
    removeAfter: 1,
    text: 'x',
    start: 0,
    end: 1,
  });
  // Bold off on bold italic leaves it italic.
  assert.equal(toggleEmphasis('***', 'x', '***', '**').removeBefore, 2);
  // Nothing selected, no word: an empty pair with the cursor inside; again: gone.
  assert.deepEqual(toggleEmphasis('', '', '', '**'), {
    removeBefore: 0,
    removeAfter: 0,
    text: '****',
    start: 2,
    end: 2,
  });
  assert.equal(toggleEmphasis('**', '', '**', '**').removeBefore, 2);
});
