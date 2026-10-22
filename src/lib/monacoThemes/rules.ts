// rules.ts
//
// The theme rules semantic tokens need. Monaco's standalone themes are
// TextMate rule lists: a rule's `token` is matched against the scope names
// a grammar produces (`entity.name.function`), or — with semantic tokens on
// — against a semantic token's name (`function`, `variable.readonly`). No
// shipped theme has the second kind, so a semantic token would fall to the
// theme's default foreground and the file would look flatter than before.
//
// So the rules are derived from the scopes the theme already colours: this
// takes a theme and returns the same theme with rules appended for the
// semantic names, each copying the colour of the scope it stands for
// (`entity.name.function` → `function` and `member`). A name the theme
// already defines is left alone — the appending keeps the theme's own
// choices authoritative.
//
// Which scopes stand for which name is VS Code's table (SOURCES below), and
// a scope is found as both editors find it: an exact rule first, then its
// longest dot-prefix (`constant.numeric.hex` falls back to `constant.numeric`).
import type * as monaco from 'monaco-editor';

type Theme = monaco.editor.IStandaloneThemeData;
type Rule = Theme['rules'][number];

/**
 * The semantic token names a theme can be asked for, each with the scopes
 * that colour it, in the order they're tried.
 *
 * The semantic part is VS Code's own table (the `scopesToProbe` of
 * tokenClassificationRegistry.ts), so a theme colours a function or a
 * constant here as it does in VS Code and Cursor. Each scope is looked up
 * by dot-prefix (`variable.other.constant` falls back to a `variable`
 * rule), as VS Code matches it, and no further: a theme with no rule for a
 * scope gets no derived rule, and Monaco then matches the token's shorter
 * name — `variable.readonly` takes the `variable` rule, as in VS Code.
 *
 * The combinations with `defaultLibrary` follow VS Code's scoring: the
 * three-part name wins when its own scope is coloured, otherwise the
 * `defaultLibrary` one beats `readonly` (registered later, equal score),
 * so `"abc".length` and `Math.PI` take `support.variable.property` /
 * `support.variable` before `variable.other.constant`.
 *
 * Then the Monarch tokenizer's own coarse scopes (`identifier`, `number`,
 * `type.identifier`, `constant`, `annotation`), which no theme here names.
 */
const SOURCES: Record<string, readonly string[]> = {
  // Types.
  namespace: ['entity.name.namespace'],
  type: ['entity.name.type', 'support.type'],
  class: ['entity.name.type.class', 'support.class'],
  interface: ['entity.name.type.interface'],
  enum: ['entity.name.type.enum'],
  typeParameter: ['entity.name.type.parameter'],
  // Values. TypeScript's `member` is VS Code's `method`.
  function: ['entity.name.function', 'support.function'],
  member: ['entity.name.function.member', 'support.function'],
  variable: ['variable.other.readwrite', 'entity.name.variable'],
  parameter: ['variable.parameter'],
  property: ['variable.other.property'],
  enumMember: ['variable.other.enummember'],
  // Modifier combinations.
  'variable.readonly': ['variable.other.constant'],
  'property.readonly': ['variable.other.constant.property'],
  'type.defaultLibrary': ['support.type'],
  'class.defaultLibrary': ['support.class'],
  'interface.defaultLibrary': ['support.class'],
  'variable.defaultLibrary': ['support.variable', 'support.other.variable'],
  'variable.readonly.defaultLibrary': [
    'support.constant',
    'support.variable',
    'support.other.variable',
  ],
  'property.defaultLibrary': ['support.variable.property'],
  'property.readonly.defaultLibrary': ['support.constant.property', 'support.variable.property'],
  'function.defaultLibrary': ['support.function'],
  'member.defaultLibrary': ['support.function'],
  // The tokenizer's own coarse scopes.
  identifier: ['variable.other.readwrite', 'variable'],
  number: ['constant.numeric', 'constant'],
  'number.hex': ['constant.numeric.hex', 'constant.numeric', 'constant'],
  'number.float': ['constant.numeric.float', 'constant.numeric', 'constant'],
  'number.octal': ['constant.numeric.octal', 'constant.numeric', 'constant'],
  'number.binary': ['constant.numeric.binary', 'constant.numeric', 'constant'],
  'type.identifier': [
    'entity.name.type.class',
    'entity.name.type',
    'support.class',
    'support.type',
  ],
  constant: ['constant.language', 'constant'],
  annotation: ['entity.name.function.decorator', 'storage.type.annotation'],
};

/** The theme's rule for one token name (the last, as Monaco's trie takes it). */
function byToken(rules: readonly Rule[]): Map<string, Rule> {
  const map = new Map<string, Rule>();
  for (const rule of rules) if (rule.token) map.set(rule.token, rule);
  return map;
}

/**
 * The rule the theme colours `token` with: the exact name's, else the
 * longest dot-prefix's — how Monaco's ThemeTrieElement matches a scope.
 */
function ruleFor(map: Map<string, Rule>, token: string): Rule | undefined {
  let name = token;
  for (;;) {
    const rule = map.get(name);
    if (rule) return rule;
    const dot = name.lastIndexOf('.');
    if (dot < 0) return undefined;
    name = name.slice(0, dot);
  }
}

/**
 * The theme with the rules its semantic tokens need appended: one per name
 * a source scope colours, and none for a name the theme itself defines.
 * `fontStyle` comes with the colour (italic functions stay italic); a
 * source with no colour doesn't count, since its token would then be
 * painted the theme's default foreground.
 */
export function withSemanticRules(theme: Theme): Theme {
  const own = theme.rules ?? [];
  const rules = byToken(own);
  const defined = new Set(rules.keys());
  const derived: Rule[] = [];
  for (const [name, sources] of Object.entries(SOURCES)) {
    if (defined.has(name)) continue;
    // The first source that has a colour. A rule carrying only a font
    // style is no use: the derived rule would leave the token the theme's
    // default foreground (the trie's root rule hands the colour down),
    // while today the token keeps its TextMate one.
    let source: Rule | undefined;
    for (const candidate of sources) {
      const found = ruleFor(rules, candidate);
      if (found?.foreground !== undefined) {
        source = found;
        break;
      }
    }
    if (!source?.foreground) continue;
    const rule: Rule = { token: name, foreground: source.foreground };
    if (source.fontStyle !== undefined) rule.fontStyle = source.fontStyle;
    derived.push(rule);
    defined.add(name);
  }
  return { ...theme, rules: [...own, ...derived] };
}
