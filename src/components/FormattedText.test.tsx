// Component test: links in a chat reply. A path opens the project's file
// in the editor (at the line a `#L12` names), a web address the browser;
// and a link stays the same element while the reply streams on, so a
// click on it isn't lost. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import FormattedText from './FormattedText';
import { setProjectFileResolver } from '../lib/editorBridge';
import { setEditorOpener, type OpenInEditor } from '../lib/openInEditor';
import { dismiss, notifications } from '../lib/notifications';

const sent: { cmd: string; args: any }[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: unknown) => {
    sent.push({ cmd, args });
  },
};

const files: Record<string, string> = {
  'src/a.ts': '/p/src/a.ts',
  'docs/b.md': '/p/docs/b.md',
};
setProjectFileResolver(async (path) => files[path] ?? null);

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function link(root: HTMLElement, text: string): HTMLAnchorElement {
  return [...root.querySelectorAll<HTMLAnchorElement>('a')].find((a) => a.textContent === text)!;
}

function click(el: Element, button = 0) {
  el.dispatchEvent(
    new MouseEvent(button === 0 ? 'click' : 'auxclick', { bubbles: true, cancelable: true, button })
  );
}

test('a path opens the file at its line, a web address the browser', async () => {
  const opened: OpenInEditor[] = [];
  setEditorOpener(async (request) => {
    opened.push(request);
  });
  const root = container();
  const dispose = render(
    () => (
      <FormattedText
        text={
          'See [a.ts](src/a.ts#L3), [b](/docs/b.md), [gone](src/gone.ts) ' +
          'and [the docs](https://example.dev/x).'
        }
      />
    ),
    root
  );

  click(link(root, 'a.ts'));
  await settle();
  assert.deepEqual(opened, [{ path: '/p/src/a.ts', focus: true, at: { line: 3 } }]);

  // A leading slash: the project's root, as on GitHub.
  click(link(root, 'b'));
  await settle();
  assert.deepEqual(opened[1], { path: '/p/docs/b.md', focus: true });

  click(link(root, 'gone'));
  await settle();
  assert.equal(opened.length, 2);
  const warning = notifications().find((n) => n.title === "Can't open src/gone.ts");
  assert.equal(warning?.kind, 'warning');
  dismiss(warning!.id);

  click(link(root, 'the docs'));
  await settle();
  assert.deepEqual(sent, [{ cmd: 'open_url', args: { url: 'https://example.dev/x' } }]);
  // A middle click on a path doesn't open it twice, or at all.
  click(link(root, 'a.ts'), 1);
  await settle();
  assert.equal(opened.length, 2);

  dispose();
  setEditorOpener(null);
});

test('a link stays the same element while the reply streams on', () => {
  const [text, setText] = createSignal('Fixed in [a.ts](src/a.ts) as');
  const root = container();
  const dispose = render(() => <FormattedText text={text()} />, root);
  const before = link(root, 'a.ts');
  for (const more of [' asked', '.\n\n- one', '\n- two']) {
    setText((t) => t + more);
    assert.equal(link(root, 'a.ts'), before);
  }
  assert.deepEqual(
    [...root.querySelectorAll('li')].map((li) => li.textContent?.trim()),
    ['one', 'two']
  );
  dispose();
});
