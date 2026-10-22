// editorSettings.ts
//
// The editor's appearance and formatting options (theme, font size,
// indentation, word wrap, minimap, format on save). Per user, not per
// project: shared by the windows (lib/sharedState.ts), so one window's
// change doesn't undo another's, and a window follows the others' as it
// comes to the front. Exposed as reactive signals so the Monaco instance
// in components/Editor.tsx follows them live.

import { createSignal } from 'solid-js';
import { COMMUNITY_DARK_THEMES, DEFAULT_THEME_ID, isDarkThemeId } from './monacoThemes';
import { sharedMap } from './sharedState';

export type EditorThemeId = string;

export interface EditorThemeOption {
  id: EditorThemeId;
  label: string;
  hint: string;
}

/** The themes offered (see lib/monacoThemes.ts): only ones that colour
 *  every semantic token. To drop a theme, delete its row there — an
 *  unknown saved id falls back to Dark Modern. */
export const EDITOR_THEMES: EditorThemeOption[] = COMMUNITY_DARK_THEMES.map((t) => ({
  id: t.id,
  label: t.label,
  hint: t.id === DEFAULT_THEME_ID ? 'VS Code’s default dark theme' : 'Community theme',
}));

export interface EditorSettings {
  theme: EditorThemeId;
  fontSize: number;
  /** Default indentation: what a file gets when neither its .editorconfig
   *  nor (with detectIndentation) its own content says otherwise. */
  tabSize: number;
  insertSpaces: boolean;
  /** Take each file's indentation from its content. Default true. */
  detectIndentation: boolean;
  /** Run the language's formatter on Ctrl+S, when it has one. Default false. */
  formatOnSave: boolean;
  /** Format with the project's command-line formatter (Prettier, rustfmt,
   *  Ruff, …) where there is one. Default true. */
  projectFormatters: boolean;
  wordWrap: 'on' | 'off';
  minimap: boolean;
  renderWhitespace: 'none' | 'boundary' | 'all';
  /** Automatically open a file the agent edited (approval reviews, writes, unreviewed edits). Default false. */
  openFilesAfterAgentEdits: boolean;
}

const STORAGE_KEY = 'rovik.editorSettings.v1';

const DEFAULTS: EditorSettings = {
  // VS Code's default dark theme. A saved id that's no longer offered (the
  // built-ins, the themes that didn't colour every token) becomes this one.
  theme: DEFAULT_THEME_ID,
  fontSize: 14,
  tabSize: 2,
  insertSpaces: true,
  detectIndentation: true,
  formatOnSave: false,
  projectFormatters: true,
  wordWrap: 'off',
  minimap: false,
  renderWhitespace: 'none',
  openFilesAfterAgentEdits: false,
};

/** The settings saved, each one checked: what isn't valid is the default. */
function load(parsed: Partial<EditorSettings>): EditorSettings {
  try {
    return {
      theme:
        typeof parsed.theme === 'string' && isDarkThemeId(parsed.theme)
          ? parsed.theme
          : DEFAULTS.theme,
      fontSize:
        typeof parsed.fontSize === 'number'
          ? Math.min(28, Math.max(8, Math.round(parsed.fontSize)))
          : DEFAULTS.fontSize,
      tabSize:
        typeof parsed.tabSize === 'number'
          ? Math.min(8, Math.max(1, Math.round(parsed.tabSize)))
          : DEFAULTS.tabSize,
      insertSpaces:
        typeof parsed.insertSpaces === 'boolean' ? parsed.insertSpaces : DEFAULTS.insertSpaces,
      detectIndentation:
        typeof parsed.detectIndentation === 'boolean'
          ? parsed.detectIndentation
          : DEFAULTS.detectIndentation,
      formatOnSave:
        typeof parsed.formatOnSave === 'boolean' ? parsed.formatOnSave : DEFAULTS.formatOnSave,
      projectFormatters:
        typeof parsed.projectFormatters === 'boolean'
          ? parsed.projectFormatters
          : DEFAULTS.projectFormatters,
      wordWrap: parsed.wordWrap === 'on' ? 'on' : 'off',
      minimap: typeof parsed.minimap === 'boolean' ? parsed.minimap : DEFAULTS.minimap,
      renderWhitespace:
        parsed.renderWhitespace === 'all' || parsed.renderWhitespace === 'boundary'
          ? parsed.renderWhitespace
          : 'none',
      openFilesAfterAgentEdits:
        typeof parsed.openFilesAfterAgentEdits === 'boolean'
          ? parsed.openFilesAfterAgentEdits
          : DEFAULTS.openFilesAfterAgentEdits,
    };
  } catch {
    return DEFAULTS;
  }
}

const saved = sharedMap('editor-settings', { key: STORAGE_KEY });
const [settings, setSettings] = createSignal<EditorSettings>(load(saved.all()));
// Another window's change.
saved.subscribe((all) => setSettings(load(all)));

/** The whole settings object (reactive). */
export function editorSettings(): EditorSettings {
  return settings();
}

/** The editor theme id (reactive): pass to monaco.editor.setTheme. */
export function editorTheme(): EditorThemeId {
  return settings().theme;
}

export function updateEditorSettings(patch: Partial<EditorSettings>): void {
  setSettings({ ...settings(), ...patch });
  // Only what changed: the others may be another window's newer ones.
  void saved.set(patch);
}

/** One line for the Settings menu. */
export function editorSummary(s: EditorSettings): string {
  const theme = EDITOR_THEMES.find((t) => t.id === s.theme)?.label ?? s.theme;
  const indent = s.insertSpaces ? `spaces (${s.tabSize})` : 'tabs';
  return `${theme} · ${s.fontSize} px · ${indent}${s.formatOnSave ? ' · format on save' : ''}`;
}
