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
