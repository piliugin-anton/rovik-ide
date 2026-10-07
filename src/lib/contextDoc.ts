// contextDoc.ts
//
// The agent's context file in CLM mode (src-tauri/src/clm/document.rs), read
// on the frontend to show what an edit to it did: which blocks it removed,
// rewrote or added, and how much context that freed.
//
// ```text
// [[LIVE_CONTEXT revision=12 turns=3]]
//
// [[CTX_TURN 1 role=user id=6f0c…]]
// Fix the failing test in parser.rs
// ```

const TURN_PREFIX = '[[CTX_TURN ';
const META_PREFIX = '[[LIVE_CONTEXT ';

/** Like history.rs's DEFAULT_CHARS_PER_TOKEN: estimates, shown with a "~". */
const CHARS_PER_TOKEN = 3;

/** Whether a file tool's `path` is the context file itself (not a revision or a scratch file). */
export function isLiveContextPath(path: string | undefined): boolean {
  if (!path) return false;
  const p = path.trim().replace(/\\/g, '/').replace(/\/+$/, '');
  if (p === '@context' || p === '@context/LIVE_CONTEXT.md') return true;
  // The real path on the volume (run_command's view), used with file tools too.
  return /(^|\/)context\/[^/]+\/LIVE_CONTEXT\.md$/.test(p);
}

export interface ContextBlock {
  /** The number in the header; informative only. */
  turn?: string;
  role: string;
  id?: string;
  body: string;
}

/** `[[CTX_TURN 3 role=tool id=abc]]`, as clm::document::parse_header reads it. */
export function parseHeader(line: string): Omit<ContextBlock, 'body'> | null {
  const trimmed = line.trimEnd();
  if (!trimmed.startsWith(TURN_PREFIX) || !trimmed.endsWith(']]')) return null;
  const parts = trimmed.slice(TURN_PREFIX.length, -2).split(/\s+/).filter(Boolean);
  let role: string | undefined;
  let id: string | undefined;
  let turn: string | undefined;
  for (const part of parts) {
    if (part.startsWith('role=')) role = part.slice(5).toLowerCase();
    else if (part.startsWith('id=')) id = part.slice(3) || undefined;
    else if (turn === undefined && !part.includes('=')) turn = part;
  }
  if (!role || !/^[a-z0-9_-]+$/.test(role)) return null;
  return { turn, role, id };
}

/**
 * Text split at block headers. `lead` is what comes before the first header:
 * in a quoted part of the file (edit_file's old_text), the end of a block
 * whose header wasn't quoted.
 */
export function splitBlocks(text: string): { lead: string; blocks: ContextBlock[] } {
  const lead: string[] = [];
  const blocks: { header: Omit<ContextBlock, 'body'>; lines: string[] }[] = [];
  for (const line of text.replace(/\r\n/g, '\n').split('\n')) {
    if (line.startsWith(META_PREFIX)) continue;
    const header = line.startsWith(TURN_PREFIX) ? parseHeader(line) : null;
    if (header) blocks.push({ header, lines: [] });
    else if (blocks.length) blocks[blocks.length - 1].lines.push(line);
    else lead.push(line);
  }
  return {
    lead: lead.join('\n').trim(),
    blocks: blocks.map((b) => ({ ...b.header, body: b.lines.join('\n').trim() })),
  };
}

export function estimateTokens(text: string): number {
  return Math.ceil([...text].length / CHARS_PER_TOKEN);
}

