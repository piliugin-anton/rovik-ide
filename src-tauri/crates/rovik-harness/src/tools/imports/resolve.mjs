// Trusted resolution adapter embedded in the Rust tool. Never import candidate modules.
import fs from 'node:fs';
import path from 'node:path';
import { createRequire, isBuiltin } from 'node:module';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const input = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const root = '/build/project';
const resolutions = new Map();
if (input.typescript_project) {
  const project = fs.realpathSync(`/oracle/${input.typescript_project}`);
  if (!project.startsWith('/oracle/')) throw new Error('Compiler project escaped the oracle');
  const config = JSON.parse(fs.readFileSync(project, 'utf8'));
  if (config.extends || config.references)
    throw new Error(
      'Use a self-contained protected TypeScript JSON project for architecture checks'
    );
  // TypeScript 7 replaced the old JS compiler API. Its pinned native compiler
  // reports the actual module resolutions, including conditional type exports.
  const trace = execFileSync(
    '/usr/local/bin/node',
    [
      '/usr/local/lib/node_modules/typescript/bin/tsc',
      '--project',
      project,
      '--traceResolution',
      '--listFilesOnly',
      '--pretty',
      'false',
      '--rootDir',
      root,
      '--outDir',
      `${root}/dist`,
    ],
    { cwd: root, encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 }
  );
  const starts = new Map(
    input.imports
      .filter((entry) => !entry.runtime_source)
      .map((entry) => [
        `======== Resolving module '${entry.module}' from '${path.join(root, entry.source)}'. ========`,
        entry,
      ])
  );
  let active;
  for (const line of trace.split(/\r?\n/)) {
    if (line.startsWith('======== Resolving module ')) {
      active = starts.get(line);
    } else if (active && line.startsWith(`======== Module name '${active.module}' `)) {
      const prefix = `======== Module name '${active.module}' was successfully resolved to '`;
      if (line.startsWith(prefix)) {
        const match = /^(.*?)'(?: with Package ID '.*')?\. ========$/.exec(
          line.slice(prefix.length)
        );
        if (!match) throw new Error('Malformed native TypeScript resolution');
        const key = JSON.stringify([active.source, active.module]);
        const paths = resolutions.get(key) ?? new Set();
        paths.add(match[1]);
        resolutions.set(key, paths);
      }
      active = undefined;
    }
  }
}

function target(filename) {
  if (input.typescript_project && filename.startsWith(`${root}/dist/`)) {
    const original = input.emitted[path.relative(root, filename)];
    if (!original) throw new Error(`Emitted import has no parsed source: ${filename}`);
    filename = path.join(root, original);
  }
  const resolved = fs.realpathSync(filename);
  if (!fs.statSync(resolved).isFile()) throw new Error(`Import is not a file: ${filename}`);
  if (!resolved.startsWith(`${root}/`))
    throw new Error(`Import escaped prepared project: ${resolved}`);
  const relative = path.relative(root, resolved);
  if (!relative.split(path.sep).includes('node_modules'))
    return [{ id: relative, external: false }];
  // The installed package identity also covers npm aliases. Third-party internals are opaque.
  let directory = path.dirname(resolved);
  while (directory !== root) {
    const parent = path.dirname(directory);
    if (
      path.basename(parent) === 'node_modules' ||
      (path.basename(parent).startsWith('@') &&
        path.basename(path.dirname(parent)) === 'node_modules')
    ) {
      const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8'));
      if (typeof manifest.name !== 'string' || !manifest.name)
        throw new Error('Package lacks identity');
      return [{ id: `npm:${manifest.name}`, external: true }];
    }
    directory = parent;
  }
  throw new Error(`Unattributed package import: ${filename}`);
}

const output = input.imports.map((entry) => {
  const from = path.join(root, entry.source);
  const specifier = entry.module;
  if (['module', 'node:module', 'vm', 'node:vm'].includes(specifier))
    throw new Error(
      'Custom loaders and dynamic code execution need a separate architecture checker'
    );
  let targets = [];
  if (input.typescript_project && !entry.runtime_source && !isBuiltin(specifier)) {
    const paths = resolutions.get(JSON.stringify([entry.source, specifier]));
    if (!paths?.size)
      throw new Error(
        `Unresolved TypeScript import ${specifier} in ${entry.source}; the protected project must cover every analyzed module`
      );
    for (const filename of paths) targets.push(...target(filename));
  }
  if (!isBuiltin(specifier) && (!input.typescript_project || entry.runtime_source)) {
    const emitted = entry.runtime_source ? path.join(root, entry.runtime_source) : from;
    const resolved = entry.require
      ? createRequire(emitted).resolve(specifier)
      : import.meta.resolve(specifier, pathToFileURL(emitted));
    if (!entry.require && !resolved.startsWith('file:'))
      throw new Error(`Unsupported import URL: ${specifier}`);
    targets.push(...target(entry.require ? resolved : fileURLToPath(resolved)));
  }
  if (isBuiltin(specifier))
    targets.push({
      id: specifier.startsWith('node:') ? specifier : `node:${specifier}`,
      external: true,
    });
  // A declaration package can describe a different runtime package (for example @types/foo).
  if (
    targets.some((item) => item.external) &&
    !isBuiltin(specifier) &&
    !specifier.startsWith('.') &&
    !specifier.startsWith('/') &&
    !specifier.startsWith('#')
  ) {
    const parts = specifier.split('/');
    const name = specifier.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0];
    targets.push({ id: `npm:${name}`, external: true });
  }
  return targets;
});
process.stdout.write(JSON.stringify(output));
