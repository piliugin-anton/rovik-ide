// libs.ts
//
// The files the TypeScript worker has besides open models: the project's
// sources and its dependencies' typings, as Monaco "extra libs". One owner
// for all of them, so every change goes through one place.
//
// Each lib carries a version the worker compares to decide whether to
// parse a file again. Monaco's `setExtraLibs` bumps the version of EVERY
// lib on every call, which made the worker parse the whole set again on
// each change (a tab opened, a file saved): with thousands of typings that
// was a storm of parsing that ate gigabytes and killed the webview. So the
// set is only ever changed file by file (`addExtraLib` keeps the version
// of a lib whose content is the same, and its disposable removes it), and
// unchanged files keep their parsed form across updates.
//
// Everything handed to the worker crosses a `postMessage` structured-clone
// boundary. A single non-cloneable value (`undefined` content from a failed
// read, a non-string path) makes the worker-side `postMessage` throw
// `DataCloneError` as an unhandled rejection with no hint at which payload
// was bad, so every entry is coerced to plain strings here.
import type * as monaco from 'monaco-editor';
import { forEachDefaults, primaryDefaults } from './defaults';

export interface LibEntry {
  filePath: unknown;
  content: unknown;
}

interface Lib {
  filePath: string;
  content: string;
}

/** Which libs an operation may touch. */
export type LibScope = (uri: string) => boolean;

const ALL: LibScope = () => true;

/** Plain `{ filePath, content }` strings, or null for an unusable entry. */
export function normalizeLib(entry: LibEntry): Lib | null {
  if (typeof entry?.filePath !== 'string' || entry.filePath.length === 0) return null;
  return { filePath: entry.filePath, content: String(entry.content ?? '') };
}

class WorkerLibs {
  private readonly libs = new Map<string, { content: string; disposers: monaco.IDisposable[] }>();

  get size(): number {
    return this.libs.size;
  }

  has(uri: string): boolean {
    return this.libs.has(uri);
  }

  contentOf(uri: string): string | undefined {
    return this.libs.get(uri)?.content;
  }

  uris(): string[] {
    return [...this.libs.keys()];
  }

  /** Gives the worker this file; nothing happens when it has this text. */
  set(uri: string, content: string): boolean {
    if (this.libs.get(uri)?.content === content) return false;
    const disposers: monaco.IDisposable[] = [];
    forEachDefaults((defaults) => disposers.push(defaults.addExtraLib(content, uri)));
    this.libs.set(uri, { content, disposers });
    return true;
  }

  delete(uri: string): boolean {
    const lib = this.libs.get(uri);
    if (!lib) return false;
    this.libs.delete(uri);
    for (const disposer of lib.disposers) {
      try {
        disposer.dispose();
      } catch {
        // ignore: the worker keeps a file it should have lost
      }
    }
    return true;
  }

  /**
   * Makes `entries` the complete set within `scope` (every lib when not
   * given): libs in the scope that aren't in `entries` go, the rest stay.
   * Returns how many libs changed.
   */
  replace(entries: Iterable<LibEntry>, scope: LibScope = ALL): number {
    const next = new Map<string, string>();
    for (const entry of entries) {
      const lib = normalizeLib(entry);
      if (lib) next.set(lib.filePath, lib.content);
    }
    let changed = 0;
    for (const uri of this.libs.keys()) {
      if (!next.has(uri) && scope(uri) && this.delete(uri)) changed++;
    }
    for (const [uri, content] of next) {
      if (this.set(uri, content)) changed++;
    }
    return changed;
  }

  /** What Monaco holds, for the diagnostics aid. */
  snapshot(): Record<string, { content: string; version: number }> {
    return primaryDefaults.getExtraLibs();
  }
}

export const workerLibs = new WorkerLibs();
