// Component tests: Settings → Project checks: the checks found in the
// project's files, each off until the user ticks it, and the list that's
// the user's once they do. Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ProjectChecksSettings from './ProjectChecksSettings';
import { DEFAULT_PERMISSIONS, type AgentPermissions } from '../lib/permissions';

/** The backend's answers in the test running now, and what it was asked to save. */
let backend: Record<string, (args: any) => unknown> = {};
let saves: unknown[] = [];
/** The permissions the page asked for ("Allow it"). */
let permissionChanges: AgentPermissions[] = [];
(window as any).__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args: any) => {
    if (cmd === 'save_project_settings') {
      saves.push(args.settings);
      return;
    }
    const answer = backend[cmd];
    if (!answer) throw new Error(`no app: ${cmd}`);
    return answer(args);
  },
};

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0));
};

/** What detection finds in the project: a type check and its tests. */
const DETECTED = {
  stack: ['TypeScript'],
  checks: [
    { kind: 'typecheck', command: 'npx tsc --noEmit', source: 'tsconfig.json' },
    { kind: 'test', command: 'npm test', source: 'package.json scripts.test' },
  ],
  instructions: null,
};

/** The page for a project whose saved settings are `saved` (none: null). */
async function page(saved: unknown = null, permissions: Partial<AgentPermissions> = {}) {
  backend = {
    project_profile: () => DETECTED,
    load_project_settings: () => saved,
  };
  saves = [];
  permissionChanges = [];
  const root = container();
  render(
    () => (
      <ProjectChecksSettings
        root="/home/me/demo"
        permissions={{ ...DEFAULT_PERMISSIONS, ...permissions }}
        onPermissionsChange={(next) => permissionChanges.push(next)}
        onOpenPage={() => {}}
      />
    ),
    root
  );
  await flush();
  return root;
}

const boxes = (root: HTMLElement) =>
  [...root.querySelectorAll('.project-check-row input[type="checkbox"]')] as HTMLInputElement[];

const status = (root: HTMLElement) => {
  const line = root.querySelector('.settings-status')!;
  return {
    text: line.textContent!.replace(/\s+/g, ' ').trim(),
    warning: line.classList.contains('warning'),
  };
};

test('check reuse is opt-in, survives loading and is cleared when the command changes', async () => {
  const root = await page({ checks: [{ kind: 'test', command: 'npm test', enabled: true }] });
  const reuse = () =>
    root.querySelector('input[aria-label^="Reuse successful"]') as HTMLInputElement;
  assert.equal(reuse().checked, false);
  reuse().click();
  await flush();
  const saved = saves.at(-1) as { checks: { reuseSuccess?: boolean }[] };
  assert.equal(saved.checks[0].reuseSuccess, true);
  const loaded = await page(saved);
  assert.equal(
    (loaded.querySelector('input[aria-label^="Reuse successful"]') as HTMLInputElement).checked,
    true
  );
  const command = loaded.querySelector('.project-check-command') as HTMLInputElement;
  command.value = 'npm run integration';
  command.dispatchEvent(new Event('change', { bubbles: true }));
  await flush();
  assert.equal((saves.at(-1) as typeof saved).checks[0].reuseSuccess, undefined);
  assert.equal(
    (loaded.querySelector('input[aria-label^="Reuse successful"]') as HTMLInputElement).checked,
    false
  );
});

test('malformed saved reuse values never opt a check in', async () => {
  const root = await page({
    checks: [{ kind: 'test', command: 'npm test', enabled: true, reuseSuccess: 'true' }],
  });
  assert.equal(
    (root.querySelector('input[aria-label^="Reuse successful"]') as HTMLInputElement).checked,
    false
  );
});

