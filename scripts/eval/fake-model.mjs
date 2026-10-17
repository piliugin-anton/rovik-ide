// A scripted OpenAI-compatible model for dry runs of the evaluation
// (`eval.mjs dry-run`): it tests the pipeline, not a model.
//
//   node scripts/eval/fake-model.mjs oracle <patch-file>
//     applies the patch with one bash call (`git apply`), then says done:
//     the run should grade as a success.
//   node scripts/eval/fake-model.mjs idle
//     says done without changing anything: the run should grade as a failure.
//   node scripts/eval/fake-model.mjs autopilot <files.json>
//     an Autopilot task's model (G03 in docs/design/autopilot-plan.md):
//     a plan naming the files, the answer each stage's brief is read for,
//     and the change as write calls (each file of files.json, the
//     reference's, whole), so the task runs to its delivery: the run
//     should grade as a success.
//
// Prints the server's base URL on its first line. Every reply reports
// usage (1,000 tokens in, 50 out) so the cost path is exercised too.
import { createServer } from 'node:http';
import { appendFileSync, readFileSync } from 'node:fs';

const [mode, patch] = process.argv.slice(2);
if (!['oracle', 'idle', 'autopilot'].includes(mode) || (mode !== 'idle' && !patch)) {
  console.error('usage: fake-model.mjs oracle <patch-file> | idle | autopilot <files.json>');
  process.exit(2);
}
/** Autopilot's change: path -> the file's content at the reference. */
const files = mode === 'autopilot' ? JSON.parse(readFileSync(patch, 'utf8')) : {};

const shellQuote = (s) => `'${s.replace(/'/g, `'\\''`)}'`;

// ------------------------------------------------------------- Autopilot

/** A message's text, whether a string or parts. */
const textOf = (m) =>
  typeof m?.content === 'string'
    ? m.content
    : Array.isArray(m?.content)
      ? m.content.map((p) => p?.text ?? '').join('')
      : '';

const CRITERIA =
  '## Criteria\n\n1. The change the request asks for is made, and the project’s tests pass.\n\n' +
  '**Out of scope**: None.\n\n**Assumptions**: None.';

/** A plan the plan's checks pass: two steps, each naming files that are there. */
function plan() {
  const paths = Object.keys(files);
  return [
    '# Make the change the request asks for',
    '',
    'The scripted model makes the reference change (the evaluation’s dry run).',
    '',
    CRITERIA,
    '',
    '## Steps',
    '',
    '1. **Make the change**',
    `   - Files: ${paths.map((p) => `\`${p}\``).join(', ')}`,
    '   - Change these files as the request asks: add or update the code it names.',
    '2. **Check the change**',
    `   - Files: \`${paths[0]}\` (read)`,
    '   - Read the changed file again and check that it keeps its other code.',
    '',
    '## Validation',
    '',
    '- The project’s own tests.',
    '',
  ].join('\n');
}

