// editorSettings.ts
//
// The editor's appearance and formatting options (theme, font size,
// indentation, word wrap, minimap, format on save). Kept in localStorage — they are
// per-user, not per-project — and exposed as reactive signals so the
// Monaco instance in components/Editor.tsx follows them live.

import { createSignal } from 'solid-js';
import { BUILTIN_DARK_THEMES, COMMUNITY_DARK_THEMES, isDarkThemeId } from './monacoThemes';

export type EditorThemeId = string;

export interface EditorThemeOption {
  id: EditorThemeId;
  label: string;
  hint: string;
}

/** Dark themes only: Monaco's built-in dark themes plus the vendored
 *  community dark themes (see lib/monacoThemes.ts). To drop a theme,
 *  delete its row there — an unknown saved id falls back to vs-dark. */
export const EDITOR_THEMES: EditorThemeOption[] = [
  ...BUILTIN_DARK_THEMES.map((t) => ({ ...t })),
  ...COMMUNITY_DARK_THEMES.map((t) => ({
    id: t.id,
    label: t.label,
    hint: 'Community theme',
  })),
];

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
  theme: 'vs-dark',
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

function load(): EditorSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULTS;
    const parsed = JSON.parse(raw) as Partial<EditorSettings>;
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

const [settings, setSettings] = createSignal<EditorSettings>(load());

function persist(next: EditorSettings) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch (err) {
    console.error('Failed to save editor settings:', err);
  }
}

/** The whole settings object (reactive). */
export function editorSettings(): EditorSettings {
  return settings();
}

/** The editor theme id (reactive): pass to monaco.editor.setTheme. */
export function editorTheme(): EditorThemeId {
  return settings().theme;
}

export function updateEditorSettings(patch: Partial<EditorSettings>): void {
  const next = { ...settings(), ...patch };
  setSettings(next);
  persist(next);
}
