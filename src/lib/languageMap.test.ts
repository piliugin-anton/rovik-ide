import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { createLanguageResolver, type LanguageInfo } from './languageMap';

/** The languages the installed Monaco registers, read from its sources. */
function monacoLanguages(): LanguageInfo[] {
  const root = join(process.cwd(), 'node_modules/monaco-editor/esm/vs/languages');
  const files = [
    ...readdirSync(join(root, 'definitions')).map((d) =>
      join(root, 'definitions', d, 'register.js')
    ),
    ...readdirSync(join(root, 'features')).map((d) => join(root, 'features', d, 'register.js')),
  ].filter((f) => existsSync(f));
  const list = (block: string, key: string) => {
    const m = block.match(new RegExp(`${key}:\\s*\\[([^\\]]*)\\]`));
    return m ? [...m[1].matchAll(/"([^"]*)"/g)].map((x) => x[1]) : undefined;
  };
  const out: LanguageInfo[] = [
    { id: 'plaintext', extensions: ['.txt'], aliases: ['Plain Text', 'text'] },
  ];
  for (const file of files) {
    for (const m of readFileSync(file, 'utf8').matchAll(
      /(?:registerLanguage|languages\.register)\(\{([\s\S]*?)\n\}\)/g
    )) {
      const id = m[1].match(/id:\s*"([^"]+)"/)?.[1];
      if (!id) continue;
      out.push({
        id,
        extensions: list(m[1], 'extensions'),
        filenames: list(m[1], 'filenames'),
        filenamePatterns: list(m[1], 'filenamePatterns'),
        aliases: list(m[1], 'aliases'),
      });
    }
  }
  return out;
}

const languages = monacoLanguages();
const r = createLanguageResolver(languages);

test('every extension Monaco registers opens in its language', () => {
  assert.ok(languages.length > 70, `only ${languages.length} languages found`);
  const owners = new Map<string, string[]>();
  for (const l of languages)
    for (const e of l.extensions ?? [])
      owners.set(e.toLowerCase(), [...(owners.get(e.toLowerCase()) ?? []), l.id]);
  for (const [ext, ids] of owners) {
    // Two languages claim it (".pp": Pascal and Puppet): the first keeps it.
    assert.equal(r.forFile(`file${ext}`), ids[0], ext);
  }
});

test('file names and the common cases', () => {
  const cases: [string, string][] = [
    ['src/App.tsx', 'typescript'],
    ['index.d.ts', 'typescript'],
    ['main.rs', 'rust'],
    ['Dockerfile', 'dockerfile'],
    ['Dockerfile.dev', 'dockerfile'],
    ['Makefile', 'shell'],
    ['Cargo.toml', 'ini'],
    ['Cargo.lock', 'ini'],
    ['Gemfile', 'ruby'],
    ['.gitconfig', 'ini'],
    ['.eslintrc', 'json'],
    ['.eslintrc.json', 'json'],
    ['tsconfig.jsonc', 'json'],
    ['App.vue', 'html'],
    ['C:\\proj\\run.PS1', 'powershell'],
    ['page.html.liquid', 'liquid'],
    ['notes.txt', 'plaintext'],
    ['LICENSE', 'plaintext'],
    ['data.unknownext', 'plaintext'],
  ];
  for (const [name, id] of cases) assert.equal(r.forFile(name), id, name);
});

test('code fence names', () => {
  const cases: [string | undefined, string][] = [
    ['ts', 'typescript'],
    ['TypeScript', 'typescript'],
    ['rs', 'rust'],
    ['py', 'python'],
    ['yml', 'yaml'],
    ['sh', 'shell'],
    ['bash', 'shell'],
    ['cs', 'csharp'],
    ['c++', 'cpp'],
    ['kotlin', 'kotlin'],
    ['toml', 'ini'],
    ['dockerfile', 'dockerfile'],
    ['nope', 'plaintext'],
    [undefined, 'plaintext'],
  ];
  for (const [alias, id] of cases) assert.equal(r.forAlias(alias), id, String(alias));
});

test('every override names a language Monaco has', async () => {
  const src = readFileSync(join(process.cwd(), 'src/lib/languageMap.ts'), 'utf8');
  const ids = new Set(languages.map((l) => l.id));
  for (const m of src.matchAll(/:\s*'([a-z0-9.-]+)',/g)) assert.ok(ids.has(m[1]), m[1]);
});
