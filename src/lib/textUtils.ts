export function isBlank(text: string | undefined | null): boolean {
  return !text || text.replace(/[\s\u200B-\u200D\uFEFF]/g, '').length === 0;
}
