// usage.mjs tests (P08 in docs/design/evolve-plan.md). Run with: npm run test:eval
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { ledgerTotals, replyUsage, usageCounts, usageGap } from './usage.mjs';

const sse = (...events) =>
  events.map((e) => `data: ${JSON.stringify(e)}\n\n`).join('') + 'data: [DONE]\n\n';

describe('what a reply says it used, as the runner reads it (P08)', () => {
  it('OpenAI’s names, the Responses API’s, Anthropic’s', () => {
    assert.deepEqual(usageCounts({ prompt_tokens: 1000, completion_tokens: 50, cost: 0.01 }), {
      input: 1000,
      output: 50,
      cost: 0.01,
    });
    assert.deepEqual(usageCounts({ input_tokens: 10, output_tokens: 5 }), {
      input: 10,
      output: 5,
      cost: null,
    });
    // Anthropic: its cache reads and writes are input too.
    assert.deepEqual(
      usageCounts({
        input_tokens: 12,
        cache_creation_input_tokens: 100,
        cache_read_input_tokens: 3000,
        output_tokens: 7,
      }),
      { input: 3112, output: 7, cost: null }
    );
    for (const none of [null, {}, { total_tokens: 9 }, { cost: 0.1 }, { prompt_tokens: 1.5 }])
      assert.equal(usageCounts(none), null, JSON.stringify(none));
  });

  it('a whole reply: JSON, or a stream’s events merged in order', () => {
    assert.deepEqual(
      replyUsage(
        JSON.stringify({ choices: [], usage: { prompt_tokens: 3, completion_tokens: 2 } })
      ),
      { input: 3, output: 2, cost: null }
    );
    // Chat completions, streamed: the last chunk has it.
    const chat = sse(
      { choices: [{ delta: { content: 'Hi' } }] },
      { choices: [], usage: { prompt_tokens: 1000, completion_tokens: 50, total_tokens: 1050 } }
    );
    assert.deepEqual(replyUsage(chat), { input: 1000, output: 50, cost: null });
    // The Responses API: its completed event.
    const responses = sse(
      { type: 'response.output_text.delta', delta: 'Hi' },
      { type: 'response.completed', response: { usage: { input_tokens: 10, output_tokens: 5 } } }
    );
    assert.deepEqual(replyUsage(responses), { input: 10, output: 5, cost: null });
    // Anthropic: input at the start, output at the end.
    const messages = sse(
      {
        type: 'message_start',
        message: { usage: { input_tokens: 400, cache_read_input_tokens: 100, output_tokens: 1 } },
      },
      { type: 'content_block_delta', delta: { text: 'Hi' } },
      { type: 'message_delta', usage: { output_tokens: 30 } }
    );
    assert.deepEqual(replyUsage(messages), { input: 500, output: 30, cost: null });
    assert.equal(replyUsage(sse({ choices: [{ delta: { content: 'x' } }], usage: null })), null);
    assert.equal(replyUsage('not a reply'), null);
  });

  it('a ledger in total: the provider’s cost when every reply had one, else the price', () => {
    const price = { inputPerMillion: 1, outputPerMillion: 10 };
    const reply = (cost, extra = {}) => ({
      status: 200,
      usage: true,
      cut: false,
      input: 1000,
      output: 50,
      cost,
      ...extra,
    });
    assert.deepEqual(ledgerTotals([reply(0.02), reply(0.03)], price), {
      tokens: 2100,
      replies: 2,
      missing: 0,
      costUsd: 0.05,
      costSource: 'provider',
    });
    const priced = ledgerTotals([reply(0.02), reply(null)], price);
    assert.equal(priced.costSource, 'price');
    assert.ok(Math.abs(priced.costUsd - 0.003) < 1e-12);
    assert.equal(ledgerTotals([reply(null)], null).costUsd, null);
    // A refused probe (the Responses API's 404) and a reply the runner cut
    // count for nothing; a whole reply without usage leaves the cost unknown.
    const none = { input: 0, output: 0, cost: null, usage: false };
    const probe = { ...none, status: 404, cut: false };
    const cutShort = { ...none, status: 200, cut: true };
    assert.equal(ledgerTotals([probe, cutShort, reply(null)], price).costSource, 'price');
    const unknown = ledgerTotals([reply(null), { ...none, status: 200, cut: false }], price);
    assert.deepEqual([unknown.missing, unknown.costUsd, unknown.costSource], [1, null, 'unknown']);
  });

  it('a runner that under-reports, or went around the meter, is caught', () => {
    const metered = { tokens: 4200, replies: 4 };
    assert.equal(usageGap(4200, metered), '');
    assert.equal(usageGap(4000, metered), '', 'within a tenth');
    assert.match(
      usageGap(2100, metered),
      /says it used 2100 tokens; its provider's replies say 4200/
    );
    assert.match(usageGap(500, { tokens: 0, replies: 0 }), /no reply went through the meter/);
    assert.equal(usageGap(0, { tokens: 0, replies: 0 }), '');
  });
});
