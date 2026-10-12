// typeRoots.ts
//
// Which @types packages a config sees by default, as tsc: those in every
// `node_modules/@types` folder from the config's own up (its default
// typeRoots). A workspace member's config sees its member's packages and
// the root's; the root's config only the root's. Pure, over URI strings.

const TYPES_PACKAGE = /^(.*\/node_modules)\/@types\/([^/]+)\//;

/** Package names by the URI of the `node_modules` folder holding their `@types`. */
export function typesPackagesByFolder(libUris: Iterable<string>): Map<string, string[]> {
  const byFolder = new Map<string, Set<string>>();
  for (const uri of libUris) {
    const match = TYPES_PACKAGE.exec(uri);
    if (!match) continue;
    const names = byFolder.get(match[1]) ?? new Set();
    names.add(match[2]);
    byFolder.set(match[1], names);
  }
  return new Map([...byFolder].map(([folder, names]) => [folder, [...names].sort()]));
}

/** The packages a config in `folderUri` sees, from its folder up to the root. */
export function installedTypesFor(
  folderUri: string,
  byFolder: ReadonlyMap<string, readonly string[]>
): string[] {
  const names = new Set<string>();
  for (const folder of ancestorsOf(folderUri)) {
    for (const name of byFolder.get(`${folder}/node_modules`) ?? []) names.add(name);
  }
  return [...names].sort();
}

/** The folder and each one above it, down to the URI's root (`file:///`). */
function ancestorsOf(folderUri: string): string[] {
  const out: string[] = [];
  let folder = folderUri.replace(/\/+$/, '');
  const rootLength = folderUri.indexOf('///') + 2; // `file://` + the root slash
  while (folder.length > rootLength) {
    out.push(folder);
    folder = folder.slice(0, Math.max(folder.lastIndexOf('/'), rootLength));
  }
  return out;
}
