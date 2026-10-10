// editorconfig.ts
//
// The .editorconfig properties that apply to a file (https://spec.editorconfig.org):
// the project's own indentation, line endings and whitespace rules, which
// take precedence over Settings → Editor. The backend hands over the
// .editorconfig files from the file's folder up to the filesystem root
// (read_editorconfigs); parsing and glob matching live here, with no Monaco
// import, so they can be tested on their own.

/** One .editorconfig file, as read_editorconfigs returns it. */
export interface EditorConfigFile {
  /** The folder it's in, forward slashes, no trailing slash. */
  dir: string;
  content: string;
}

/** The properties the editor acts on, after the spec's defaulting rules. */
export interface EditorConfigProps {
  indentStyle?: 'space' | 'tab';
  /** Columns per indentation level. */
  indentSize?: number;
  /** Columns a tab character takes. */
  tabWidth?: number;
  endOfLine?: 'lf' | 'crlf';
  insertFinalNewline?: boolean;
  trimTrailingWhitespace?: boolean;
}

interface Section {
  glob: string;
  props: Record<string, string>;
}

interface ParsedFile {
  root: boolean;
  sections: Section[];
}

export function parseEditorConfig(content: string): ParsedFile {
  const parsed: ParsedFile = { root: false, sections: [] };
  let current: Section | null = null;
  for (const raw of content.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith('#') || line.startsWith(';')) continue;
    if (line.startsWith('[') && line.endsWith(']')) {
      current = { glob: line.slice(1, -1), props: {} };
      parsed.sections.push(current);
      continue;
    }
    const eq = line.indexOf('=');
    if (eq < 0) continue;
    const key = line.slice(0, eq).trim().toLowerCase();
    const value = line.slice(eq + 1).trim();
    if (!key) continue;
    if (current) {
      current.props[key] = value.toLowerCase();
    } else if (key === 'root') {
      parsed.root = value.toLowerCase() === 'true';
    }
  }
  return parsed;
}

/** Index of the `}` closing the `{` at `open`, or -1. */
function closingBrace(glob: string, open: number): number {
  let depth = 0;
  for (let i = open; i < glob.length; i++) {
    const c = glob[i];
    if (c === '\\') i++;
    else if (c === '{') depth++;
    else if (c === '}' && --depth === 0) return i;
  }
  return -1;
}

/** Splits brace content on its top-level commas. */
function splitAlternatives(body: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (c === '\\') i++;
    else if (c === '{') depth++;
    else if (c === '}') depth--;
    else if (c === ',' && depth === 0) {
      parts.push(body.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(body.slice(start));
  return parts;
}

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');

/**
 * A glob as a regular expression body. Numeric ranges ({1..3}) become
 * capture groups, checked after the match (`ranges`, in group order).
 */
function globToRegex(glob: string, ranges: [number, number][]): string {
  let re = '';
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === '\\' && i + 1 < glob.length) {
      re += escapeRe(glob[++i]);
    } else if (c === '*') {
      if (glob[i + 1] === '*') {
        i++;
        // `**/` also matches no folder at all: a/**/b matches a/b.
        if (glob[i + 1] === '/') {
          i++;
          re += '(?:.*/)?';
        } else {
          re += '.*';
        }
      } else {
        re += '[^/]*';
      }
    } else if (c === '?') {
      re += '[^/]';
    } else if (c === '[') {
      const end = glob.indexOf(']', i + 1);
      if (end < 0) {
        re += '\\[';
        continue;
      }
      let body = glob.slice(i + 1, end);
      const negate = body.startsWith('!');
      if (negate) body = body.slice(1);
      re += `[${negate ? '^' : ''}${body.replace(/[\\\]^]/g, '\\$&')}]`;
      i = end;
    } else if (c === '{') {
      const end = closingBrace(glob, i);
      if (end < 0) {
        re += '\\{';
        continue;
      }
      const body = glob.slice(i + 1, end);
      const range = /^([+-]?\d+)\.\.([+-]?\d+)$/.exec(body);
      const alternatives = splitAlternatives(body);
      if (range) {
        ranges.push([Number(range[1]), Number(range[2])]);
        re += '([+-]?\\d+)';
      } else if (alternatives.length > 1) {
        re += `(?:${alternatives.map((a) => globToRegex(a, ranges)).join('|')})`;
      } else {
        // {single} is literal, per the spec.
        re += `\\{${globToRegex(body, ranges)}\\}`;
      }
      i = end;
    } else {
      re += escapeRe(c);
    }
  }
  return re;
}

