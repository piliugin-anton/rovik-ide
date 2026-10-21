// P08 (docs/design/evolve-plan.md): what a provider's reply says it used,
// read as the runner reads it (rovik-core: agent_run/log.rs
// `usage_counts`, and chat_request's normalising of Anthropic's), so that
// the meter and an honest runner agree to the token. Pure: meter.mjs and
// eval.mjs call it.

/** A token count: a whole number, 0 or more (as `token_count` takes it). */
const count = (v) => (Number.isFinite(v) && v >= 0 && Number.isInteger(v) ? v : null);

/**
 * Tokens in and out and the cost of one usage object: OpenAI's names, the
 * Responses API's, or Anthropic's (its cache reads and writes are input
 * too). Null when it has no token count.
 */
export function usageCounts(u) {
  if (!u || typeof u !== 'object') return null;
  const anthropicIn =
    count(u.input_tokens) === null
      ? null
      : u.input_tokens +
        (count(u.cache_creation_input_tokens) ?? 0) +
        (count(u.cache_read_input_tokens) ?? 0);
  const input = count(u.prompt_tokens) ?? anthropicIn;
  const output = count(u.completion_tokens) ?? count(u.output_tokens);
  if (input === null && output === null) return null;
  return {
    input: input ?? 0,
    output: output ?? 0,
    cost: typeof u.cost === 'number' && Number.isFinite(u.cost) ? u.cost : null,
  };
}

/**
 * The usage a whole reply reports: a JSON body's `usage`, or in a stream
 * of events every usage object merged in order (OpenAI's last chunk, the
 * Responses API's `response.completed`, Anthropic's `message_start` then
 * `message_delta`). Null when it reports none.
 */
export function replyUsage(text) {
  const trimmed = text.trimStart();
  if (trimmed.startsWith('{')) {
    try {
      return usageCounts(JSON.parse(trimmed).usage);
    } catch {
      return null;
    }
  }
  const merged = {};
  let seen = false;
  for (const line of text.split('\n')) {
    if (!line.startsWith('data:')) continue;
    let event;
    try {
      event = JSON.parse(line.slice(5).trim());
    } catch {
      continue; // "[DONE]", or not ours
    }
    for (const u of [event?.usage, event?.message?.usage, event?.response?.usage]) {
      if (!u || typeof u !== 'object') continue;
      for (const [key, value] of Object.entries(u)) if (value !== null) merged[key] = value;
      seen = true;
    }
  }
  return seen ? usageCounts(merged) : null;
}

/**
 * A meter's ledger in total (its lines: a reply each, with its status,
 * whether it had usage, whether the runner cut it short, and its tokens
 * and cost): the tokens, the replies with usage, those that came back
 * whole without any (`missing`: then nothing is known of the cost), and
 * the cost: the provider's when every reply with usage had one, else the
 * tokens at `price` (US dollars a million in and out), else null.
 */
export function ledgerTotals(lines, price) {
  const counted = lines.filter((l) => l.usage);
  const missing = lines.filter((l) => !l.usage && l.status === 200 && !l.cut).length;
  const tokens = counted.reduce((a, l) => a + l.input + l.output, 0);
  const reported = counted.length > 0 && counted.every((l) => typeof l.cost === 'number');
  const priced = price?.inputPerMillion > 0 && price?.outputPerMillion > 0;
  const byPrice = (l) =>
    (l.input * price.inputPerMillion + l.output * price.outputPerMillion) / 1e6;
  const [costUsd, costSource] = missing
    ? [null, 'unknown']
    : reported
      ? [counted.reduce((a, l) => a + l.cost, 0), 'provider']
      : priced
        ? [counted.reduce((a, l) => a + byPrice(l), 0), 'price']
        : [null, 'unknown'];
  return { tokens, replies: counted.length, missing, costUsd, costSource };
}

/**
 * Whether a runner's own count of its tokens (`reported`) falls short of
 * what its provider's replies said (`metered`) by more than `slack`: a
 * runner that under-reports. A runner whose replies never reached the
 * meter, but says it used tokens, went around it.
 */
export function usageGap(reported, metered, { slack = 0.1 } = {}) {
  if (metered.replies === 0 && reported > 0)
    return `the runner says it used ${reported} tokens, and no reply went through the meter`;
  if (reported < metered.tokens * (1 - slack))
    return `the runner says it used ${reported} tokens; its provider's replies say ${metered.tokens}`;
  return '';
}
