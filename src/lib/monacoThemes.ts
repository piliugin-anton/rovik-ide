// monacoThemes.ts
//
// Dark-only editor themes, each one that colours code completely: every
// semantic token TypeScript reports (class, interface, enum, type,
// typeParameter, namespace, function, method, variable, constant,
// parameter, property, enum member) gets a colour of the theme's own once
// ./monacoThemes/rules.ts has derived its semantic rules. Themes that left
// some of those to the flat default foreground were removed, and so were
// Monaco's built-ins (vs-dark, hc-black): their rule sets can't be read or
// extended, so they can't have semantic colouring at all.
//
// The files are vendored from `monaco-themes` (MIT,
// brijeshb42/monaco-themes) into ./monacoThemes/data/, all with
// `"base": "vs-dark"`; Dark-Modern.json is a port of VS Code's default
// dark theme (dark_plus) and the default here (DEFAULT_THEME_ID).
//
// Themes load lazily: import.meta.glob keeps each JSON in its own chunk,
// and defineTheme runs once per theme id, with the semantic token rules
// derived from the theme's own scopes. Monaco's vs-dark is only the
// fallback while a theme loads or if one fails to.

import * as monaco from 'monaco-editor';
import { withSemanticRules } from './monacoThemes/rules';

export interface CommunityThemeDef {
  /** Theme id for defineTheme/setTheme (also the persisted setting). */
  id: string;
  label: string;
  /** File in ./monacoThemes/data/ (exact name, spaces included). */
  file: string;
}

/** The theme of an install with none saved, and what an unknown saved id becomes. */
export const DEFAULT_THEME_ID = 'dark-modern';

/** The themes offered (defineTheme'd on first use). Only add a theme
 *  that colours every semantic token (see the header; monacoThemes.test.ts
 *  checks it) — an unknown saved id falls back to DEFAULT_THEME_ID. */
export const COMMUNITY_DARK_THEMES: CommunityThemeDef[] = [
  { id: 'dark-modern', label: 'Dark Modern', file: 'Dark-Modern.json' },
  { id: 'cobalt', label: 'Cobalt', file: 'Cobalt.json' },
  { id: 'cobalt2', label: 'Cobalt2', file: 'Cobalt2.json' },
  { id: 'github-dark', label: 'GitHub Dark', file: 'GitHub Dark.json' },
  { id: 'spacecadet', label: 'SpaceCadet', file: 'SpaceCadet.json' },
  { id: 'twilight', label: 'Twilight', file: 'Twilight.json' },
  { id: 'krtheme', label: 'krTheme', file: 'krTheme.json' },
  { id: 'monoindustrial', label: 'monoindustrial', file: 'monoindustrial.json' },
];

/** Every theme id the settings page offers (dark only). */
export const DARK_THEME_IDS: ReadonlySet<string> = new Set(COMMUNITY_DARK_THEMES.map((t) => t.id));

export function isDarkThemeId(id: string): boolean {
  return DARK_THEME_IDS.has(id);
}

/**
 * Whether a theme carries rules for semantic tokens, i.e. whether the file
 * editor may turn semantic colouring on for it.
 *
 * Monaco matches a semantic token against the theme's rules like a scope
 * name, and `ThemeTrieElement.match` returns the deepest matching node's
 * rule — for a token nothing matches, the root's, which carries the
 * theme's default foreground (`ColorMap.getId` never returns 0, and an
 * inheriting theme takes `vs-dark`'s `#D4D4D4`). So a semantic token
 * without a rule is painted that flat foreground instead of the colour the
 * theme's TextMate rules give it: on `vs-dark` that would strip the teal
 * from every class name. The built-ins are Monaco's own and their rule
 * sets can't be read or extended here, so they keep semantic colouring
 * off; every shipped theme gets the rules derived by withSemanticRules
 * (monacoThemes/rules.ts) as it's defined.
 *
 * The provider goes one step further and paints only the tokens whose name
 * one of those rules covers (appliedThemeRuleNames, semanticTokens.ts), so
 * a theme that colours functions but not identifiers keeps today's look
 * for the identifiers.
 */
export function hasSemanticRules(id: string): boolean {
  return COMMUNITY_DARK_THEMES.some((theme) => theme.id === id);
}

// One lazy importer per file (Vite code-splits each JSON). Keys are the
// relative paths as written — glob keys always use ./ prefix + exact name.
const themeModules = import.meta.glob<{ default: monaco.editor.IStandaloneThemeData }>(
  './monacoThemes/data/*.json'
);

const defined = new Set<string>(['vs-dark', 'hc-black']);
const pending = new Map<string, Promise<void>>();

/**
 * The token names each defined theme colours: its own rules' and the ones
 * derived for it. A name this set doesn't cover is one the theme has no
 * opinion on, and the semantic layer leaves it to the TextMate grammar
 * (semanticTokens.ts) — otherwise the trie's root rule would paint it the
 * theme's default foreground.
 */
const ruleNames = new Map<string, ReadonlySet<string>>();

/**
 * The names of the theme applied last (`applyTheme`), which is what the
 * semantic tokens provider filters against; undefined for a built-in, or
 * before any theme of ours has been applied.
 */
let appliedNames: ReadonlySet<string> | undefined;

/** What `applyTheme` set: the token names the editor's theme gives a colour. */
export function appliedThemeRuleNames(): ReadonlySet<string> | undefined {
  return appliedNames;
}

/** defineTheme for a community theme (no-op for built-ins / repeats). */
export function ensureThemeLoaded(id: string): Promise<void> {
  if (defined.has(id)) return Promise.resolve();
  const def = COMMUNITY_DARK_THEMES.find((t) => t.id === id);
  // Unknown id (e.g. removed from the list, or a stale saved value):
  // don't reject — applyTheme falls back to vs-dark.
  if (!def) return Promise.resolve();
  const existing = pending.get(id);
  if (existing) return existing;
  const importer = themeModules[`./monacoThemes/data/${def.file}`];
  const task =
    importer?.().then((mod) => {
      // The theme's own rules, then the semantic names they imply.
      const theme = withSemanticRules(mod.default);
      monaco.editor.defineTheme(id, theme);
      ruleNames.set(id, new Set(tokenNames(theme)));
      defined.add(id);
      pending.delete(id);
    }) ?? Promise.resolve();
  pending.set(id, task);
  return task;
}

/** Every token name a theme's rules give (the root's empty one aside). */
function tokenNames(theme: monaco.editor.IStandaloneThemeData): string[] {
  return (theme.rules ?? []).map((rule) => rule.token).filter((name): name is string => !!name);
}

/** Ensure the theme is defined, then activate it. Never rejects. */
export function applyTheme(id: string): Promise<void> {
  const target = isDarkThemeId(id) ? id : DEFAULT_THEME_ID;
  return ensureThemeLoaded(target)
    .catch((err) => console.error(`Failed to load editor theme "${target}":`, err))
    .then(() => {
      const actual = defined.has(target) ? target : 'vs-dark';
      try {
        monaco.editor.setTheme(actual);
      } catch (err) {
        console.error(`Failed to apply editor theme "${target}":`, err);
      }
      appliedNames = ruleNames.get(actual);
    });
}

/** Preload a theme's JSON without activating it (e.g. on settings open). */
export function preloadTheme(id: string): void {
  void ensureThemeLoaded(id).catch((err) =>
    console.error(`Failed to preload editor theme "${id}":`, err)
  );
}
