/** "1 file", "3 files", "2 entries" (`irregular` for a plural that isn't `word` + s). */
export function plural(n: number, word: string, irregular = `${word}s`): string {
  return `${n} ${n === 1 ? word : irregular}`;
}

/** A size for people: "300 bytes", "12 KB", "1.5 MB", "20 MB". */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return plural(bytes, 'byte');
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  const mb = bytes / (1024 * 1024);
  return `${mb < 10 ? mb.toFixed(1).replace(/\.0$/, '') : Math.round(mb)} MB`;
}

export function isBlank(text: string | undefined | null): boolean {
  return !text || text.replace(/[\s\u200B-\u200D\uFEFF]/g, '').length === 0;
}
