// @ts-nocheck
// toolGroups.ts tests (node:test). Run with: npm test
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { commandName, describeGroup, domainName, editCounts, groupToolCalls } from './toolGroups';

let next = 0;
function call(name, args = {}) {
  return { id: `c${next++}`, name, arguments: JSON.stringify(args) };
}

describe('groupToolCalls', () => {
  it('groups consecutive calls to one tool, whatever their arguments', () => {
    const runs = groupToolCalls([
      call('read', { path: 'a.ts' }),
      call('read', { path: 'b.ts' }),
      call('bash', { command: 'ls' }),
      call('read', { path: 'c.ts' }),
    ]);
    assert.deepEqual(
      runs.map((r) => [r.name, r.calls.length]),
      [
        ['read', 2],
        ['bash', 1],
        ['read', 1],
      ]
    );
  });

  it('keeps each skill on a line of its own', () => {
    const runs = groupToolCalls([
      call('skill', { skill: 'pdf' }),
      call('skill', { skill: 'docx' }),
    ]);
    assert.deepEqual(
      runs.map((r) => [r.calls.length, !!r.alone]),
      [
        [1, true],
        [1, true],
      ]
    );
  });

  it('keeps questions, checklists and context edits apart', () => {
    const runs = groupToolCalls([
      call('todo', { items: [] }),
      call('todo', { items: [] }),
      call('write', { path: 'a.ts', content: '' }),
      call('write', { path: '@context', content: '' }),
      call('write', { path: 'b.ts', content: '' }),
    ]);
    assert.deepEqual(
      runs.map((r) => [r.calls.length, !!r.alone]),
      [
        [1, true],
        [1, true],
        [1, false],
        [1, true],
        [1, false],
      ]
    );
  });
});

describe('task calls', () => {
  it('each subagent keeps its own line, named by its kind and goal (P1.6)', () => {
    const runs = groupToolCalls([
      call('task', { kind: 'scout', goal: 'Where is the parser?' }),
      call('task', { kind: 'scout', goal: 'Who calls it?' }),
      call('task', { continue: 'scout-1', goal: 'And then?' }),
    ]);
    assert.deepEqual(
      runs.map((r) => [r.name, r.calls.length, r.alone]),
      [
        ['task', 1, true],
        ['task', 1, true],
        ['task', 1, true],
      ]
    );
    const label = describeGroup('task', [{ kind: 'scout', goal: 'Where is the parser?' }], true);
    assert.equal(label.verb, 'Scout');
    assert.deepEqual(
      label.items.map((i) => i.text),
      ['Where is the parser?']
    );
    assert.equal(
      describeGroup('task', [{ continue: 'scout-1', goal: 'x' }], false).verb,
      'Follow-up'
    );
  });
});

describe('one_and_three_task_calls_stand_alone', () => {
  it('each task is a run of its own, and reads either side of one don’t merge (P1.6)', () => {
    const runs = groupToolCalls([
      call('read', { path: 'a.ts' }),
      call('task', { kind: 'scout', goal: 'One' }),
      call('read', { path: 'b.ts' }),
      call('task', { kind: 'scout', goal: 'Two' }),
      call('task', { kind: 'reviewer', goal: 'Three' }),
      call('write', { path: 'c.ts', content: '' }),
    ]);
    assert.deepEqual(
      runs.map((r) => [r.name, r.calls.length]),
      [
        ['read', 1],
        ['task', 1],
        ['read', 1],
        ['task', 1],
        ['task', 1],
        ['write', 1],
      ]
    );
    // A single task alone is a run of one too.
    assert.deepEqual(
      groupToolCalls([call('task', { kind: 'scout', goal: 'x' })]).map((r) => [
        r.calls.length,
        r.alone,
      ]),
      [[1, true]]
    );
  });

  it('describe_group_task_edge_cases', () => {
    const goal = 'g'.repeat(100);
    const long = describeGroup('task', [{ kind: 'scout', goal }], true);
    assert.equal(long.items[0].text.length, 60);
    assert.ok(long.items[0].text.endsWith('…'));
    assert.equal(long.items[0].title, goal, 'the whole goal in the tooltip');
    assert.equal(
      describeGroup('task', [{ kind: 'workers', group: 'api' }], true).verb,
      'Parallel workers'
    );
    assert.deepEqual(
      describeGroup('task', [{ kind: 'scout' }], false).items,
      [],
      'no goal: no items'
    );
    assert.deepEqual(describeGroup('task', [], false).items, []);
  });
});

