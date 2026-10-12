// A node:test reporter printing one JSON line per test:
// {"test":"<file> > <suite> > <name>","status":"pass|fail|skip"}.
// ts-test.mjs passes the bundles' source files in ROVIK_EVAL_TS_FILES.
const sources = JSON.parse(process.env.ROVIK_EVAL_TS_FILES ?? '{}');

export default async function* reporter(source) {
  // Each file's open suites, by nesting level.
  const stacks = new Map();
  for await (const { type, data } of source) {
    if (!data || data.file === undefined) continue;
    const stack = stacks.get(data.file) ?? [];
    stacks.set(data.file, stack);
    if (type === 'test:start') {
      stack.length = data.nesting;
      stack.push(data.name);
      continue;
    }
    if (type !== 'test:pass' && type !== 'test:fail') continue;
    if (data.details?.type === 'suite') continue;
    const file = sources[data.file] ?? data.file;
    const name = [file, ...stack.slice(0, data.nesting), data.name].join(' > ');
    const status = data.skip || data.todo ? 'skip' : type === 'test:pass' ? 'pass' : 'fail';
    yield JSON.stringify({ test: name, status }) + '\n';
  }
}
