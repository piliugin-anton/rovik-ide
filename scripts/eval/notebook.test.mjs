// notebook.mjs tests (N01 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  CUT_CLASSES,
  EDIT_MISS_CLASSES,
  VERDICTS,
  appDataDir,
  campaignRuns,
  chatNotes,
  chatRuns,
  clip,
  disagreement,
  evaluatedRun,
  fenced,
  findCampaigns,
  linkedKeys,
  notebook,
  notebookMarkdown,
  readChatLog,
  toolTally,
  transcriptMarkdown,
  verifyRounds,
} from './notebook.mjs';

const CORE = join(dirname(fileURLToPath(import.meta.url)), '../../src-tauri/crates/rovik-core/src');

/** A `const NAME: &[&str] = &[...]` of the core's source, as strings. */
function rustStrings(file, name) {
  const text = readFileSync(join(CORE, file), 'utf8');
  const list = text.match(new RegExp(`const ${name}: &\\[&str\\] = &\\[([^\\]]*)\\]`))?.[1];
  assert.ok(list, `${name} in ${file}`);
  return [...list.matchAll(/"([^"]*)"/g)].map((m) => m[1]);
}

const tool = (name, over = {}) => ({
  event: 'tool',
  dialogId: 'lead',
  tool: name,
  failed: false,
  error: null,
  ...over,
});
const failedTool = (name, error, over = {}) => tool(name, { failed: true, error, ...over });
const verifyLine = (round, status, progress, over = {}) => ({
  event: 'verify',
  dialogId: 'lead',
  round,
  status,
  progress,
  newFailures: 0,
  fixed: 0,
  integrityFlags: 0,
  checks: [
    { kind: 'test', command: 'go test ./...', status, exitCode: status === 'passed' ? 0 : 1 },
  ],
  ...over,
});
const identity = (settingsHash = 's') => ({ runnerHash: 'r', settingsHash, policyHash: 'p' });
const result = (over = {}) => ({
  success: true,
  outcome: 'done',
  tampered: [],
  review: [],
  verification: null,
  grade: { success: true, required: 3, passed: 3, failed: [], missing: [] },
  costUsd: 0.01,
  tokens: 1000,
  toolCalls: 0,
  toolErrors: 0,
  toolErrorClasses: {},
  editMisses: 0,
  identity: identity(),
  ...over,
});
const failing = (over = {}) =>
  result({
    success: false,
    grade: { success: false, required: 3, passed: 1, failed: ['TestA', 'TestB'], missing: [] },
    ...over,
  });
const run = (campaign, task, rep, r = result(), lines = [], over = {}) =>
  evaluatedRun({
    campaign,
    task,
    rep,
    result: r,
    report: { dialogId: 'lead', model: 'm', runLog: lines },
    ...over,
  });

