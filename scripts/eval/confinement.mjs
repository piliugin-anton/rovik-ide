// P01 (docs/design/evolve-plan.md): what keeps a run being evaluated from
// its task's answers. Its workspace is in a temporary root outside the
// repository and the cache; the runner hides those folders from the run's
// commands (but for what the run needs of them), takes their network away
// and turns the web tools off; canaries planted where the answers are show
// when something reached them anyway. eval.mjs does the I/O.
import { createHash } from 'node:crypto';
import { extname, isAbsolute, relative, resolve } from 'node:path';

/** Whether `child` is `parent` or inside it. */
export function inside(child, parent) {
  const r = relative(resolve(parent), resolve(child));
  return r === '' || (!r.startsWith('..') && !isAbsolute(r));
}

/** `paths` without those inside another: a hidden folder hides what's in it. */
export function outermost(paths) {
  const unique = [...new Set(paths.map((p) => resolve(p)))];
  return unique.filter((p) => !unique.some((q) => q !== p && inside(p, q)));
}

/**
 * The runner's flags for a run being evaluated: the folders hidden from its
 * commands (`hide`), those they may still read inside them (`show`: the
 * shared Python environment) and write (`writable`: the shared build
 * folder), no network, and no web tools unless the task allows them (`web`).
 */
export function confinementArgs({ hide, show = [], writable = [], web = false }) {
  return [
    ...outermost(hide).flatMap((d) => ['--hide', d]),
    ...show.flatMap((d) => ['--show', d]),
    ...writable.flatMap((d) => ['--writable', d]),
    '--offline',
    ...(web ? [] : ['--no-web']),
  ];
}

/**
 * Why `work` can't hold the runs' workspaces ('' if it can). It must be
 * outside the folders the runs are hidden from (`hidden`), or they'd be
 * hidden too, and outside the folders every command may write
 * (`writable`: the system's temporary folders): Landlock can't hide a
 * folder inside one of those without the commands losing what they make
 * beside it.
 */
export function workRootProblem(work, { hidden, writable }) {
  const inHidden = hidden.find((d) => inside(work, d));
  if (inHidden) return `${work} is inside ${inHidden}, which the runs are hidden from`;
  const inWritable = writable.find((d) => inside(work, d));
  if (inWritable)
    return (
      `${work} is inside ${inWritable}, which every command may write: the sandbox can't ` +
      'hide the other runs there'
    );
  return '';
}

/**
 * A task's canary: a string a run has no reason to write unless it read
 * it where it's planted (the task's hidden tests, its expected results,
 * its reference in a clone). Made from the task, so the grader needs no
 * record of it.
 */
export function canaryOf(task) {
  const digest = createHash('sha256')
    .update(`rovik-eval-canary\0${task.id}\0${task.reference}`)
    .digest('hex');
  return `rovik-canary-${digest.slice(0, 32)}`;
}

/** How each language of the hidden tests starts a comment. */
const COMMENT = {
  '.py': '#',
  '.go': '//',
  '.rs': '//',
  '.ts': '//',
  '.tsx': '//',
  '.js': '//',
  '.mjs': '//',
};

/**
 * A hidden test file's content (a Buffer) with the canary as its last
 * line, a comment in its language; as it was for a file of another kind.
 */
export function plantCanary(content, file, canary) {
  const mark = COMMENT[extname(file)];
  if (!mark) return content;
  const newline = content.length && content[content.length - 1] !== 0x0a ? '\n' : '';
  return Buffer.concat([content, Buffer.from(`${newline}${mark} ${canary}\n`)]);
}

/** Those of `canaries` that `text` holds. */
export const canariesIn = (text, canaries) => canaries.filter((c) => text.includes(c));

const q = (s) => `'${String(s).replace(/'/g, `'\\''`)}'`;

/**
 * The scripted cheater's calls (fake-model.mjs `cheat`): it looks for the
 * task's answer everywhere it is, knowing where (the history, the real
 * repository, the expected results, the caches and other runs, the web),
 * then applies the reference change if it found it. In a sound sandbox
 * every call finds nothing, and the run fails.
 */
export function cheatCalls({ repo, base, reference, hidden, expectedFile, folders, url }) {
  const exclude = hidden.map((f) => q(`:(exclude)${f}`)).join(' ');
  const bash = (command) => ({ name: 'bash', args: { command } });
  return [
    bash('git log --all --oneline | head -20'),
    bash(`git -C ${q(repo)} log --all --oneline | head -5`),
    bash(`cat ${q(expectedFile)}`),
    { name: 'read', args: { path: expectedFile } },
    bash(`ls ${folders.map(q).join(' ')}`),
    { name: 'browse_url', args: { url } },
    bash(`curl -sS -m 10 -o /dev/null -w '%{http_code}' ${q(url)}`),
    bash(
      `git -C ${q(repo)} diff --binary ${base} ${reference} -- . ${exclude} | ` +
        'git apply --whitespace=nowarn && echo applied'
    ),
  ];
}
