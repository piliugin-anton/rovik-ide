// monacoThemes.ts
//
// Dark-only editor themes. Monaco ships two dark built-ins (vs-dark,
// hc-black); the rest are community themes vendored from `monaco-themes`
// (MIT, brijeshb42/monaco-themes) into ./monacoThemes/data/ — every file
// there was verified to have `"base": "vs-dark"`, light (`"base": "vs"`)
// themes were left out.
//
// Community themes load lazily: import.meta.glob keeps each JSON in its
// own chunk, and defineTheme runs once per theme id.

import * as monaco from 'monaco-editor';

export interface CommunityThemeDef {
  /** Theme id for defineTheme/setTheme (also the persisted setting). */
  id: string;
  label: string;
  /** File in ./monacoThemes/data/ (exact name, spaces included). */
  file: string;
}

/** Built-in dark themes: no definition needed, Monaco already knows them. */
export const BUILTIN_DARK_THEMES = [
  { id: 'vs-dark', label: 'Dark', hint: 'The default dark theme' },
  { id: 'hc-black', label: 'High contrast (dark)', hint: 'Dark theme for high-contrast mode' },
] as const;

/** Community dark themes (defineTheme'd on first use). Delete rows freely —
 *  the settings page lists exactly what's here, and an unknown saved id
 *  falls back to vs-dark. */
export const COMMUNITY_DARK_THEMES: CommunityThemeDef[] = [
  { id: 'all-hallows-eve', label: 'All Hallows Eve', file: 'All Hallows Eve.json' },
  { id: 'amy', label: 'Amy', file: 'Amy.json' },
  { id: 'birds-of-paradise', label: 'Birds of Paradise', file: 'Birds of Paradise.json' },
  { id: 'blackboard', label: 'Blackboard', file: 'Blackboard.json' },
  { id: 'brilliance-black', label: 'Brilliance Black', file: 'Brilliance Black.json' },
  { id: 'brilliance-dull', label: 'Brilliance Dull', file: 'Brilliance Dull.json' },
  { id: 'clouds-midnight', label: 'Clouds Midnight', file: 'Clouds Midnight.json' },
  { id: 'cobalt', label: 'Cobalt', file: 'Cobalt.json' },
  { id: 'cobalt2', label: 'Cobalt2', file: 'Cobalt2.json' },
  { id: 'dominion-day', label: 'Dominion Day', file: 'Dominion Day.json' },
  { id: 'dracula', label: 'Dracula', file: 'Dracula.json' },
  { id: 'espresso-libre', label: 'Espresso Libre', file: 'Espresso Libre.json' },
  { id: 'github-dark', label: 'GitHub Dark', file: 'GitHub Dark.json' },
  { id: 'merbivore', label: 'Merbivore', file: 'Merbivore.json' },
  { id: 'merbivore-soft', label: 'Merbivore Soft', file: 'Merbivore Soft.json' },
  { id: 'monokai', label: 'Monokai', file: 'Monokai.json' },
  { id: 'monokai-bright', label: 'Monokai Bright', file: 'Monokai Bright.json' },
  { id: 'night-owl', label: 'Night Owl', file: 'Night Owl.json' },
  { id: 'nord', label: 'Nord', file: 'Nord.json' },
  { id: 'oceanic-next', label: 'Oceanic Next', file: 'Oceanic Next.json' },
  { id: 'pastels-on-dark', label: 'Pastels on Dark', file: 'Pastels on Dark.json' },
  { id: 'solarized-dark', label: 'Solarized Dark', file: 'Solarized-dark.json' },
  { id: 'spacecadet', label: 'SpaceCadet', file: 'SpaceCadet.json' },
  { id: 'sunburst', label: 'Sunburst', file: 'Sunburst.json' },
  { id: 'tomorrow-night', label: 'Tomorrow Night', file: 'Tomorrow-Night.json' },
  { id: 'tomorrow-night-blue', label: 'Tomorrow Night Blue', file: 'Tomorrow-Night-Blue.json' },
  {
    id: 'tomorrow-night-bright',
    label: 'Tomorrow Night Bright',
    file: 'Tomorrow-Night-Bright.json',
  },
  {
    id: 'tomorrow-night-eighties',
    label: 'Tomorrow Night Eighties',
    file: 'Tomorrow-Night-Eighties.json',
  },
  { id: 'twilight', label: 'Twilight', file: 'Twilight.json' },
  { id: 'upstream-sunburst', label: 'Upstream Sunburst', file: 'Upstream Sunburst.json' },
  { id: 'vibrant-ink', label: 'Vibrant Ink', file: 'Vibrant Ink.json' },
  { id: 'zenburnesque', label: 'Zenburnesque', file: 'Zenburnesque.json' },
  { id: 'idlefingers', label: 'idleFingers', file: 'idleFingers.json' },
  { id: 'krtheme', label: 'krTheme', file: 'krTheme.json' },
  { id: 'monoindustrial', label: 'monoindustrial', file: 'monoindustrial.json' },
];

/** Every theme id the settings page offers (dark only). */
export const DARK_THEME_IDS: ReadonlySet<string> = new Set([
  ...BUILTIN_DARK_THEMES.map((t) => t.id),
  ...COMMUNITY_DARK_THEMES.map((t) => t.id),
]);

export function isDarkThemeId(id: string): boolean {
  return DARK_THEME_IDS.has(id);
}

// One lazy importer per file (Vite code-splits each JSON). Keys are the
// relative paths as written — glob keys always use ./ prefix + exact name.
const themeModules = import.meta.glob<{ default: monaco.editor.IStandaloneThemeData }>(
  './monacoThemes/data/*.json'
);

const defined = new Set<string>(['vs-dark', 'hc-black']);
const pending = new Map<string, Promise<void>>();

/** defineTheme for a community theme (no-op for built-ins / repeats). */
export function ensureThemeLoaded(id: string): Promise<void> {
  if (defined.has(id)) return Promise.resolve();
  const def = COMMUNITY_DARK_THEMES.find((t) => t.id === id);
  // Unknown id (e.g. removed from the list, or a stale saved value):
  // don't reject — callers fall back to vs-dark.
  if (!def) return Promise.resolve();
  const existing = pending.get(id);
  if (existing) return existing;
  const importer = themeModules[`./monacoThemes/data/${def.file}`];
  const task =
    importer?.().then((mod) => {
      monaco.editor.defineTheme(id, mod.default);
      defined.add(id);
      pending.delete(id);
    }) ?? Promise.resolve();
  pending.set(id, task);
  return task;
}

/** Ensure the theme is defined, then activate it. Never rejects. */
export function applyTheme(id: string): Promise<void> {
  const target = isDarkThemeId(id) ? id : 'vs-dark';
  return ensureThemeLoaded(target)
    .catch((err) => console.error(`Failed to load editor theme "${target}":`, err))
    .then(() => {
      try {
        monaco.editor.setTheme(defined.has(target) ? target : 'vs-dark');
      } catch (err) {
        console.error(`Failed to apply editor theme "${target}":`, err);
      }
    });
}

/** Preload a theme's JSON without activating it (e.g. on settings open). */
export function preloadTheme(id: string): void {
  void ensureThemeLoaded(id).catch((err) =>
    console.error(`Failed to preload editor theme "${id}":`, err)
  );
}
