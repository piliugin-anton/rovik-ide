// markdownPreview.ts
//
// The rendered view of a Markdown file (components/MarkdownPreview.tsx):
// GitHub-flavored Markdown as HTML, each block carrying its source line
// (`data-line`, 0-based) so the preview and the editor scroll together and
// a click in one finds the spot in the other. Headings get GitHub's anchors,
// `> [!NOTE]` blockquotes become GitHub's alerts, and YAML front matter is
// folded away. Also: where a link or an image in the document points.
//
// The preview isn't a sandboxed frame: the document's HTML goes into the
// app's own page. sanitizePreviewHtml strips what could act on the app
// (scripts, forms, styles, navigation targets); images are never fetched
// by the browser, the preview loads local ones itself (no remote ones: the
// app's CSP allows none, and a document's image URL is a way to send data).
import {
  Lexer,
  Parser,
  Renderer,
  getDefaults,
  type MarkedOptions,
  type Token,
  type Tokens,
} from 'marked';
import DOMPurify, { type DOMPurify as Purifier } from 'dompurify';

export interface RenderedMarkdown {
  /** Not yet sanitized: show it only through sanitizePreviewHtml. */
  html: string;
  /** Each heading's anchor (without sanitizing's `user-content-` prefix) and line. */
  headings: { id: string; line: number }[];
}

/** A block's source line, set on the tokens before rendering. */
type Lined = { line?: number };

const lineOf = (token: object) => (token as Lined).line;
const lineAttr = (token: object) => {
  const line = lineOf(token);
  return line === undefined ? '' : ` data-line="${line}"`;
};
const newlines = (text: string) => {
  let n = 0;
  for (let i = text.indexOf('\n'); i !== -1; i = text.indexOf('\n', i + 1)) n++;
  return n;
};

/** `html` with `attrs` added to its first tag: `<h1>` → `<h1 data-line="3">`. */
function withAttrs(html: string, attrs: string): string {
  return attrs ? html.replace(/^<([a-z][a-z0-9-]*)/i, `<$1${attrs}`) : html;
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/*
 * Lines from the tokens' raw text: a document's top-level tokens add up to
 * it exactly, and so do a list's items. A list item's and a quote's own
 * blocks are counted from where the item or quote starts.
 */
function annotateLines(tokens: Token[], first: number) {
  let line = first;
  for (const token of tokens) {
    (token as Lined).line = line;
    if (token.type === 'list') {
      let itemLine = line;
      for (const item of (token as Tokens.List).items) {
        (item as Lined).line = itemLine;
        annotateLines(item.tokens, itemLine);
        itemLine += newlines(item.raw);
      }
    } else if (token.type === 'blockquote') {
      annotateLines((token as Tokens.Blockquote).tokens, line);
    }
    line += newlines(token.raw);
  }
}

/**
 * A heading's anchor as GitHub makes it: lowercase, punctuation dropped,
 * spaces as hyphens.
 */
export function headingSlug(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, '')
    .replace(/ /g, '-');
}

const ALERTS = {
  note: 'Note',
  tip: 'Tip',
  important: 'Important',
  warning: 'Warning',
  caution: 'Caution',
} as const;
type AlertKind = keyof typeof ALERTS;

/**
 * GitHub's alert syntax: a quote whose first line is only `[!NOTE]` (or TIP,
 * IMPORTANT, WARNING, CAUTION). Takes the marker out of the quote's tokens.
 */
function takeAlert(quote: Tokens.Blockquote): AlertKind | null {
  const first = quote.tokens[0];
  if (first?.type !== 'paragraph') return null;
  const paragraph = first as Tokens.Paragraph;
  const head = paragraph.tokens[0];
  if (head?.type !== 'text') return null;
  const text = head as Tokens.Text;
  const m = /^\[!(note|tip|important|warning|caution)\][ \t]*(?:\n|$)/i.exec(text.text);
  if (!m) return null;
  text.text = text.text.slice(m[0].length);
  if (text.text === '') paragraph.tokens.shift();
  if (paragraph.tokens.length === 0) quote.tokens.shift();
  return m[1].toLowerCase() as AlertKind;
}

class PreviewRenderer extends Renderer {
  readonly headings: { id: string; line: number }[] = [];
  /** Task checkboxes can be clicked (the document can be edited). */
  private readonly tasks: boolean;
  /** Slugs given out, with how many times each base slug was asked for. */
  private readonly occurrences = new Map<string, number>();

  constructor(tasks: boolean) {
    super();
    this.tasks = tasks;
  }

