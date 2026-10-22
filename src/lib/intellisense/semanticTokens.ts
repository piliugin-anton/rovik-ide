// semanticTokens.ts
//
// Semantic colouring for TypeScript/JavaScript. Monaco 0.57 gives none for
// TS/JS: its TypeScript mode registers no semantic token provider, so
// functions, constants and classes are painted by the TextMate grammar
// alone, flat. VS Code and Cursor get their colours from the language
// service; this module brings them over.
//
// The worker already holds the program, the libs and the tsconfig scope,
// and its language service implements `getEncodedSemanticClassifications`
// (projectWorker.ts), so nothing new is parsed here: a document provider
// asks it for the file's classifications, decodes TypeScript's 24-bit
// encoding into Monaco's 5-tuples, and hands them to the editor.
//
// The encoding ('2028') is `((typeIndex + 1) << 8) | modifierBits`, with
// TokenType = class, enum, interface, namespace, typeParameter, type,
// parameter, variable, enumMember, property, function, member and
// TokenModifier bits declaration, static, async, readonly, defaultLibrary,
// local.
//
// The legend's modifier order is load-bearing: Monaco joins a token's
// modifiers in legend order and matches a theme rule against that name by
// dot-prefix (ThemeTrieElement.match), so a module-scope `const` can only
// be `variable.readonly` — the name the themes' rules use — while
// `readonly` comes before `declaration`. Changing the order below silently
// stops those rules matching; semanticTokens.test.ts and
// projectWorker.test.ts pin it.
//
// A theme without a rule for a token paints it its default foreground
// (ThemeTrieElement.match falls back to the root rule), so the feature is
// gated twice: the editor turns colouring on per theme (monacoThemes.ts
// hasSemanticRules, Editor.tsx) — as VS Code's per-theme
// `semanticHighlighting` flag does — and the provider only emits a token
// whose name the applied theme's rules cover (appliedThemeRuleNames), so a
// theme that colours, say, `entity.name.function` but nothing for
// identifiers keeps the look it has today for them.
import * as monaco from 'monaco-editor';
import type { EncodedClassifications } from 'monaco-editor/language/typescript/ts.worker.js';
import { appliedThemeRuleNames } from '../monacoThemes';

/** TypeScript's token types, in its index order. */
export const SEMANTIC_TOKEN_TYPES = [
  'class',
  'enum',
  'interface',
  'namespace',
  'typeParameter',
  'type',
  'parameter',
  'variable',
  'enumMember',
  'property',
  'function',
  'member',
] as const;

/** TypeScript's modifier bits, in its order (the low six bits of the encoding). */
const TS_MODIFIER_ORDER = ['declaration', 'static', 'async', 'readonly', 'defaultLibrary', 'local'];

/**
 * Our legend order: `readonly` first, so `variable.readonly` and
 * `property.readonly` are names a theme rule can match.
 */
export const SEMANTIC_TOKEN_MODIFIERS = [
  'readonly',
  'declaration',
  'defaultLibrary',
  'static',
  'async',
  'local',
];

/** The legend, as registered with the providers (Monaco's own key names). */
export const SEMANTIC_LEGEND: monaco.languages.SemanticTokensLegend = {
  tokenTypes: [...SEMANTIC_TOKEN_TYPES],
  tokenModifiers: [...SEMANTIC_TOKEN_MODIFIERS],
};

/**
 * How TypeScript's modifier bits move to the legend's, as
 * `[tsBit, legendBit]`: only the order differs, each modifier keeps one bit.
 */
const MODIFIER_BITS: readonly (readonly [number, number])[] = SEMANTIC_TOKEN_MODIFIERS.map(
  (name) => [1 << TS_MODIFIER_ORDER.indexOf(name), 1 << SEMANTIC_TOKEN_MODIFIERS.indexOf(name)]
);

/** A raw TypeScript modifier mask, moved into the legend's bit positions. */
function legendBits(tsBits: number): number {
  let bits = 0;
  for (const [tsBit, legendBit] of MODIFIER_BITS) if (tsBits & tsBit) bits |= legendBit;
  return bits;
}

