// composerImages.ts tests (node:test): what a paste inserts, how big
// images are scaled, and what a chip's payload reads back as.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  fittedSize,
  imageFiles,
  MAX_IMAGE_SIDE,
  parseComposerImage,
  pasteTakes,
} from './composerImages';

test('text wins over a picture of it, unless the text only names the image', () => {
  // A screenshot: an image and nothing else.
  assert.equal(pasteTakes('', 1), 'images');
  assert.equal(pasteTakes('  \n', 1), 'images');
  // Cells copied from a spreadsheet come with a picture of them.
  assert.equal(pasteTakes('a\tb\n1\t2', 1), 'text');
  assert.equal(pasteTakes('Some prose that mentions shot.png', 1), 'text');
  assert.equal(pasteTakes('a picture of shot.png', 1), 'text');
  // A file copied in a file manager: its name or path, and the image.
  assert.equal(pasteTakes('shot.png', 1), 'images');
  assert.equal(pasteTakes('/home/me/Pictures/Screenshot 2026.PNG', 1), 'images');
  assert.equal(pasteTakes('file:///home/me/shot.jpeg', 1), 'images');
  assert.equal(pasteTakes('C:\\Users\\me\\shot.webp', 1), 'images');
  // No image: text as always, or nothing.
  assert.equal(pasteTakes('shot.png', 0), 'text');
  assert.equal(pasteTakes('', 0), 'nothing');
});

test('big images are scaled to the longest side, small ones kept', () => {
  assert.deepEqual(fittedSize(1280, 800), { width: 1280, height: 800 });
  assert.deepEqual(fittedSize(MAX_IMAGE_SIDE, 10), { width: MAX_IMAGE_SIDE, height: 10 });
  assert.deepEqual(fittedSize(3840, 2160), { width: 2048, height: 1152 });
  assert.deepEqual(fittedSize(1000, 4096), { width: 500, height: 2048 });
  // A sliver keeps at least a pixel.
  assert.deepEqual(fittedSize(100_000, 10), { width: 2048, height: 1 });
});

test('a chip payload reads back only as a whole image', () => {
  const id = 'a'.repeat(64);
  assert.deepEqual(parseComposerImage(JSON.stringify({ id, width: 10, height: 20 })), {
    id,
    width: 10,
    height: 20,
  });
  assert.equal(parseComposerImage(undefined), null);
  assert.equal(parseComposerImage('not json'), null);
  assert.equal(parseComposerImage('null'), null);
  assert.equal(parseComposerImage(JSON.stringify({ id: 'short', width: 1, height: 1 })), null);
  assert.equal(parseComposerImage(JSON.stringify({ id: '../x'.padEnd(64, 'a') })), null);
  assert.equal(parseComposerImage(JSON.stringify({ id, width: '10', height: 20 })), null);
});

/** Just enough of a DataTransfer: items, files and types. */
function transfer(
  items: { kind: string; type: string; file?: File }[],
  types: string[] = items.map((i) => i.type)
): DataTransfer {
  return {
    items: items.map((i) => ({ kind: i.kind, type: i.type, getAsFile: () => i.file ?? null })),
    files: items.flatMap((i) => (i.file ? [i.file] : [])),
    types,
  } as unknown as DataTransfer;
}

test('pasted image files are found in the items, else in the files', () => {
  const png = new File([new Uint8Array([1])], 'a.png', { type: 'image/png' });
  const pdf = new File([new Uint8Array([1])], 'a.pdf', { type: 'application/pdf' });
  const both = transfer([
    { kind: 'string', type: 'text/html' },
    { kind: 'file', type: 'image/png', file: png },
    { kind: 'file', type: 'application/pdf', file: pdf },
  ]);
  assert.deepEqual(imageFiles(both), [png]);
  assert.deepEqual(imageFiles(null), []);

  // Only `files` listed (no items): still found.
  const filesOnly = { items: [], files: [pdf, png], types: ['Files'] } as unknown as DataTransfer;
  assert.deepEqual(imageFiles(filesOnly), [png]);
});
