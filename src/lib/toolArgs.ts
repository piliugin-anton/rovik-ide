// toolArgs.ts
//
// A tool call's arguments and result, as the chat shows them: shared by a
// call's own block (ToolCallBlock) and a group of calls (ToolCallGroup).

export type Args = Record<string, unknown>;

/**
 * Arguments stream in as partial JSON, so a strict parse fails until the
 * last chunk lands. Fall back to pulling complete string/number fields out
 * with a regex so the header can say "Reading src/foo.ts" mid-stream.
 */
export function parseArgsLenient(raw: string): Args {
  const trimmed = raw.trim();
  if (!trimmed) return {};

  try {
    const parsed = JSON.parse(trimmed);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
  } catch {
    const out: Args = {};
    const fieldRe = /"(\w+)"\s*:\s*(?:"((?:[^"\\]|\\.)*)"|(-?\d+))/g;
    let m: RegExpExecArray | null;
    while ((m = fieldRe.exec(trimmed)) !== null) {
      const [, key, str, num] = m;
      if (str !== undefined) {
        try {
          out[key] = JSON.parse(`"${str}"`);
        } catch {
          out[key] = str;
        }
      } else if (num !== undefined) {
        out[key] = Number(num);
      }
    }
    // A string still arriving (a file's content as it's written): what
    // there is so far, so the header can count its lines while it streams.
    const open = /"(\w+)"\s*:\s*"((?:[^"\\]|\\.)*\\?)$/.exec(trimmed);
    if (open && !(open[1] in out)) {
      // Drop a half-received escape at the end ("\", "\u00").
      const raw = open[2].replace(/\\(u[0-9a-fA-F]{0,3})?$/, '');
      try {
        out[open[1]] = JSON.parse(`"${raw}"`);
      } catch {
        out[open[1]] = raw;
      }
    }
    return out;
  }
}

/** A tool result reporting a failure. */
export function isErrorResult(result?: string) {
  return result !== undefined && /^Error:/.test(result.trimStart());
}

/**
 * A directory as the model named it, for a tool call's line: inside the
 * project relative to it, without a leading "./" or a trailing separator,
 * and "project root" for the root itself: "", ".", "./", ".\\", or the
 * project's own absolute path. Windows paths compare ignoring case and
 * which slash they use.
 */
export function dirName(path: string | undefined, root?: string | null): string {
  let dir = (path?.trim() ?? '').replace(/(.)[\\/]+$/, '$1');
  if (root) {
    const key = (p: string) => {
      const forward = p.replace(/\\/g, '/').replace(/\/+$/, '');
      return /^[a-zA-Z]:/.test(forward) ? forward.toLowerCase() : forward;
    };
    const base = key(root);
    const target = key(dir);
    if (base && target === base) return 'project root';
    if (base && target.startsWith(`${base}/`))
      dir = dir.slice(root.replace(/[\\/]+$/, '').length + 1);
  }
  dir = dir.replace(/^(\.[\\/]+)+/, '');
  return dir === '' || dir === '.' ? 'project root' : dir;
}
