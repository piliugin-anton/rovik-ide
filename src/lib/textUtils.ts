/** "1 file", "3 files", "2 entries" (`irregular` for a plural that isn't `word` + s). */
export function plural(n: number, word: string, irregular = `${word}s`): string {
  return `${n} ${n === 1 ? word : irregular}`;
}

/** A size for people: "300 bytes", "12 KB", "1.5 MB", "20 MB", "3.4 GB". */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return plural(bytes, 'byte');
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  let size = bytes / (1024 * 1024);
  let unit = 'MB';
  for (const larger of ['GB', 'TB']) {
    if (size < 1024) break;
    size /= 1024;
    unit = larger;
  }
  return `${size < 10 ? size.toFixed(1).replace(/\.0$/, '') : Math.round(size)} ${unit}`;
}

export function isBlank(text: string | undefined | null): boolean {
  return !text || text.replace(/[\s\u200B-\u200D\uFEFF]/g, '').length === 0;
}
