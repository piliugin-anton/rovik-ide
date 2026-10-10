// src/lib/language.ts
//
// File types: which files aren't text, and which Monaco language a file or
// a code fence is in (languageMap.ts, from Monaco's own registry).
import * as monaco from 'monaco-editor';
import { createLanguageResolver, type LanguageResolver } from './languageMap';

// Extensions Monaco can't usefully edit as text — checked before we even
// attempt a read, so obviously-binary files never round-trip through IPC.
// The Rust side also null-byte-sniffs as a fallback for anything with no
// extension or a misleading one.
const BINARY_EXTENSIONS = new Set([
  'png',
  'jpg',
  'jpeg',
  'gif',
  'bmp',
  'ico',
  'webp',
  'avif',
  'tiff',
  'pdf',
  'zip',
  'tar',
  'gz',
  'bz2',
  'xz',
  '7z',
  'rar',
  'woff',
  'woff2',
  'ttf',
  'otf',
  'eot',
  'mp3',
  'mp4',
  'wav',
  'ogg',
  'webm',
  'mov',
  'avi',
  'flac',
  'exe',
  'dll',
  'so',
  'dylib',
  'bin',
  'class',
  'wasm',
  'sqlite',
  'db',
  'node',
]);

export function extensionOf(name: string): string {
  const dot = name.lastIndexOf('.');
  return dot === -1 ? '' : name.slice(dot + 1).toLowerCase();
}

export function isLikelyBinary(name: string): boolean {
  return BINARY_EXTENSIONS.has(extensionOf(name));
}

// Built on first use: by then every language Monaco ships is registered
// (importing monaco-editor registers them all).
let resolver: LanguageResolver | null = null;
function languages(): LanguageResolver {
  resolver ??= createLanguageResolver(monaco.languages.getLanguages());
  return resolver;
}

/** The Monaco language of a file, by its name or path; 'plaintext' if none. */
export function languageForFile(name: string): string {
  return languages().forFile(name);
}

/** The Monaco language a code fence names (```ts, ```Rust); 'plaintext' if none. */
export function languageForAlias(alias: string | undefined): string {
  return languages().forAlias(alias);
}
