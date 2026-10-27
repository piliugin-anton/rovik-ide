// Component test: the file tree's menu for one of the window's folders
// (docs/design/workspaces-plan.md C01): what makes something in the folder,
// what brings it back, and Remove from workspace — never Rename, Copy, Cut or
// Delete, which the backend refuses on a folder of the window anyway.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import FileContextMenu, { type FileContextMenuProps } from './FileContextMenu';

/** The menu's entries, in the order they're offered. */
const labels = () =>
  [...document.querySelectorAll('.context-menu-label')].map((el) => el.textContent);

function menu(props: Partial<FileContextMenuProps>) {
  const calls: string[] = [];
  const dispose = render(
    () => (
      <FileContextMenu
        ref={() => {}}
        x={10}
        y={10}
        hasSelection={false}
        singleSelection={false}
        canPaste
        onNewFile={() => calls.push('new file')}
        onNewFolder={() => calls.push('new folder')}
        onRename={() => calls.push('rename')}
        onCopy={() => calls.push('copy')}
        onCut={() => calls.push('cut')}
        onPaste={() => calls.push('paste')}
        onDelete={() => calls.push('delete')}
        onRemoveFolder={() => calls.push('remove')}
        onLocateFolder={() => calls.push('locate')}
        onTryFolderAgain={() => calls.push('try again')}
        {...props}
      />
    ),
    container()
  );
  return { calls, dispose };
}

test('a folder row’s menu offers New file, New folder, Paste and Remove from workspace', () => {
  const { dispose } = menu({ folder: { missing: false } });
  assert.deepEqual(labels(), ['New file', 'New folder', 'Paste', 'Remove from workspace']);
  dispose();
});

test('a missing folder row’s menu offers Locate…, Try again and Remove from workspace', () => {
  const { dispose } = menu({ folder: { missing: true } });
  assert.deepEqual(labels(), ['Locate…', 'Try again', 'Remove from workspace']);
  dispose();
});
