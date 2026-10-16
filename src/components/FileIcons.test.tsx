// Component test: the tree's folder icon follows its folder opening and
// closing, on the row it was first drawn for. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createSignal } from 'solid-js';
import { render } from 'solid-js/web';
import { FolderIcon } from './FileIcons';

test('a folder icon opens and closes with its folder', () => {
  const [open, setOpen] = createSignal(false);
  const root = container();
  const dispose = render(() => <FolderIcon open={open()} />, root);
  const paths = () => root.querySelectorAll('path').length;
  assert.equal(paths(), 1, 'closed: one outline');
  setOpen(true);
  assert.equal(paths(), 2, 'open: the open folder');
  setOpen(false);
  assert.equal(paths(), 1, 'closed again');
  dispose();
});