/** "~1.2k tokens", "~40 tokens". */
export function formatTokens(tokens: number): string {
  const n = Math.abs(tokens);
  const shown =
    n >= 10_000 ? `${Math.round(n / 1000)}k` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`;
  return `~${shown} token${n === 1 ? '' : 's'}`;
}

export type BlockChangeKind = 'removed' | 'added' | 'edited' | 'unchanged' | 'partial' | 'written';

export interface BlockChange {
  kind: BlockChangeKind;
  /** The block's header after the edit, else before it. Absent for `partial`. */
  block?: Omit<ContextBlock, 'body'>;
  before: string;
  after: string;
}

export interface ContextEdit {
  changes: BlockChange[];
  /** Estimated tokens of what was replaced and of the replacement. */
  tokensBefore: number;
  tokensAfter: number;
}

const sameText = (a: string, b: string) =>
  a
    .split('\n')
    .map((l) => l.trimEnd())
    .join('\n')
    .trim() ===
  b
    .split('\n')
    .map((l) => l.trimEnd())
    .join('\n')
    .trim();

/**
 * What an edit_file call on the context file changes: its quoted text and
 * the replacement, matched block by block by id (the way the app reads the
 * edited file back). Text before the first header in either is part of a
 * block the quote starts inside.
 */
export function contextEdit(oldText: string, newText: string): ContextEdit {
  const a = splitBlocks(oldText);
  const b = splitBlocks(newText);
  const changes: BlockChange[] = [];

  if (a.lead || b.lead) {
    changes.push({
      kind: sameText(a.lead, b.lead) ? 'unchanged' : 'partial',
      before: a.lead,
      after: b.lead,
    });
  }

  // Blocks are matched by id; one without an id is removed or added.
  const byId = new Map<string, number[]>();
  a.blocks.forEach((blk, i) => {
    if (blk.id) byId.set(blk.id, [...(byId.get(blk.id) ?? []), i]);
  });
  const newIndexOf = new Map<number, number>();
  const fromNew: BlockChange[] = b.blocks.map((blk, i) => {
    const at = blk.id ? byId.get(blk.id)?.shift() : undefined;
    if (at === undefined) return { kind: 'added', block: blk, before: '', after: blk.body };
    newIndexOf.set(at, i);
    const old = a.blocks[at];
    return {
      kind: old.role === blk.role && sameText(old.body, blk.body) ? 'unchanged' : 'edited',
      block: blk,
      before: old.body,
      after: blk.body,
    };
  });

  // In the new order, a removed block right after the kept one it followed.
  const removedAfter = new Map<number, BlockChange[]>();
  let last = -1;
  a.blocks.forEach((old, i) => {
    const at = newIndexOf.get(i);
    if (at !== undefined) {
      last = at;
      return;
    }
    const removed: BlockChange = { kind: 'removed', block: old, before: old.body, after: '' };
    removedAfter.set(last, [...(removedAfter.get(last) ?? []), removed]);
  });
  changes.push(...(removedAfter.get(-1) ?? []));
  fromNew.forEach((c, i) => changes.push(c, ...(removedAfter.get(i) ?? [])));

  return {
    changes,
    tokensBefore: estimateTokens(oldText),
    tokensAfter: estimateTokens(newText),
  };
}

/** A write_file of the whole context file: every block it writes. */
export function contextWrite(content: string): ContextEdit {
  const { lead, blocks } = splitBlocks(content);
  const changes: BlockChange[] = blocks.map((blk) => ({
    kind: 'written',
    block: blk,
    before: '',
    after: blk.body,
  }));
  if (lead) changes.unshift({ kind: 'written', block: { role: 'notes' }, before: '', after: lead });
  return { changes, tokensBefore: 0, tokensAfter: estimateTokens(content) };
}

/** "2 removed · 1 edited", for the tool call's header. */
export function describeChanges(changes: BlockChange[]): string | undefined {
  const count = (kind: BlockChangeKind) => changes.filter((c) => c.kind === kind).length;
  const blocks = (n: number) => `${n} block${n === 1 ? '' : 's'}`;
  const written = count('written');
  if (written) return blocks(written);
  const kinds: [number, string][] = (
    [
      [count('removed'), 'removed'],
      [count('edited') + count('partial'), 'edited'],
      [count('added'), 'added'],
    ] as [number, string][]
  ).filter(([n]) => n > 0);
  if (!kinds.length) return undefined;
  // "2 blocks removed" when they all changed the same way.
  if (kinds.length === 1) return `${blocks(kinds[0][0])} ${kinds[0][1]}`;
  const total = kinds.reduce((sum, [n]) => sum + n, 0);
  return `${blocks(total)}: ${kinds.map(([n, what]) => `${n} ${what}`).join(', ')}`;
}
