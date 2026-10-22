// withSemanticRules: the rules a standalone Monaco theme needs for semantic
// tokens, derived from the TextMate scopes it already colours.
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import type * as monaco from 'monaco-editor';
import { withSemanticRules } from './rules';

type Theme = monaco.editor.IStandaloneThemeData;
type Rule = Theme['rules'][number];

/** Where the app ships its themes' JSON. */
const THEMES = join(process.cwd(), 'src/lib/monacoThemes/data');

const theme = (rules: Rule[], base: Theme['base'] = 'vs-dark'): Theme => ({
  base,
  inherit: true,
  rules,
  colors: {},
});
const hex = (color: string | undefined) => color?.replace(/^#/, '').toUpperCase();
const shipped = (name: string): Theme =>
  JSON.parse(readFileSync(join(THEMES, name), 'utf8')) as Theme;
const shippedFiles = () => readdirSync(THEMES).filter((name) => name.endsWith('.json'));
const ownRules = (input: Theme): Rule[] => input.rules ?? [];

/** The rule `out` derives for `token`, which the theme's own rules don't name. */
function derived(out: Theme, owner: Theme, token: string): Rule | undefined {
  const rules = out.rules.slice(ownRules(owner).length).filter((rule) => rule.token === token);
  assert.ok(rules.length <= 1, `at most one derived rule for ${token}`);
  return rules[0];
}

/** The colour the theme's rules (its own, then derived) give `token`. */
function color(out: Theme, owner: Theme, token: string): string {
  const rule = derived(out, owner, token);
  assert.ok(rule, `a derived rule for ${token}`);
  return hex(rule.foreground) ?? '';
}

/** The colour a rule of the theme itself has, by exact token name. */
function ownColor(input: Theme, token: string): string | undefined {
  return hex(ownRules(input).find((rule) => rule.token === token)?.foreground);
}

/** The colour the theme's own rules give a scope: exact name, then dot-prefixes. */
function ruleColor(input: Theme, scope: string): string | undefined {
  for (let name = scope; name; name = name.slice(0, Math.max(0, name.lastIndexOf('.')))) {
    const rules = ownRules(input).filter((rule) => rule.token === name && rule.foreground);
    if (rules.length) return hex(rules[rules.length - 1].foreground);
  }
  return undefined;
}

describe('withSemanticRules', () => {
  it("keeps the theme's own rules and appends what its scopes imply", () => {
    const rules: Rule[] = [
      { token: 'keyword', foreground: 'C586C0' },
      { token: 'entity.name.function', foreground: 'DCDCAA', fontStyle: 'italic' },
    ];
    const input = theme(rules);
    const out = withSemanticRules(input);
    assert.notEqual(out, input);
    assert.notEqual(out.rules, input.rules);
    assert.equal(out.base, 'vs-dark');
    assert.equal(out.inherit, true);
    assert.deepEqual(out.rules.slice(0, rules.length), rules, "the theme's own rules come first");
    assert.deepEqual(input.rules, rules, 'the theme itself is left as it was');
    // `entity.name.function` drives both `function` and `member`, colour and
    // font style copied.
    assert.equal(color(out, input, 'function'), 'DCDCAA');
    assert.equal(color(out, input, 'member'), 'DCDCAA');
    assert.equal(derived(out, input, 'function')?.fontStyle, 'italic');
  });

  it('never overwrites a token name the theme itself defines', () => {
    const input = theme([
      { token: 'function', foreground: 'FF0000' },
      { token: 'property', foreground: '00FF00' },
      { token: 'variable', foreground: '9CDCFE' },
      { token: 'entity.name.function', foreground: 'DCDCAA' },
      { token: 'variable.other.constant', foreground: '4FC1FF' },
    ]);
    const out = withSemanticRules(input);
    for (const [token, expected] of [
      ['function', 'FF0000'],
      ['property', '00FF00'],
      ['variable', '9CDCFE'],
      ['variable.other.constant', '4FC1FF'],
    ] as const) {
      const found = out.rules.filter((rule) => rule.token === token);
      assert.equal(found.length, 1, `one rule for ${token}`);
      assert.equal(hex(found[0].foreground), expected, `${token} keeps the theme's colour`);
    }
  });

  it('splits readonly off from the writeable token it was kept with', () => {
    const input = theme([
      { token: 'variable.other.constant', foreground: '4FC1FF' },
      { token: 'variable', foreground: '9CDCFE' },
    ]);
    const out = withSemanticRules(input);
    // `const` and readonly members are #4FC1FF; other variables the theme's own.
    assert.equal(color(out, input, 'variable.readonly'), '4FC1FF');
    assert.equal(color(out, input, 'property.readonly'), '4FC1FF');
    // `variable` itself is the theme's own rule: derivation never takes a
    // name the theme defines, so the effective colour stays the theme's.
    const variables = out.rules.filter((rule) => rule.token === 'variable');
    assert.equal(variables.length, 1, 'one rule for variable');
    assert.equal(hex(variables[0].foreground), '9CDCFE');
  });

  it("falls back through the source's own dot-prefixes", () => {
    // The theme colours `support.variable` but no `support.variable.property`;
    // an exact lookup alone would leave built-in properties uncoloured.
    const input = theme([{ token: 'support.variable', foreground: 'AA0000' }]);
    const out = withSemanticRules(input);
    assert.equal(color(out, input, 'property.defaultLibrary'), 'AA0000');
    assert.equal(color(out, input, 'variable.defaultLibrary'), 'AA0000');
    assert.equal(derived(out, input, 'property'), undefined, 'no source for a plain property');
    assert.equal(derived(out, input, 'variable'), undefined, 'no source for a plain variable');
  });

  it("never paints a const with the theme's literal colour (VS Code has no such fallback)", () => {
    // Themes colour `true`/`false` (`constant.language`) and `constant.other`
    // apart from `variable.other.constant`: VS Code never gives a const the
    // literal colour, and so must the derived rules.
    for (const file of ['GitHub Dark.json', 'Twilight.json', 'SpaceCadet.json', 'Cobalt.json']) {
      const input = shipped(file);
      const out = withSemanticRules(input);
      for (const name of ['variable.readonly', 'property.readonly', 'enumMember']) {
        const rule = derived(out, input, name);
        const expected = ruleColor(
          input,
          name === 'enumMember' ? 'variable.other.enummember' : 'variable.other.constant'
        );
        assert.equal(hex(rule?.foreground), expected, `${file}: ${name}`);
      }
    }
  });

  it('colours readonly built-ins as VS Code does, not as constants', () => {
    // `"abc".length` is `property.readonly.defaultLibrary`, `Math.PI` too: a
    // theme's support scopes win over its constant colour.
    const input = theme([
      { token: 'variable', foreground: '9CDCFE' },
      { token: 'variable.other.constant', foreground: '4FC1FF' },
      { token: 'support.variable', foreground: '9CDCFE' },
    ]);
    const out = withSemanticRules(input);
    assert.equal(color(out, input, 'property.readonly.defaultLibrary'), '9CDCFE');
    assert.equal(color(out, input, 'variable.readonly.defaultLibrary'), '9CDCFE');
    // …and the theme's own `support.constant` when it has one.
    const own = theme([
      { token: 'support.constant', foreground: '66D9EF' },
      { token: 'support.variable', foreground: '9CDCFE' },
    ]);
    const ownOut = withSemanticRules(own);
    assert.equal(color(ownOut, own, 'variable.readonly.defaultLibrary'), '66D9EF');
    assert.equal(color(ownOut, own, 'property.readonly.defaultLibrary'), '66D9EF');
  });

  it('colours built-in types with the support scopes', () => {
    const input = theme([
      { token: 'entity.name.type', foreground: 'A6E22E' },
      { token: 'support.class', foreground: '66D9EF' },
      { token: 'support.type', foreground: '66D9EE' },
    ]);
    const out = withSemanticRules(input);
    assert.equal(color(out, input, 'class'), 'A6E22E');
    assert.equal(color(out, input, 'class.defaultLibrary'), '66D9EF');
    assert.equal(color(out, input, 'interface.defaultLibrary'), '66D9EF');
    assert.equal(color(out, input, 'type.defaultLibrary'), '66D9EE');
  });

  it('emits nothing for a token the theme gives no source', () => {
    const rules: Rule[] = [{ token: 'keyword', foreground: 'C586C0' }];
    const input = theme(rules);
    assert.deepEqual(withSemanticRules(input).rules, rules);
    assert.deepEqual(withSemanticRules(theme([])).rules, []);
  });

  it('skips a source that has only a font style', () => {
    // A derived rule with no colour would paint the token the theme's
    // default foreground (the trie's root rule hands it down), which is
    // flatter than the TextMate colour it has today: leave it alone.
    const input = theme([{ token: 'entity.name.type', fontStyle: 'underline' }]);
    assert.deepEqual(withSemanticRules(input).rules, input.rules);
    const mixed = theme([
      { token: 'entity.name.type', fontStyle: 'underline' },
      { token: 'support.class', foreground: '4EC9B0' },
    ]);
    const out = withSemanticRules(mixed);
    assert.equal(color(out, mixed, 'class'), '4EC9B0');
    assert.equal(derived(out, mixed, 'interface'), undefined, 'only a style to copy');
  });

  it('colours every shipped theme with its own values (criterion 4)', () => {
    const files = shippedFiles();
    assert.ok(files.length >= 8, `themes shipped: ${files.length}`);
    let functions = 0;
    let numbers = 0;
    for (const file of files) {
      const input = shipped(file);
      const out = withSemanticRules(input);
      const own = ownRules(input);
      const appended = out.rules.slice(own.length);
      assert.deepEqual(out.rules.slice(0, own.length), own, `${file}: its own rules are kept`);
      const names = appended.map((rule) => rule.token);
      assert.equal(new Set(names).size, names.length, `${file}: no repeated derived rule`);
      for (const name of names) {
        assert.ok(
          !own.some((rule) => rule.token === name),
          `${file}: derived ${name} does not take a name the theme defines`
        );
      }
      const fn = ownColor(input, 'entity.name.function');
      if (fn) {
        functions++;
        assert.equal(color(out, input, 'function'), fn, `${file}: function`);
        assert.equal(color(out, input, 'member'), fn, `${file}: member`);
      }
      const number = ownColor(input, 'constant.numeric');
      if (number) {
        numbers++;
        assert.equal(color(out, input, 'number'), number, `${file}: number`);
      }
    }
    assert.ok(functions >= 1, `themes colouring entity.name.function: ${functions}`);
    assert.ok(numbers >= 1, `themes colouring constant.numeric: ${numbers}`);
  });

  it("derives types only from VS Code's probe scopes, never keyword or base-class colours", () => {
    // VS Code probes `entity.name.type.class`/`support.class` for a class,
    // `entity.name.type.interface` for an interface, `entity.name.type`/
    // `support.type` for a type. A theme naming only `storage.type` (the
    // keyword colour) or `entity.other.inherited-class` gets no rule, and the
    // token keeps its grammar colour — as in VS Code.
    const input = theme([
      { token: 'storage.type', foreground: '66D9EF' },
      { token: 'entity.other.inherited-class', foreground: 'FC83FF' },
      { token: 'entity.name.class', foreground: 'A6E22E' },
    ]);
    const out = withSemanticRules(input);
    for (const name of ['class', 'interface', 'type', 'enum', 'typeParameter']) {
      assert.equal(derived(out, input, name), undefined, name);
    }
  });

  it("derives Dark Modern's colours, VS Code's own (criterion 1)", () => {
    const input = shipped('Dark-Modern.json');
    const out = withSemanticRules(input);
    // Later rules win in Monaco's trie, so the effective colour is the last
    // rule for a name, whether the theme wrote it or it was derived.
    const effective = (token: string) => {
      const rules = out.rules.filter((rule) => rule.token === token);
      return hex(rules[rules.length - 1]?.foreground);
    };
    assert.equal(effective('function'), 'DCDCAA', 'functions and methods');
    assert.equal(effective('member'), 'DCDCAA', 'methods');
    assert.equal(effective('variable.readonly'), '4FC1FF', 'const and readonly members');
    assert.equal(effective('property.readonly'), '4FC1FF', 'readonly members');
    assert.equal(effective('variable'), '9CDCFE', 'variables');
    assert.equal(effective('property'), '9CDCFE', 'properties');
    assert.equal(effective('class'), '4EC9B0', 'classes');
    assert.equal(effective('interface'), '4EC9B0', 'interfaces');
    assert.equal(effective('type'), '4EC9B0', 'types');
    // Four kinds, four colours: not one flat foreground.
    const distinct = new Set([
      effective('function'),
      effective('variable.readonly'),
      effective('variable'),
      effective('class'),
    ]);
    assert.equal(distinct.size, 4);
  });
});
