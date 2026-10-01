// chatSearch.ts
//
// Ranked search over chat titles and messages.
//
// History is local and small (at most a few MB), so a linear scan over
// cached, normalized text is fast enough per keystroke and keeps exact
// positions for highlighting. If history ever grows to tens of MB, swap the
// scan for an inverted index (e.g. MiniSearch) and keep the scoring rules.
//
// Rules:
//  - Normalized matching: lowercase, accents stripped, Unicode-aware word
//    boundaries (Cyrillic works like Latin).
//  - AND: every query term must appear in the title or some message.
//  - Title hits outweigh message hits; word-start beats mid-word; whole word
//    beats prefix; the full phrase earns a bonus; message hits are capped so
//    one huge chat can't dominate; recency breaks ties.
//  - Typo tolerance only as a fallback, only on titles (Damerau-Levenshtein).
//  - Tool messages are skipped: file contents and page text match everything.
import type { ChatDialog } from '../types';

export type Range = [start: number, end: number];

export interface Highlighted {
  text: string;
  ranges: Range[];
}

export interface ChatSearchHit {
  dialog: ChatDialog;
  score: number;
  title: Highlighted;
  /** Excerpt from the best-matching message, when the match isn't title-only. */
  snippet?: Highlighted;
}

/*
 * ---------------------------------------------------------------------------
 * Normalization
 * ---------------------------------------------------------------------------
 */

const MARKS = /\p{M}/gu;
const WORD_CHAR = /[\p{L}\p{N}_]/u;

function normalize(text: string): string {
  return text.normalize('NFKD').replace(MARKS, '').toLowerCase();
}

/**
 * Normalizes and records, for every UTF-16 unit of the result, the index of
 * the original unit it came from. Normalizing can change length ("é" → "e",
 * "ß" stays, "İ" → "i̇"), so this is what lets highlights land on the
 * original text. Only built for the few strings actually displayed.
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
  map.push(text.length); // end sentinel
  return { norm, map };
}

function isWordStart(text: string, index: number): boolean {
  return index === 0 || !WORD_CHAR.test(text[index - 1]);
}

function isWordEnd(text: string, index: number): boolean {
  return index >= text.length || !WORD_CHAR.test(text[index]);
}

/** Splits a query into terms; "quoted text" stays one term. */
export function parseQuery(query: string): string[] {
  const terms: string[] = [];
  const re = /"([^"]+)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(query)) !== null) {
    const term = normalize((m[1] ?? m[2]).trim());
    if (term) terms.push(term);
  }
  return [...new Set(terms)];
}

/*
 * ---------------------------------------------------------------------------
 * Per-dialog cache
 * ---------------------------------------------------------------------------
 */

interface IndexedMessage {
  original: string;
  norm: string;
}

interface IndexedDialog {
  updatedAt: number;
  messageCount: number;
  titleNorm: string;
  messages: IndexedMessage[];
}

const cache = new Map<string, IndexedDialog>();

function indexDialog(dialog: ChatDialog): IndexedDialog {
  const cached = cache.get(dialog.id);
  if (
    cached &&
    cached.updatedAt === dialog.updatedAt &&
    cached.messageCount === dialog.messages.length
  ) {
    return cached;
  }

  const entry: IndexedDialog = {
    updatedAt: dialog.updatedAt,
    messageCount: dialog.messages.length,
    titleNorm: normalize(dialog.title),
    messages: dialog.messages
      .filter((m) => (m.role === 'user' || m.role === 'assistant') && !m.error && m.content)
      .map((m) => ({ original: m.content, norm: normalize(m.content) })),
  };
  cache.set(dialog.id, entry);
  return entry;
}

/** Drops cache entries for dialogs that no longer exist. */
export function pruneSearchCache(dialogs: ChatDialog[]) {
  const alive = new Set(dialogs.map((d) => d.id));
  for (const id of cache.keys()) if (!alive.has(id)) cache.delete(id);
}

/*
 * ---------------------------------------------------------------------------
 * Scoring
 * ---------------------------------------------------------------------------
 */

