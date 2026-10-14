// The Markdown preview's rendering (markdownPreview.ts): source lines on
// blocks, anchors, alerts, front matter, and where links and images point.
// Run with: npm test
//
// Not sanitizePreviewHtml: DOMPurify doesn't work on happy-dom's DOM (it
// drops ordinary elements such as <h1> and <div> even with its defaults).
import '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  headingLine,
  headingSlug,
  renderMarkdown,
  resolveDocumentPath,
  resolvePreviewImage,
  resolvePreviewLink,
  toggleTaskLine,
} from './markdownPreview';

/** The source line each element with `data-line` names, by its text. */
function lines(source: string): [string, number][] {
  const el = document.createElement('div');
  el.innerHTML = renderMarkdown(source).html;
  return [...el.querySelectorAll('[data-line]')].map((node) => [
    `${node.tagName.toLowerCase()}:${(node.textContent ?? '').trim().split('\n')[0]}`,
    Number(node.getAttribute('data-line')),
  ]);
}

const DOC = [
  '# Title', // 0
  '',
  'Para one', // 2
  'continues.',
  '',
  '- item a', // 5
  '- [ ] task b', // 6
  '  nested para',
  '',
  '  - sub 1', // 9
  '  - sub 2', // 10
  '- [x] item c', // 11
  '',
  '> quote', // 13
  '> more',
  '',
  '```js', // 16
  'const x = 1;',
  '```',
  '',
  '| a | b |', // 20
  '|---|---|',
  '| 1 | 2 |',
  '',
  '---', // 24
  '',
  'Setext', // 26
  '------',
].join('\n');

test('every block carries the line it starts on', () => {
  assert.deepEqual(lines(DOC), [
    ['h1:Title', 0],
    ['p:Para one', 2],
    ['ul:item a', 5],
    ['li:item a', 5],
    ['p:item a', 5],
    ['li:task b', 6],
    ['p:task b', 6],
    ['ul:sub 1', 9],
    ['li:sub 1', 9],
    ['li:sub 2', 10],
    ['li:item c', 11],
    ['p:item c', 11],
    ['blockquote:quote', 13],
    ['p:quote', 13],
    ['pre:const x = 1;', 16],
    ['div:a', 20],
    ['hr:', 24],
    ['h2:Setext', 26],
  ]);
});

test('Windows line endings count the same', () => {
  assert.deepEqual(lines(DOC.replace(/\n/g, '\r\n')), lines(DOC));
});

test('headings get GitHub’s anchors, repeats numbered', () => {
  const { html, headings } = renderMarkdown(
    [
      '# Hello World',
      '## Hello World',
      '### Hello World',
      '# C++ & Rust!',
      '# Привет, мир',
      '# Use `code` and [links](x.md)',
    ].join('\n')
  );
  assert.deepEqual(headings, [
    { id: 'hello-world', line: 0 },
    { id: 'hello-world-1', line: 1 },
    { id: 'hello-world-2', line: 2 },
    { id: 'c--rust', line: 3 },
    { id: 'привет-мир', line: 4 },
    { id: 'use-code-and-links', line: 5 },
  ]);
  assert.match(html, /<h1 data-line="0" id="hello-world">Hello World<\/h1>/);
  assert.equal(headingSlug('  Spaces  around '), 'spaces--around');
});

test('a heading is found by its anchor', () => {
  const source = '# Intro\n\ntext\n\n## Set up\n\n## Set up\n';
  assert.equal(headingLine(source, '#set-up'), 4);
  assert.equal(headingLine(source, 'set-up-1'), 6);
  assert.equal(headingLine(source, '#Intro'), 0);
  assert.equal(headingLine(source, '#missing'), null);
});

test('GitHub alerts: a quote starting with [!NOTE] alone on its line', () => {
  const { html } = renderMarkdown('> [!WARNING]\n> Mind **this**.\n');
  assert.match(html, /<div class="md-alert md-alert-warning" data-line="0">/);
  assert.match(html, /<p class="md-alert-title">Warning<\/p>/);
  assert.match(html, /<p data-line="0">Mind <strong>this<\/strong>.<\/p>/);
  assert.doesNotMatch(html, /\[!WARNING\]/);

  // The marker on a line of its own, then a blank line.
  assert.match(renderMarkdown('> [!tip]\n>\n> Later.\n').html, /md-alert-tip[\s\S]*Later\./);

  // Not alone on its line: an ordinary quote.
  const plain = renderMarkdown('> [!NOTE] Title\n> body\n').html;
  assert.match(plain, /<blockquote/);
  assert.doesNotMatch(plain, /md-alert/);
});

test('front matter is folded away, and later lines still count from the top', () => {
  const source = '---\ntitle: Post\ntags: [a, b]\n---\n# Heading\n\nText\n';
  const { html, headings } = renderMarkdown(source);
  assert.match(html, /^<details class="md-front-matter" data-line="0"><summary>Front matter/);
  assert.match(html, /<code class="language-yaml">title: Post\ntags: \[a, b\]\n<\/code>/);
  assert.deepEqual(headings, [{ id: 'heading', line: 4 }]);
  assert.match(html, /<p data-line="6">Text<\/p>/);
});

