// browserStartup.ts tests (node:test): when the app downloads the browser
// for the web tools, and what its notification says as it goes.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { needsDownload, progressNote, type BrowserStatus } from './browserStartup';

const status: BrowserStatus = {
  name: 'Chrome for Testing',
  version: '155.0.8059.39',
  path: null,
  size: 198_195_769,
};

test('it’s downloaded when it’s missing and there’s a build for this system', () => {
  assert.equal(needsDownload(status), true);
  assert.equal(
    needsDownload({ ...status, path: '/opt/rovik/browser/155.0.8059.39/chrome' }),
    false
  );
  assert.equal(needsDownload({ ...status, size: null }), false);
});

test('the notification shows how far the download is, then the unpacking', () => {
  assert.deepEqual(progressNote(0, 0), {
    progress: 'busy',
    message: 'Another Rovik window is downloading it…',
  });
  assert.deepEqual(progressNote(0, 200 * 1024 * 1024), {
    progress: 0,
    message: '0 bytes of 200 MB',
  });
  assert.deepEqual(progressNote(50 * 1024 * 1024, 200 * 1024 * 1024), {
    progress: 0.25,
    message: '50 MB of 200 MB',
  });
  assert.deepEqual(progressNote(200 * 1024 * 1024, 200 * 1024 * 1024), {
    progress: 'busy',
    message: 'Unpacking…',
  });
});
