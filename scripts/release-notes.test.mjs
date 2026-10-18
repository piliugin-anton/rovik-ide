// release-notes.mjs tests (node:test).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { plainMentions } from './release-notes.mjs';

const PULLS = 'https://github.com/o/r/pull';

test("a pull request title's mentions become code, its author stays mentioned", () => {
  const notes = [
    "## What's Changed",
    `* Open @context files from the chat in the editor by @someone in ${PULLS}/42`,
    `* Keep @types/node out of browser code by @someone in ${PULLS}/73`,
    `* Chat: plans can be referenced with "@" when .rovik/ is ignored by @someone in ${PULLS}/103`,
    `* Mail noreply@example.com, and \`@already\` code by @someone in ${PULLS}/7`,
    '',
    '## New Contributors',
    `* @someone made their first contribution in ${PULLS}/1`,
    '',
    '**Full Changelog**: https://github.com/o/r/commits/v1.0.0',
  ].join('\n');
  assert.equal(
    plainMentions(notes),
    [
      "## What's Changed",
      `* Open \`@context\` files from the chat in the editor by @someone in ${PULLS}/42`,
      `* Keep \`@types/node\` out of browser code by @someone in ${PULLS}/73`,
      `* Chat: plans can be referenced with "@" when .rovik/ is ignored by @someone in ${PULLS}/103`,
      `* Mail noreply@example.com, and \`@already\` code by @someone in ${PULLS}/7`,
      '',
      '## New Contributors',
      `* @someone made their first contribution in ${PULLS}/1`,
      '',
      '**Full Changelog**: https://github.com/o/r/commits/v1.0.0',
    ].join('\n')
  );
});

test('it reads the notes on stdin and writes them on stdout', () => {
  const script = fileURLToPath(new URL('./release-notes.mjs', import.meta.url));
  const out = spawnSync(process.execPath, [script], {
    input: `* Use @context by @someone in ${PULLS}/1\n`,
    encoding: 'utf8',
  });
  assert.equal(out.status, 0, out.stderr);
  assert.equal(out.stdout, `* Use \`@context\` by @someone in ${PULLS}/1\n`);
});