test('task boxes are clickable only where the document can be edited', () => {
  const source = '- [ ] open\n- [x] done\n';
  const editable = renderMarkdown(source, { tasks: true }).html;
  assert.match(editable, /<li data-line="0" class="md-task"><input type="checkbox"> open/);
  assert.match(
    editable,
    /<li data-line="1" class="md-task"><input type="checkbox" checked=""> done/
  );
  assert.match(editable, /<ul data-line="0" class="md-tasks">/);
  assert.match(renderMarkdown(source).html, /<input type="checkbox" disabled="">/);
});

test('a task line’s box ticks and clears', () => {
  assert.equal(toggleTaskLine('- [ ] write tests'), '- [x] write tests');
  assert.equal(toggleTaskLine('  * [X] nested'), '  * [ ] nested');
  assert.equal(toggleTaskLine('> 1. [ ] in a quote'), '> 1. [x] in a quote');
  assert.equal(toggleTaskLine('- plain item'), null);
  assert.equal(toggleTaskLine('[ ] not a list'), null);
});

test('paths in a document resolve like GitHub resolves them', () => {
  const doc = '/home/u/proj/docs/guide.md';
  const root = '/home/u/proj';
  assert.equal(resolveDocumentPath('intro.md', doc, root), '/home/u/proj/docs/intro.md');
  assert.equal(resolveDocumentPath('./img/a.png', doc, root), '/home/u/proj/docs/img/a.png');
  assert.equal(resolveDocumentPath('../README.md', doc, root), '/home/u/proj/README.md');
  assert.equal(resolveDocumentPath('../../../../../x.md', doc, root), '/x.md');
  // A leading slash is the project's root; a full path into the project stays.
  assert.equal(resolveDocumentPath('/src/main.rs', doc, root), '/home/u/proj/src/main.rs');
  assert.equal(resolveDocumentPath('/home/u/proj/a.md', doc, root), '/home/u/proj/a.md');
  assert.equal(resolveDocumentPath('/etc/x.md', doc, null), '/etc/x.md');
  // Windows.
  const win = 'C:\\work\\proj\\docs\\guide.md';
  assert.equal(resolveDocumentPath('../a b.md', win, 'C:\\work\\proj'), 'C:\\work\\proj\\a b.md');
  assert.equal(
    resolveDocumentPath('/src/x.ts', win, 'C:\\work\\proj'),
    'C:\\work\\proj\\src\\x.ts'
  );
  assert.equal(resolveDocumentPath('D:\\other\\y.md', win, 'C:\\work\\proj'), 'D:\\other\\y.md');
});

test('links: anchors, web pages, files at a heading or a line', () => {
  const doc = '/p/docs/guide.md';
  assert.deepEqual(resolvePreviewLink('#Set-up', doc, '/p'), { kind: 'anchor', id: 'Set-up' });
  assert.deepEqual(resolvePreviewLink('#caf%C3%A9', doc, '/p'), { kind: 'anchor', id: 'café' });
  assert.deepEqual(resolvePreviewLink('https://a.dev/x?y=1', doc, '/p'), {
    kind: 'web',
    url: 'https://a.dev/x?y=1',
  });
  assert.deepEqual(resolvePreviewLink('mailto:me@a.dev', doc, '/p'), {
    kind: 'web',
    url: 'mailto:me@a.dev',
  });
  assert.deepEqual(resolvePreviewLink('//a.dev/x', doc, '/p'), {
    kind: 'web',
    url: 'https://a.dev/x',
  });
  assert.deepEqual(resolvePreviewLink('other.md', doc, '/p'), {
    kind: 'file',
    path: '/p/docs/other.md',
  });
  assert.deepEqual(resolvePreviewLink('my%20notes.md#Next-steps', doc, '/p'), {
    kind: 'file',
    path: '/p/docs/my notes.md',
    anchor: 'Next-steps',
  });
  assert.deepEqual(resolvePreviewLink('../src/app.ts?plain=1#L12-L20', doc, '/p'), {
    kind: 'file',
    path: '/p/src/app.ts',
    line: 12,
  });
  assert.deepEqual(resolvePreviewLink('vscode:extension/x', doc, '/p'), { kind: 'none' });
  assert.deepEqual(resolvePreviewLink('?tab=1', doc, '/p'), { kind: 'none' });
  assert.deepEqual(resolvePreviewLink('', doc, '/p'), { kind: 'none' });
});

test('images: local ones with their type, remote ones never fetched', () => {
  const doc = '/p/README.md';
  assert.deepEqual(resolvePreviewImage('docs/shot.PNG?raw=true', doc, '/p'), {
    kind: 'local',
    path: '/p/docs/shot.PNG',
    type: 'image/png',
  });
  assert.deepEqual(resolvePreviewImage('logo%20dark.svg', doc, '/p'), {
    kind: 'local',
    path: '/p/logo dark.svg',
    type: 'image/svg+xml',
  });
  assert.deepEqual(resolvePreviewImage('https://img.shields.io/badge.svg', doc, '/p'), {
    kind: 'remote',
    url: 'https://img.shields.io/badge.svg',
  });
  assert.deepEqual(resolvePreviewImage('notes.txt', doc, '/p'), { kind: 'none' });
  assert.deepEqual(resolvePreviewImage('file:///etc/a.png', doc, '/p'), { kind: 'none' });
});
