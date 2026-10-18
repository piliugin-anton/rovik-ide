// A scriptable OpenAI-compatible model for driving the app's AI chat in
// end-to-end tests. The *last user message* steers it with directives:
//
//   @tool <name> <json args>   one tool call per directive, in order; each
//                              runs after the previous tool's result is back
//   @text <text>               the final reply (default: an echo)
//   @slow <seconds>            stream the final reply slowly over that long
//   @error <status> [message]  answer with that HTTP error
//   @think <text>              send reasoning_content before the reply
//   @thinkslow <seconds>       stream (lorem) reasoning for that long before
//                              the reply (after the tool calls, if any)
//   @parallel                  send all @tool calls in one reply
//
// Without directives it echoes the request in Markdown (with a code block
// and a list) so rendering is exercised. Every request body is appended
// to $FAKE_LOG (JSONL) for inspection.
import { createServer } from 'node:http';
import { appendFileSync } from 'node:fs';

const port = Number(process.env.FAKE_PORT || 0);
const log = process.env.FAKE_LOG;
const usage = { prompt_tokens: 1200, completion_tokens: 60, total_tokens: 1260 };

function textOf(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content))
    return content
      .map((p) => (typeof p === 'string' ? p : (p.text ?? (p.type ? `[${p.type}]` : ''))))
      .join('\n');
  return '';
}

function parseDirectives(text) {
  const tools = [];
  let final = null;
  let slow = 0;
  let error = null;
  let think = null;
  let thinkSlow = 0;
  let parallel = false;
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    let m;
    if ((m = line.match(/^@tool\s+(\w+)\s+(\{.*\})\s*$/))) {
      try {
        tools.push({ name: m[1], args: JSON.parse(m[2]) });
      } catch (e) {
        tools.push({ name: m[1], args: { _bad_json: m[2] } });
      }
    } else if ((m = line.match(/^@text\s+(.*)$/))) final = (final ? final + '\n' : '') + m[1];
    else if ((m = line.match(/^@slow\s+(\d+)/))) slow = Number(m[1]);
    else if ((m = line.match(/^@error\s+(\d+)\s*(.*)$/)))
      error = { status: Number(m[1]), message: m[2] };
    else if ((m = line.match(/^@thinkslow\s+(\d+)/))) thinkSlow = Number(m[1]);
    else if ((m = line.match(/^@think\s+(.*)$/))) think = m[1];
    else if (line === '@parallel') parallel = true;
  }
  return { tools, final, slow, error, think, thinkSlow, parallel };
}

let callSeq = 0;