const TITLE_WHOLE_WORD = 14;
const TITLE_WORD_START = 10;
const TITLE_SUBSTRING = 4;
const MSG_WHOLE_WORD = 2.5;
const MSG_WORD_START = 2;
const MSG_SUBSTRING = 1;
const MSG_HITS_CAP = 6; // per term, across all messages
const PHRASE_IN_TITLE = 15;
const PHRASE_IN_MESSAGE = 6;
const FUZZY_TITLE = 6;
const RECENCY_MAX = 2; // fades over ~a month

function hitWeight(
  text: string,
  index: number,
  termLength: number,
  weights: [whole: number, start: number, sub: number]
): number {
  if (!isWordStart(text, index)) return weights[2];
  return isWordEnd(text, index + termLength) ? weights[0] : weights[1];
}

/** Best single-occurrence weight of `term` in `text`, or 0. */
function bestHit(text: string, term: string, weights: [number, number, number]): number {
  let best = 0;
  for (let i = text.indexOf(term); i !== -1; i = text.indexOf(term, i + 1)) {
    best = Math.max(best, hitWeight(text, i, term.length, weights));
    if (best === weights[0]) break;
  }
  return best;
}

function scoreDialog(dialog: ChatDialog, entry: IndexedDialog, terms: string[]): number | null {
  let score = 0;

  for (const term of terms) {
    const titleScore = bestHit(entry.titleNorm, term, [
      TITLE_WHOLE_WORD,
      TITLE_WORD_START,
      TITLE_SUBSTRING,
    ]);

    let messageScore = 0;
    let hits = 0;
    for (const msg of entry.messages) {
      for (
        let i = msg.norm.indexOf(term);
        i !== -1 && hits < MSG_HITS_CAP;
        i = msg.norm.indexOf(term, i + 1)
      ) {
        messageScore += hitWeight(msg.norm, i, term.length, [
          MSG_WHOLE_WORD,
          MSG_WORD_START,
          MSG_SUBSTRING,
        ]);
        hits++;
      }
      if (hits >= MSG_HITS_CAP) break;
    }

    if (titleScore === 0 && messageScore === 0) return null; // AND
    score += titleScore + messageScore;
  }

  if (terms.length > 1) {
    const phrase = terms.join(' ');
    if (entry.titleNorm.includes(phrase)) score += PHRASE_IN_TITLE;
    else if (entry.messages.some((m) => m.norm.includes(phrase))) score += PHRASE_IN_MESSAGE;
  }

  return score + recencyBonus(dialog.updatedAt);
}

function recencyBonus(updatedAt: number): number {
  const ageDays = Math.max(0, (Date.now() - updatedAt) / 86_400_000);
  return RECENCY_MAX * Math.exp(-ageDays / 30);
}

/*
 * ---------------------------------------------------------------------------
 * Fuzzy fallback (titles only)
 * ---------------------------------------------------------------------------
 */

/** Optimal-string-alignment distance, bailing out once it exceeds `max`. */
function editDistance(a: string, b: string, max: number): number {
  if (Math.abs(a.length - b.length) > max) return max + 1;

  let prev2: number[] = [];
  let prev = Array.from({ length: b.length + 1 }, (_, j) => j);

  for (let i = 1; i <= a.length; i++) {
    const cur = [i];
    let rowMin = i;
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      let v = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost);
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) {
        v = Math.min(v, prev2[j - 2] + 1);
      }
      cur.push(v);
      rowMin = Math.min(rowMin, v);
    }
    if (rowMin > max) return max + 1;
    prev2 = prev;
    prev = cur;
  }
  return prev[b.length];
}

function maxTypos(term: string): number {
  if (term.length >= 8) return 2;
  if (term.length >= 4) return 1;
  return 0; // short terms: too many accidental matches
}

/** Title words matching `term` within its typo budget, as [start, end) in titleNorm. */
function fuzzyTitleMatches(titleNorm: string, term: string): Range[] {
  const budget = maxTypos(term);
  if (budget === 0) return [];
  const ranges: Range[] = [];
  for (const m of titleNorm.matchAll(/[\p{L}\p{N}_]+/gu)) {
    const word = m[0];
    // Compare against the word, and its prefix of the term's length, so
    // "tauir" also matches "tauri-ide".
    const prefix = word.slice(0, term.length);
    if (
      editDistance(term, word, budget) <= budget ||
      (word.length > term.length && editDistance(term, prefix, budget) <= budget)
    ) {
      ranges.push([m.index!, m.index! + word.length]);
    }
  }
  return ranges;
}

