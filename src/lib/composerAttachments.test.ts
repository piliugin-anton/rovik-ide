// composerAttachments.ts tests (node:test): what a paste inserts, and how
// attached files become segments of the chat input.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chatApi } from './chatApi';
import { MAX_PDF_BYTES, MAX_TEXT_FILE_BYTES } from './attachmentFormats';
import {
  attachmentSegment,
  parseComposerPdf,
  pastedFiles,
  pasteTakes,
} from './composerAttachments';

test('text wins over a picture of it, unless the text only names the files', () => {
  // A screenshot: an image and nothing else.
  assert.equal(pasteTakes('', 1), 'files');
  assert.equal(pasteTakes('  \n', 1), 'files');
  // Cells copied from a spreadsheet come with a picture of them.
  assert.equal(pasteTakes('a\tb\n1\t2', 1), 'text');
  assert.equal(pasteTakes('Some prose that mentions shot.png', 1), 'text');
  assert.equal(pasteTakes('a picture of shot.png', 1), 'text');
  // Files copied in a file manager: their names or paths, and the files.
  assert.equal(pasteTakes('shot.png', 1), 'files');
  assert.equal(pasteTakes('/home/me/Pictures/Screenshot 2026.PNG', 1), 'files');
  assert.equal(pasteTakes('file:///home/me/report.pdf', 1), 'files');
  assert.equal(pasteTakes('C:\\Users\\me\\notes.md', 1), 'files');
  assert.equal(pasteTakes('/home/me/a.pdf\n/home/me/b.txt\n', 2), 'files');
  // No file: text as always, or nothing.
  assert.equal(pasteTakes('shot.png', 0), 'text');
  assert.equal(pasteTakes('', 0), 'nothing');
});

/** Just enough of a DataTransfer: items and files. */
function transfer(items: { kind: string; type: string; file?: File }[]): DataTransfer {
  return {
    items: items.map((i) => ({ kind: i.kind, type: i.type, getAsFile: () => i.file ?? null })),
    files: items.flatMap((i) => (i.file ? [i.file] : [])),
    types: items.map((i) => i.type),
  } as unknown as DataTransfer;
}

test('pasted files are found in the items, else in the files', () => {
  const png = new File([new Uint8Array([1])], 'a.png', { type: 'image/png' });
  const pdf = new File([new Uint8Array([1])], 'a.pdf', { type: 'application/pdf' });
  const both = transfer([
    { kind: 'string', type: 'text/html' },
    { kind: 'file', type: 'image/png', file: png },
    { kind: 'file', type: 'application/pdf', file: pdf },
  ]);
  assert.deepEqual(pastedFiles(both), [png, pdf]);
  assert.deepEqual(pastedFiles(null), []);

  // Only `files` listed (no items): still found.
  const filesOnly = { items: [], files: [pdf, png], types: ['Files'] } as unknown as DataTransfer;
  assert.deepEqual(pastedFiles(filesOnly), [pdf, png]);
});

test('a PDF chip payload reads back only as a whole PDF', () => {
  const pdf = { id: 'c'.repeat(64), name: 'a.pdf', size: 10, pages: 2 };
  assert.deepEqual(parseComposerPdf(JSON.stringify(pdf)), pdf);
  assert.equal(parseComposerPdf(undefined), null);
  assert.equal(parseComposerPdf('{'), null);
  assert.equal(parseComposerPdf(JSON.stringify({ ...pdf, id: 'x' })), null);
  assert.equal(parseComposerPdf(JSON.stringify({ ...pdf, name: undefined })), null);
});

test('a text file becomes a named block of its text', async () => {
  const file = new File(['# Notes\r\nline two\r\n'], 'notes.md', { type: 'text/markdown' });
  assert.deepEqual(await attachmentSegment(file, 'text'), {
    type: 'pasted',
    text: '# Notes\nline two\n',
    name: 'notes.md',
  });
  const binary = new File([new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0, 0])], 'data');
  await assert.rejects(attachmentSegment(binary, 'text'), /isn't text/);
  const huge = new File([new Uint8Array(MAX_TEXT_FILE_BYTES + 1).fill(97)], 'huge.log');
  await assert.rejects(attachmentSegment(huge, 'text'), /at most/);
});

test('a PDF is stored, and named and counted as the backend says', async () => {
  const api = chatApi as { storeAttachment: typeof chatApi.storeAttachment };
  const original = api.storeAttachment;
  const stored: Uint8Array[] = [];
  let mime = 'application/pdf';
  api.storeAttachment = async (bytes) => {
    stored.push(bytes);
    return { id: 'd'.repeat(64), mime, pages: 4 };
  };
  try {
    const file = new File(['%PDF-1.7 …'], 'report.pdf', { type: 'application/pdf' });
    assert.deepEqual(await attachmentSegment(file, 'pdf'), {
      type: 'pdf',
      pdf: { id: 'd'.repeat(64), name: 'report.pdf', size: file.size, pages: 4 },
    });
    assert.equal(new TextDecoder().decode(stored[0]), '%PDF-1.7 …');

    // Named .pdf, but the backend found something else in it.
    mime = 'image/png';
    await assert.rejects(attachmentSegment(file, 'pdf'), /isn't a PDF/);

    const huge = new File([new Uint8Array(MAX_PDF_BYTES + 1)], 'big.pdf');
    await assert.rejects(attachmentSegment(huge, 'pdf'), /20 MB at most/);
    assert.equal(stored.length, 2, 'a PDF over the limit is never sent to be stored');
  } finally {
    api.storeAttachment = original;
  }
});
