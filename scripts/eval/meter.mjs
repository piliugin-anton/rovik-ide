// P08 (docs/design/evolve-plan.md): the meter between a run's runner and
// its model's provider. The runner sends its requests here (its
// `--meter`), with a key that opens nothing; the meter sends them on to
// the provider with the real one (ROVIK_METER_KEY), so a runner that went
// around it would get no answer. It reads each reply's usage as it passes
// (usage.mjs), writes a line of it to its ledger, and refuses a request
// once the run has used its tokens. The grader takes a run's tokens and
// cost from the ledger, not from the runner.
//
//   node scripts/eval/meter.mjs <provider-origin> <ledger-file> [max-tokens]
//
// Prints its own origin on its first line. It reaches the provider
// directly, or through the HTTPS proxy only where Node's fetch takes one.
import { createServer } from 'node:http';
import { appendFileSync } from 'node:fs';
import { replyUsage } from './usage.mjs';

const [upstream, ledger, cap] = process.argv.slice(2);
if (!upstream || !ledger) {
  console.error('usage: meter.mjs <provider-origin> <ledger-file> [max-tokens]');
  process.exit(2);
}
const maxTokens = cap ? Number(cap) : Infinity;
const key = process.env.ROVIK_METER_KEY ?? '';
let used = 0;

/** What isn't passed on, either way: the connection's own, and a body's encoding. */
const HOP = [
  'host',
  'connection',
  'keep-alive',
  'proxy-connection',
  'transfer-encoding',
  'content-length',
  'content-encoding',
  'accept-encoding',
];

/** The request's headers for the provider: the real key in place of the runner's. */
function headersFor(incoming) {
  const out = {};
  for (const [k, v] of Object.entries(incoming)) if (!HOP.includes(k)) out[k] = v;
  // Whole bodies, so the usage can be read and the bytes passed on as are.
  out['accept-encoding'] = 'identity';
  if (key && 'authorization' in out) out.authorization = `Bearer ${key}`;
  if (key && 'x-api-key' in out) out['x-api-key'] = key;
  return out;
}

const write = (line) => appendFileSync(ledger, `${JSON.stringify(line)}\n`);

const refuse = (res, status, message) => {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ error: { message } }));
};

const server = createServer(async (req, res) => {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const post = req.method === 'POST';
  if (post && used >= maxTokens)
    return refuse(res, 402, `The evaluation's meter: the run has used its ${maxTokens} tokens.`);
  let up;
  const aborted = new AbortController();
  // The runner stopped reading (a Stop, a timeout): so does the meter.
  let cut = false;
  res.on('close', () => {
    if (!res.writableFinished) {
      cut = true;
      aborted.abort();
    }
  });
  try {
    up = await fetch(upstream + req.url, {
      method: req.method,
      headers: headersFor(req.headers),
      body: post ? Buffer.concat(chunks) : undefined,
      signal: aborted.signal,
    });
  } catch (e) {
    if (!cut) refuse(res, 502, `The evaluation's meter couldn't reach the provider: ${e.message}`);
    return;
  }
  const headers = {};
  up.headers.forEach((v, k) => {
    if (!HOP.includes(k)) headers[k] = v;
  });
  res.writeHead(up.status, headers);
  const seen = [];
  try {
    for await (const chunk of up.body ?? []) {
      seen.push(Buffer.from(chunk));
      res.write(chunk);
    }
  } catch {
    cut = true;
  }
  // In the ledger before the runner has the reply's end: once it has, it
  // may be over, and the meter stopped.
  if (post) {
    const usage = replyUsage(Buffer.concat(seen).toString('utf8'));
    if (usage) used += usage.input + usage.output;
    write({
      t: new Date().toISOString(),
      path: req.url,
      status: up.status,
      usage: !!usage,
      cut,
      ...(usage ?? { input: 0, output: 0, cost: null }),
    });
  }
  res.end();
});

server.listen(0, '127.0.0.1', () => console.log(`http://127.0.0.1:${server.address().port}`));
