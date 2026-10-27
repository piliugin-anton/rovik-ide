/**
 * Whether this computer's file systems ignore the case of a path: Windows
 * and macOS do (a folder has one spelling, whatever case is typed), Linux
 * doesn't. Read from the webview's platform on each call, so a test can
 * stand in for another platform.
 */
export function pathsIgnoreCase(): boolean {
  const platform = typeof navigator === 'undefined' ? '' : navigator.platform;
  return /^(?:win|mac)/i.test(platform);
}

/**
 * A path as paths are compared here: one kind of separator, no trailing
 * one, and lower case where the file system ignores case. A file system
 * root keeps the empty form (`/` and `D:\` become `/` and `D:`), which
 * `isAtOrUnder` reads as "everything below it".
 */
function comparablePath(path: string): string {
  const slashed = path.replace(/\\/g, '/').replace(/\/+$/, '');
  return pathsIgnoreCase() ? slashed.toLowerCase() : slashed;
}

/**
 * Whether `path` is `base` or inside it: `/p/src/a.ts` is under `/p`, its
 * sibling `/p/src2/a.ts` isn't, and everything is under `/` or `D:\`.
 */
export function isAtOrUnder(path: string, base: string): boolean {
  const under = comparablePath(path);
  const folder = comparablePath(base);
  return under === folder || under.startsWith(folder.endsWith('/') ? folder : `${folder}/`);
}

/** Whether two paths name the same file, as this file system compares them. */
export function samePath(a: string, b: string): boolean {
  return comparablePath(a) === comparablePath(b);
}

/**
 * The folder containing `path`, keeping its separator as written, or null
 * for a file system root (`/`, `D:\`, a share's `\\server\share`) and for a
 * path with no folder at all.
 */
export function parentOf(path: string): string | null {
  const trimmed = path.replace(/[\\/]+$/, '');
  // A root is its own top: '/', 'D:' (from 'D:\'), '\\server\share'.
  if (
    trimmed === '' ||
    /^[A-Za-z]:$/.test(trimmed) ||
    /^[\\/]{2}[^\\/]+[\\/][^\\/]+$/.test(trimmed)
  ) {
    return null;
  }
  const at = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  if (at < 0) return null;
  if (at === 0) return trimmed.slice(0, 1);
  const parent = trimmed.slice(0, at);
  // 'D:\x' → 'D:\': a drive root keeps the separator it was given.
  return /^[A-Za-z]:$/.test(parent) ? parent + trimmed[at] : parent;
}

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

/** Whether `path` is absolute (`/x`, `\x`, `C:\x`) rather than relative to a folder. */
export function isAbsolutePath(path: string): boolean {
  return /^([a-zA-Z]:[\\/]|[\\/])/.test(path);
}

/**
 * A path the model gave (relative to the project, or already absolute)
 * as the absolute path the editor tabs and file tree use.
 */
export function toAbsolutePath(path: string, root: string): string {
  if (isAbsolutePath(path)) return path;
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
