// meter.mjs tests (P08 in docs/design/evolve-plan.md), the meter as a
// process between a client and a made-up provider. Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { startServer } from './proc.mjs';

const METER = join(dirname(fileURLToPath(import.meta.url)), 'meter.mjs');

/** A provider that answers every request with 10 tokens in and 5 out, and the key it was sent. */
function provider() {
  const server = createServer((req, res) => {
    req.resume();
    req.on('end', () => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          path: req.url,
          auth: req.headers.authorization ?? null,
          usage: { prompt_tokens: 10, completion_tokens: 5 },
        })
      );
    });
  });
  return new Promise((done) =>
    server.listen(0, '127.0.0.1', () =>
      done({ origin: `http://127.0.0.1:${server.address().port}`, close: () => server.close() })
    )
  );
}

describe('the meter (P08)', () => {
  it('passes requests on with the real key, writes each reply’s usage, refuses past the cap', async () => {
    const up = await provider();
    const ledger = join(mkdtempSync(join(tmpdir(), 'meter-')), 'meter.jsonl');
    const meter = await startServer(METER, [up.origin, ledger, '20'], {
      env: { ROVIK_METER_KEY: 'the-real-key' },
    });
    const ask = () =>
      fetch(`${meter.url}/v1/chat/completions`, {
        method: 'POST',
        headers: { authorization: 'Bearer rovik-metered', 'content-type': 'application/json' },
        body: '{}',
      });
    try {
      const first = await (await ask()).json();
      assert.equal(first.path, '/v1/chat/completions', 'the same path');
      assert.equal(first.auth, 'Bearer the-real-key', 'the real key in place of the runner’s');
      assert.equal((await ask()).status, 200, '15 tokens used of 20: one more');
      const refused = await ask();
      assert.equal(refused.status, 402, '30 used: the cap is reached');
      assert.match((await refused.json()).error.message, /used its 20 tokens/);
      const lines = readFileSync(ledger, 'utf8')
        .trim()
        .split('\n')
        .map((l) => JSON.parse(l));
      assert.equal(lines.length, 2, 'the refused request was never sent');
      assert.deepEqual(
        lines.map((l) => [l.status, l.usage, l.input, l.output]),
        [
          [200, true, 10, 5],
          [200, true, 10, 5],
        ]
      );
    } finally {
      meter.stop();
      up.close();
    }
  });
});
