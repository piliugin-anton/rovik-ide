// The semantic tokens layer: TypeScript's 24-bit classifications turned into
// Monaco's 5-tuples, read against a legend whose modifier order makes
// `variable.readonly` a name a theme rule can match.
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  SEMANTIC_LEGEND,
  SEMANTIC_TOKEN_MODIFIERS,
  SEMANTIC_TOKEN_TYPES,
  encodeSemanticTokens,
  refreshSemanticTokens,
  registerSemanticTokens,
  semanticTokenName,
} from './semanticTokens';

// TypeScript's own token types, in its index order (format '2028').
const TYPE = {
  class: 0,
  enum: 1,
  interface: 2,
  namespace: 3,
  typeParameter: 4,
  type: 5,
  parameter: 6,
  variable: 7,
  enumMember: 8,
  property: 9,
  function: 10,
  member: 11,
};

// TypeScript's modifier bits, as its encoding carries them.
const TS = { declaration: 1, static: 2, async: 4, readonly: 8, defaultLibrary: 16, local: 32 };

// The bits our legend gives them, in the legend's own order.
const LEGEND = { readonly: 1, declaration: 2, defaultLibrary: 4, static: 8, async: 16, local: 32 };

/** One raw TypeScript classification: `(typeIndex + 1) << 8 | modifierBits`. */
const span = (type: number, modifiers = 0) => ((type + 1) << 8) | modifiers;

/** TypeScript's modifier bits moved to their places in our legend. */
function legendBits(ts: number): number {
  let bits = 0;
  if (ts & TS.readonly) bits |= LEGEND.readonly;
  if (ts & TS.declaration) bits |= LEGEND.declaration;
  if (ts & TS.defaultLibrary) bits |= LEGEND.defaultLibrary;
  if (ts & TS.static) bits |= LEGEND.static;
  if (ts & TS.async) bits |= LEGEND.async;
  if (ts & TS.local) bits |= LEGEND.local;
  return bits;
}

describe('the semantic token legend', () => {
  it("is TypeScript's twelve types, and its modifiers with readonly first (criterion 3)", () => {
    assert.deepEqual(SEMANTIC_TOKEN_TYPES, [
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
    ]);
    // Monaco joins a token's modifiers in legend order and matches a theme
    // rule by dot-prefix: `variable.readonly` needs readonly before declaration.
    assert.deepEqual(SEMANTIC_TOKEN_MODIFIERS, [
      'readonly',
      'declaration',
      'defaultLibrary',
      'static',
      'async',
      'local',
    ]);
    // Monaco's own key names: `tokenTypes` and `tokenModifiers`.
    assert.deepEqual(SEMANTIC_LEGEND, {
      tokenTypes: SEMANTIC_TOKEN_TYPES,
      tokenModifiers: SEMANTIC_TOKEN_MODIFIERS,
    });
  });

  it('names a raw classification the way a theme rule is matched (criterion 3)', () => {
    assert.equal(semanticTokenName(span(TYPE.variable, TS.readonly)), 'variable.readonly');
    assert.equal(
      semanticTokenName(span(TYPE.variable, TS.declaration | TS.readonly)),
      'variable.readonly.declaration'
    );
    assert.equal(semanticTokenName(span(TYPE.function)), 'function');
    assert.equal(
      semanticTokenName(span(TYPE.variable, TS.defaultLibrary)),
      'variable.defaultLibrary'
    );
    assert.equal(
      semanticTokenName(span(TYPE.property, TS.readonly | TS.static)),
      'property.readonly.static'
    );
  });
});

