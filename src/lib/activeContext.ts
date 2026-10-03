// src/lib/activeContext.ts

/** A non-empty editor selection, 1-based; the end column is exclusive (Monaco's convention). */
export interface EditorSelection {
  startLine: number;
  startColumn: number;
  endLine: number;
  endColumn: number;
}

/**
 * The selection as whole lines, 1-based and inclusive. A selection that ends
 * at column 1 (lines picked by dragging to the start of the next one) doesn't
 * include that last line.
 */
export function selectedLines(sel: EditorSelection): { start: number; end: number } {
  const end = sel.endColumn === 1 && sel.endLine > sel.startLine ? sel.endLine - 1 : sel.endLine;
  return { start: sel.startLine, end };
}

/** What the editor reports on every cursor/selection change. */
export interface CursorInfo {
  /** The file it belongs to, so a report from a previous tab is never applied to another. */
  path: string;
  line: number;
  column: number;
  selection: EditorSelection | null;
}

export interface ActiveFileContext {
  path: string;
  language: string;
  line: number;
  column: number;
  enclosingSymbol: string | null;
  selection: EditorSelection | null;
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
  // The selection is deliberately not here. A selection that is just lying
  // around in the editor says nothing about what the message is about, and
  // models took it as an instruction to edit there. To point at code, the
  // user mentions it: "@" offers the selection as "file.ts:12-14".
  return `[Active editor context — ${parts.join('; ')}. Call read_file if you need the actual code.]`;
}
