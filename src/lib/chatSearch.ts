// chatSearch.ts
//
// Search itself runs in Rust (src-tauri/src/chat_db/search.rs, FTS5). What
// stays here is highlighting chat titles: titles are short, and normalizing
// here maps highlight ranges exactly onto the original text.

export type Range = [start: number, end: number];

export interface Highlighted {
  text: string;
  ranges: Range[];
}

const MARKS = /\p{M}/gu;

function normalize(text: string): string {
  return text.normalize('NFKD').replace(MARKS, '').toLowerCase();
}

/**
 * Normalizes and records, for every UTF-16 unit of the result, the index of
 * the original unit it came from ("é" → "e" changes lengths).
 */
function normalizeWithMap(text: string): { norm: string; map: number[] } {
  let norm = '';
  const map: number[] = [];
  let i = 0;
  for (const ch of text) {
    const n = normalize(ch);
    norm += n;
    for (let k = 0; k < n.length; k++) map.push(i);
    i += ch.length;
  }
  map.push(text.length);
  return { norm, map };
}

/** Same term rules as the Rust search: words, "quoted phrases", 2+ characters. */
export function parseQuery(query: string): string[] {
  const terms: string[] = [];
  const re = /"([^"]+)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(query)) !== null) {
    const term = normalize((m[1] ?? m[2]).trim());
    if ([...term].length >= 2) terms.push(term);
  }
  return [...new Set(terms)];
}

function mergeRanges(ranges: Range[]): Range[] {
  const sorted = [...ranges].sort((a, b) => a[0] - b[0]);
  const out: Range[] = [];
  for (const r of sorted) {
    const last = out[out.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else out.push([r[0], r[1]]);
  }
  return out;
}

export function highlightTitle(title: string, query: string): Highlighted {
  const terms = parseQuery(query);
  const { norm, map } = normalizeWithMap(title);
  const ranges: Range[] = [];
  for (const term of terms) {
    for (let i = norm.indexOf(term); i !== -1; i = norm.indexOf(term, i + 1)) {
      ranges.push([i, i + term.length]);
    }
  }
  return { text: title, ranges: mergeRanges(ranges).map(([s, e]) => [map[s], map[e]]) };
}