function plan(body) {
  const messages = body.messages ?? [];
  // Responses API input, just in case.
  const lastUserIdx = messages.map((m) => m.role).lastIndexOf('user');
  const lastUser = lastUserIdx >= 0 ? textOf(messages[lastUserIdx].content) : '';
  const after = lastUserIdx >= 0 ? messages.slice(lastUserIdx + 1) : [];
  const toolResults = after.filter((m) => m.role === 'tool').length;
  const d = parseDirectives(lastUser);
  const offered = new Set((body.tools ?? []).map((t) => t.function?.name).filter(Boolean));
  if (d.error) return { error: d.error };
  // Requests without tools (titles, summaries, reviewers): a short answer.
  if (!body.tools || body.tools.length === 0) {
    const sys = textOf(messages[0]?.content ?? '')
      .slice(0, 200)
      .toLowerCase();
    if (sys.includes('title')) return { text: 'Fake chat title' };
    return { text: d.final ?? 'OK.' };
  }
  if (d.parallel && d.tools.length > 0 && toolResults === 0) {
    return { calls: d.tools.filter((t) => offered.has(t.name)), think: d.think };
  }
  if (!d.parallel && toolResults < d.tools.length) {
    const t = d.tools[toolResults];
    if (offered.has(t.name)) return { calls: [t], think: toolResults === 0 ? d.think : null };
  }
  const lastTool = [...after].reverse().find((m) => m.role === 'tool');
  const echo =
    d.final ??
    (d.tools.length
      ? `Done. Last tool said:\n\n\`\`\`\n${textOf(lastTool?.content ?? '').slice(0, 300)}\n\`\`\``
      : `Echo of your message:\n\n> ${lastUser.split('\n')[0].slice(0, 200)}\n\n- item one\n- item **two**\n\n\`\`\`ts\nconst answer: number = 42;\n\`\`\``);
  return {
    text: echo,
    slow: d.slow,
    think: d.tools.length ? null : d.think,
    thinkSlow: d.thinkSlow,
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function stream(res, r, model) {
  res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
  const chunk = (o) =>
    res.write(
      `data: ${JSON.stringify({ id: 'fake', object: 'chat.completion.chunk', model, created: Math.floor(Date.now() / 1000), ...o })}\n\n`
    );
  if (r.think) {
    for (const piece of r.think.match(/.{1,20}/g) ?? []) {
      chunk({ choices: [{ index: 0, delta: { role: 'assistant', reasoning_content: piece } }] });
      await sleep(20);
    }
  }
  if (r.thinkSlow) {
    // A word every 50 ms, a sentence a line.
    const words = 'Let me think about this carefully and look at the code before answering.'.split(
      ' '
    );
    const end = Date.now() + r.thinkSlow * 1000;
    for (let i = 0; Date.now() < end; i++) {
      if (res.destroyed) return;
      const word = words[i % words.length] + (i % words.length === words.length - 1 ? '\n' : ' ');
      chunk({ choices: [{ index: 0, delta: { role: 'assistant', reasoning_content: word } }] });
      await sleep(50);
    }
  }
  if (r.calls) {
    r.calls.forEach((t, index) => {
      const call = {
        index,
        id: `call_${++callSeq}`,
        type: 'function',
        function: { name: t.name, arguments: JSON.stringify(t.args) },
      };
      chunk({ choices: [{ index: 0, delta: { role: 'assistant', tool_calls: [call] } }] });
    });
    chunk({ choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }] });
  } else {
    const text = r.text;
    const pieces = text.match(/[\s\S]{1,12}/g) ?? [''];
    const delay = r.slow ? (r.slow * 1000) / pieces.length : 5;
    for (const p of pieces) {
      if (res.destroyed) return;
      chunk({ choices: [{ index: 0, delta: { role: 'assistant', content: p } }] });
      await sleep(delay);
    }
    chunk({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] });
  }
  chunk({ choices: [], usage });
  res.end('data: [DONE]\n\n');
}

function json(res, r, model) {
  const message = r.calls
    ? {
        role: 'assistant',
        content: null,
        tool_calls: r.calls.map((t) => ({
          id: `call_${++callSeq}`,
          type: 'function',
          function: { name: t.name, arguments: JSON.stringify(t.args) },
        })),
      }
    : { role: 'assistant', content: r.text };
  res.writeHead(200, { 'Content-Type': 'application/json' });
  res.end(
    JSON.stringify({
      id: 'fake',
      object: 'chat.completion',
      model,
      choices: [{ index: 0, message, finish_reason: r.calls ? 'tool_calls' : 'stop' }],
      usage,
    })
  );
}

const server = createServer((req, res) => {
  let data = '';
  req.on('data', (c) => (data += c));
  req.on('end', async () => {
    let body = {};
    try {
      body = JSON.parse(data || '{}');
    } catch {}
    if (log) {
      try {
        appendFileSync(
          log,
          JSON.stringify({
            t: new Date().toISOString(),
            method: req.method,
            url: req.url,
            auth: req.headers.authorization ? 'set' : 'none',
            body,
          }) + '\n'
        );
      } catch {}
    }
    if (req.method === 'GET' && req.url.endsWith('/models')) {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(
        JSON.stringify({
          object: 'list',
          data: [
            { id: 'fake-model', object: 'model', owned_by: 'e2e', context_length: 128000 },
            { id: 'fake-vision', object: 'model', owned_by: 'e2e', context_length: 64000 },
          ],
        })
      );
      return;
    }
    if (req.method !== 'POST') {
      res.writeHead(404);
      res.end('not found');
      return;
    }
    // A provider without the Responses API, which the app tries first: it
    // goes on to chat completions (chat_request/responses.rs `wrong_route`).
    if (req.url.endsWith('/responses')) {
      res.writeHead(404, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: { message: `Invalid URL (POST ${req.url})` } }));
      return;
    }
    const r = plan(body);
    if (r.error) {
      res.writeHead(r.error.status, { 'Content-Type': 'application/json' });
      res.end(
        JSON.stringify({
          error: { message: r.error.message || `fake error ${r.error.status}`, type: 'fake' },
        })
      );
      return;
    }
    const model = body.model ?? 'fake-model';
    if (body.stream) await stream(res, r, model);
    else json(res, r, model);
  });
});
server.listen(port, '127.0.0.1', () => {
  console.log(`http://127.0.0.1:${server.address().port}/v1`);
});
