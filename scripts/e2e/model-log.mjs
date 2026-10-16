// What the app sent the fake model, one line per request: the newest
// message's role and what it says (a user's message: its end, after the
// context put before it). Tool results show what the agent's tools did (a
// write that failed, a command's output), which the page may not.
//
//   node scripts/e2e/model-log.mjs e2e-out/fake-model.jsonl
import { readFileSync } from 'node:fs';

const file = process.argv[2] ?? 'e2e-out/fake-model.jsonl';
let text = '';
try {
  text = readFileSync(file, 'utf8');
} catch (e) {
  console.log(`(no ${file}: ${e.message})`);
  process.exit(0);
}

const flat = (content) =>
  Array.isArray(content)
    ? content.map((part) => part?.text ?? `[${part?.type ?? 'part'}]`).join(' ')
    : String(content ?? '');

for (const line of text.split('\n')) {
  if (!line.trim()) continue;
  let entry;
  try {
    entry = JSON.parse(line);
  } catch {
    continue;
  }
  const messages = entry.body?.messages;
  if (!Array.isArray(messages) || messages.length === 0) continue;
  const last = messages[messages.length - 1];
  const time = String(entry.t ?? '').slice(11, 19);
  const said = flat(last.content).replace(/\s+/g, ' ').trim();
  const shown = last.role === 'user' ? said.slice(-160) : said.slice(0, 400);
  console.log(`${time} ${last.role}: ${shown}`);
}