test('project checks: found in the project’s files, none is ticked and nothing runs', async () => {
  const root = await page();
  assert.deepEqual(
    boxes(root).map((b) => [b.getAttribute('aria-label'), b.checked]),
    [
      ['Run Type check', false],
      ['Run Tests', false],
    ]
  );
  assert.deepEqual(status(root), {
    text: 'No check is ticked: nothing runs after the agent’s changes until you tick one below. Agent runs',
    warning: true,
  });
  assert.match(root.textContent!, /Found in the project’s files\. None runs until you tick it/);
  assert.deepEqual(saves, [], 'opening the page saves nothing');

  const off = await page(null, { verifyChanges: false });
  assert.deepEqual(status(off), {
    text: 'Checking is off: these checks don’t run after the agent’s changes. Agent runs',
    warning: true,
  });
});

test('project checks: ticking one turns on that one alone, and the list is the user’s', async () => {
  const root = await page();
  boxes(root)[1].click();
  await flush();
  assert.deepEqual(saves, [
    {
      checks: [
        { kind: 'typecheck', command: 'npx tsc --noEmit', enabled: false, source: 'tsconfig.json' },
        { kind: 'test', command: 'npm test', enabled: true, source: 'package.json scripts.test' },
      ],
    },
  ]);
  assert.deepEqual(
    boxes(root).map((b) => b.checked),
    [false, true]
  );
  assert.deepEqual(status(root), {
    text: 'After a run in which the agent changed files, the ticked checks run; a failure goes back to the agent to fix. Agent runs',
    warning: false,
  });
  assert.match(root.textContent!, /Your list for this project\./);
});

test('project checks: saving the checks keeps the project’s restricted files', async () => {
  const restrictedFiles = [{ path: '.env', blockRead: false, blockWrite: true }];
  const root = await page({ restrictedFiles });
  boxes(root)[0].click();
  await flush();
  assert.deepEqual(saves, [
    {
      restrictedFiles,
      checks: [
        { kind: 'typecheck', command: 'npx tsc --noEmit', enabled: true, source: 'tsconfig.json' },
        { kind: 'test', command: 'npm test', enabled: false, source: 'package.json scripts.test' },
      ],
    },
  ]);
  // Back to the detected checks: only the checks go.
  const custom = await page({ checks: [], restrictedFiles });
  (
    [...custom.querySelectorAll('button.link-button')].find(
      (b) => b.textContent === 'Use detected checks'
    ) as HTMLButtonElement
  ).click();
  await flush();
  assert.deepEqual(saves, [{ restrictedFiles }]);
});

test('project checks: the user’s list stands; back to the detected ones, none is ticked', async () => {
  const root = await page({ checks: [{ kind: 'test', command: 'npm test', enabled: true }] });
  assert.deepEqual(
    boxes(root).map((b) => b.checked),
    [true]
  );
  assert.equal(status(root).warning, false);
  (
    [...root.querySelectorAll('button.link-button')].find(
      (b) => b.textContent === 'Use detected checks'
    ) as HTMLButtonElement
  ).click();
  await flush();
  assert.deepEqual(saves, [null], 'the saved list is removed');
  assert.deepEqual(
    boxes(root).map((b) => b.checked),
    [false, false]
  );
  assert.equal(status(root).warning, true);
});

test('project checks: a check in a folder runs when what it runs there is allowed, and Allow it adds just that', async () => {
  const root = await page({
    checks: [
      { kind: 'lint', command: 'cd api && go vet ./...', enabled: true },
      { kind: 'test', command: 'cd web && npm test', enabled: true },
    ],
  });
  const warnings = [...root.querySelectorAll('.project-check-warning')];
  assert.equal(warnings.length, 1, '`npm test` is allowed, so its check in web runs');
  const command = warnings[0]
    .closest('.project-check')!
    .querySelector('.project-check-command') as HTMLInputElement;
  assert.equal(command.value, 'cd api && go vet ./...');
  (warnings[0].querySelector('button') as HTMLButtonElement).click();
  assert.deepEqual(
    permissionChanges.map((p) => p.allowedCommands),
    [[...DEFAULT_PERMISSIONS.allowedCommands, 'go vet ./...']]
  );
});
