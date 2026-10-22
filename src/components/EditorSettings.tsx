// EditorSettings.tsx
//
// Settings → Editor: theme, indentation, display and formatting. Changes
// apply live (components/Editor.tsx follows the same store) and persist to
// localStorage via lib/editorSettings.ts. What the editor does with the
// agent's edits is on Settings → Agent runs.

import { For } from 'solid-js';
import {
  EDITOR_THEMES,
  editorSettings,
  updateEditorSettings,
  type EditorThemeId,
} from '../lib/editorSettings';
import { COMMUNITY_DARK_THEMES, preloadTheme } from '../lib/monacoThemes';
import { Segmented } from './SettingsControls';

export default function EditorSettings() {
  const s = () => editorSettings();
  const themeLabel = () => EDITOR_THEMES.find((t) => t.id === s().theme)?.label ?? s().theme;
  const themeHint = () => EDITOR_THEMES.find((t) => t.id === s().theme)?.hint ?? '';

  return (
    <div class="permissions-settings editor-settings">
      <p class="settings-hint">
        How code looks and is formatted in the editor, on this computer. Changes apply right away to
        every open file. The theme colors the Markdown preview too; a project’s .editorconfig takes
        precedence over the indentation set here.
      </p>

      <div class="settings-section">
        <div class="settings-section-title">Theme</div>
        <div class="setting-row">
          <span class="setting-row-label">Color theme</span>
          <div class="setting-row-control">
            <select
              class="editor-settings-select"
              value={s().theme}
              aria-label="Editor color theme"
              onFocus={() => COMMUNITY_DARK_THEMES.forEach((t) => preloadTheme(t.id))}
              onChange={(e) =>
                updateEditorSettings({ theme: e.currentTarget.value as EditorThemeId })
              }
            >
              <For each={COMMUNITY_DARK_THEMES}>
                {(t) => <option value={t.id}>{t.label}</option>}
              </For>
            </select>
            <p class="setting-explain">
              {themeLabel()} — {themeHint()}
            </p>
          </div>
        </div>
        <div class="setting-row">
          <span class="setting-row-label">Font size</span>
          <div class="setting-row-control">
            <input
              class="editor-settings-number"
              type="number"
              min={8}
              max={28}
              step={1}
              value={s().fontSize}
              aria-label="Editor font size"
              onChange={(e) => {
                const n = Math.min(
                  28,
                  Math.max(8, Math.round(Number(e.currentTarget.value) || 14))
                );
                updateEditorSettings({ fontSize: n });
              }}
            />
          </div>
        </div>
      </div>

      <div class="settings-section">
        <div class="settings-section-title">Indentation</div>
        <div class="setting-row">
          <span class="setting-row-label">Indent</span>
          <div class="setting-row-control">
            <Segmented
              label="Indentation"
              value={s().insertSpaces ? 'spaces' : 'tabs'}
              options={[
                { value: 'spaces', label: 'Spaces' },
                { value: 'tabs', label: 'Tabs' },
              ]}
              onChange={(v) => updateEditorSettings({ insertSpaces: v === 'spaces' })}
            />
          </div>
        </div>
        <div class="setting-row">
          <span class="setting-row-label">Tab size</span>
          <div class="setting-row-control">
            <input
              class="editor-settings-number"
              type="number"
              min={1}
              max={8}
              step={1}
              value={s().tabSize}
              aria-label="Tab size"
              onChange={(e) => {
                const n = Math.min(8, Math.max(1, Math.round(Number(e.currentTarget.value) || 2)));
                updateEditorSettings({ tabSize: n });
              }}
            />
          </div>
        </div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={s().detectIndentation}
            onChange={(e) => updateEditorSettings({ detectIndentation: e.currentTarget.checked })}
          />
          <span>
            Detect from file
            <span class="setting-checkbox-sub">
              Follow the indentation a file already uses; the settings above apply where it shows
              none.
            </span>
          </span>
        </label>
      </div>

      <div class="settings-section">
        <div class="settings-section-title">Display</div>
        <div class="setting-row">
          <span class="setting-row-label">Whitespace</span>
          <div class="setting-row-control">
            <Segmented
              label="Render whitespace"
              value={s().renderWhitespace}
              options={[
                { value: 'none', label: 'Hidden', title: 'Spaces and tabs aren’t drawn' },
                {
                  value: 'boundary',
                  label: 'Boundary',
                  title: 'Drawn where several are in a row, not between words',
                },
                { value: 'all', label: 'All', title: 'Every space and tab is drawn' },
              ]}
              onChange={(renderWhitespace) => updateEditorSettings({ renderWhitespace })}
            />
          </div>
        </div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={s().wordWrap === 'on'}
            onChange={(e) =>
              updateEditorSettings({ wordWrap: e.currentTarget.checked ? 'on' : 'off' })
            }
          />
          <span>
            Word wrap
            <span class="setting-checkbox-sub">Wrap long lines instead of scrolling sideways.</span>
          </span>
        </label>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={s().minimap}
            onChange={(e) => updateEditorSettings({ minimap: e.currentTarget.checked })}
          />
          <span>
            Minimap
            <span class="setting-checkbox-sub">
              Show the zoomed-out code overview on the right.
            </span>
          </span>
        </label>
      </div>

      <div class="settings-section">
        <div class="settings-section-title">Formatting</div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={s().formatOnSave}
            onChange={(e) => updateEditorSettings({ formatOnSave: e.currentTarget.checked })}
          />
          <span>
            Format on save
            <span class="setting-checkbox-sub">
              Run the file's formatter on Ctrl+S. Format Document (Shift+Alt+F) runs it by hand.
            </span>
          </span>
        </label>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={s().projectFormatters}
            onChange={(e) => updateEditorSettings({ projectFormatters: e.currentTarget.checked })}
          />
          <span>
            Use the project's formatters
            <span class="setting-checkbox-sub">
              Format with the tool the project uses (Prettier, Biome, rustfmt, Ruff, gofmt,
              clang-format, …), its own copy first, or the language's standard formatter when it's
              installed. Off: only the editor's built-in formatters (TypeScript, JavaScript, JSON,
              CSS, HTML) and language servers.
            </span>
          </span>
        </label>
      </div>
    </div>
  );
}
