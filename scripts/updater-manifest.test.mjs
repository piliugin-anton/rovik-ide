// updater-manifest.mjs tests (node:test).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { archOf, downloadUrl, manifest, platformsOf } from './updater-manifest.mjs';

/** A release's files as tauri-action and the daemon's job attach them (v0.1.18's, and the signatures a build with the updater's bundles adds). */
const BUNDLES = [
  'Rovik.IDE-1.2.3-1.x86_64.rpm',
  'Rovik.IDE_1.2.3_aarch64.dmg',
  'Rovik.IDE_1.2.3_amd64.AppImage',
  'Rovik.IDE_1.2.3_amd64.deb',
  'Rovik.IDE_1.2.3_x64-setup.exe',
  'Rovik.IDE_1.2.3_x64.dmg',
  'Rovik.IDE_1.2.3_x64_en-US.msi',
  'Rovik.IDE_aarch64.app.tar.gz',
  'Rovik.IDE_x64.app.tar.gz',
];
const ASSETS = [
  ...BUNDLES,
  ...BUNDLES.filter((name) => !name.endsWith('.dmg')).map((name) => `${name}.sig`),
  'rovik-agentd-1.2.3-aarch64.gz',
  'rovik-agentd-1.2.3-aarch64.sig',
  'rovik-agentd-1.2.3-x86_64.gz',
  'rovik-agentd-1.2.3-x86_64.sig',
  'SHA256SUMS',
  'SHA256SUMS.sig',
];

const signed = (name) => `signature of ${name}`;
const url = (name) => downloadUrl('o/r', 'v1.2.3', name);

test('each kind of app gets its bundle, and each platform its main one', () => {
  const platforms = platformsOf(ASSETS, signed, url);
  const entry = (name) => ({ signature: signed(name), url: url(name) });
  assert.deepEqual(platforms, {
    'linux-x86_64': entry('Rovik.IDE_1.2.3_amd64.AppImage'),
    'linux-x86_64-appimage': entry('Rovik.IDE_1.2.3_amd64.AppImage'),
    'linux-x86_64-deb': entry('Rovik.IDE_1.2.3_amd64.deb'),
    'linux-x86_64-rpm': entry('Rovik.IDE-1.2.3-1.x86_64.rpm'),
    'windows-x86_64': entry('Rovik.IDE_1.2.3_x64-setup.exe'),
    'windows-x86_64-nsis': entry('Rovik.IDE_1.2.3_x64-setup.exe'),
    'windows-x86_64-msi': entry('Rovik.IDE_1.2.3_x64_en-US.msi'),
    'darwin-aarch64': entry('Rovik.IDE_aarch64.app.tar.gz'),
    'darwin-aarch64-app': entry('Rovik.IDE_aarch64.app.tar.gz'),
    'darwin-x86_64': entry('Rovik.IDE_x64.app.tar.gz'),
    'darwin-x86_64-app': entry('Rovik.IDE_x64.app.tar.gz'),
  });
});

test('a bundle without its signature, or the daemon’s files, update nothing', () => {
  const unsigned = ASSETS.filter((name) => name !== 'Rovik.IDE_1.2.3_amd64.deb.sig');
  const platforms = platformsOf(unsigned, signed, url);
  assert.equal(platforms['linux-x86_64-deb'], undefined);
  assert.ok(platforms['linux-x86_64-rpm']);
  // A .sig named on the release but not downloaded.
  const unread = platformsOf(ASSETS, (name) => (name.endsWith('.msi') ? null : signed(name)), url);
  assert.equal(unread['windows-x86_64-msi'], undefined);
  assert.ok(unread['windows-x86_64-nsis']);
  assert.ok(Object.keys(platforms).every((key) => !key.includes('agentd')));
});

test('a universal macOS build serves both CPUs unless one has its own', () => {
  const assets = [
    'Rovik.IDE_universal.app.tar.gz',
    'Rovik.IDE_universal.app.tar.gz.sig',
    'Rovik.IDE_aarch64.app.tar.gz',
    'Rovik.IDE_aarch64.app.tar.gz.sig',
  ];
  const platforms = platformsOf(assets, signed, url);
  assert.equal(platforms['darwin-aarch64'].url, url('Rovik.IDE_aarch64.app.tar.gz'));
  assert.equal(platforms['darwin-x86_64'].url, url('Rovik.IDE_universal.app.tar.gz'));
  assert.equal(platforms['darwin-x86_64-app'].url, url('Rovik.IDE_universal.app.tar.gz'));
});

test('the CPU is read from the name', () => {
  assert.equal(archOf('Rovik.IDE_1.2.3_amd64.AppImage'), 'x86_64');
  assert.equal(archOf('Rovik.IDE_1.2.3_arm64.deb'), 'aarch64');
  assert.equal(archOf('Rovik.IDE-1.2.3-1.aarch64.rpm'), 'aarch64');
  assert.equal(archOf('Rovik.IDE_1.2.3_x86-setup.exe'), 'i686');
  assert.equal(archOf('Rovik.IDE_1.2.3_x64_en-US.msi'), 'x86_64');
  assert.equal(archOf('Rovik.IDE.app.tar.gz'), null);
});

test('latest.json has the version, the notes and when it was made', () => {
  const date = new Date('2026-10-21T11:07:27.000Z');
  assert.deepEqual(manifest({ version: '1.2.3', notes: 'n', date, platforms: {} }), {
    version: '1.2.3',
    notes: 'n',
    pub_date: '2026-10-21T11:07:27.000Z',
    platforms: {},
  });
  assert.equal(
    downloadUrl('o/r', 'v1.2.3-4', 'Rovik.IDE_1.2.3-4_x64-setup.exe'),
    'https://github.com/o/r/releases/download/v1.2.3-4/Rovik.IDE_1.2.3-4_x64-setup.exe'
  );
});

test('it reads the release’s files and signatures, and fails a platform it lacks', () => {
  const script = fileURLToPath(new URL('./updater-manifest.mjs', import.meta.url));
  const dir = mkdtempSync(join(tmpdir(), 'updater-manifest-'));
  try {
    writeFileSync(join(dir, 'assets.txt'), `${ASSETS.join('\n')}\n`);
    writeFileSync(join(dir, 'notes.md'), "## What's Changed\n");
    for (const name of ASSETS.filter((n) => n.endsWith('.sig'))) {
      writeFileSync(join(dir, name), `${name.slice(0, -4)} signed\n`);
    }
    const run = (expect) =>
      spawnSync(
        process.execPath,
        [
          script,
          ...['--version', '1.2.3', '--tag', 'v1.2.3', '--repo', 'o/r'],
          ...['--assets', join(dir, 'assets.txt'), '--sigs', dir, '--notes', join(dir, 'notes.md')],
          ...['--expect', expect],
        ],
        { encoding: 'utf8' }
      );
    const out = run('linux-x86_64,windows-x86_64,darwin-aarch64,darwin-x86_64');
    assert.equal(out.status, 0, out.stderr);
    const latest = JSON.parse(out.stdout);
    assert.equal(latest.version, '1.2.3');
    assert.equal(latest.notes, "## What's Changed");
    assert.equal(
      latest.platforms['linux-x86_64'].signature,
      'Rovik.IDE_1.2.3_amd64.AppImage signed'
    );
    assert.equal(
      latest.platforms['windows-x86_64'].url,
      'https://github.com/o/r/releases/download/v1.2.3/Rovik.IDE_1.2.3_x64-setup.exe'
    );
    const lacking = run('linux-x86_64,linux-aarch64');
    assert.equal(lacking.status, 1);
    assert.match(lacking.stderr, /No signed update for linux-aarch64/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
