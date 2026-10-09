// EditorSettings.tsx
//
// The Settings → Editor page: theme selector plus formatting options.
// Changes apply live (components/Editor.tsx follows the same store) and
// persist to localStorage via lib/editorSettings.ts.

import { For } from 'solid-js';
import {
  EDITOR_THEMES,
  editorSettings,
  updateEditorSettings,
  type EditorThemeId,
} from '../lib/editorSettings';
import { BUILTIN_DARK_THEMES, COMMUNITY_DARK_THEMES, preloadTheme } from '../lib/monacoThemes';

export default function EditorSettings() {
  const s = () => editorSettings();
  const themeLabel = () => EDITOR_THEMES.find((t) => t.id === s().theme)?.label ?? s().theme;
  const themeHint = () => EDITOR_THEMES.find((t) => t.id === s().theme)?.hint ?? '';

  return (
    <div class="permissions-settings editor-settings">
      <p class="settings-hint">
        How the editor looks and formats text. Applies right away to every open file.
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
              <optgroup label="Built in">
                <For each={BUILTIN_DARK_THEMES}>
                  {(t) => <option value={t.id}>{t.label}</option>}
                </For>
              </optgroup>
              <optgroup label="Community (dark)">
                <For each={COMMUNITY_DARK_THEMES}>
                  {(t) => <option value={t.id}>{t.label}</option>}
                </For>
              </optgroup>
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
        <div class="settings-section-title">Formatting</div>
        <div class="setting-row">
          <span class="setting-row-label">Indent</span>
          <div class="setting-row-control">
            <div class="segmented" role="group" aria-label="Indentation">
              <button
                type="button"
                class="segmented-option"
                classList={{ selected: s().insertSpaces }}
                onClick={() => updateEditorSettings({ insertSpaces: true })}
              >
                Spaces
              </button>
              <button
                type="button"
                class="segmented-option"
                classList={{ selected: !s().insertSpaces }}
                onClick={() => updateEditorSettings({ insertSpaces: false })}
              >
                Tabs
              </button>
            </div>
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
        <div class="setting-row">
          <span class="setting-row-label">Whitespace</span>
          <div class="setting-row-control">
            <div class="segmented" role="group" aria-label="Render whitespace">
              <button
                type="button"
                class="segmented-option"
                classList={{ selected: s().renderWhitespace === 'none' }}
                onClick={() => updateEditorSettings({ renderWhitespace: 'none' })}
              >
                Hidden
              </button>
              <button
                type="button"
                class="segmented-option"
                classList={{ selected: s().renderWhitespace === 'boundary' }}
                onClick={() => updateEditorSettings({ renderWhitespace: 'boundary' })}
              >
                Boundary
              </button>
              <button
                type="button"
                class="segmented-option"
                classList={{ selected: s().renderWhitespace === 'all' }}
                onClick={() => updateEditorSettings({ renderWhitespace: 'all' })}
              >
                All
              </button>
            </div>
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
        <div class="settings-section-title">Agent</div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={s().openFilesAfterAgentEdits}
            onChange={(e) =>
              updateEditorSettings({ openFilesAfterAgentEdits: e.currentTarget.checked })
            }
          />
          <span>
            Open files after agent edits
            <span class="setting-checkbox-sub">
              Automatically open a file the agent edited so you can review it.
            </span>
          </span>
        </label>
      </div>
    </div>
  );
}
