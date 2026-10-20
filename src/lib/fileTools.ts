/**
 * One spelling of a path, for keys: `/` for `\`, runs of them collapsed,
 * a Windows drive letter in lower case. The backend, the file tree and the
 * chat each spell the same file their own way.
 */
export function normalizePath(path: string): string {
  const slashes = path.replace(/\\/g, '/').replace(/\/+/g, '/');
  return /^[A-Z]:\//.test(slashes) ? slashes[0].toLowerCase() + slashes.slice(1) : slashes;
}

/*
 * Strip the rootPath prefix from an absolute path.
 * Falls back to the original path if it isn't under the root.
 */
export function toRelativePath(path: string, root: string | null): string {
  if (!root) return path;

  const normalize = (p: string) => p.replace(/\\/g, '/').replace(/\/+$/, '');
  const normalizedRoot = normalize(root) + '/';
  const normalizedPath = normalize(path);

  if (normalizedPath.startsWith(normalizedRoot)) {
    return normalizedPath.slice(normalizedRoot.length);
  }

  return path;
}

/**
 * A path the model gave (relative to the project, or already absolute)
 * as the absolute path the editor tabs and file tree use.
 */
export function toAbsolutePath(path: string, root: string): string {
  if (/^([a-zA-Z]:[\\/]|[\\/])/.test(path)) return path;
  const sep = root.includes('\\') && !root.includes('/') ? '\\' : '/';
  const relative = path
    .replace(/^\.[\\/]/, '')
    .replace(/[\\/]+/g, sep)
    .replace(/[\\/]+$/, '');
  return `${root.replace(/[\\/]+$/, '')}${sep}${relative}`;
}

/**
 * Where `path` is after files or folders moved (a rename, a cut and paste,
 * a drag in the tree): the path itself, or one inside, follows its move.
 * Null when no move covers it. The rest of the path takes the new path's
 * own separator, so it matches what the tree lists there.
 */
export function movedPath(path: string, moves: { from: string; to: string }[]): string | null {
  for (const { from, to } of moves) {
    const base = from.replace(/[\\/]+$/, '');
    if (
      path !== from &&
      path !== base &&
      !path.startsWith(`${base}/`) &&
      !path.startsWith(`${base}\\`)
    ) {
      continue;
    }
    const rest = path.slice(base.length).replace(/^[\\/]+/, '');
    if (!rest) return to;
    const sep = to.includes('\\') && !to.includes('/') ? '\\' : '/';
    return `${to.replace(/[\\/]+$/, '')}${sep}${rest.replace(/[\\/]+/g, sep)}`;
  }
  return null;
}
