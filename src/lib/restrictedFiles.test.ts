// restrictedFiles.ts tests (node:test): the user's list as the settings
// page edits it, the paths it keeps, the menu's line, and that saving the
// list keeps the project's other settings. Run with: npm test
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  isPattern,
  projectPath,
  restrictedSummary,
  savedEntries,
  updateRestrictedFiles,
  withEntry,
  withoutEntry,
  type RestrictedFile,
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

const LISTED: RestrictedFile[] = [
  { path: '.env.local', blockRead: false, blockWrite: true },
  { path: 'config/keys.json', blockRead: true, blockWrite: true },
];

test('setting a file’s boxes adds or replaces its entry in the user’s list', () => {
  // A file found but never changed gets its first entry.
  assert.deepEqual(withEntry(LISTED, { path: '.env', blockRead: false, blockWrite: true }), [
    ...LISTED,
    { path: '.env', blockRead: false, blockWrite: true },
  ]);
  // One already listed is replaced, not doubled.
  assert.deepEqual(withEntry(LISTED, { path: '.env.local', blockRead: true, blockWrite: true }), [
    { path: 'config/keys.json', blockRead: true, blockWrite: true },
    { path: '.env.local', blockRead: true, blockWrite: true },
  ]);
  assert.deepEqual(withoutEntry(LISTED, 'config/keys.json'), [LISTED[0]]);
});

test('the saved list is read as the backend reads it: a box left out blocks', () => {
  assert.deepEqual(savedEntries(undefined), []);
  assert.deepEqual(savedEntries({ path: 'x' }), []);
  assert.deepEqual(
    savedEntries([
      { path: 'a', blockRead: false },
      { path: '', blockRead: true },
      'b',
      { path: 'c', blockRead: true, blockWrite: false },
    ]),
    [
      { path: 'a', blockRead: false, blockWrite: true },
      { path: 'c', blockRead: true, blockWrite: false },
    ]
  );
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

test('changing the list keeps the project’s checks; an empty list is removed', async () => {
  const checks = [{ kind: 'test', command: 'npm test', enabled: true }];
  stored = { checks };
  const entry = { path: '.env', blockRead: false, blockWrite: true };
  await updateRestrictedFiles('/home/me/demo', (entries) => withEntry(entries, entry));
  assert.deepEqual(stored, { checks, restrictedFiles: [entry] });
  assert.equal(calls[calls.length - 1].args.root, '/home/me/demo');

  await updateRestrictedFiles('/home/me/demo', (entries) => withoutEntry(entries, '.env'));
  assert.deepEqual(stored, { checks });
  stored = { restrictedFiles: [entry] };
  await updateRestrictedFiles('/home/me/demo', () => []);
  assert.equal(stored, null, 'nothing left: the project’s settings are removed');
});

test('changes made one after another each keep the one before, however long saving takes', async () => {
  stored = null;
  // The first takes longer to save: the second still starts from it.
  saveTakes = (settings) => (settings?.restrictedFiles?.length === 1 ? 30 : 0);
  const a = { path: 'a', blockRead: true, blockWrite: true };
  const b = { path: 'b', blockRead: false, blockWrite: true };
  await Promise.all([
    updateRestrictedFiles('/p', (entries) => withEntry(entries, a)),
    updateRestrictedFiles('/p', (entries) => withEntry(entries, b)),
  ]);
  assert.deepEqual(stored, { restrictedFiles: [a, b] });
  saveTakes = () => 0;
});

test('a typed pattern is told apart from a path', () => {
  assert.equal(isPattern('*.pem'), true);
  assert.equal(isPattern('config/key?.json'), true);
  assert.equal(isPattern('config/keys.json'), false);
  // Brackets are part of names: Next.js's dynamic routes.
  assert.equal(isPattern('app/[id]/secret.json'), false);
  assert.equal(isPattern('app/[...slug]/.env.local'), false);
});
