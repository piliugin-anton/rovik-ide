/** "1 file", "3 files", "2 entries" (`irregular` for a plural that isn't `word` + s). */
export function plural(n: number, word: string, irregular = `${word}s`): string {
  return `${n} ${n === 1 ? word : irregular}`;
}

export function isBlank(text: string | undefined | null): boolean {
  return !text || text.replace(/[\s\u200B-\u200D\uFEFF]/g, '').length === 0;
}