describe('describeGroup', () => {
  it('names the background tasks the agent checked on', () => {
    const label = describeGroup(
      'background',
      [{ action: 'output', id: 'bg1' }, { action: 'list' }],
      false
    );
    assert.equal(label.verb, 'Checked on');
    assert.deepEqual(
      label.items.map((i) => i.text),
      ['bg1', 'the background tasks']
    );
  });

  it('names files once each, with where to open them', () => {
    const label = describeGroup(
      'read',
      [{ path: 'src/App.tsx', start_line: 10 }, { path: 'src/main.tsx' }, { path: 'src/App.tsx' }],
      false
    );
    assert.equal(label.verb, 'Read');
    assert.deepEqual(
      label.items.map((i) => [i.text, i.path, i.line]),
      [
        ['App.tsx', 'src/App.tsx', 10],
        ['main.tsx', 'src/main.tsx', undefined],
      ]
    );
  });

  it('says editing for partial writes and caps the list', () => {
    const args = ['a', 'b', 'c', 'd', 'e', 'f'].map((n) => ({ path: `${n}.ts`, content: '' }));
    args[2].old_text = 'x';
    const label = describeGroup('write', args, true);
    assert.equal(label.verb, 'Editing');
    assert.equal(label.items.length, 4);
    assert.equal(label.more, 2);
    assert.equal(describeGroup('write', [{ path: 'a.ts' }], false).verb, 'Wrote');
  });

  it('describes questions, checklists, plans and context edits', () => {
    const asked = describeGroup('ask_user', [{ questions: [{ question: 'Which DB?' }] }], false);
    assert.equal(asked.verb, 'Asked');
    assert.equal(asked.items[0].text, 'Which DB?');
    const todo = describeGroup(
      'todo',
      [
        {
          items: [
            { title: 'One', status: 'done' },
            { title: 'Two', status: 'in_progress' },
          ],
        },
      ],
      false
    );
    assert.equal(todo.verb, 'Updated the checklist');
    assert.equal(todo.items[0].text, '1 of 2 done · Two');
    // S06: the answers to the change review, in a line.
    const answered = describeGroup(
      'answer_review',
      [{ responses: [{ finding: 1, status: 'fixed', note: 'n' }] }],
      false
    );
    assert.equal(answered.verb, 'Answered the review');
    assert.deepEqual(
      answered.items.map((i) => i.text),
      ['1 fixed']
    );
    const plan = describeGroup('submit_plan', [{ plan: '# Add caching\n\nSteps' }], true);
    assert.deepEqual([plan.verb, plan.items[0].text], ['Saving the plan', 'Add caching']);
    // Step 7: an edits-only submit reads its plan from the result's meta.
    const edits = (n: number) => ({
      edits: Array.from({ length: n }, () => ({ old_text: 'a', new_text: 'b' })),
    });
    const edited = describeGroup('submit_plan', [edits(2), edits(1)], false, [
      { plan: '# Add caching\n\nSteps' },
      { plan: '# Add caching\n\nSteps' },
    ]);
    assert.deepEqual(
      edited.items.map((i) => i.text),
      ['Add caching (2 edits)', 'Add caching (1 edit)']
    );
    const context = describeGroup(
      'write',
      [{ path: '@context', content: 'x', old_text: 'y' }],
      false
    );
    assert.deepEqual([context.verb, context.items.length], ['Edited its context', 0]);
  });

  it('names the skill a connector uses', () => {
    const using = describeGroup('skill', [{ skill: 'slow-census' }], true);
    assert.deepEqual([using.verb, using.items[0].text], ['Using skill', 'slow-census']);
    const used = describeGroup('skill', [{ skill: 'slow-census', args: '--all' }], false);
    assert.deepEqual([used.verb, used.items[0].text], ['Used skill', 'slow-census']);
  });

  it('names commands by their program and pages by their site', () => {
    const ran = describeGroup(
      'bash',
      [{ command: 'npm test -- --watch' }, { command: 'git status' }, { command: 'npm run build' }],
      false
    );
    assert.equal(ran.verb, 'Ran');
    assert.deepEqual(
      ran.items.map((i) => i.text),
      ['npm', 'git']
    );
    const browsed = describeGroup(
      'browse_url',
      [{ url: 'https://www.docs.rs/tokio' }, { url: 'docs.rs/serde' }, { url: 'github.com/x' }],
      true
    );
    assert.equal(browsed.verb, 'Browsing');
    assert.deepEqual(
      browsed.items.map((i) => i.text),
      ['docs.rs', 'github.com']
    );
  });
});

