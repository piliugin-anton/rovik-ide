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
