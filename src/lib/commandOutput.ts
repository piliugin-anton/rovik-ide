// commandOutput.ts
//
// Shapes command output for the model: what it needs to act on, in as few
// tokens as possible. Used for run_command results (lib/tools.ts) and for
// the verification gate's failure reports (lib/verification.ts).
//
//   - capOutput: long output keeps its start and end (errors and summaries
//     come last), whole lines only.
//   - focusFailures: a failing build or test run keeps its error blocks and
//     final summary, not thousands of lines of passing tests and progress.
//   - errorLocations: the files the errors point at, so a report can say
//     whether they're files the agent changed.
//
// Plain text heuristics that work across tsc, eslint, cargo, rustc, jest,
// vitest, pytest and go test, without parsing any one tool's format.

/** Color and cursor codes. Commands run with NO_COLOR, but not all obey. */
export function stripAnsi(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07/g, '');
}

/**
 * The start and the end of long output, whole lines. Errors and summaries
 * are usually at the end; the start shows what ran.
 */
export function capOutput(text: string, headLines = 15, tailLines = 120, maxChars = 8_000): string {
  const lines = text.split('\n');
  let kept =
    lines.length > headLines + tailLines
      ? [
          ...lines.slice(0, headLines),
          `[… ${lines.length - headLines - tailLines} lines omitted …]`,
          ...lines.slice(-tailLines),
        ]
      : lines;
  let out = kept.join('\n');
  // Very long lines: keep the end, where the summary is.
  if (out.length > maxChars) {
    kept = [];
    let size = 0;
    for (let i = lines.length - 1; i >= 0 && size + lines[i].length < maxChars; i--) {
      kept.unshift(lines[i]);
      size += lines[i].length + 1;
    }
    out = `[… output shortened to its last ${kept.length} lines …]\n${kept.join('\n')}`;
  }
  return out;
}

/**
 * A line that starts or marks a failure. Deliberately broad: a few extra
 * lines kept cost little; a missed error makes the report useless.
 */
const FAILURE_LINE =
  /\b(error|errors|fail|failed|failure|failures|failing|panicked|exception|traceback|assertion|assert)\b|^\s*(✗|✕|×|●|E\s{2,}|FAIL\b|---- .+ ----$|--- FAIL)|^\s*>\s+\d+\s*\|/i;

/** Lines that say it worked, which FAILURE_LINE's words also match ("0 failed"). */
const ALL_CLEAR = /\b0 (failed|failures|errors?)\b|\bno (errors|failures)\b/i;

/** Lines from the end that are always kept: the run's summary. */
const SUMMARY_LINES = 15;
/** How far an error block runs on after its first line (until a blank line). */
const BLOCK_LINES = 30;
/** Lines kept before an error line (the code it points at, a test name). */
const CONTEXT_BEFORE = 2;

/**
 * A failing run's output, cut down to its error blocks and its closing
 * summary. Short output is returned whole. Output with no recognizable
 * error lines falls back to its start and end (capOutput).
 */
export function focusFailures(text: string, maxChars = 8_000): string {
  const clean = stripAnsi(text).replace(/\r\n?/g, '\n').trimEnd();
  const lines = clean.split('\n');
  if (lines.length <= 80 && clean.length <= maxChars) return clean;

  // Which lines to keep: each failure line, a little before it, and the
  // block after it up to a blank line.
  const keep = new Uint8Array(lines.length);
  let blocks = 0;
  for (let i = 0; i < lines.length; i++) {
    if (!FAILURE_LINE.test(lines[i]) || ALL_CLEAR.test(lines[i])) continue;
    if (!keep[i]) blocks++;
    for (let j = Math.max(0, i - CONTEXT_BEFORE); j < i; j++) {
      if (lines[j].trim()) keep[j] = 1;
    }
    for (let j = i; j < lines.length && j < i + BLOCK_LINES; j++) {
      if (j > i && !lines[j].trim()) break;
      keep[j] = 1;
    }
  }
  if (blocks === 0) return capOutput(clean, 15, 120, maxChars);

  const summaryFrom = Math.max(0, lines.length - SUMMARY_LINES);
  for (let i = summaryFrom; i < lines.length; i++) keep[i] = 1;

  // Assemble: kept lines in order, gaps marked. The first errors usually
  // cause the rest, so over budget the later ones go, never the summary.
  const summary = lines.slice(summaryFrom).join('\n');
  const budget = maxChars - summary.length - 200;
  const out: string[] = [];
  let size = 0;
  let omitted = 0;
  let gapStart = -1;
  const closeGap = (at: number) => {
    if (gapStart < 0) return;
    const gap = at - gapStart;
    // A short gap costs less than its marker: keep its lines.
    if (gap <= 2) out.push(...lines.slice(gapStart, at));
    else out.push(`[… ${gap} lines …]`);
    gapStart = -1;
  };
  for (let i = 0; i < summaryFrom; i++) {
    if (!keep[i]) {
      if (gapStart < 0 && omitted === 0) gapStart = i;
      continue;
    }
    if (size > budget) {
      omitted++;
      continue;
    }
    closeGap(i);
    out.push(lines[i]);
    size += lines[i].length + 1;
  }
  if (omitted > 0) out.push(`[… ${omitted} more error lines omitted …]`);
  else closeGap(summaryFrom);
  out.push(summary);
  return out.join('\n');
}

/**
 * Source files the output points at: "src/a.ts:12:5", "src/a.ts(12,5)",
 * "--> src/main.rs:3:1", 'File "app/x.py", line 9'. Returned as written
 * (relative paths), each once, in order of appearance.
 */
export function errorLocations(text: string): string[] {
  const found = new Set<string>();
  const patterns = [
    // path:line[:col] and path(line,col)
    /(?:^|[\s"'(`[])((?:\.{1,2}\/|\/)?(?:[\w@.+-]+\/)*[\w@.+-]+\.[A-Za-z][A-Za-z0-9]{0,5})(?::(\d+)(?::\d+)?\b|\((\d+),\d+\))/gm,
    // Python tracebacks
    /File "([^"]+\.[A-Za-z0-9]{1,6})", line \d+/g,
  ];
  for (const re of patterns) {
    for (const m of stripAnsi(text).matchAll(re)) {
      const path = m[1].replace(/^\.\//, '');
      // Not source files: URLs, versions, node internals, minified bundles.
      if (/^(https?|node|file):|^\d+(\.\d+)+$|\/node_modules\/|\.min\.js$/.test(path)) continue;
      found.add(path);
    }
  }
  return [...found];
}

/**
 * Splits the files errors point at into ones the agent changed in this run
 * and others. Paths are compared by their tail: tools print them relative
 * to where they ran (cargo in src-tauri/ prints "src/x.rs" for
 * "src-tauri/src/x.rs").
 */
export function splitByChanged(
  locations: string[],
  changedFiles: string[]
): { changed: string[]; other: string[] } {
  const norm = (p: string) => p.replace(/\\/g, '/').replace(/^\.\//, '').replace(/^\/+/, '');
  const changed = changedFiles.map(norm);
  const isChanged = (loc: string) => {
    const l = norm(loc);
    return changed.some((c) => c === l || c.endsWith(`/${l}`) || l.endsWith(`/${c}`));
  };
  const result = { changed: [] as string[], other: [] as string[] };
  for (const loc of locations) (isChanged(loc) ? result.changed : result.other).push(loc);
  return result;
}
