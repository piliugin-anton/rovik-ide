// Component test: the composer's `@` file list follows the picked row. The
// list is a scroll box a few rows tall (styles.css), and the arrow keys only
// moved the pick: past the rows on screen it walked out of sight and the keys
// looked dead, so the composer now asks for the picked row to be scrolled
// into view. happy-dom has no layout, so the test records that request
// (test/scroll.ts) instead of measuring the box. Run with: npm test
import { container } from '../test/dom';
import { flush, installTauri, press, rows, typeText } from '../test/chatPanel';
import { recordScrollIntoView } from '../test/scroll';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ChatPanel from './ChatPanel';

// More files than the box shows, so the pick has to walk past its bottom.
const FILES = Array.from({ length: 40 }, (_, i) => `src/file-${String(i + 1).padStart(2, '0')}.ts`);
installTauri(FILES);

test('the @ list scrolls each picked row into view, and back to the first for a new query', async () => {
  localStorage.clear();
  const root = container();
  const scrolls = recordScrollIntoView();
  const dispose = render(
    () => (
      <ChatPanel
        rootPath={() => '/p'}
        activeFileContext={() => null}
        files={() => []}
        projectId={() => 'project-1'}
      />
    ),
    root
  );
  try {
    await flush();

    // "@" offers the project's files, the first row picked.
    await typeText(root, '@');
    assert.equal(rows(root).length, FILES.length, 'every file is offered');
    assert.ok(scrolls.revealed.at(-1) === rows(root)[0], 'the first row is in view');

    // Each arrow press brings its row into view, past the box's bottom.
    for (let i = 1; i <= 12; i++) {
      await press(root, 'ArrowDown');
      const picked = rows(root)[i];
      assert.ok(picked.classList.contains('selected'), `row ${i} is picked`);
      assert.ok(scrolls.revealed.at(-1) === picked, `row ${i} is scrolled into view`);
    }

    // A query narrows the list, which starts again at its first row.
    await typeText(root, '@file-1');
    assert.ok(rows(root).length > 0 && rows(root).length < FILES.length, 'fewer rows match');
    assert.ok(rows(root)[0].classList.contains('selected'), 'the first row is picked again');
    assert.ok(scrolls.revealed.at(-1) === rows(root)[0], 'and back in view');
  } finally {
    dispose();
    scrolls.restore();
  }
});
