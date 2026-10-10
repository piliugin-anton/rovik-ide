// languageMap.ts
//
// Which Monaco language a file or a code fence is in. The languages and
// their file types come from Monaco's own registry
// (monaco.languages.getLanguages(), see language.ts): every language the
// editor can color, with the extensions, file names and aliases it was
// registered with. On top of that, the file types Monaco has no language
// for get the closest one it has (OVERRIDES). No Monaco import here, so it
// can be tested on its own.

/** As Monaco registers a language (languages.ILanguageExtensionPoint). */
export interface LanguageInfo {
  id: string;
  extensions?: string[];
  filenames?: string[];
  filenamePatterns?: string[];
  aliases?: string[];
}

/**
 * File types Monaco has no language for, by file name (lowercase) or by
 * extension (with its dot): the nearest language it does have.
 */
const FILENAME_OVERRIDES: Record<string, string> = {
  // Make: shell-like variables, comments and strings.
  makefile: 'shell',
  gnumakefile: 'shell',
  containerfile: 'dockerfile',
  'cargo.lock': 'ini',
  '.bashrc': 'shell',
  '.bash_profile': 'shell',
  '.bash_aliases': 'shell',
  '.bash_logout': 'shell',
  '.zshrc': 'shell',
  '.zshenv': 'shell',
  '.zprofile': 'shell',
  '.profile': 'shell',
  '.envrc': 'shell',
  '.env': 'ini',
  '.npmrc': 'ini',
  '.yarnrc': 'yaml',
  '.prettierrc': 'json',
  '.swcrc': 'json',
  // Starlark (Bazel, Buck) is a Python dialect.
  build: 'python',
  'build.bazel': 'python',
  workspace: 'python',
  'workspace.bazel': 'python',
  // Ruby DSLs.
  vagrantfile: 'ruby',
  podfile: 'ruby',
  brewfile: 'ruby',
  fastfile: 'ruby',
  guardfile: 'ruby',
};

const EXTENSION_OVERRIDES: Record<string, string> = {
  '.jsonc': 'json',
  '.json5': 'json',
  '.jsonl': 'json',
  '.ipynb': 'json',
  '.webmanifest': 'json',
  '.code-workspace': 'json',
  '.toml': 'ini',
  '.cfg': 'ini',
  '.conf': 'ini',
  '.env': 'ini',
  '.vue': 'html',
  '.svelte': 'html',
  '.astro': 'html',
  '.zsh': 'shell',
  '.ksh': 'shell',
  '.fish': 'shell',
  '.mk': 'shell',
  '.bzl': 'python',
  '.star': 'python',
  '.pyi': 'python',
  '.h++': 'cpp',
  '.c++': 'cpp',
  '.ino': 'cpp',
  '.cu': 'cpp',
  '.cuh': 'cpp',
  '.mm': 'objective-c',
  '.groovy': 'java',
  '.gradle': 'java',
  '.rake': 'ruby',
  '.ru': 'ruby',
  '.erb': 'html',
  '.ejs': 'html',
  '.njk': 'twig',
  '.jinja': 'twig',
  '.jinja2': 'twig',
  '.j2': 'twig',
  '.plist': 'xml',
  '.xib': 'xml',
  '.storyboard': 'xml',
  '.resx': 'xml',
  '.vcxproj': 'xml',
  '.fsproj': 'xml',
  '.vbproj': 'xml',
  '.nuspec': 'xml',
  '.wsdl': 'xml',
  '.gpx': 'xml',
  '.kml': 'xml',
  '.rss': 'xml',
  '.atom': 'xml',
};

