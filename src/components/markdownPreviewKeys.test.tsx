// Component test: the Markdown preview's Ctrl+S saves on a keyboard layout
// without Latin letters. A Russian Ctrl+S types "ы" (its key's place is
// "KeyS"): the preview read `key` and saved nothing. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import MarkdownPreview from './MarkdownPreview';

(window as any).__TAURI_INTERNALS__ = { invoke: async () => null, transformCallback: () => 0 };

test('Ctrl+S in the preview saves on a Russian layout too', () => {
  let saves = 0;
  const dispose = render(
    () => (
      <MarkdownPreview
        path="/p/README.md"
        root={() => '/p'}
        content={() => '# Notes\n'}
        editor={() => undefined}
        beside={() => false}
        readOnly={() => false}
        onSave={() => saves++}
      />
    ),
    container()
  );
  const preview = document.querySelector('.md-preview')!;
  for (const key of ['s', 'ы']) {
    preview.dispatchEvent(
      new KeyboardEvent('keydown', { key, code: 'KeyS', ctrlKey: true, bubbles: true })
    );
  }
  assert.equal(saves, 2);
  dispose();
});