/** "satisfied" for each criterion the acceptance brief lists (at least one). */
function verdicts(brief) {
  const section = brief.split(/^## Criteria\s*$/m)[1] ?? '';
  const until = section.split(/^(\*\*Out of scope\*\*|## )/m)[0];
  const n = Math.max(1, (until.match(/^\d+\.\s/gm) ?? []).length);
  const lines = Array.from(
    { length: n },
    (_, i) => `${i + 1}. satisfied — the change makes it so.`
  );
  return (
    `## Verdicts\n\n${lines.join('\n')}\n\n## Evidence\n\nThe reference change, as written.\n\n` +
    '## Unmet\n\nNone.'
  );
}

const BLOCKED = '## Blocked\n\nThe scripted model doesn’t do this part.';

/** Each subagent's brief, by its first line, and what it answers. */
const BRIEFS = [
  ['# Review this plan', () => '## Findings\n\nNone.'],
  ['# Review this change', () => '## Findings\n\nNone.'],
  ['# Write the tests before the change', () => BLOCKED],
  ['# Write tests for this change', () => BLOCKED],
  ['# Check this change against its criteria', verdicts],
  ['# Triage this failure', () => BLOCKED],
  // Thorough: two criteria writers and their merge, three planners and
  // their judge, the adversarial tester, the candidates' judge.
  ['# Write the criteria for this task', () => CRITERIA],
  [
    '# Merge these criteria',
    () => CRITERIA.replace('**Assumptions**: None.', '**Assumptions**: The two sets are the same.'),
  ],
  ['# Write a plan for this task', () => `## Plan\n\n${plan()}`],
  ['# Judge these plans', () => '## Choice\n\na\n\n## Why\n\nThe plans are the same.'],
  ['# Judge these changes', () => '## Choice\n\na\n\n## Why\n\nThe changes are the same.'],
  ['# Break this change', () => BLOCKED],
];

/** Turns whose brief is the run's last message: the task's report, the reviewer's view. */
const LAST = [
  [
    "# Write the task's report",
    () =>
      '## Assumptions\n\nNone.\n\n## What changed\n\nThe reference change.\n\n' +
      '## How it was verified\n\nThe project’s checks.\n\n## Open\n\nNothing.',
  ],
  ['# Write your view of the result', () => 'The change does what the request asks.'],
];

const todo = (status) => ({
  id: `call_todo_${status}`,
  name: 'todo',
  args: {
    items: [
      { title: 'Make the change', status },
      { title: 'Check the change', status: status === 'done' ? 'done' : 'pending' },
    ],
  },
});

function autopilot(body) {
  const messages = body.messages ?? [];
  const first = textOf(messages.find((m) => m.role !== 'system'));
  const last = textOf(messages.at(-1));
  const calls = messages.flatMap((m) => m.tool_calls ?? []);
  const called = (name) => calls.some((c) => c.function?.name === name);
  for (const [heading, answer] of LAST) {
    if (last.includes(heading)) return { text: answer() };
  }
  // A subagent's brief: its first message, after the repository map; the
  // first of its lines that is a brief's heading (what it quotes after
  // that, a plan's title among them, is the brief's).
  for (const line of first.split('\n')) {
    const brief = BRIEFS.find(([heading]) => line.startsWith(heading));
    if (brief) return { text: brief[1](first) };
  }
  // The plan stage: a Plan-mode run, offered submit_plan.
  if ((body.tools ?? []).some((t) => t.function?.name === 'submit_plan')) {
    if (!called('submit_plan'))
      return { tools: [{ id: 'call_plan', name: 'submit_plan', args: { plan: plan() } }] };
    return { text: 'Saved the plan.' };
  }
  // The implementation: its checklist, the change, the checklist done.
  if (!called('todo')) return { tools: [todo('in_progress')] };
  if (!called('write'))
    return {
      tools: Object.entries(files).map(([path, content], i) => ({
        id: `call_write_${i}`,
        name: 'write',
        args: { path, content },
      })),
    };
  const lastTodo = calls.filter((c) => c.function?.name === 'todo').at(-1);
  let items = [];
  try {
    items = JSON.parse(lastTodo.function.arguments).items ?? [];
  } catch {
    // not ours: the checklist again
  }
  if (!items.every((i) => i.status === 'done')) return { tools: [todo('done')] };
  return { text: 'Applied the change.' };
}

// ----------------------------------------------------------- Agent mode

function reply(body) {
  if (mode === 'autopilot') return autopilot(body);
  const ranTool = (body.messages ?? []).some((m) => m.role === 'tool');
  if (
    mode === 'oracle' &&
    !ranTool &&
    (body.tools ?? []).some((t) => t.function?.name === 'bash')
  ) {
    return {
      tool: {
        id: 'call_apply',
        name: 'bash',
        args: { command: `git apply --whitespace=nowarn ${shellQuote(patch)}` },
      },
    };
  }
  return { text: mode === 'oracle' ? 'Applied the change.' : 'Done.' };
}

const usage = { prompt_tokens: 1000, completion_tokens: 50, total_tokens: 1050 };

/** A reply's tool calls: several (`tools`), one (`tool`) or none. */
const callsOf = (r) => r.tools ?? (r.tool ? [r.tool] : []);

function sse(r) {
  const chunk = (o) =>
    `data: ${JSON.stringify({ id: 'fake', object: 'chat.completion.chunk', ...o })}\n\n`;
  let out = '';
  const tools = callsOf(r);
  if (tools.length) {
    const calls = tools.map((t, index) => ({
      index,
      id: t.id,
      type: 'function',
      function: { name: t.name, arguments: JSON.stringify(t.args) },
    }));
    out += chunk({ choices: [{ index: 0, delta: { role: 'assistant', tool_calls: calls } }] });
    out += chunk({ choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }] });
  } else {
    out += chunk({ choices: [{ index: 0, delta: { role: 'assistant', content: r.text } }] });
    out += chunk({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] });
  }
  return out + chunk({ choices: [], usage }) + 'data: [DONE]\n\n';
}

function json(r) {
  const tools = callsOf(r);
  const message = tools.length
    ? {
        role: 'assistant',
        content: null,
        tool_calls: tools.map((t) => ({
          id: t.id,
          type: 'function',
          function: { name: t.name, arguments: JSON.stringify(t.args) },
        })),
      }
    : { role: 'assistant', content: r.text };
  return JSON.stringify({
    id: 'fake',
    object: 'chat.completion',
    choices: [{ index: 0, message, finish_reason: tools.length ? 'tool_calls' : 'stop' }],
    usage,
  });
}

const server = createServer((req, res) => {
  let data = '';
  req.on('data', (c) => (data += c));
  req.on('end', () => {
    let body = {};
    try {
      body = JSON.parse(data || '{}');
    } catch {
      // answered as an empty request
    }
    if (req.method === 'GET') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ data: [{ id: 'fake-model' }] }));
      return;
    }
    // A provider without the Responses API, which the app tries first: it
    // goes on to chat completions (chat_request/responses.rs `wrong_route`).
    if (req.url.endsWith('/responses')) {
      res.writeHead(404, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: { message: `Invalid URL (POST ${req.url})` } }));
      return;
    }
    // FAKE_MODEL_LOG=file: each request, a JSON line, to see what a run sent.
    if (process.env.FAKE_MODEL_LOG) appendFileSync(process.env.FAKE_MODEL_LOG, `${data}\n`);
    const r = reply(body);
    if (body.stream) {
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.end(sse(r));
    } else {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(json(r));
    }
  });
});
server.listen(0, '127.0.0.1', () => {
  console.log(`http://127.0.0.1:${server.address().port}/v1`);
});