/** Fence names models write that aren't a language's alias. */
const ALIAS_OVERRIDES: Record<string, string> = {
  sh: 'shell',
  zsh: 'shell',
  console: 'shell',
  shellscript: 'shell',
  ps: 'powershell',
  ps1: 'powershell',
  pwsh: 'powershell',
  cmd: 'bat',
  batch: 'bat',
  toml: 'ini',
  jsonc: 'json',
  json5: 'json',
  vue: 'html',
  svelte: 'html',
  make: 'shell',
  makefile: 'shell',
  text: 'plaintext',
  txt: 'plaintext',
  plain: 'plaintext',
  diff: 'plaintext',
  patch: 'plaintext',
  objc: 'objective-c',
  'c++': 'cpp',
  golang: 'go',
  docker: 'dockerfile',
  terraform: 'hcl',
  tf: 'hcl',
  proto3: 'proto',
  protobuf: 'proto',
};

export interface LanguageResolver {
  /** The language of a file, by its name (or path). 'plaintext' if none. */
  forFile(name: string): string;
  /** The language a code fence names (```ts, ```Rust, ```py). 'plaintext' if none. */
  forAlias(alias: string | undefined): string;
}

/** A glob from `filenamePatterns` (`*` and `?` only) as a regex. */
function globRegex(glob: string): RegExp {
  const body = glob
    .toLowerCase()
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '.*')
    .replace(/\?/g, '.');
  return new RegExp(`^${body}$`);
}

export function createLanguageResolver(languages: LanguageInfo[]): LanguageResolver {
  const known = new Set(languages.map((l) => l.id));
  const byFilename = new Map<string, string>();
  const byExtension = new Map<string, string>();
  const byAlias = new Map<string, string>();
  const patterns: { re: RegExp; id: string }[] = [];
  // The first language to claim a file type keeps it, as in Monaco.
  const claim = (map: Map<string, string>, key: string, id: string) => {
    if (!map.has(key)) map.set(key, id);
  };

  for (const lang of languages) {
    claim(byAlias, lang.id.toLowerCase(), lang.id);
    for (const alias of lang.aliases ?? []) claim(byAlias, alias.toLowerCase(), lang.id);
    for (const name of lang.filenames ?? []) claim(byFilename, name.toLowerCase(), lang.id);
    for (const ext of lang.extensions ?? []) claim(byExtension, ext.toLowerCase(), lang.id);
    for (const glob of lang.filenamePatterns ?? [])
      patterns.push({ re: globRegex(glob), id: lang.id });
  }
  // Ours fill gaps only where the language exists in this build.
  const add = (map: Map<string, string>, table: Record<string, string>) => {
    for (const [key, id] of Object.entries(table)) if (known.has(id)) claim(map, key, id);
  };
  add(byFilename, FILENAME_OVERRIDES);
  add(byExtension, EXTENSION_OVERRIDES);
  add(byAlias, ALIAS_OVERRIDES);

  const forFile = (path: string): string => {
    const name = (path.split(/[\\/]/).pop() ?? path).toLowerCase();
    const exact = byFilename.get(name);
    if (exact) return exact;
    const pattern = patterns.find((p) => p.re.test(name));
    if (pattern) return pattern.id;
    // "Dockerfile.dev", "Makefile.am": the name before the first dot.
    const stem = name.split('.')[0];
    if (stem && stem !== name) {
      const byStem = byFilename.get(stem);
      if (byStem && (byStem === 'dockerfile' || byStem === 'shell')) return byStem;
    }
    // Longest extension first: ".html.liquid" before ".liquid". A leading
    // dot counts (".gitconfig", ".eslintrc").
    for (let i = name.indexOf('.'); i !== -1; i = name.indexOf('.', i + 1)) {
      const id = byExtension.get(name.slice(i));
      if (id) return id;
    }
    return 'plaintext';
  };

  const forAlias = (alias: string | undefined): string => {
    const key = alias?.trim().toLowerCase();
    if (!key) return 'plaintext';
    // A name (```rust), else a file extension used as one (```rs, ```yml).
    return byAlias.get(key) ?? byExtension.get(`.${key}`) ?? 'plaintext';
  };

  return { forFile, forAlias };
}
