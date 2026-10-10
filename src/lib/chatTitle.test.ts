import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cleanTitle, fallbackTitle, withoutPasteMarkers } from './chatTitle';

const pasted =
  'Fix this\n<pasted_text lines="2">\nerror: build failed\nat main.rs\n</pasted_text>\n';

test('paste markers are not part of the message', () => {
  assert.equal(withoutPasteMarkers(pasted), 'Fix this\nerror: build failed\nat main.rs\n');
});

test('the fallback title has no paste markers', () => {
  assert.equal(fallbackTitle(pasted), 'Fix this error: build failed at');
  assert.equal(
    fallbackTitle('<pasted_text lines="1">\nsegfault in parser\n</pasted_text>'),
    'Segfault in parser'
  );
});

test('markup the model copies into a title is dropped', () => {
  assert.equal(cleanTitle('<pasted_text lines="40"> Build error fix'), 'Build error fix');
  assert.equal(cleanTitle('Fix <b>build</b> error'), 'Fix build error');
});