/*
 * ---------------------------------------------------------------------------
 * Highlights and snippets
 * ---------------------------------------------------------------------------
 */

function findRanges(norm: string, terms: string[]): Range[] {
  const ranges: Range[] = [];
  for (const term of terms) {
    for (let i = norm.indexOf(term); i !== -1; i = norm.indexOf(term, i + 1)) {
      ranges.push([i, i + term.length]);
    }
  }
  return mergeRanges(ranges);
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

/** Maps ranges on normalized text back onto the original string. */
function toOriginal(ranges: Range[], map: number[]): Range[] {
  return ranges.map(([s, e]) => [map[s], map[e]]);
}

function highlightTitle(title: string, terms: string[]): Highlighted {
  const { norm, map } = normalizeWithMap(title);
  return { text: title, ranges: toOriginal(findRanges(norm, terms), map) };
}

const SNIPPET_BEFORE = 40;
const SNIPPET_LENGTH = 140;

/** Excerpt from the message containing the most distinct terms. */
function buildSnippet(entry: IndexedDialog, terms: string[]): Highlighted | undefined {
  let best: IndexedMessage | undefined;
  let bestCount = 0;
  for (const msg of entry.messages) {
    const count = terms.filter((t) => msg.norm.includes(t)).length;
    if (count > bestCount) {
      best = msg;
      bestCount = count;
      if (count === terms.length) break;
    }
  }
  if (!best) return undefined;

  // Collapse whitespace first so the excerpt reads as one line.
  const flat = best.original.replace(/\s+/g, ' ').trim();
  const { norm, map } = normalizeWithMap(flat);
  const all = findRanges(norm, terms);
  if (all.length === 0) return undefined;

  const firstHit = map[all[0][0]];
  let start = Math.max(0, firstHit - SNIPPET_BEFORE);
  let end = Math.min(flat.length, start + SNIPPET_LENGTH);
  start = Math.max(0, end - SNIPPET_LENGTH);

  // Don't cut words in half at the edges.
  if (start > 0) {
    const space = flat.indexOf(' ', start);
    if (space !== -1 && space < firstHit) start = space + 1;
  }
  if (end < flat.length) {
    const space = flat.lastIndexOf(' ', end);
    if (space > firstHit) end = space;
  }

  const prefix = start > 0 ? '…' : '';
  const suffix = end < flat.length ? '…' : '';
  const ranges = toOriginal(all, map)
    .filter(([s, e]) => s >= start && e <= end)
    .map(([s, e]): Range => [s - start + prefix.length, e - start + prefix.length]);

  return { text: prefix + flat.slice(start, end) + suffix, ranges };
}

/*
 * ---------------------------------------------------------------------------
 * Entry point
 * ---------------------------------------------------------------------------
 */

export function searchChats(dialogs: ChatDialog[], query: string, limit = 50): ChatSearchHit[] {
  const terms = parseQuery(query);
  if (terms.length === 0) return [];

  const hits: ChatSearchHit[] = [];
  for (const dialog of dialogs) {
    const entry = indexDialog(dialog);
    const score = scoreDialog(dialog, entry, terms);
    if (score === null) continue;

    const title = highlightTitle(dialog.title, terms);
    hits.push({
      dialog,
      score,
      title,
      snippet:
        title.ranges.length > 0 && terms.every((t) => entry.titleNorm.includes(t))
          ? undefined
          : buildSnippet(entry, terms),
    });
  }

  // Nothing matched exactly: retry titles with typo tolerance.
  if (hits.length === 0) {
    for (const dialog of dialogs) {
      const entry = indexDialog(dialog);
      const perTerm = terms.map((t) => fuzzyTitleMatches(entry.titleNorm, t));
      if (perTerm.some((r) => r.length === 0)) continue; // still AND

      const { map } = normalizeWithMap(dialog.title);
      hits.push({
        dialog,
        score: FUZZY_TITLE * terms.length + recencyBonus(dialog.updatedAt),
        title: { text: dialog.title, ranges: toOriginal(mergeRanges(perTerm.flat()), map) },
      });
    }
  }

  return hits
    .sort((a, b) => b.score - a.score || b.dialog.updatedAt - a.dialog.updatedAt)
    .slice(0, limit);
}