/**
 * Whether a section glob matches `rel`, the file's path relative to the
 * .editorconfig's folder (forward slashes). A glob without a `/` matches the
 * file name at any depth; one with a `/` is anchored to that folder.
 */
export function globMatches(glob: string, rel: string): boolean {
  const anchored = glob.includes('/');
  const pattern = glob.startsWith('/') ? glob.slice(1) : glob;
  const ranges: [number, number][] = [];
  const body = globToRegex(pattern, ranges);
  let re: RegExp;
  try {
    re = new RegExp(`^${anchored ? '' : '(?:.*/)?'}${body}$`);
  } catch {
    return false;
  }
  const m = re.exec(rel);
  if (!m) return false;
  // Range groups come after any the alternation added, in order; they are
  // the only capturing groups, so they line up with `ranges`.
  return ranges.every(([lo, hi], k) => {
    const n = Number(m[k + 1]);
    return n >= Math.min(lo, hi) && n <= Math.max(lo, hi);
  });
}

function positiveInt(v: string | undefined): number | undefined {
  if (v === undefined || !/^\d+$/.test(v)) return undefined;
  const n = Number(v);
  return n > 0 ? n : undefined;
}

function bool(v: string | undefined): boolean | undefined {
  return v === 'true' ? true : v === 'false' ? false : undefined;
}

/**
 * The properties for `filePath` from `files` (nearest folder first, as
 * read_editorconfigs returns them). Stops at the first file marked
 * `root = true`; nearer files and later sections win.
 */
export function resolveEditorConfig(
  files: EditorConfigFile[],
  filePath: string
): EditorConfigProps {
  const path = filePath.replace(/\\/g, '/');
  const chain: { dir: string; parsed: ParsedFile }[] = [];
  for (const f of files) {
    const parsed = parseEditorConfig(f.content);
    chain.push({ dir: f.dir.replace(/\\/g, '/').replace(/\/+$/, ''), parsed });
    if (parsed.root) break;
  }

  const raw: Record<string, string> = {};
  for (const { dir, parsed } of chain.reverse()) {
    const prefix = `${dir}/`;
    if (!path.startsWith(prefix)) continue;
    const rel = path.slice(prefix.length);
    for (const section of parsed.sections) {
      if (!globMatches(section.glob, rel)) continue;
      Object.assign(raw, section.props);
    }
  }
  for (const [k, v] of Object.entries(raw)) if (v === 'unset') delete raw[k];

  const props: EditorConfigProps = {};
  if (raw.indent_style === 'space' || raw.indent_style === 'tab') {
    props.indentStyle = raw.indent_style;
  }
  const tabWidth = positiveInt(raw.tab_width);
  const indentSizeIsTab =
    raw.indent_size === 'tab' || (raw.indent_size === undefined && props.indentStyle === 'tab');
  const indentSize = indentSizeIsTab ? undefined : positiveInt(raw.indent_size);
  props.tabWidth = tabWidth ?? indentSize;
  props.indentSize = indentSizeIsTab ? tabWidth : indentSize;
  if (props.tabWidth === undefined) delete props.tabWidth;
  if (props.indentSize === undefined) delete props.indentSize;
  if (raw.end_of_line === 'lf' || raw.end_of_line === 'crlf') props.endOfLine = raw.end_of_line;
  const finalNewline = bool(raw.insert_final_newline);
  if (finalNewline !== undefined) props.insertFinalNewline = finalNewline;
  const trim = bool(raw.trim_trailing_whitespace);
  if (trim !== undefined) props.trimTrailingWhitespace = trim;
  return props;
}
