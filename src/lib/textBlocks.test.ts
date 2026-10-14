// textBlocks.ts tests (node:test): how pasted text and attached text files
// are written into a message, and read back out of it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { blockMarkup, splitBlocks, withoutBlockMarkup } from './textBlocks';

test('a paste and a file each go on lines of their own, with their line count', () => {
  assert.equal(blockMarkup({ text: 'A\nB' }), '\n<pasted_text lines="2">\nA\nB\n</pasted_text>\n');
  assert.equal(
    blockMarkup({ text: 'x = 1', name: 'main.py' }),
    '\n<attached_file name="main.py" lines="1">\nx = 1\n</attached_file>\n'
  );
  // A name can't break out of its attribute.
  assert.equal(
    blockMarkup({ text: '', name: 'a"b<c>&d\n.md' }),
    '\n<attached_file name="a&quot;b&lt;c&gt;&amp;d .md" lines="1">\n\n</attached_file>\n'
  );
});

test('blocks read back in order, between the text around them', () => {
  const content = `Fix this${blockMarkup({ text: 'A\nB' })}and see${blockMarkup({
    text: '# Notes',
    name: 'my "notes".md',
  })}please`;
  assert.deepEqual(splitBlocks(content), [
    'Fix this\n',
    { text: 'A\nB', lines: 2 },
    '\nand see\n',
    { text: '# Notes', lines: 1, name: 'my "notes".md' },
    '\nplease',
  ]);
  assert.deepEqual(splitBlocks('no blocks here'), ['no blocks here']);
  assert.deepEqual(splitBlocks(''), []);
});

test('a file holding a closing tag of its own ends at its last line', () => {
  const text = 'const close = "</attached_file>";\n</pasted_text>\nend';
  const content = `See${blockMarkup({ text, name: 'tags.ts' })}`;
  assert.deepEqual(splitBlocks(content), ['See\n', { text, lines: 3, name: 'tags.ts' }, '\n']);
});

test('blocks written before line counts, or with a wrong one, end at their closing tag', () => {
  assert.deepEqual(splitBlocks('<pasted_text>\nold\n</pasted_text>'), [{ text: 'old', lines: 1 }]);
  assert.deepEqual(splitBlocks('<pasted_text lines="9">\na\nb\n</pasted_text>!'), [
    { text: 'a\nb', lines: 2 },
    '!',
  ]);
  // Never closed: plain text.
  assert.deepEqual(splitBlocks('<pasted_text lines="1">\nopen'), ['<pasted_text lines="1">\nopen']);
});

test('for people, a paste is its text and a file its name', () => {
  const content = `Compare${blockMarkup({ text: 'A\nB' })}with${blockMarkup({
    text: 'long file',
    name: 'notes.md',
  })}`;
  assert.equal(withoutBlockMarkup(content), 'Compare\nA\nB\nwith\n[notes.md]\n');
});
