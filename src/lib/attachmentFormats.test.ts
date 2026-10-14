// attachmentFormats.ts tests (node:test): which files can be attached, as
// what, and which models take them.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  ATTACH_ACCEPT,
  attachmentKind,
  decodeText,
  modelTakes,
  unsupportedReason,
} from './attachmentFormats';

const kind = (name: string, type = '') => attachmentKind({ name, type });

test('images: PNG, JPEG, WebP and GIF, by type or by name', () => {
  for (const type of ['image/png', 'image/jpeg', 'image/webp', 'image/gif']) {
    assert.equal(kind('', type), 'image', type);
  }
  assert.equal(kind('Screenshot.PNG'), 'image');
  assert.equal(kind('photo.jpg', 'image/jpeg'), 'image');
  // Formats no provider takes.
  assert.equal(kind('scan.bmp', 'image/bmp'), null);
  assert.equal(kind('photo.heic', 'image/heic'), null);
  assert.equal(kind('', 'image/tiff'), null);
});

test('PDFs, by type or by name', () => {
  assert.equal(kind('report.pdf', 'application/pdf'), 'pdf');
  assert.equal(kind('REPORT.PDF'), 'pdf');
});

test('text and code, whatever type the system gives them', () => {
  assert.equal(kind('notes.md'), 'text');
  assert.equal(kind('data.csv', 'text/csv'), 'text');
  assert.equal(kind('config.json', 'application/json'), 'text');
  // A TypeScript file is "video/mp2t" to some systems.
  assert.equal(kind('main.ts', 'video/mp2t'), 'text');
  // An SVG is its source.
  assert.equal(kind('icon.svg', 'image/svg+xml'), 'text');
  // No type known: text if it reads as text (decodeText).
  assert.equal(kind('Makefile'), 'text');
  assert.equal(kind('.env'), 'text');
  assert.equal(kind('data.unknown', 'application/octet-stream'), 'text');
});

test('documents, archives and media models get nothing of', () => {
  assert.equal(
    kind('report.docx', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'),
    null
  );
  assert.equal(kind('sheet.xlsx'), null);
  assert.equal(kind('code.zip', 'application/zip'), null);
  assert.equal(kind('song.mp3', 'audio/mpeg'), null);
  assert.equal(kind('clip.mp4', 'video/mp4'), null);
  assert.equal(kind('blob.bin', 'application/octet-stream'), null);
  assert.equal(kind('font.woff2', 'font/woff2'), null);
});

test('why a file is refused says what to do', () => {
  assert.match(unsupportedReason({ name: 'report.docx', type: '' }), /save it as PDF/);
  assert.match(
    unsupportedReason({ name: 'code.zip', type: 'application/zip' }),
    /“code\.zip” can't be attached: models take images \(PNG, JPEG, WebP, GIF\), PDFs and text/
  );
  assert.match(unsupportedReason({ name: '', type: 'image/bmp' }), /^This file can't/);
});

test('images need Vision, PDFs need PDF, text any model', () => {
  const textOnly = { vision: false, pdf: false };
  const everything = { vision: true, pdf: true };
  assert.equal(modelTakes(textOnly, 'text'), true);
  assert.equal(modelTakes(textOnly, 'image'), false);
  assert.equal(modelTakes(textOnly, 'pdf'), false);
  assert.equal(modelTakes({ vision: true, pdf: false }, 'image'), true);
  assert.equal(modelTakes({ vision: true, pdf: false }, 'pdf'), false);
  assert.equal(modelTakes(everything, 'pdf'), true);
});

test('text is UTF-8 without NUL, with \\n line breaks', () => {
  const bytes = (s: string) => new TextEncoder().encode(s);
  assert.equal(decodeText(bytes('a\r\nb\rc\n')), 'a\nb\nc\n');
  assert.equal(decodeText(bytes('﻿with a BOM')), 'with a BOM');
  assert.equal(decodeText(bytes('Привет, 世界')), 'Привет, 世界');
  assert.equal(decodeText(new Uint8Array([0x61, 0xff, 0x62])), null, 'not UTF-8');
  assert.equal(decodeText(bytes('a\0b')), null, 'binary');
  assert.equal(decodeText(new Uint8Array()), '');
});

test('the file picker offers every format that can be attached', () => {
  const accepted = ATTACH_ACCEPT.split(',');
  for (const entry of ['image/png', '.jpg', '.webp', '.gif', 'application/pdf', '.pdf']) {
    assert.ok(accepted.includes(entry), entry);
  }
  for (const entry of ['text/*', '.md', '.ts', '.py', '.csv', '.json', '.log']) {
    assert.ok(accepted.includes(entry), entry);
  }
  assert.ok(!accepted.includes('.docx'));
});