  /** A unique anchor: a repeated heading gets `-1`, `-2`, … as on GitHub. */
  private slug(text: string): string {
    const base = headingSlug(text);
    let slug = base;
    while (this.occurrences.has(slug)) {
      const n = (this.occurrences.get(base) ?? 0) + 1;
      this.occurrences.set(base, n);
      slug = `${base}-${n}`;
    }
    this.occurrences.set(slug, 0);
    return slug;
  }

  heading(token: Tokens.Heading): string {
    const text = this.parser
      .parseInline(token.tokens, this.parser.textRenderer)
      .replace(/<[^>]*>/g, '');
    const id = this.slug(text);
    const line = lineOf(token);
    if (line !== undefined) this.headings.push({ id, line });
    return withAttrs(super.heading(token), `${lineAttr(token)} id="${escapeHtml(id)}"`);
  }

  paragraph(token: Tokens.Paragraph): string {
    return withAttrs(super.paragraph(token), lineAttr(token));
  }

  code(token: Tokens.Code): string {
    return withAttrs(super.code(token), lineAttr(token));
  }

  blockquote(token: Tokens.Blockquote): string {
    const alert = takeAlert(token);
    if (!alert) return withAttrs(super.blockquote(token), lineAttr(token));
    return (
      `<div class="md-alert md-alert-${alert}"${lineAttr(token)}>\n` +
      `<p class="md-alert-title">${ALERTS[alert]}</p>\n${this.parser.parse(token.tokens)}</div>\n`
    );
  }

  list(token: Tokens.List): string {
    const tasks = token.items.some((item) => item.task) ? ' class="md-tasks"' : '';
    return withAttrs(super.list(token), lineAttr(token) + tasks);
  }

  listitem(item: Tokens.ListItem): string {
    return withAttrs(super.listitem(item), lineAttr(item) + (item.task ? ' class="md-task"' : ''));
  }

  checkbox({ checked }: Tokens.Checkbox): string {
    return `<input type="checkbox"${checked ? ' checked=""' : ''}${this.tasks ? '' : ' disabled=""'}> `;
  }

  // Wide tables scroll inside their own box instead of widening the page.
  table(token: Tokens.Table): string {
    return `<div class="md-table"${lineAttr(token)}>\n${super.table(token)}</div>\n`;
  }

  hr(token: Tokens.Hr): string {
    return withAttrs(super.hr(token), lineAttr(token));
  }

  html(token: Tokens.HTML | Tokens.Tag): string {
    const html = super.html(token);
    return token.block ? withAttrs(html, lineAttr(token)) : html;
  }
}

/**
 * Front matter (`---` YAML `---` at the very top, as Jekyll and most site
 * generators use): folded into a collapsed block, not rendered as Markdown.
 */
const FRONT_MATTER = /^---[ \t]*\n(?:[\s\S]*?\n)?(?:---|\.\.\.)[ \t]*(?:\n|$)/;

export function renderMarkdown(source: string, opts: { tasks?: boolean } = {}): RenderedMarkdown {
  const text = source.replace(/\r\n?/g, '\n');
  const frontMatter = FRONT_MATTER.exec(text)?.[0] ?? '';
  const options: MarkedOptions = { ...getDefaults(), gfm: true, breaks: false };
  const tokens = Lexer.lex(text.slice(frontMatter.length), options);
  annotateLines(tokens, newlines(frontMatter));
  const renderer = new PreviewRenderer(opts.tasks ?? false);
  let html = Parser.parse(tokens, { ...options, renderer });
  if (frontMatter) {
    const yaml = frontMatter.replace(/^---[ \t]*\n/, '').replace(/(?:---|\.\.\.)[ \t]*\n?$/, '');
    html =
      `<details class="md-front-matter" data-line="0"><summary>Front matter</summary>` +
      `<pre><code class="language-yaml">${escapeHtml(yaml)}</code></pre></details>\n` +
      html;
  }
  return { html, headings: renderer.headings };
}

