#!/usr/bin/env node
// The release's latest.json, which the app's updates read
// (src-tauri/src/updates.rs, tauri-plugin-updater): for each kind of app,
// the bundle on the release that updates it and that bundle's signature,
// which `tauri build` made with the release key. The release workflow
// writes it once every platform's bundles are on the draft release:
//
//   node scripts/updater-manifest.mjs --version 1.2.3 --tag v1.2.3 \
//     --repo OWNER/REPO --assets assets.txt --sigs sigs [--notes notes.md] \
//     [--expect linux-x86_64,windows-x86_64] > latest.json
//
// `assets.txt` names the release's files, a line each, and `sigs` holds
// their .sig files. The bundles, as tauri-action names them (the product
// name's space a dot), and what each updates:
//
//   Rovik.IDE_1.2.3_amd64.AppImage   linux-x86_64-appimage
//   Rovik.IDE_1.2.3_amd64.deb        linux-x86_64-deb
//   Rovik.IDE-1.2.3-1.x86_64.rpm     linux-x86_64-rpm
//   Rovik.IDE_1.2.3_x64-setup.exe    windows-x86_64-nsis
//   Rovik.IDE_1.2.3_x64_en-US.msi    windows-x86_64-msi
//   Rovik.IDE_aarch64.app.tar.gz     darwin-aarch64-app
//   Rovik.IDE_x64.app.tar.gz         darwin-x86_64-app
//
// An app looks for its own kind first ({os}-{arch}-{installer}), then for
// {os}-{arch}: the AppImage, the NSIS installer and the app bundle. A
// platform `--expect` names that has none fails the release: its apps
// would be told the release has nothing for them.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseArgs } from 'node:util';
import { pathToFileURL } from 'node:url';

/** The bundles an updater installs, by name; `primary` is its platform's {os}-{arch}. */
const KINDS = [
  { test: /\.AppImage$/, os: 'linux', installer: 'appimage', primary: true },
  { test: /\.deb$/, os: 'linux', installer: 'deb' },
  { test: /\.rpm$/, os: 'linux', installer: 'rpm' },
  { test: /-setup\.exe$/, os: 'windows', installer: 'nsis', primary: true },
  { test: /\.msi$/, os: 'windows', installer: 'msi' },
  { test: /\.app\.tar\.gz$/, os: 'darwin', installer: 'app', primary: true },
];

/** The CPU a bundle's name says, as the updater names it; null when it says none. */
export function archOf(name) {
  const word = (pattern) => new RegExp(`(^|[._-])(${pattern})([._-]|$)`, 'i').test(name);
  if (word('universal')) return 'universal';
  if (word('aarch64|arm64')) return 'aarch64';
  if (word('x86_64|amd64|x64')) return 'x86_64';
  if (word('i686|i386|x86')) return 'i686';
  return null;
}

/**
 * The platforms of latest.json: for each bundle among `assets` that has a
 * signature (`signatureOf(name)`, null when it has none), its kind's key
 * and, for its platform's primary kind, {os}-{arch}. `urlOf(name)` is the
 * bundle's download.
 */
export function platformsOf(assets, signatureOf, urlOf) {
  const platforms = {};
  const names = new Set(assets);
  for (const name of [...names].sort()) {
    const kind = KINDS.find((k) => k.test.test(name));
    if (!kind) continue;
    const arch = archOf(name);
    if (!arch || !names.has(`${name}.sig`)) continue;
    const signature = signatureOf(name);
    if (!signature) continue;
    const entry = { signature, url: urlOf(name) };
    // A universal build (macOS) serves both CPUs, unless one has its own.
    const archs = arch === 'universal' ? ['aarch64', 'x86_64'] : [arch];
    for (const cpu of archs) {
      const keys = [`${kind.os}-${cpu}-${kind.installer}`];
      if (kind.primary) keys.push(`${kind.os}-${cpu}`);
      for (const key of keys) {
        if (arch === 'universal' && platforms[key]) continue;
        platforms[key] = entry;
      }
    }
  }
  return platforms;
}

/** latest.json, as tauri-plugin-updater reads it. */
export function manifest({ version, notes = '', date = new Date(), platforms }) {
  return { version, notes, pub_date: date.toISOString(), platforms };
}

/** A release file's download once the release is published. */
export function downloadUrl(repo, tag, name) {
  return `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`;
}

function main() {
  const { values } = parseArgs({
    options: {
      version: { type: 'string' },
      tag: { type: 'string' },
      repo: { type: 'string' },
      assets: { type: 'string' },
      sigs: { type: 'string' },
      notes: { type: 'string' },
      expect: { type: 'string' },
    },
  });
  for (const required of ['version', 'tag', 'repo', 'assets', 'sigs']) {
    if (!values[required]) throw new Error(`--${required} is missing`);
  }
  const assets = readFileSync(values.assets, 'utf8')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
  const signatureOf = (name) => {
    try {
      return readFileSync(join(values.sigs, `${name}.sig`), 'utf8').trim() || null;
    } catch {
      return null;
    }
  };
  const platforms = platformsOf(assets, signatureOf, (name) =>
    downloadUrl(values.repo, values.tag, name)
  );
  const missing = (values.expect ?? '')
    .split(',')
    .map((p) => p.trim())
    .filter((p) => p && !platforms[p]);
  if (missing.length > 0) {
    throw new Error(`No signed update for ${missing.join(', ')} among the release's files.`);
  }
  const notes = values.notes ? readFileSync(values.notes, 'utf8').trim() : '';
  const latest = manifest({ version: values.version, notes, platforms });
  process.stdout.write(`${JSON.stringify(latest, null, 2)}\n`);
  for (const key of Object.keys(platforms).sort()) {
    process.stderr.write(`${key}: ${platforms[key].url}\n`);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  try {
    main();
  } catch (err) {
    process.stderr.write(`updater-manifest: ${err instanceof Error ? err.message : err}\n`);
    process.exit(1);
  }
}
