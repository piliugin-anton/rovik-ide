// The theme registry: only themes that colour code completely are offered,
// each carries derived semantic rules, and Dark Modern is the default.
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import type * as monaco from 'monaco-editor';
import { editorSettings, EDITOR_THEMES } from './editorSettings';
import {
  COMMUNITY_DARK_THEMES,
  DARK_THEME_IDS,
  DEFAULT_THEME_ID,
  hasSemanticRules,
  isDarkThemeId,
} from './monacoThemes';
import { withSemanticRules } from './monacoThemes/rules';

type Theme = monaco.editor.IStandaloneThemeData;

const DATA = join(process.cwd(), 'src/lib/monacoThemes/data');

/** Every name TypeScript's semantic tokens are matched by (semanticTokens.ts). */
const SEMANTIC_NAMES = [
  'class',
  'interface',
  'enum',
  'type',
  'typeParameter',
  'namespace',
  'function',
  'member',
  'variable',
  'variable.readonly',
  'parameter',
  'property',
  'enumMember',
];

/** Whether a coloured rule names `name` or a dot-prefix of it (Monaco's trie match). */
function colours(theme: Theme, name: string): boolean {
  const coloured = new Set(
    theme.rules.filter((rule) => rule.token && rule.foreground).map((rule) => rule.token)
  );
  for (let current = name; ; current = current.slice(0, current.lastIndexOf('.'))) {
    if (coloured.has(current)) return true;
    if (!current.includes('.')) return false;
  }
}

describe('the themes offered', () => {
  it('are exactly the shipped files, one entry each', () => {
    const files = readdirSync(DATA).filter((name) => name.endsWith('.json'));
    assert.deepEqual(COMMUNITY_DARK_THEMES.map((theme) => theme.file).sort(), [...files].sort());
    assert.equal(DARK_THEME_IDS.size, COMMUNITY_DARK_THEMES.length);
    assert.deepEqual(
      EDITOR_THEMES.map((theme) => theme.id),
      COMMUNITY_DARK_THEMES.map((theme) => theme.id)
    );
  });

  it('colour every semantic token TypeScript reports', () => {
    for (const def of COMMUNITY_DARK_THEMES) {
      const theme = withSemanticRules(
        JSON.parse(readFileSync(join(DATA, def.file), 'utf8')) as Theme
      );
      const missing = SEMANTIC_NAMES.filter((name) => !colours(theme, name));
      assert.deepEqual(missing, [], `${def.label} leaves these to the default foreground`);
    }
  });

  it("don't include Monaco's built-ins, whose rules can't be extended", () => {
    for (const id of ['vs-dark', 'hc-black']) {
      assert.equal(isDarkThemeId(id), false, id);
      assert.equal(hasSemanticRules(id), false, id);
    }
  });

  it('all get semantic colouring', () => {
    for (const theme of COMMUNITY_DARK_THEMES) assert.equal(hasSemanticRules(theme.id), true);
  });
});

describe('Dark Modern', () => {
  it('is offered first and is the default', () => {
    assert.equal(DEFAULT_THEME_ID, 'dark-modern');
    assert.equal(COMMUNITY_DARK_THEMES[0].id, 'dark-modern');
    assert.equal(COMMUNITY_DARK_THEMES[0].label, 'Dark Modern');
  });

  it('is the theme of an install with none saved', () => {
    assert.equal(editorSettings().theme, 'dark-modern');
  });
});
