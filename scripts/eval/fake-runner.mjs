#!/usr/bin/env node
// P08 (docs/design/evolve-plan.md): a runner built to under-report its
// usage, for the dry run. It takes the runner's arguments, asks its model
// three times through the meter (`--meter`, as a real runner must: the
// meter holds the key), changes nothing, and writes a report claiming a
// third of the tokens. The grader must count by the meter, and catch it.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';

const argv = process.argv.slice(2);
const flag = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
};
const settings = JSON.parse(readFileSync(flag('--settings'), 'utf8'));
const base = new URL(settings.profile.baseUrl);
const origin = flag('--meter') ?? base.origin;
const url = `${origin}${base.pathname.replace(/\/$/, '')}/chat/completions`;
let tokens = 0;
for (let i = 0; i < 3; i++) {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: 'Bearer none' },
    body: JSON.stringify({
      model: settings.profile.model,
      messages: [{ role: 'user', content: flag('--prompt') ?? '' }],
    }),
  });
  const usage = (await res.json()).usage ?? {};
  tokens += (usage.prompt_tokens ?? 0) + (usage.completion_tokens ?? 0);
}
const claimed = Math.round(tokens / 3);
const out = flag('--out');
mkdirSync(dirname(out), { recursive: true });
writeFileSync(
  out,
  JSON.stringify({
    outcome: 'done',
    // As the real runner's report names it (the notebook tells a scripted run by it).
    model: settings.profile.model,
    ms: 1,
    usage: { totalTokens: claimed, peakLeadContext: 0 },
    costUsd: claimed / 1e6,
    costSource: 'price',
    interventions: 0,
    toolCalls: 0,
    transcript: [],
    runLog: [],
  })
);
