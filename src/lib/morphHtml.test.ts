// morphHtml: new HTML in place of the old, the nodes that didn't change
// kept (a streaming reply's links stay clickable while it's written).
// Run with: npm test
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { morphHtml } from './morphHtml';

function host(html: string): HTMLElement {
  const el = document.createElement('div');
  morphHtml(el, html);
  return el as unknown as HTMLElement;
}

test('a growing reply keeps what was already written', () => {
  const el = host('<p>See <a href="src/a.ts">a.ts</a> for the fix</p>');
  const paragraph = el.querySelector('p');
  const link = el.querySelector('a');
  morphHtml(el, '<p>See <a href="src/a.ts">a.ts</a> for the fix, and</p><ul><li>one</li></ul>');
  assert.equal(el.querySelector('p'), paragraph);
  assert.equal(el.querySelector('a'), link);
  assert.equal(el.querySelector('p')?.textContent, 'See a.ts for the fix, and');
  assert.equal(el.querySelector('li')?.textContent, 'one');

  // A list item growing: the list and its first item stay.
  const list = el.querySelector('ul');
  const first = el.querySelector('li');
  morphHtml(
    el,
    '<p>See <a href="src/a.ts">a.ts</a> for the fix, and</p><ul><li>one</li><li>tw</li></ul>'
  );
  assert.equal(el.querySelector('ul'), list);
  assert.equal(el.querySelector('li'), first);
  assert.deepEqual(
    [...el.querySelectorAll('li')].map((li) => li.textContent),
    ['one', 'tw']
  );
});

test('what changed is changed: elements, attributes, removed nodes', () => {
  const el = host('<p>one</p><p>two <a href="x.ts" title="x">x</a></p><p>three</p>');
  morphHtml(el, '<h2>one</h2><p>two <a href="y.ts">x</a></p>');
  assert.equal(el.innerHTML, '<h2>one</h2><p>two <a href="y.ts">x</a></p>');
  morphHtml(el, '');
  assert.equal(el.innerHTML, '');
});

test('a <details> the user opened stays open', () => {
  const el = host('<details><summary>More</summary><p>a</p></details>');
  el.querySelector('details')!.setAttribute('open', '');
  morphHtml(el, '<details><summary>More</summary><p>ab</p></details>');
  assert.equal(el.querySelector('details')?.hasAttribute('open'), true);
  assert.equal(el.querySelector('details p')?.textContent, 'ab');
});
