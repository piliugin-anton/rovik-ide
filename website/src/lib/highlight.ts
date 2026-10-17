// Colors a line of TypeScript the way the editor's dark theme does, for the
// drawn window's code (components/AppWindow.tsx). Enough of the language
// for that sample, not a parser.

export interface Token {
  text: string;
  kind?: 'control' | 'keyword' | 'string' | 'number' | 'type' | 'function' | 'variable' | 'comment';
}

const RULES: [RegExp, Token['kind']][] = [
  [/^\/\/.*/, 'comment'],
  [/^'(?:[^'\\]|\\.)*'/, 'string'],
  [/^\d+(?:\.\d+)?/, 'number'],
  [/^(?:import|from|export|return|for|if|throw|await)\b/, 'control'],
  [/^(?:const|let|async|function|new|as|interface|type|true|false)\b/, 'keyword'],
  [/^(?:number|string|boolean|void|unknown)\b/, 'type'],
  [/^[A-Z][\w$]*/, 'type'],
  // A call, or a generic one: fetchJson<T>(…), not `a < b`.
  [/^[a-z_$][\w$]*(?=\(|<[A-Z])/, 'function'],
  [/^[a-z_$][\w$]*/, 'variable'],
];

export function highlight(line: string): Token[] {
  const tokens: Token[] = [];
  let rest = line;
  while (rest) {
    let matched = false;
    for (const [rule, kind] of RULES) {
      const m = rule.exec(rest);
      if (m) {
        tokens.push({ text: m[0], kind });
        rest = rest.slice(m[0].length);
        matched = true;
        break;
      }
    }
    if (!matched) {
      // Punctuation and spaces, as one plain run up to the next word.
      const m = /^[^\w$'/]+|^./.exec(rest)!;
      const last = tokens[tokens.length - 1];
      if (last && !last.kind) last.text += m[0];
      else tokens.push({ text: m[0] });
      rest = rest.slice(m[0].length);
    }
  }
  return tokens;
}
