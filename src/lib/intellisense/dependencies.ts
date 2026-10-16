// dependencies.ts
//
// Which dependencies the editor has loaded (project.ts): the packages, by
// name and by real folder, the links between folders, and the imports no
// installed package was found for. From these, which imports of a file
// still need a package loaded, and what to tell the backend it has.
// Pure: paths as the backend gives them, no URIs.
import type { LoadedTypings } from '../../types';
import { candidatePackages } from './imports';

export class Dependencies {
  private readonly names = new Set<string>();
  private readonly dirs = new Set<string>();
  private readonly missing = new Set<string>();
  private readonly links = new Map<string, string>();

  constructor(typings?: LoadedTypings) {
    if (typings) this.absorb(typings);
  }

  /** The package folders loaded: what a later load may skip. */
  get loadedDirs(): string[] {
    return [...this.dirs];
  }

  /** Every link to a folder known so far, path → target. */
  get allLinks(): ReadonlyMap<string, string> {
    return this.links;
  }

  /** Takes in a load's packages, links and missing imports. Returns whether the links changed. */
  absorb(typings: LoadedTypings): boolean {
    for (const pkg of typings.packages) {
      this.names.add(pkg.name);
      this.dirs.add(pkg.dir);
    }
    for (const spec of typings.missing) this.missing.add(spec);
    let linksChanged = false;
    for (const link of typings.links) {
      if (this.links.get(link.path) !== link.target) {
        this.links.set(link.path, link.target);
        linksChanged = true;
      }
    }
    return linksChanged;
  }

  /** Starts over, with this load (the dependencies were read again). */
  reset(typings: LoadedTypings): void {
    this.names.clear();
    this.dirs.clear();
    this.missing.clear();
    this.links.clear();
    this.absorb(typings);
  }

  /**
   * Of these specifiers, the ones whose package isn't loaded and wasn't
   * found missing: what to ask the backend for. A specifier whose package
   * or `@types` is loaded needs nothing; one that resolves to no package
   * (a relative path, an alias) is left to TypeScript's `paths`.
   */
  wanted(specifiers: Iterable<string>): string[] {
    const out: string[] = [];
    for (const spec of specifiers) {
      if (this.missing.has(spec) || out.includes(spec)) continue;
      const candidates = candidatePackages(spec);
      if (candidates.length === 0 || candidates.some((name) => this.names.has(name))) continue;
      out.push(spec);
    }
    return out;
  }

  /** Nothing is missing any more: the dependencies were installed again. */
  forgetMissing(): void {
    this.missing.clear();
  }
}
