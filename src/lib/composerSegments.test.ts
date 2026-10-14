// composerSegments.ts tests (node:test): what the chat input's content is
// sent as, copied as, and read back from a sent message as.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  asSegment,
  attachmentIds,
  messageText,
  pdfsOf,
  plainText,
  segmentKind,
  segmentsOfMessage,
  type DraftSegment,
} from './composerSegments';

const image = { id: 'a'.repeat(64), width: 10, height: 20 };
const pdf = { id: 'b'.repeat(64), name: 'report.pdf', size: 1234, pages: 3 };
const file = { id: 'f1', path: '/p/src/App.tsx', name: 'App.tsx', relativePath: 'src/App.tsx' };

const input: DraftSegment[] = [
  { type: 'text', text: 'Look at ' },
  { type: 'file', file: { ...file, lines: { start: 3, end: 3 } } },
  { type: 'text', text: ' and ' },
  { type: 'pasted', text: 'A\nB' },
  { type: 'text', text: ' with ' },
  { type: 'pasted', text: '# Notes', name: 'notes.md' },
  { type: 'text', text: ' ' },
  { type: 'image', image },
  { type: 'text', text: ' ' },
  { type: 'pdf', pdf },
  { type: 'text', text: ' ' },
];

test('the message text has blocks in their tags, and no images or PDFs', () => {
  assert.equal(
    messageText(input),
    'Look at App.tsx:3 and \n<pasted_text lines="2">\nA\nB\n</pasted_text>\n with ' +
      '\n<attached_file name="notes.md" lines="1">\n# Notes\n</attached_file>\n   '
  );
});

test('a copy for other apps has the paste in full and names the rest', () => {
  assert.equal(
    plainText(input),
    'Look at App.tsx:3 and \nA\nB\n with [notes.md] [image] [report.pdf]'
  );
});

test("a message's attachments are its images and PDFs, once each", () => {
  assert.deepEqual(attachmentIds([...input, { type: 'image', image }]), [image.id, pdf.id]);
  assert.deepEqual(pdfsOf([...input, { type: 'pdf', pdf }]), [pdf]);
  assert.deepEqual(
    input.map(segmentKind).filter(Boolean),
    ['text', 'image', 'pdf'],
    'a named block is an attached file; a paste is not an attachment'
  );
});

test('a sent message reads back as the segments it was written with', () => {
  const content = messageText(input).trim();
  const segments = segmentsOfMessage({
    content,
    attachments: [image.id, pdf.id],
    meta: { pdfs: [pdf] },
  });
  assert.deepEqual(segments, [
    { type: 'text', text: 'Look at App.tsx:3 and ' },
    { type: 'pasted', text: 'A\nB' },
    { type: 'text', text: ' with ' },
    { type: 'pasted', text: '# Notes', name: 'notes.md' },
    { type: 'text', text: '\n' },
    { type: 'image', image: { id: image.id } },
    { type: 'pdf', pdf },
  ]);
  // Nothing but attachments, or nothing at all.
  assert.deepEqual(segmentsOfMessage({ content: '', attachments: [image.id] }), [
    { type: 'image', image: { id: image.id } },
  ]);
  assert.deepEqual(segmentsOfMessage({ content: 'Hi' }), [{ type: 'text', text: 'Hi' }]);
});

test('only well-formed segments are read back from the clipboard', () => {
  for (const segment of input) assert.deepEqual(asSegment(segment), segment);
  assert.equal(asSegment(null), null);
  assert.equal(asSegment('text'), null);
  assert.equal(asSegment({ type: 'text' }), null);
  assert.equal(asSegment({ type: 'pasted', text: 'x', name: 3 }), null);
  assert.equal(asSegment({ type: 'image', image: { id: '../etc' } }), null);
  assert.equal(asSegment({ type: 'pdf', pdf: { ...pdf, pages: '3' } }), null);
  assert.equal(asSegment({ type: 'file', file: { path: '/x' } }), null);
  assert.equal(asSegment({ type: 'file', file: { ...file, lines: { start: 'a' } } }), null);
  assert.equal(asSegment({ type: 'script', text: 'x' }), null);
});
