// A scripted OpenAI-compatible model for dry runs of the evaluation
// (`eval.mjs dry-run`): it tests the pipeline, not a model.
//
//   node scripts/eval/fake-model.mjs oracle <patch-file>
//     applies the patch with one bash call (`git apply`), then says done:
//     the run should grade as a success.
//   node scripts/eval/fake-model.mjs idle
//     says done without changing anything: the run should grade as a failure.
//
// Prints the server's base URL on its first line. Every reply reports
// usage (1,000 tokens in, 50 out) so the cost path is exercised too.
import { createServer } from 'node:http';

const [mode, patch] = process.argv.slice(2);
if (!['oracle', 'idle'].includes(mode) || (mode === 'oracle' && !patch)) {
  console.error('usage: fake-model.mjs oracle <patch-file> | idle');
  process.exit(2);
}

const shellQuote = (s) => `'${s.replace(/'/g, `'\\''`)}'`;

function reply(body) {
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

function sse(r) {
  const chunk = (o) =>
    `data: ${JSON.stringify({ id: 'fake', object: 'chat.completion.chunk', ...o })}\n\n`;
  let out = '';
  if (r.tool) {
    const call = {
      index: 0,
      id: r.tool.id,
      type: 'function',
      function: { name: r.tool.name, arguments: JSON.stringify(r.tool.args) },
    };
    out += chunk({ choices: [{ index: 0, delta: { role: 'assistant', tool_calls: [call] } }] });
    out += chunk({ choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }] });
  } else {
    out += chunk({ choices: [{ index: 0, delta: { role: 'assistant', content: r.text } }] });
    out += chunk({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] });
  }
  return out + chunk({ choices: [], usage }) + 'data: [DONE]\n\n';
}

function json(r) {
  const message = r.tool
    ? {
        role: 'assistant',
        content: null,
        tool_calls: [
          {
            id: r.tool.id,
            type: 'function',
            function: { name: r.tool.name, arguments: JSON.stringify(r.tool.args) },
          },
        ],
      }
    : { role: 'assistant', content: r.text };
  return JSON.stringify({
    id: 'fake',
    object: 'chat.completion',
    choices: [{ index: 0, message, finish_reason: r.tool ? 'tool_calls' : 'stop' }],
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