/** The 0-based line of the heading `anchor` names (`#Some-Title` or `some-title`), if any. */
export function headingLine(source: string, anchor: string): number | null {
  const id = anchor.replace(/^#/, '');
  const { headings } = renderMarkdown(source);
  const found =
    headings.find((h) => h.id === id) ?? headings.find((h) => h.id === id.toLowerCase());
  return found ? found.line : null;
}

// ---------------------------------------------------------------------------
// Sanitizing
// ---------------------------------------------------------------------------

/** The prefix sanitizing puts on every `id` and `name` (as GitHub does). */
export const ANCHOR_PREFIX = 'user-content-';

let purifier: Purifier | null = null;

/*
 * Its own DOMPurify instance: the hooks below apply to the preview only,
 * not to the chat's formatted text (FormattedText.tsx), which shares the
 * default instance.
 */
function previewPurifier(): Purifier {
  if (purifier) return purifier;
  const p = DOMPurify(window);
  p.addHook('uponSanitizeAttribute', (_node, data) => {
    // Classes only from the renderer's own vocabulary: a document's HTML
    // can't put the app's classes (overlays, dialogs) on its elements.
    if (data.attrName === 'class') {
      data.attrValue = data.attrValue
        .split(/\s+/)
        .filter((c) => /^(md-|language-)/.test(c))
        .join(' ');
      data.keepAttr = data.attrValue !== '';
    }
  });
  p.addHook('afterSanitizeAttributes', (node) => {
    // The browser never fetches a document's images; the preview loads
    // local ones itself (data-src), remote ones not at all.
    if (node.tagName === 'IMG') {
      const src = node.getAttribute('src');
      if (src !== null && !/^data:image\//i.test(src)) {
        node.setAttribute('data-src', src);
        node.removeAttribute('src');
      }
    }
    // Only task checkboxes are for clicking; other inputs are just shown.
    if (node.tagName === 'INPUT' && node.getAttribute('type') !== 'checkbox') {
      node.setAttribute('disabled', '');
    }
  });
  purifier = p;
  return p;
}

/** The rendered document, safe to put into the app's page. */
export function sanitizePreviewHtml(html: string): string {
  return previewPurifier().sanitize(html, {
    // Prefixed with ANCHOR_PREFIX: a heading named "root" mustn't be #root.
    SANITIZE_NAMED_PROPS: true,
    FORBID_TAGS: [
      'style',
      'form',
      'button',
      'select',
      'option',
      'optgroup',
      'textarea',
      'datalist',
      'dialog',
      'template',
      'slot',
      'audio',
      'video',
      'source',
      'track',
      'picture',
      'map',
      'area',
      'marquee',
    ],
    FORBID_ATTR: [
      'style',
      'srcset',
      'background',
      'poster',
      'target',
      'download',
      'tabindex',
      'autofocus',
      'popover',
      'popovertarget',
      'popovertargetaction',
      'command',
      'commandfor',
    ],
  });
}

// ---------------------------------------------------------------------------
// Links and images
// ---------------------------------------------------------------------------

export type PreviewLink =
  /** A heading in this document. */
  | { kind: 'anchor'; id: string }
  /** Opens in the system browser. */
  | { kind: 'web'; url: string }
  /** A file: opens in a tab, at a heading or a line (`#L12`) if the link says. */
  | { kind: 'file'; path: string; anchor?: string; line?: number }
  | { kind: 'none' };

function decode(text: string): string {
  try {
    return decodeURIComponent(text);
  } catch {
    return text;
  }
}

/** A scheme (`https:`, `vscode:`), but not a Windows drive (`C:\`). */
const SCHEME = /^[a-z][a-z0-9+.-]*:/i;
const WINDOWS_DRIVE = /^[a-z]:[\\/]/i;

/**
 * Where a path in a document points, as on GitHub: relative to the
 * document's folder, or with a leading `/` to the project's root. A full
 * path into the project works too.
 */
export function resolveDocumentPath(target: string, documentPath: string, root: string | null) {
  const sep = documentPath.includes('\\') && !documentPath.includes('/') ? '\\' : '/';
  const slashes = (p: string) => p.replace(/[\\/]+/g, '/');
  const rel = slashes(target);
  const normalizedRoot = root ? slashes(root).replace(/\/$/, '') : null;
  let base: string;
  let rest: string;
  if (WINDOWS_DRIVE.test(rel)) {
    base = rel.slice(0, 2);
    rest = rel.slice(3);
  } else if (rel.startsWith('/')) {
    const inProject =
      normalizedRoot !== null && (rel === normalizedRoot || rel.startsWith(`${normalizedRoot}/`));
    base = normalizedRoot !== null && !inProject ? normalizedRoot : '';
    rest = rel.slice(1);
  } else {
    base = slashes(documentPath).replace(/\/[^/]*$/, '');
    rest = rel;
  }
  const parts = base.split('/');
  for (const segment of rest.split('/')) {
    if (segment === '' || segment === '.') continue;
    // Never above the root ('' or a drive).
    if (segment === '..') {
      if (parts.length > 1) parts.pop();
    } else parts.push(segment);
  }
  return parts.join(sep) || sep;
}

/** What a link in a document at `documentPath` does when clicked. */
export function resolvePreviewLink(
  href: string,
  documentPath: string,
  root: string | null
): PreviewLink {
  const link = href.trim();
  if (link === '') return { kind: 'none' };
  if (link.startsWith('#')) return { kind: 'anchor', id: decode(link.slice(1)) };
  if (/^(https?:|mailto:)/i.test(link)) return { kind: 'web', url: link };
  if (link.startsWith('//')) return { kind: 'web', url: `https:${link}` };
  if (SCHEME.test(link) && !WINDOWS_DRIVE.test(link)) return { kind: 'none' };
  const hash = link.indexOf('#');
  const pathPart = (hash === -1 ? link : link.slice(0, hash)).replace(/\?.*$/, '');
  if (pathPart === '') return { kind: 'none' };
  const fragment = hash === -1 ? '' : decode(link.slice(hash + 1));
  const path = resolveDocumentPath(decode(pathPart), documentPath, root);
  // GitHub's line links: #L12, #L12-L20 (and #L12C3).
  const lines = /^L(\d+)(?:C\d+)?(?:-L?\d+(?:C\d+)?)?$/.exec(fragment);
  if (lines) return { kind: 'file', path, line: Number(lines[1]) };
  return fragment ? { kind: 'file', path, anchor: fragment } : { kind: 'file', path };
}

export type ChatLink =
  { kind: 'web'; url: string } | { kind: 'file'; path: string; line?: number } | { kind: 'none' };

/**
 * What a link in a chat reply does when clicked: a web link opens in the
 * browser, a path is a file of the project (models link the files they
 * mention), opened in the editor, at its line for GitHub's `#L12`. From
 * the link as written: the page's own URL would make a path a web address
 * (http://tauri.localhost/… on Windows).
 */
export function resolveChatLink(href: string): ChatLink {
  const link = href.trim();
  if (link === '' || link.startsWith('#')) return { kind: 'none' };
  if (/^(https?:|mailto:)/i.test(link)) return { kind: 'web', url: link };
  if (link.startsWith('//')) return { kind: 'web', url: `https:${link}` };
  if (SCHEME.test(link) && !WINDOWS_DRIVE.test(link)) return { kind: 'none' };
  const hash = link.indexOf('#');
  const pathPart = (hash === -1 ? link : link.slice(0, hash)).replace(/\?.*$/, '');
  if (pathPart === '') return { kind: 'none' };
  const fragment = hash === -1 ? '' : decode(link.slice(hash + 1));
  const lines = /^L(\d+)/.exec(fragment);
  const path = decode(pathPart).replace(/^\.[\\/]/, '');
  return lines ? { kind: 'file', path, line: Number(lines[1]) } : { kind: 'file', path };
}

export type PreviewImage =
  | { kind: 'local'; path: string; type: string }
  | { kind: 'remote'; url: string }
  | { kind: 'none' };

/** Image types the preview shows, by extension (the backend's read_image_file reads these). */
const IMAGE_TYPES: Record<string, string> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  avif: 'image/avif',
  svg: 'image/svg+xml',
  bmp: 'image/bmp',
  ico: 'image/x-icon',
};