/**
 * The name a theme rule is matched against for a raw TypeScript
 * classification: `type` plus our modifiers in legend order. Pins the
 * contract with Monaco's `TokenTheme._match`; a module-scope const is
 * `variable.readonly.declaration`.
 */
export function semanticTokenName(encoded: number): string {
  const type = SEMANTIC_TOKEN_TYPES[(encoded >> 8) - 1];
  if (type === undefined) return '';
  const bits = legendBits(encoded & 0xff);
  const modifiers = SEMANTIC_TOKEN_MODIFIERS.filter((_, i) => bits & (1 << i));
  return modifiers.length ? `${type}.${modifiers.join('.')}` : type;
}

/** Where each line starts in `text` (a CRLF, a lone CR or a lone LF ends one). */
function lineStarts(text: string): number[] {
  const starts = [0];
  for (let i = 0; i < text.length; i++) {
    const ch = text.charCodeAt(i);
    if (ch === 13) {
      if (text.charCodeAt(i + 1) === 10) i++;
      starts.push(i + 1);
    } else if (ch === 10) {
      starts.push(i + 1);
    }
  }
  return starts;
}

/** The line holding `index`, from `starts` (ascending). */
function lineOf(starts: readonly number[], index: number): number {
  let low = 0;
  let high = starts.length - 1;
  while (low < high) {
    const middle = (low + high + 1) >> 1;
    if (starts[middle] <= index) low = middle;
    else high = middle - 1;
  }
  return low;
}

/**
 * TypeScript's encoded classifications as Monaco's 5-tuples:
 * `deltaLine, deltaStart, length, typeIndex, modifierBits`, the start
 * measured from the previous token when on its line, from the line's start
 * otherwise. Empty spans and types outside the legend are dropped, and so
 * is a token `include` rejects — the provider passes the names the editor's
 * theme colours, so a token the theme has no rule for keeps its TextMate
 * colour rather than taking the theme's default foreground.
 */
export function encodeSemanticTokens(
  spans: readonly number[],
  text: string,
  include?: (name: string) => boolean
): Uint32Array {
  const starts = lineStarts(text);
  const data: number[] = [];
  let previousLine = 0;
  let previousStart = 0;
  for (let i = 0; i + 2 < spans.length; i += 3) {
    const [start, length, encoded] = [spans[i], spans[i + 1], spans[i + 2]];
    const typeIndex = (encoded >> 8) - 1;
    if (length <= 0 || typeIndex < 0 || typeIndex >= SEMANTIC_TOKEN_TYPES.length) continue;
    if (include && !include(semanticTokenName(encoded))) continue;
    const line = lineOf(starts, start);
    const deltaLine = line - previousLine;
    const deltaStart = deltaLine === 0 ? start - previousStart : start - starts[line];
    data.push(deltaLine, deltaStart, length, typeIndex, legendBits(encoded & 0xff));
    previousLine = line;
    previousStart = start;
  }
  return Uint32Array.from(data);
}

/**
 * Whether a theme's rules colour `name`: it names the token, or a
 * dot-prefix of it — `ThemeTrieElement.match` walks the trie that way, so a
 * `variable` rule also colours `variable.readonly`. Anything else would be
 * matched against the theme's root rule, i.e. painted its default
 * foreground, which is what the caller means to avoid.
 */
function coversName(names: ReadonlySet<string>, name: string): boolean {
  let current = name;
  for (;;) {
    if (names.has(current)) return true;
    const dot = current.lastIndexOf('.');
    if (dot < 0) return false;
    current = current.slice(0, dot);
  }
}

/**
 * The one answer Monaco keeps: a model's 5-tuples, and the model version
 * and program generation they were made from. `generation` is what makes a
 * new program (libs loaded, compiler options changed) re-classify files
 * whose text didn't change.
 */
interface CachedTokens {
  uri: string;
  version: number;
  generation: number;
  resultId: string;
  data: Uint32Array;
}

let generation = 0;
let cache: CachedTokens | undefined;
let failureLogged = false;