describe('mcp', () => {
  it('names the server’s tools the agent called, or read about', () => {
    const called = describeGroup(
      'mcp',
      [
        { tool: 'docs.read', arguments: { path: 'intro' } },
        { tool: 'docs.read', arguments: { path: 'setup' } },
        { tool: 'github.create_issue', arguments: {} },
      ],
      false
    );
    assert.equal(called.verb, 'Called');
    assert.deepEqual(
      called.items.map((i) => i.text),
      ['docs.read', 'github.create_issue']
    );
    const described = describeGroup('mcp', [{ tool: 'docs.read', describe: true }], true);
    assert.equal(described.verb, 'Reading about');
    assert.deepEqual(
      described.items.map((i) => i.text),
      ['docs.read']
    );
  });
});

describe('list_dir', () => {
  it('calls the project root by name, however the model wrote it', () => {
    const listed = describeGroup(
      'list_dir',
      [{}, { path: '.' }, { path: './' }, { path: ' .\\ ' }, { path: 'src' }, { path: './src' }],
      false
    );
    assert.deepEqual(
      listed.items.map((i) => i.text),
      ['project root', 'src']
    );
  });

  it('names the folder the backend listed, once it has', () => {
    const listed = describeGroup(
      'list_dir',
      [{ path: 'C:\\Proj' }, { path: '/home/me/proj/src/' }, { path: 'D:\\other\\' }],
      false,
      [{ dir: '' }, { dir: 'src' }, undefined]
    );
    assert.deepEqual(
      listed.items.map((i) => i.text),
      ['project root', 'src', 'D:\\other']
    );
  });
});

describe('editCounts', () => {
  it('totals the lines writes add and remove, skipping context edits', () => {
    assert.deepEqual(
      editCounts([
        { args: { path: 'a.ts', old_text: 'one\ntwo', content: 'one\nTWO\nthree' } },
        { args: { path: 'b.ts', old_text: 'gone', content: '' } },
        { args: { path: 'c.ts', content: 'whole\nfile' }, result: { added: 5, removed: 1 } },
        { args: { path: '@context', old_text: 'x', content: 'y' } },
      ]),
      { added: 7, removed: 3 }
    );
    // A whole-file write from before results counted its lines.
    assert.equal(editCounts([{ args: { path: 'c.ts', content: 'whole' } }]), undefined);
  });
});

describe('commandName', () => {
  it('skips variable assignments and paths', () => {
    assert.equal(commandName('FOO=1 BAR=2 /usr/bin/cargo build'), 'cargo');
    assert.equal(commandName('  "./run.sh" --fast'), 'run.sh');
    assert.equal(commandName(''), undefined);
  });
});

describe('domainName', () => {
  it('takes the host from a URL with or without a scheme', () => {
    assert.equal(domainName('http://localhost:5173/app'), 'localhost');
    assert.equal(domainName('example.org/path'), 'example.org');
    assert.equal(domainName(undefined), undefined);
  });
});
