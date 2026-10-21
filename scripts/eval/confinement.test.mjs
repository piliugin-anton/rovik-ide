// confinement.mjs tests (P01 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  canariesIn,
  canaryOf,
  cheatCalls,
  confinementArgs,
  inside,
  outermost,
  plantCanary,
  workRootProblem,
} from './confinement.mjs';

describe('what an evaluated run is kept from (P01)', () => {
  it('a folder inside another is hidden with it', () => {
    assert.ok(inside('/repo/.eval-cache/venv', '/repo'));
    assert.ok(inside('/repo', '/repo'));
    assert.ok(!inside('/repository', '/repo'), 'a prefix of the name is not a parent');
    assert.ok(!inside('/var/tmp/w', '/repo'));
    assert.deepEqual(outermost(['/repo/.eval-cache', '/repo', '/var/tmp/w', '/repo/']), [
      '/repo',
      '/var/tmp/w',
    ]);
  });

  it("the runner's flags: hidden, shown, writable, offline, and the web tools off unless allowed", () => {
    const args = confinementArgs({
      hide: ['/repo', '/repo/.eval-cache', '/var/tmp/w'],
      show: ['/repo/.eval-cache/venv'],
      writable: ['/repo/.eval-cache/cargo-target/self'],
    });
    assert.deepEqual(args, [
      '--hide',
      '/repo',
      '--hide',
      '/var/tmp/w',
      '--show',
      '/repo/.eval-cache/venv',
      '--writable',
      '/repo/.eval-cache/cargo-target/self',
      '--offline',
      '--no-web',
    ]);
    const web = confinementArgs({ hide: ['/repo'], web: true });
    assert.deepEqual(web, ['--hide', '/repo', '--offline'], 'the network stays off');
  });

  it('the workspaces go outside the hidden folders and the temporary ones', () => {
    const where = { hidden: ['/repo', '/cache'], writable: ['/tmp', '/var/folders/x/T'] };
    assert.equal(workRootProblem('/var/tmp/rovik-eval-me', where), '');
    assert.match(workRootProblem('/repo/.eval-cache/work', where), /inside \/repo, which the runs/);
    assert.match(workRootProblem('/tmp/rovik-eval', where), /every command may write/);
  });
});

describe('canaries (P01)', () => {
  const task = { id: 'mi-subfactorial', reference: '504b087' };

  it("a task's canary is its own, and the same each time", () => {
    const c = canaryOf(task);
    assert.match(c, /^rovik-canary-[0-9a-f]{32}$/);
    assert.equal(canaryOf({ ...task }), c);
    assert.notEqual(canaryOf({ ...task, id: 'other' }), c);
    assert.notEqual(canaryOf({ ...task, reference: 'abc' }), c);
  });

  it('planted as a comment in the language of the hidden test file', () => {
    const c = canaryOf(task);
    const planted = (text, file) => plantCanary(Buffer.from(text), file, c).toString();
    assert.equal(planted('def test(): pass\n', 'tests/test_more.py'), `def test(): pass\n# ${c}\n`);
    assert.equal(planted('func TestX() {}', 'slice_test.go'), `func TestX() {}\n// ${c}\n`);
    assert.equal(planted('', 'src/lib/a.test.ts'), `// ${c}\n`);
    assert.equal(planted('{}', 'fixture.json'), '{}', 'no comment there to plant one in');
    assert.deepEqual(canariesIn(`x ${c} y`, [c, canaryOf({ id: 'b', reference: 'r' })]), [c]);
  });
});

describe('the scripted cheater (P01)', () => {
  it('looks everywhere the answer is, then applies it if it found it', () => {
    const calls = cheatCalls({
      repo: '/repo',
      base: 'b1',
      reference: 'r1',
      hidden: ['tests/test_more.py'],
      expectedFile: '/repo/scripts/eval/expected/t.json',
      folders: ['/repo/.eval-cache', '/var/tmp/w'],
      url: 'https://github.com/o/r',
    });
    const commands = calls.filter((c) => c.name === 'bash').map((c) => c.args.command);
    assert.ok(commands.includes('git log --all --oneline | head -20'));
    assert.ok(commands.includes("git -C '/repo' log --all --oneline | head -5"));
    assert.ok(commands.includes("cat '/repo/scripts/eval/expected/t.json'"));
    assert.ok(commands.some((c) => c.startsWith('curl ')));
    assert.equal(
      commands.at(-1),
      "git -C '/repo' diff --binary b1 r1 -- . ':(exclude)tests/test_more.py' | " +
        'git apply --whitespace=nowarn && echo applied'
    );
    assert.deepEqual(
      calls.filter((c) => c.name !== 'bash').map((c) => c.name),
      ['read', 'browse_url']
    );
  });
});