/** Whoever listens for "classify again" (the providers' onDidChange). */
type Listener = () => void;
const listeners = new Set<Listener>();

/** Monaco's `IEvent<void>`: `refreshSemanticTokens` tells the editor. */
const onDidChange = (listener: Listener): { dispose(): void } => {
  listeners.add(listener);
  return { dispose: () => listeners.delete(listener) };
};

/**
 * Classify again what's been classified: the program changed (the libs
 * finished loading, the compiler options did), so files whose text didn't
 * must be coloured from the new one.
 */
export function refreshSemanticTokens(): void {
  generation++;
  cache = undefined;
  for (const listener of listeners) listener();
}

/** What the worker's `getEncodedSemanticClassifications` answers (projectWorker.ts). */
interface Classifier {
  getEncodedSemanticClassifications(
    fileName: string,
    span: { start: number; length: number },
    format?: string
  ): Promise<EncodedClassifications>;
}

/** The worker's proxy for a model, fetched per request (a restart drops old ones). */
async function classifierFor(model: monaco.editor.ITextModel): Promise<Classifier> {
  const getter =
    model.getLanguageId() === 'typescript'
      ? await monaco.typescript.getTypeScriptWorker()
      : await monaco.typescript.getJavaScriptWorker();
  const proxy = await getter(model.uri);
  // The project worker's own method (projectWorker.ts): not in Monaco's type.
  return proxy as unknown as Classifier;
}

const provider: monaco.languages.DocumentSemanticTokensProvider = {
  onDidChange,
  getLegend: () => SEMANTIC_LEGEND,
  async provideDocumentSemanticTokens(model, _lastResultId, token) {
    // Chat code blocks are `inmemory://` models (no path): TextMate only.
    // Decided here rather than through the global `semanticHighlighting`
    // option, which is shared app-wide.
    if (model.uri.scheme !== 'file') return null;
    // The theme's own rules (its TextMate names and the semantic ones
    // derived from them, monacoThemes.ts): a token no rule of the theme's
    // covers would be painted the theme's default foreground, so it's left
    // to the grammar instead. A built-in theme has no names to read.
    const names = appliedThemeRuleNames();
    if (!names) return null;
    const uri = model.uri.toString();
    // The text the spans are numbered in, read before the request: the
    // model can be edited while the worker answers.
    const version = model.getVersionId();
    const text = model.getValue();
    const cached = cache;
    if (cached?.uri === uri && cached.version === version && cached.generation === generation) {
      return { resultId: cached.resultId, data: cached.data };
    }
    try {
      const classifier = await classifierFor(model);
      const { spans } = await classifier.getEncodedSemanticClassifications(uri, {
        start: 0,
        length: text.length,
      });
      if (token.isCancellationRequested) return null;
      // Edited meanwhile: the spans belong to the old text. Monaco asks
      // again for the new version; answering this one would put the
      // colours on the wrong characters, and caching it under the new
      // version would keep them there until the next edit.
      if (model.getVersionId() !== version) return null;
      const entry: CachedTokens = {
        uri,
        version,
        generation,
        resultId: `${generation}:${version}`,
        data: encodeSemanticTokens(spans, text, (name) => coversName(names, name)),
      };
      cache = entry;
      return { resultId: entry.resultId, data: entry.data };
    } catch (err) {
      if (!failureLogged) {
        failureLogged = true;
        console.error('Semantic tokens failed:', err);
      }
      return null;
    }
  },
  releaseDocumentSemanticTokens: () => {
    // One entry: nothing to release.
  },
};

let registered = false;

/** Register the provider for TS/JS once (index.ts, after the defaults). */
export function registerSemanticTokens(): void {
  if (registered) return;
  registered = true;
  try {
    for (const language of ['typescript', 'javascript']) {
      monaco.languages.registerDocumentSemanticTokensProvider(language, provider);
    }
  } catch (err) {
    // No language registry (a Node test, a Monaco that failed to load):
    // the editor keeps its TextMate colours.
    console.warn('Semantic tokens: the provider was not registered:', err);
  }
}