/** Where an image in a document comes from (its src, as data-src after sanitizing). */
export function resolvePreviewImage(
  src: string,
  documentPath: string,
  root: string | null
): PreviewImage {
  const link = src.trim();
  if (/^https?:/i.test(link)) return { kind: 'remote', url: link };
  if (link.startsWith('//')) return { kind: 'remote', url: `https:${link}` };
  if (link === '' || (SCHEME.test(link) && !WINDOWS_DRIVE.test(link))) return { kind: 'none' };
  const pathPart = link.replace(/[?#].*$/, '');
  const ext = /\.([a-z0-9]+)$/i.exec(pathPart)?.[1].toLowerCase() ?? '';
  const type = IMAGE_TYPES[ext];
  if (!type) return { kind: 'none' };
  return { kind: 'local', path: resolveDocumentPath(decode(pathPart), documentPath, root), type };
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/**
 * A task list item's line with its box ticked or cleared (`- [ ] x` ⇄
 * `- [x] x`); null when the line has no task box.
 */
export function toggleTaskLine(line: string): string | null {
  const m = /^(\s*(?:>\s*)*(?:[-*+]|\d{1,9}[.)])\s+\[)([ xX])(\])/.exec(line);
  if (!m) return null;
  const next = m[2] === ' ' ? 'x' : ' ';
  return m[1] + next + m[3] + line.slice(m[0].length);
}
