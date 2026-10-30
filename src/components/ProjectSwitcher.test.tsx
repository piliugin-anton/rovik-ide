import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ProjectSwitcher from './ProjectSwitcher';
import { resolveProject } from '../lib/projects';

let open = () => Promise.resolve<unknown>(null);
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string) => {
    if (cmd === 'chat_open_project') return open();
    if (cmd === 'project_claim') return { claimed: true, openElsewhere: false };
    if (cmd === 'chat_list_projects') return [];
    throw new Error(`unexpected ${cmd}`);
  },
};

test('the switcher shows a spinner and Loading while opening a project', async () => {
  const root = container();
  const dispose = render(
    () => (
      <ProjectSwitcher
        onOpenFolder={() => {}}
        onNewProject={() => {}}
        onNewWindow={() => {}}
        onOpen={() => {}}
        onRelink={() => {}}
        onDelete={() => {}}
      />
    ),
    root
  );
  const name = () => root.querySelector('.project-switcher-name')!.textContent;
  try {
    assert.equal(name(), 'Loading…', 'startup has not chosen a project yet');
    assert.ok(root.querySelector('[role="status"][aria-label="Loading project"]'));
    await resolveProject(null);
    assert.equal(name(), 'No project open');
    assert.equal(root.querySelector('[role="status"]'), null);

    let finish!: (value: unknown) => void;
    open = () => new Promise((resolve) => (finish = resolve));
    const resolved = resolveProject('/p');
    assert.equal(name(), 'Loading…');
    assert.equal(
      root.querySelector('.project-switcher-trigger')?.getAttribute('title'),
      'Loading…'
    );
    finish({ project: { id: 'p', name: 'Demo', path: '/p' }, suggestion: null });
    await resolved;
    assert.equal(name(), 'Demo');
    assert.equal(root.querySelector('[role="status"]'), null);
    assert.equal(root.querySelector('.project-switcher-trigger')?.getAttribute('title'), '/p');
  } finally {
    dispose();
    root.remove();
  }
});
