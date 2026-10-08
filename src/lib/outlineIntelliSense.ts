// outlineIntelliSense.ts
//
// Worker-free editor intelligence from the Rust backend's tree-sitter
// outlines (src-tauri/src/outline/). Covers what a parse tree can do
// without type information:
//
//   - document symbols (breadcrumbs, Go to Symbol, outline panel),
//   - hover showing a declaration's signature,
//   - same-file go-to-definition (the closest declaration with the name
//     under the cursor).
//
// The backend takes the editor's live `content`, so results follow typing
// with no save round-trip; unparseable files (too large, too slow, unknown
// language) return null and the provider quietly does nothing.
//
// What this is NOT: typed intelligence. Cross-file TS/JS definitions,
// hover types, completions, auto-imports, diagnostics, rename and
// references still come from the TypeScript worker (lib/intellisense.ts)
// and language servers (lib/lsp/). tree-sitter sees syntax, not types.
import * as monaco from 'monaco-editor';
import { invoke } from '@tauri-apps/api/core';

export interface OutlineSymbol {
  name: string;
  kind: string;
  start_line: number;
  end_line: number;
  depth: number;
  signature: string;
}

async function symbolsFor(model: monaco.editor.ITextModel): Promise<OutlineSymbol[] | null> {
  try {
    // fsPath of a file:/// URI: the absolute project path the backend's
    // language_for() uses to pick a grammar.
    const path = (model.uri as unknown as { fsPath: string }).fsPath ?? model.uri.path;
    const symbols = await invoke<OutlineSymbol[] | null>('outline_symbols', {
      path,
      content: model.getValue(),
    });
    return symbols;
  } catch (err) {
    console.warn('outline_symbols failed:', err);
    return null;
  }
}

/** tree-sitter node kind → Monaco symbol kind (best effort). */
function toSymbolKind(kind: string): monaco.languages.SymbolKind {
  const K = monaco.languages.SymbolKind;
  const k = kind.toLowerCase();
  if (k.includes('class')) return K.Class;
  if (k.includes('interface')) return K.Interface;
  if (k.includes('struct') || k.includes('union')) return K.Struct;
  if (k.includes('enum')) return K.Enum;
  if (k.includes('function') || k.includes('method') || k.includes('constructor')) return K.Function;
  if (k.includes('module') || k.includes('namespace')) return K.Module;
  if (k.includes('type') && (k.includes('alias') || k.includes('declaration'))) return K.TypeParameter;
  if (k.includes('constant')) return K.Constant;
  if (k.includes('variable') || k.includes('field') || k.includes('property')) return K.Variable;
  if (k.includes('heading') || k.includes('section')) return K.String;
  if (k.includes('key')) return K.Key;
  return K.Function;
}

/** Flat (depth-marked) outline → nested DocumentSymbols, outer before inner. */
function toDocumentSymbols(symbols: OutlineSymbol[]): monaco.languages.DocumentSymbol[] {
  const roots: monaco.languages.DocumentSymbol[] = [];
  // Stack of (symbol, depth).
  const stack: { node: monaco.languages.DocumentSymbol; depth: number }[] = [];
  for (const s of symbols) {
    if (!s.name) continue;
    const range = new monaco.Range(s.start_line, 1, s.end_line, 1);
    const node: monaco.languages.DocumentSymbol = {
      name: s.name,
      detail: s.signature && s.signature !== s.name ? s.signature : '',
      kind: toSymbolKind(s.kind),
      tags: [],
      range,
      selectionRange: range,
      children: [],
    };
    while (stack.length > 0 && s.depth <= stack[stack.length - 1]!.depth) stack.pop();
    if (stack.length === 0) roots.push(node);
    else stack[stack.length - 1]!.node.children!.push(node);
    stack.push({ node, depth: s.depth });
  }
  return roots;
}

/** Smallest declaration containing the position (for hover). */
function enclosing(symbols: OutlineSymbol[], line: number): OutlineSymbol | null {
  let best: OutlineSymbol | null = null;
  for (const s of symbols) {
    if (line < s.start_line || line > s.end_line) continue;
    // Smaller range wins; ties prefer the deeper (more nested) one.
    const size = s.end_line - s.start_line;
    const bestSize = best ? best.end_line - best.start_line : Infinity;
    if (size < bestSize || (size === bestSize && s.depth > (best?.depth ?? -1))) best = s;
  }
  return best;
}

let registered = false;

/**
 * Registers the Rust-backed providers once (all languages, file://
 * scheme). Safe to call from every Editor mount; subsequent calls no-op.
 * Monaco merges providers, so the TypeScript worker and rust-analyzer
 * keep providing typed results alongside these.
 */
export function ensureOutlineIntelliSense(): void {
  if (registered) return;
  registered = true;
  const selector: monaco.languages.LanguageSelector = { scheme: 'file' };

  monaco.languages.registerDocumentSymbolProvider(selector, {
    displayName: 'tree-sitter outline (Rust)',
    provideDocumentSymbols: async (model) => {
      const symbols = await symbolsFor(model);
      if (!symbols || symbols.length === 0) return [];
      return toDocumentSymbols(symbols);
    },
  });

  monaco.languages.registerHoverProvider(selector, {
    provideHover: async (model, position) => {
      const symbols = await symbolsFor(model);
      if (!symbols) return null;
      const hit = enclosing(symbols, position.lineNumber);
      if (!hit) return null;
      return {
        range: new monaco.Range(hit.start_line, 1, hit.end_line, 1),
        contents: [{ value: `\`\`\`\n${hit.signature || hit.name}\n\`\`\`` }],
      };
    },
  });

  monaco.languages.registerDefinitionProvider(selector, {
    provideDefinition: async (model, position) => {
      const word = model.getWordAtPosition(position);
      if (!word?.word) return null;
      const symbols = await symbolsFor(model);
      if (!symbols) return null;
      // Same-file fallback: declarations with this name, preferring one
      // defined above (or at) the cursor so `f` inside `f` jumps to `f`.
      const named = symbols.filter((s) => s.name === word.word);
      if (named.length === 0) return null;
      const atOrAbove = named.filter((s) => s.start_line <= position.lineNumber);
      const target =
        atOrAbove.length > 0
          ? atOrAbove[atOrAbove.length - 1]!
          : named[0]!;
      // Already at the definition: let the typed provider (if any) answer.
      if (target.start_line === position.lineNumber) return null;
      return {
        uri: model.uri,
        range: new monaco.Range(target.start_line, 1, target.start_line, 1),
      };
    },
  });
}
