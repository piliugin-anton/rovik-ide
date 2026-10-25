// restrictedFiles.ts tests (node:test): the user's list as the settings
// page edits it, the paths it keeps, the menu's line, and that saving the
// list keeps the project's other settings. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  listedEntries,
  projectPath,
  restrictedSummary,
  saveRestrictedFiles,
  withEntry,
  withoutEntry,
  type RestrictedFileRow,
} from './restrictedFiles';

/** The project's saved settings, as the backend keeps them, and what it was asked. */
let stored: unknown = null;
const calls: { cmd: string; args: any }[] = [];
/** How long the backend takes to save these settings, in milliseconds. */
let saveTakes = (_settings: any) => 0;
(globalThis as any).window = {
  __TAURI_INTERNALS__: {
    invoke: async (cmd: string, args: any) => {
      calls.push({ cmd, args });
      if (cmd === 'load_project_settings') return stored;
      if (cmd === 'save_project_settings') {
        await new Promise((resolve) => setTimeout(resolve, saveTakes(args.settings)));
        stored = args.settings;
        return;
      }
      throw new Error(`no app: ${cmd}`);
    },
  },
};

const row = (
  path: string,
  blockRead: boolean,
  blockWrite: boolean,
  detected: boolean,
  listed: boolean
): RestrictedFileRow => ({ path, blockRead, blockWrite, detected, listed });

const ROWS = [
  row('.env', true, true, true, false),
  row('.env.local', false, true, true, true),
  row('config/keys.json', true, true, false, true),
];

test('the user’s list is the entries they made; setting a file’s boxes adds or replaces its entry', () => {
  assert.deepEqual(listedEntries(ROWS), [
    { path: '.env.local', blockRead: false, blockWrite: true },
    { path: 'config/keys.json', blockRead: true, blockWrite: true },
  ]);
  // A file found but never changed gets its first entry.
  assert.deepEqual(withEntry(ROWS, { path: '.env', blockRead: false, blockWrite: true }), [
    { path: '.env.local', blockRead: false, blockWrite: true },
    { path: 'config/keys.json', blockRead: true, blockWrite: true },
    { path: '.env', blockRead: false, blockWrite: true },
  ]);
  // One already listed is replaced, not doubled.
  assert.deepEqual(withEntry(ROWS, { path: '.env.local', blockRead: true, blockWrite: true }), [
    { path: 'config/keys.json', blockRead: true, blockWrite: true },
    { path: '.env.local', blockRead: true, blockWrite: true },
  ]);
  assert.deepEqual(withoutEntry(ROWS, 'config/keys.json'), [
    { path: '.env.local', blockRead: false, blockWrite: true },
  ]);
});

test('a typed path is kept relative to the project, and one outside it is refused', () => {
  const root = '/home/me/demo';
  assert.equal(projectPath('config/keys.json', root), 'config/keys.json');
  assert.equal(projectPath('  ./config//keys.json ', root), 'config/keys.json');
  assert.equal(projectPath('config\\keys.json', root), 'config/keys.json');
  assert.equal(projectPath('/home/me/demo/secrets/', root), 'secrets');
  assert.equal(projectPath('../other/.env', root), null);
  assert.equal(projectPath('/etc/passwd', root), null);
  assert.equal(projectPath('C:\\Users\\me\\.aws\\credentials', root), null);
  assert.equal(projectPath('/home/me/demo', root), null, 'the project itself');
  assert.equal(projectPath('.', root), null);
  assert.equal(projectPath('', root), null);
  assert.equal(projectPath('C:\\proj\\.env', 'C:\\proj'), '.env');
});

test('the menu’s line says how many files are kept from the agent', () => {
  assert.equal(restrictedSummary([]), 'No files with credentials found');
  assert.equal(restrictedSummary(ROWS), '3 files kept from the agent');
  assert.equal(restrictedSummary([ROWS[0]]), '1 file kept from the agent');
  assert.equal(
    restrictedSummary([row('.env', false, false, true, true)]),
    'Every file listed is open to the agent'
  );
});

test('saving the list keeps the project’s checks; an empty list is removed', async () => {
  const checks = [{ kind: 'test', command: 'npm test', enabled: true }];
  stored = { checks };
  const entry = { path: '.env', blockRead: false, blockWrite: true };
  await saveRestrictedFiles('/home/me/demo', [entry]);
  assert.deepEqual(stored, { checks, restrictedFiles: [entry] });
  assert.equal(calls[calls.length - 1].args.root, '/home/me/demo');

  await saveRestrictedFiles('/home/me/demo', []);
  assert.deepEqual(stored, { checks });
  stored = { restrictedFiles: [entry] };
  await saveRestrictedFiles('/home/me/demo', []);
  assert.equal(stored, null, 'nothing left: the project’s settings are removed');
});

test('saves are made in order: a later one is never overwritten by an earlier one', async () => {
  stored = null;
  // The first takes longer to save: it still lands first.
  saveTakes = (settings) => (settings?.restrictedFiles?.[0]?.path === 'a' ? 30 : 0);
  const first = saveRestrictedFiles('/p', [{ path: 'a', blockRead: true, blockWrite: true }]);
  const second = saveRestrictedFiles('/p', [{ path: 'b', blockRead: true, blockWrite: true }]);
  await Promise.all([first, second]);
  assert.deepEqual(stored, { restrictedFiles: [{ path: 'b', blockRead: true, blockWrite: true }] });
});
