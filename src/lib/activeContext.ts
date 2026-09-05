// src/lib/activeContext.ts — new file
export interface ActiveFileContext {
  path: string;
  language: string;
  line: number;
  column: number;
  enclosingSymbol: string | null;
  selectionStartLine: number | null;
  selectionEndLine: number | null;
}

const SYMBOL_PATTERNS: Record<string, RegExp[]> = {
  typescript: [/\b(?:export\s+)?(?:async\s+)?function\s+(\w+)/, /\bclass\s+(\w+)/],
  javascript: [/\b(?:export\s+)?(?:async\s+)?function\s+(\w+)/, /\bclass\s+(\w+)/],
  rust: [/\bfn\s+(\w+)/, /\b(?:struct|impl|trait|enum)\s+(\w+)/],
  python: [/\bdef\s+(\w+)/, /\bclass\s+(\w+)/],
  go: [/\bfunc\s+(?:\([^)]*\)\s*)?(\w+)/, /\btype\s+(\w+)\s+struct/],
};
const FALLBACK_PATTERNS = Object.values(SYMBOL_PATTERNS).flat();

/**
 * Best-effort "what am I inside" lookup: scans upward from the cursor line
 * for the nearest matching declaration. Not real symbol resolution (no
 * brace/indent tracking) — just a cheap breadcrumb. Runs on content already
 * held locally; only its short result (a name) ever leaves the app.
 */
export function findEnclosingSymbol(
  content: string,
  line: number,
  language: string
): string | null {
  const patterns = SYMBOL_PATTERNS[language] ?? FALLBACK_PATTERNS;
  const lines = content.split('\n');
  for (let i = Math.min(line, lines.length) - 1; i >= 0; i--) {
    for (const re of patterns) {
      const match = lines[i].match(re);
      if (match?.[1]) return match[1];
    }
  }
  return null;
}

/** Positions and names only — never file content. The model can call read_file for that. */
export function buildContextHeader(ctx: ActiveFileContext | null): string | null {
  if (!ctx) return null;
  const parts = [
    `file ${ctx.path} (${ctx.language})`,
    `cursor at line ${ctx.line}, col ${ctx.column}`,
  ];
  if (ctx.enclosingSymbol) parts.push(`inside ${ctx.enclosingSymbol}()`);
  if (
    ctx.selectionStartLine != null &&
    ctx.selectionEndLine != null &&
    ctx.selectionStartLine !== ctx.selectionEndLine
  ) {
    parts.push(`selection: lines ${ctx.selectionStartLine}-${ctx.selectionEndLine}`);
  }
  return `[Active editor context — ${parts.join('; ')}. Call read_file if you need the actual code.]`;
}
