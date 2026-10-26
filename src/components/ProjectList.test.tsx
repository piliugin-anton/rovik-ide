// Component test: the project list hides Delete for every project the
// window has open, not only for the one it shows (docs/design/workspaces-
// plan.md, 3.4, B06). Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ProjectList from './ProjectList';
import type { ProjectSummary } from '../lib/chatApi';

const summary = (id: string, name: string, missing = false): ProjectSummary => ({
  id,
  name,
  path: `/work/${name}`,
  chatCount: 2,
  lastOpenedAt: Date.now(),
  missing,
});

const list = (openIds?: ReadonlySet<string>) => {
  const root = container();
  const deleted: string[] = [];
  const dispose = render(
    () => (
      <ProjectList
        projects={[summary('p-api', 'api'), summary('p-web', 'web'), summary('p-old', 'old', true)]}
        openIds={openIds}
        onOpen={() => undefined}
        onRelink={() => undefined}
        onDelete={(p) => deleted.push(p.id)}
      />
    ),
    root
  );
  const rows = [...root.querySelectorAll('.project-row')];
  const deleteOf = (i: number) => rows[i]?.querySelector('.project-row-delete') ?? null;
  return { root, rows, deleteOf, deleted, dispose };
};

test('no Delete on any project this window has open', () => {
  const { rows, deleteOf, deleted, dispose } = list(new Set(['p-api', 'p-web']));
  assert.equal(rows.length, 3);
  assert.equal(deleteOf(0), null, 'open');
  assert.equal(deleteOf(1), null, 'open too, though another one may be shown');
  assert.ok(deleteOf(2), 'not open: it can be deleted');
  (deleteOf(2) as HTMLButtonElement).click();
  assert.deepEqual(deleted, ['p-old']);
  dispose();
});

test('with nothing open, every project can be deleted', () => {
  const { deleteOf, dispose } = list();
  assert.ok(deleteOf(0) && deleteOf(1) && deleteOf(2));
  dispose();
});

test('a missing project is offered Relink, and Delete unless the window has it', () => {
  const { rows, root, dispose } = list(new Set(['p-old']));
  const missing = rows[2];
  assert.ok(missing?.querySelector('.project-row-actions .link-button'));
  assert.equal(
    missing?.querySelector('.project-row-delete'),
    null,
    'missing, but the window has it: no Delete'
  );
  assert.ok(root.textContent?.includes('folder missing'));
  dispose();
});