describe('the notebook (N01)', () => {
  it("counts what the runner counts, by the core's own classes", () => {
    assert.deepEqual(CUT_CLASSES, rustStrings('agent_run/tools.rs', 'CUT_CLASSES'));
    assert.deepEqual(EDIT_MISS_CLASSES, rustStrings('agent_run/tools.rs', 'EDIT_MISS_CLASSES'));
    const repair = readFileSync(join(CORE, 'agent_run/repair.rs'), 'utf8');
    const variants = repair
      .match(/pub enum Verdict \{([\s\S]*?)\n\}/)[1]
      .split('\n')
      .map((l) => l.trim().match(/^([A-Z]\w*),$/)?.[1])
      .filter(Boolean)
      .map((v) => v.replace(/([a-z])([A-Z])/g, '$1-$2').toLowerCase());
    assert.deepEqual(VERDICTS, variants);

    const t = toolTally([
      tool('read'),
      failedTool('write', 'old_text not found'),
      failedTool('write', 'old_text not unique'),
      failedTool('bash', 'stopped'), // the run's, not the model's
      failedTool('read', null), // no class: "other"
      tool('bash', { exitCode: 0 }),
      tool('bash', { exitCode: 2 }),
      tool('bash', { exitCode: null, timedOut: true }),
      tool('read', { role: 'sub', parentDialogId: 'lead', dialogId: 'sub-1' }),
      { event: 'reply', dialogId: 'lead' },
    ]);
    assert.equal(t.calls, 9);
    assert.equal(t.errors, 3);
    assert.deepEqual(t.classes, { 'old_text not found': 1, 'old_text not unique': 1, other: 1 });
    assert.equal(t.editMisses, 2);
    assert.deepEqual(t.tools.write, { calls: 2, errors: 2 });
    assert.deepEqual(t.tools.bash, { calls: 4, errors: 0 });
    assert.deepEqual([t.commands, t.commandsFailed, t.timedOut], [3, 2, 1]);
  });

  it("reads the lead's rounds of the checks and their verdicts", () => {
    const rounds = verifyRounds(
      [
        verifyLine(1, 'failed', 'first', { newFailures: 2 }),
        verifyLine(1, 'passed', 'first', { dialogId: 'worker' }),
        verifyLine(2, 'rolled-back', 'regression'),
        verifyLine(3, 'passed', 'progress', { fixed: 2 }),
      ],
      'lead'
    );
    assert.deepEqual(
      rounds.map((r) => [r.round, r.status, r.verdict]),
      [
        [1, 'failed', 'first'],
        [2, 'rolled-back', 'regression'],
        [3, 'passed', 'progress'],
      ]
    );
    assert.deepEqual(rounds[0].failing, ['test']);
    assert.equal(rounds[0].newFailures, 2);
    assert.equal(rounds[2].fixed, 2);
  });

  it('reads an evaluated run from its result and its report', () => {
    const r = run('c', 't', 1, failing({ verification: 'passed' }), [
      failedTool('write', 'old_text not found'),
    ]);
    assert.equal(r.key, 'c/t/1');
    assert.equal(r.link, 'transcripts/c/t/1.md');
    assert.equal(r.success, false);
    assert.equal(r.checks, 'passed');
    assert.deepEqual(r.tests.failed, ['TestA', 'TestB']);
    assert.equal(r.tally.editMisses, 1);
    assert.equal(r.configuration, 'r:s:p');
    assert.equal(r.scripted, false);
    // No report (the runner failed): its result's counts.
    const bare = evaluatedRun({
      campaign: 'c',
      task: 't',
      rep: 2,
      result: failing({ toolCalls: 4, toolErrors: 1, toolErrorClasses: { 'file changed': 1 } }),
    });
    assert.deepEqual([bare.tally.calls, bare.tally.errors], [4, 1]);
    assert.deepEqual(bare.tally.classes, { 'file changed': 1 });
    assert.deepEqual(bare.rounds, []);
    const scripted = evaluatedRun({
      campaign: 'c',
      task: 't',
      rep: 3,
      result: result(),
      report: { model: 'fake-model' },
    });
    assert.equal(scripted.scripted, true);
  });

  it('finds the campaigns in a folder, and their runs and attempts', () => {
    const root = mkdtempSync(join(tmpdir(), 'notebook-'));
    try {
      const put = (path, value) => {
        mkdirSync(dirname(join(root, path)), { recursive: true });
        writeFileSync(join(root, path), JSON.stringify(value));
      };
      put('race/current/t1/1/result.json', result());
      put('race/current/t1/1/report.json', {
        dialogId: 'lead',
        model: 'm',
        runLog: [tool('read')],
      });
      put('race/current/t1/2/result.json', failing());
      put('race/cheap/t1/1/result.json', failing());
      // P06's arm: two attempts, the second picked by the checks.
      put('race/arm/t1/1/result.json', { ...result(), picked: 1 });
      put('race/arm/t1/1/attempt-1/result.json', failing());
      put('race/arm/t1/1/attempt-2/result.json', result());
      // Not finished, and not a campaign.
      mkdirSync(join(root, 'race/cheap/t2/1'), { recursive: true });
      put('race/notes/readme.json', {});
      const found = findCampaigns(join(root, 'race')).map((d) => d.slice(root.length + 1));
      assert.deepEqual(found, ['race/arm', 'race/cheap', 'race/current']);
      assert.deepEqual(findCampaigns(join(root, 'race'), { skip: [join(root, 'race')] }), []);

      const current = campaignRuns(join(root, 'race/current'), 'race/current');
      assert.deepEqual(
        current.map((r) => [r.task, r.rep, r.success, r.tally.calls]),
        [
          ['t1', 1, true, 1],
          ['t1', 2, false, 0],
        ]
      );
      const arm = campaignRuns(join(root, 'race/arm'), 'race/arm');
      assert.deepEqual(
        arm.map((r) => [r.key, r.attempt, r.picked, r.success]),
        [
          ['race/arm/t1/1-1', 1, false, false],
          ['race/arm/t1/1-2', 2, true, true],
        ]
      );
      assert.deepEqual(campaignRuns(join(root, 'race/cheap'), 'race/cheap').length, 1);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("compares each folder's campaigns with its current, and names the regressions", () => {
    const group = 'race';
    const current = {
      name: 'race/current',
      group,
      runs: [
        run('race/current', 'a', 1),
        run('race/current', 'a', 2),
        run('race/current', 'b', 1, failing()),
      ],
    };
    const cheap = {
      name: 'race/cheap',
      group,
      runs: [
        run('race/cheap', 'a', 1, failing({ identity: identity('t') })),
        run('race/cheap', 'a', 2, failing({ identity: identity('t') })),
        run('race/cheap', 'b', 1, result({ identity: identity('t') })),
      ],
    };
    // The same runner, settings and policy as current: chance, not a regression.
    const again = { name: 'race/again', group, runs: [run('race/again', 'a', 1, failing())] };
    const other = {
      name: 'pilot',
      group: '',
      runs: [run('pilot', 'a', 1, failing({ identity: identity('u') }))],
    };
    const data = notebook([current, cheap, again, other]);
    assert.equal(data.regressions.length, 2, 'pilot has no reference in its folder');
    const [vsCheap, vsAgain] = data.regressions;
    assert.equal(vsCheap.campaign, 'race/cheap');
    assert.equal(vsCheap.shared, 2);
    assert.deepEqual(
      vsCheap.regressions.map((x) => [x.task, x.before, x.after]),
      [['a', { passed: 2, runs: 2 }, { passed: 0, runs: 2 }]]
    );
    assert.deepEqual(
      vsCheap.regressions[0].failed.map((r) => r.key),
      ['race/cheap/a/1', 'race/cheap/a/2']
    );
    assert.deepEqual(
      vsCheap.regressions[0].passedBefore.map((r) => r.key),
      ['race/current/a/1']
    );
    assert.deepEqual(
      vsCheap.fixes.map((x) => x.task),
      ['b']
    );
    assert.equal(vsAgain.sameConfiguration, true);
    assert.deepEqual(vsAgain.regressions, []);

    // --against: every campaign against the one named, across folders.
    const against = notebook([current, cheap, other], { against: 'pilot' }).regressions;
    assert.deepEqual(
      against.map((g) => g.campaign),
      ['race/current', 'race/cheap']
    );
    assert.throws(() => notebook([current], { against: 'nope' }), /no such campaign/);

    const md = notebookMarkdown(data);
    assert.match(md, /### cheap against current\n\n_In race; 2 tasks in both\._/);
    assert.match(md, /\| a \| 2\/2 \| 0\/2 \| \[1\]\(transcripts\/race\/cheap\/a\/1\.md\) \[2\]/);
    assert.match(md, /Passed more often: b \(0\/1 → 1\/1\)/);
    assert.match(
      md,
      /### again against current\n\n_In race; 1 task in both\. The same runner, settings and policy/
    );
  });

  it('scores an arm by the attempt its checks picked, and counts every attempt in the failures', () => {
    const arm = {
      name: 'arm',
      group: '',
      runs: [
        run('arm', 't', 1, failing(), [failedTool('write', 'old_text not found')], {
          attempt: 1,
          picked: false,
        }),
        run('arm', 't', 1, result(), [], { attempt: 2, picked: true }),
      ],
    };
    const [c] = notebook([arm]).campaigns;
    assert.deepEqual([c.runs, c.agentRuns, c.passed, c.editMisses], [1, 2, 1, 1]);
    assert.deepEqual(
      c.tasks[0].failed.map((r) => r.key),
      ['arm/t/1-1']
    );
    assert.match(notebookMarkdown(notebook([arm])), /\| arm \| 2 \(1 repetitions\) \| 1 \(100%\)/);
  });

  it('finds where the checks and the hidden tests disagree', () => {
    const fooled = run('c', 't', 1, failing({ verification: 'passed' }));
    const tooStrict = run('c', 't', 2, result({ verification: 'gave-up' }));
    const tampered = run(
      'c',
      't',
      3,
      failing({ verification: 'passed', tampered: ['it holds its canary'] })
    );
    const agree = run('c', 't', 4, result({ verification: 'passed' }));
    assert.equal(disagreement(fooled), 'the checks passed; the hidden tests failed');
    assert.equal(disagreement(tooStrict), 'the checks failed; the hidden tests passed');
    assert.equal(disagreement(tampered), null, 'listed as tampered');
    assert.equal(disagreement(agree), null);
    const data = notebook([{ name: 'c', group: '', runs: [fooled, tooStrict, tampered, agree] }]);
    assert.deepEqual(
      data.disagreements.map((d) => d.key),
      ['c/t/1', 'c/t/2']
    );
    assert.equal(data.disagreements[0].tests, '1/3; failed: TestA, TestB');
    assert.deepEqual(
      data.tampered.map((d) => [d.key, d.why]),
      [['c/t/3', ['it holds its canary']]]
    );
    assert.deepEqual([...linkedKeys(data)].sort(), ['c/t/1', 'c/t/2', 'c/t/3']);
  });

  it("notes a campaign's tasks: classes, the checks' verdicts, the tests that fail", () => {
    const lines = [
      failedTool('write', 'old_text not found'),
      verifyLine(1, 'failed', 'first'),
      verifyLine(2, 'failed', 'regression'),
      verifyLine(3, 'gave-up', 'no-progress'),
    ];
    const c = {
      name: 'base',
      group: '',
      runs: [
        run('base', 'hard', 1, failing({ verification: 'gave-up' }), lines),
        run(
          'base',
          'hard',
          2,
          failing({ grade: { required: 3, passed: 0, failed: [], missing: ['A', 'B', 'C'] } })
        ),
        run('base', 'easy', 1),
      ],
    };
    const data = notebook([c]);
    const [hard, easy] = data.campaigns[0].tasks;
    assert.equal(hard.task, 'hard', 'the worst first');
    assert.deepEqual([hard.passed, hard.runs, hard.rounds], [0, 2, 3]);
    assert.deepEqual(hard.verdicts, { first: 1, regression: 1, 'no-progress': 1 });
    assert.deepEqual(hard.endings, { 'gave-up': 1 });
    assert.deepEqual(hard.failingTests, [
      { name: 'TestA', runs: 1 },
      { name: 'TestB', runs: 1 },
    ]);
    assert.deepEqual(hard.notRun, { runs: 1, most: 3 });
    assert.equal(easy.passed, 1);
    const md = notebookMarkdown(data);
    assert.match(
      md,
      /\| hard \| 0\/2 \| old_text not found 1 \| 1 \| 3 rounds \(regression 1, no progress 1\); ended gave-up 1 \| failed: TestA \(1\/2\), TestB \(1\/2\); up to 3 didn't run \(1\/2\) \| \[1\]\(transcripts\/base\/hard\/1\.md\) \[2\]/
    );
    assert.match(md, /1 other task passed every run, with no failed tool call\./);
    assert.match(
      md,
      /- The project's checks: 3 rounds in 1 run \(failed 2, gave-up 1\); against the round before: regression 1, no progress 1, first 1/
    );
    assert.match(md, /\| old_text not found \| 1 \| 1 \| 1 \| 1 \|/);
    assert.doesNotMatch(md, /scripted model/, 'a real model');
  });

  it('says when every campaign ran on the scripted model', () => {
    const fake = evaluatedRun({
      campaign: 'dry',
      task: 't',
      rep: 1,
      result: result(),
      report: { model: 'fake-model' },
    });
    const md = notebookMarkdown(notebook([{ name: 'dry', group: '', runs: [fake] }]));
    assert.match(md, /^# Notebook\n\n> \*\*Every campaign here ran on the scripted model/);
    assert.match(md, /\| dry \(scripted\) \|/);
  });

  it('keeps links and table cells whole', () => {
    const odd = run(
      'my runs/a(1)',
      't',
      1,
      failing({ grade: { required: 1, passed: 0, failed: ['x|y'], missing: [] } })
    );
    const md = notebookMarkdown(
      notebook([{ name: 'my runs/a(1)', group: 'my runs', runs: [odd] }])
    );
    assert.match(md, /\[1\]\(transcripts\/my%20runs\/a%281%29\/t\/1\.md\)/);
    assert.match(md, /failed: x\\\|y/);
  });

  it("splits ordinary chats' log into runs, a subagent's lines in its lead's", () => {
    const at = (m) => `2026-10-0${m}T10:00:00.000Z`;
    const lines = [
      { ...tool('read'), dialogId: 'chat-1', t: at(1) },
      { event: 'run', dialogId: 'chat-1', model: 'm1', outcome: 'done', t: at(1) },
      { ...failedTool('write', 'old_text not found'), dialogId: 'chat-2', t: at(2) },
      // A subagent of chat-2: its tool lines name their lead; its own run line doesn't end chat-2's.
      {
        ...failedTool('bash', 'timed out'),
        dialogId: 'sub-1',
        role: 'sub',
        parentDialogId: 'chat-2',
        t: at(2),
      },
      { event: 'run', dialogId: 'sub-1', model: 'small', outcome: 'done', t: at(2) },
      { ...verifyLine(1, 'failed', 'first'), dialogId: 'chat-2', t: at(3) },
      { ...verifyLine(2, 'gave-up', 'regression'), dialogId: 'chat-2', t: at(3) },
      { event: 'run', dialogId: 'chat-2', model: 'm2', outcome: 'verify-gave-up', t: at(3) },
      { event: 'run', dialogId: 'chat-1', model: 'm1', outcome: 'verify-needs-approval', t: at(4) },
      // The app closed mid-run.
      { ...tool('read'), dialogId: 'chat-3', t: at(5) },
      { event: 'reply', dialogId: 'chat-3', model: 'm3', t: at(5) },
    ];
    const runs = chatRuns(lines);
    assert.deepEqual(
      runs.map((r) => [r.dialogId, r.outcome, r.tally.calls, r.rounds.length]),
      [
        ['chat-1', 'done', 1, 0],
        ['chat-2', 'verify-gave-up', 2, 2],
        ['chat-1', 'verify-needs-approval', 0, 0],
        ['chat-3', 'no end in the log', 1, 0],
      ]
    );
    assert.equal(runs[1].model, 'm2');
    assert.equal(runs[3].model, 'm3');
    assert.deepEqual(runs[1].tally.classes, { 'old_text not found': 1, 'timed out': 1 });
    assert.deepEqual(
      chatRuns(lines, { since: '2026-10-03' }).map((r) => r.outcome),
      ['verify-gave-up', 'verify-needs-approval', 'no end in the log']
    );

    const notes = chatNotes(runs, { source: '/data/rovik-ide' });
    assert.deepEqual([notes.runs, notes.chats, notes.editMisses], [4, 3, 1]);
    assert.deepEqual(
      notes.toLookAt.map((r) => [r.dialogId, r.outcome]),
      [
        ['chat-3', 'no end in the log'],
        ['chat-2', 'verify-gave-up'],
      ],
      'the newest first; done and waiting on an approval left out'
    );
    assert.deepEqual(notes.toLookAt[1].verdicts, { first: 1, regression: 1 });
    const md = notebookMarkdown(notebook([], { chats: notes }));
    assert.match(
      md,
      /## Ordinary chats\n\nFrom the app's agent log in `\/data\/rovik-ide`, opted in with `--chats`/
    );
    assert.match(md, /4 runs in 3 chats, 2026-10-01 10:00 to 2026-10-05 10:00 \(UTC\)/);
    assert.match(
      md,
      /\| 2026-10-03 10:00 \| chat-2 \| m2 \| verify-gave-up \| 2 rounds \(regression 1, first 1\); ended gave-up \| old_text not found 1, timed out 1 \|/
    );
    assert.doesNotMatch(md, /## Per campaign/);
  });

  it("reads the app's agent log, the rotated file first, and finds its folder", () => {
    const dir = mkdtempSync(join(tmpdir(), 'agent-log-'));
    try {
      assert.throws(() => readChatLog(dir), /no agent log/);
      writeFileSync(join(dir, 'agent-log.1.jsonl'), '{"event":"run","n":1}\n');
      writeFileSync(join(dir, 'agent-log.jsonl'), '{"event":"run","n":2}\n{"event":"to\n');
      assert.deepEqual(
        readChatLog(dir).map((l) => l.n),
        [1, 2]
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
    const home = '/home/u';
    assert.equal(
      appDataDir({ platform: 'linux', env: {}, home }),
      join(home, '.local/share/rovik-ide')
    );
    assert.equal(
      appDataDir({ platform: 'linux', env: { XDG_DATA_HOME: '/x' }, home }),
      join('/x', 'rovik-ide')
    );
    assert.equal(
      appDataDir({ platform: 'darwin', env: {}, home }),
      join(home, 'Library/Application Support/rovik-ide')
    );
    assert.equal(
      appDataDir({ platform: 'win32', env: { APPDATA: '/r' }, home }),
      join('/r', 'rovik-ide')
    );
  });

  it('writes a transcript a person can read', () => {
    const report = {
      dialogId: 'lead',
      model: 'm',
      runLog: [verifyLine(1, 'failed', 'first'), verifyLine(2, 'passed', 'progress')],
      subagents: [{ childDialogId: 'scout-123456789', kind: 'scout', status: 'done' }],
      transcript: [
        { dialogId: 'lead', role: 'user', content: 'Fix the bug.' },
        {
          dialogId: 'lead',
          role: 'assistant',
          content: 'Looking.',
          toolCalls: [
            { id: 'c1', name: 'bash', arguments: JSON.stringify({ command: 'go test ./...' }) },
            {
              id: 'c2',
              name: 'write',
              arguments: JSON.stringify({
                path: 'a.go',
                old_text: 'x := 1',
                content: 'func A() {\n\treturn\n}',
              }),
            },
          ],
        },
        {
          dialogId: 'lead',
          role: 'tool',
          toolCallId: 'c1',
          content: 'FAIL ```\n' + 'y'.repeat(9000),
        },
        {
          dialogId: 'lead',
          role: 'tool',
          toolCallId: 'c2',
          content: 'Error: old_text was not found',
        },
        { dialogId: 'scout-123456789', role: 'user', content: 'Find where A is.' },
        {
          dialogId: 'lead',
          role: 'assistant',
          content: 'Done.',
          toolCalls: [],
          error: 'the provider\nfailed',
        },
      ],
    };
    const r = evaluatedRun({
      campaign: 'base',
      task: 't',
      rep: 2,
      result: failing({ verification: 'passed' }),
      report,
    });
    const md = transcriptMarkdown(r, report, { 'the change': '../../../x/changes.diff' });
    assert.match(md, /^# t · base · run 2\n/);
    assert.match(md, /- Passed: no; the hidden tests: 1\/3; failed: TestA, TestB/);
    assert.match(md, /- Outcome: done; the checks: failed → passed \(progress 1, first 1\)/);
    assert.match(md, /- Files: \[the change\]\(\.\.\/\.\.\/\.\.\/x\/changes\.diff\)/);
    assert.match(md, /→ `bash`\n\n```sh\ngo test \.\/\.\.\.\n```/);
    assert.match(
      md,
      /→ `write`\n\n- `path`: `a\.go`\n- `old_text`: `x := 1`\n\n`content`:\n\n```\nfunc A\(\) \{/
    );
    assert.match(md, /← `bash`\n\n````\nFAIL ```/, 'a fence longer than the backticks in it');
    assert.match(md, /characters left out/);
    assert.match(md, /← `write`\n\n```\nError: old_text was not found\n```/);
    assert.match(md, /\*\*Error:\*\* the provider failed/);
    const sub = md.indexOf('## Subagent: scout (done) · scout-12');
    assert.ok(sub > md.indexOf('Done.'), "a subagent's chat after its lead's");
    assert.match(
      transcriptMarkdown(
        evaluatedRun({ campaign: 'c', task: 't', rep: 1, result: failing() }),
        null
      ),
      /_No transcript: the runner wrote no report\._/
    );
  });

  it('cuts long text in the middle and fences any text', () => {
    assert.equal(clip('abc', 5), 'abc');
    const cut = clip('a'.repeat(70) + 'b'.repeat(30) + 'c'.repeat(100), 100);
    assert.ok(cut.startsWith('a'.repeat(70)));
    assert.ok(cut.endsWith('c'.repeat(30)));
    assert.match(cut, /… 100 characters left out …/);
    assert.deepEqual(fenced('plain'), ['```', 'plain', '```']);
    assert.deepEqual(fenced('has ```` four', 'sh'), ['`````sh', 'has ```` four', '`````']);
  });
});