describe('encodeSemanticTokens', () => {
  it("makes Monaco's 5-tuples, with line and character deltas (criterion 3)", () => {
    const text = [
      'const count = 1;',
      'function add(step: number) {',
      '  return count + step;',
      '}',
    ].join('\n');
    const lineStarts = [0];
    for (let i = 0; i < text.length; i++) if (text[i] === '\n') lineStarts.push(i + 1);
    const lineOf = (index: number) => {
      let line = 0;
      while (line + 1 < lineStarts.length && lineStarts[line + 1] <= index) line++;
      return line;
    };
    // `add` and `step` share a line: there the character delta is measured
    // from the previous token, and on a new line from the line's start.
    const tokens = [
      {
        at: text.indexOf('count'),
        length: 'count'.length,
        type: TYPE.variable,
        modifiers: TS.declaration | TS.readonly,
      },
      { at: text.indexOf('add'), length: 'add'.length, type: TYPE.function, modifiers: 0 },
      {
        at: text.indexOf('step'),
        length: 'step'.length,
        type: TYPE.parameter,
        modifiers: TS.declaration,
      },
      {
        at: text.lastIndexOf('count'),
        length: 'count'.length,
        type: TYPE.variable,
        modifiers: TS.readonly,
      },
    ];
    const spans = tokens.flatMap((token) => [
      token.at,
      token.length,
      span(token.type, token.modifiers),
    ]);
    const expected: number[] = [];
    let previousLine = 0;
    let previousStart = 0;
    for (const token of tokens) {
      const line = lineOf(token.at);
      const deltaLine = line - previousLine;
      const lineStart = lineStarts[line];
      const deltaStart = deltaLine === 0 ? token.at - previousStart : token.at - lineStart;
      expected.push(deltaLine, deltaStart, token.length, token.type, legendBits(token.modifiers));
      previousLine = line;
      previousStart = token.at;
    }
    assert.deepEqual([...encodeSemanticTokens(spans, text)], expected);
    // Spelled out: a module-scope const is a readonly variable with the three
    // bits of the legend, and `step` after `add` is a delta of 4, not 13 from
    // the start of its line.
    assert.deepEqual(expected.slice(0, 5), [
      0,
      text.indexOf('count'),
      'count'.length,
      TYPE.variable,
      3,
    ]);
    const step = expected.slice(10, 15);
    assert.deepEqual(step, [0, 4, 'step'.length, TYPE.parameter, LEGEND.declaration]);
  });

  it('skips what the legend cannot hold: no length, no type, a type past its end', () => {
    const text = 'const a = 1;';
    const spans = [
      text.indexOf('a'),
      1,
      span(TYPE.variable, TS.readonly),
      // A zero-length span, a type index outside the twelve, and an
      // encoding with no type at all.
      text.indexOf('1'),
      0,
      span(TYPE.variable, TS.readonly),
      text.indexOf(';'),
      1,
      span(12, 0),
      text.indexOf('='),
      1,
      0,
    ];
    assert.deepEqual(
      [...encodeSemanticTokens(spans, text)],
      [0, text.indexOf('a'), 1, TYPE.variable, LEGEND.readonly]
    );
    assert.deepEqual([...encodeSemanticTokens([], text)], []);
  });

  it('drops the tokens the applied theme has no rule for, keeping prefix coverage', () => {
    // What the provider passes: the names the editor's theme colours (its
    // own rules and the semantic ones derived from them). An uncovered
    // token would be painted the theme's default foreground by the trie's
    // root rule, so it keeps its TextMate colour instead.
    const text = 'const count = 1;\nfunction add(step: number) { return count + step; }';
    const spans = [
      text.indexOf('count'),
      'count'.length,
      span(TYPE.variable, TS.declaration | TS.readonly),
      text.indexOf('add'),
      'add'.length,
      span(TYPE.function),
      text.indexOf('step'),
      'step'.length,
      span(TYPE.parameter),
    ];
    const theme = new Set(['variable']);
    const covered = (name: string) => {
      let current = name;
      for (;;) {
        if (theme.has(current)) return true;
        const dot = current.lastIndexOf('.');
        if (dot < 0) return false;
        current = current.slice(0, dot);
      }
    };
    const kept = [...encodeSemanticTokens(spans, text, covered)];
    // `variable.readonly.declaration` is covered by the theme's `variable`;
    // `function` and `parameter` are not, so they are left alone.
    assert.deepEqual(kept, [0, text.indexOf('count'), 'count'.length, TYPE.variable, 3]);
    // Without a filter every one of the three is emitted.
    assert.equal(encodeSemanticTokens(spans, text).length, kept.length + 10);
  });

  it('moves each TypeScript modifier bit to its place in the legend (criterion 3)', () => {
    const text = 'const a = 1;';
    const at = text.indexOf('a');
    const bitsOf = (modifiers: number) => {
      const spans = [at, 1, span(TYPE.variable, modifiers)];
      return encodeSemanticTokens(spans, text)[4];
    };
    assert.equal(bitsOf(TS.readonly), LEGEND.readonly);
    assert.equal(bitsOf(TS.declaration), LEGEND.declaration);
    assert.equal(bitsOf(TS.defaultLibrary), LEGEND.defaultLibrary);
    assert.equal(bitsOf(TS.static), LEGEND.static);
    assert.equal(bitsOf(TS.async), LEGEND.async);
    assert.equal(bitsOf(TS.local), LEGEND.local);
    assert.equal(bitsOf(TS.readonly | TS.declaration), LEGEND.readonly | LEGEND.declaration);
    assert.equal(bitsOf(0), 0);
  });
});

describe('the provider', () => {
  it('registers once and can be refreshed without throwing (criterion 6)', () => {
    assert.doesNotThrow(() => {
      registerSemanticTokens();
      registerSemanticTokens();
      refreshSemanticTokens();
      refreshSemanticTokens();
    });
  });
});
