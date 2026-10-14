// composerClipboard.ts tests (node:test): a copy of chips pastes back as
// chips, through the clipboard's own type or, where a paste can't read it,
// by its text.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { copiedSegments, pastedSegments, writeSegments } from './composerClipboard';
import type { DraftSegment } from './composerSegments';

/** A clipboard as a copy event's DataTransfer has it. */
function clipboard(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  return {
    data,
    transfer: {
      setData: (type: string, value: string) => void data.set(type, value),
      getData: (type: string) => data.get(type) ?? '',
    } as unknown as DataTransfer,
  };
}

const withFile: DraftSegment[] = [
  { type: 'text', text: 'Summarize ' },
  { type: 'pasted', text: 'x'.repeat(5000), name: 'notes.md' },
  { type: 'text', text: ' please' },
  { type: 'pdf', pdf: { id: 'b'.repeat(64), name: 'report.pdf', size: 9, pages: 1 } },
];

test('a copy with chips pastes back as those chips', () => {
  const copy = clipboard();
  writeSegments(copy.transfer, withFile);
  // Other apps get the text, the file by its name: not 5,000 x's.
  assert.equal(copy.data.get('text/plain'), 'Summarize [notes.md] please [report.pdf]');
  assert.deepEqual(pastedSegments(copy.transfer), withFile);
});

test("where a paste can't read the app's type, the copy is known by its text", () => {
  const copy = clipboard();
  writeSegments(copy.transfer, withFile);
  const text = copy.data.get('text/plain')!;
  // The async clipboard API, or a system that gave the text Windows line breaks.
  assert.deepEqual(copiedSegments(text), withFile);
  assert.deepEqual(pastedSegments(clipboard({ 'text/plain': text }).transfer), withFile);
  // Something else copied since: its own text.
  assert.equal(copiedSegments('Summarize notes.md please'), null);
  assert.equal(pastedSegments(clipboard({ 'text/plain': 'other' }).transfer), null);
  assert.equal(copiedSegments(''), null);
});

test('line breaks may come back as \\r\\n', () => {
  const segments: DraftSegment[] = [
    { type: 'text', text: 'one\ntwo ' },
    { type: 'image', image: { id: 'a'.repeat(64) } },
  ];
  writeSegments(clipboard().transfer, segments);
  assert.deepEqual(copiedSegments('one\r\ntwo [image]'), segments);
});

test('a copy of plain text is only text', () => {
  writeSegments(clipboard().transfer, withFile);
  const copy = clipboard();
  writeSegments(copy.transfer, [{ type: 'text', text: 'just words' }]);
  assert.deepEqual([...copy.data.keys()], ['text/plain']);
  // And the earlier copy is gone: the clipboard holds this one now.
  assert.equal(copiedSegments('Summarize [notes.md] please [report.pdf]'), null);
  assert.equal(pastedSegments(copy.transfer), null);
});

test("what isn't a copy of segments is ignored", () => {
  for (const raw of ['{', '{"type":"text"}', '[{"type":"text"}]', '[{"type":"pdf","pdf":{}}]']) {
    const paste = clipboard({ 'application/x-rovik-segments+json': raw, 'text/plain': 'x' });
    assert.equal(pastedSegments(paste.transfer), null, raw);
  }
  assert.equal(pastedSegments(null), null);
});
