import fs from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Compare frozen memory cases using record identity and actual packet delivery. */
export function auditMemoryResults(baseline, candidate, { memoryBudget = 1000 } = {}) {
  if (!Number.isSafeInteger(memoryBudget) || memoryBudget < 0)
    throw new Error('Invalid packet budget');
  const errors = [];
  const gains = [];
  const fail = (id, kind, fact) => errors.push({ id, kind, ...(fact ? { fact } : {}) });
  const rows = (result, side) => {
    const map = new Map();
    for (const c of result.cases) {
      if (map.has(c.id)) fail(c.id, `${side}:duplicate-case`);
      map.set(c.id, c);
      if (
        !Array.isArray(c.required_facts) ||
        !Array.isArray(c.rendered_ids) ||
        !Array.isArray(c.full_text_ids)
      ) {
        fail(c.id, `${side}:missing-packet-evidence`);
        continue;
      }
      if (
        c.prohibited !== false ||
        c.embedding_fallback !== false ||
        typeof c.answerable !== 'boolean' ||
        !Number.isSafeInteger(c.returned) ||
        c.returned < 0 ||
        c.returned > 8 ||
        !Number.isSafeInteger(c.packet_tokens) ||
        c.packet_tokens < 0 ||
        c.packet_tokens > memoryBudget
      )
        fail(c.id, `${side}:safety`);
      if (c.returned !== c.returned_ids.length || new Set(c.returned_ids).size !== c.returned)
        fail(c.id, `${side}:invalid-returned-ids`);
      if (
        c.rendered_ids.some((id) => !c.returned_ids.includes(id)) ||
        c.full_text_ids.some((id) => !c.rendered_ids.includes(id)) ||
        new Set(c.rendered_ids).size !== c.rendered_ids.length ||
        new Set(c.full_text_ids).size !== c.full_text_ids.length
      )
        fail(c.id, `${side}:invalid-packet-trace`);
      const primaryIndex = c.returned_ids.indexOf(c.primary_id);
      if (
        typeof c.primary_claim_hash !== 'string' ||
        c.rank !== (primaryIndex < 0 ? null : primaryIndex + 1)
      )
        fail(c.id, `${side}:invalid-primary-evidence`);
      if (c.primary_delivered !== c.full_text_ids.includes(c.primary_id))
        fail(c.id, `${side}:invalid-primary-delivery`);
      const ids = new Set();
      for (const fact of c.required_facts) {
        if (typeof fact.claim_hash !== 'string') fail(c.id, `${side}:missing-claim-hash`, fact.id);
        if (ids.has(fact.id)) fail(c.id, `${side}:duplicate-required-fact`, fact.id);
        ids.add(fact.id);
        if (
          (fact.rank !== null && c.returned_ids[fact.rank - 1] !== fact.id) ||
          fact.delivered !== c.full_text_ids.includes(fact.id)
        )
          fail(c.id, `${side}:invalid-fact-evidence`, fact.id);
      }
      const delivered =
        Number(c.primary_delivered) + c.required_facts.filter((f) => f.delivered).length;
      if (
        c.required_count !== 1 + c.required_facts.length ||
        c.required_delivered !== delivered ||
        c.answer_delivered !== (delivered === c.required_count)
      )
        fail(c.id, `${side}:invalid-answer-evidence`);
    }
    return map;
  };
  const before = rows(baseline, 'baseline');
  const after = rows(candidate, 'candidate');
  for (const [id, p] of before) {
    const c = after.get(id);
    if (!c) {
      fail(id, 'missing-case');
      continue;
    }
    if (
      c.answerable !== p.answerable ||
      c.primary_id !== p.primary_id ||
      c.primary_claim_hash !== p.primary_claim_hash
    )
      fail(id, 'case-definition-changed');
    if (!p.answerable) {
      for (const key of ['returned_ids', 'rendered_ids', 'full_text_ids'])
        if (JSON.stringify(c[key]) !== JSON.stringify(p[key])) fail(id, `negative-${key}`);
    }
    if (p.primary_delivered && (!c.primary_delivered || c.rank > p.rank))
      fail(id, 'primary-loss-or-rank');
    if (p.answer_delivered && !c.answer_delivered) fail(id, 'complete-answer-loss');
    const facts = new Map((c.required_facts ?? []).map((f) => [f.id, f]));
    for (const fact of p.required_facts ?? []) {
      const next = facts.get(fact.id);
      if (!next) {
        fail(id, 'missing-required-fact', fact.id);
        continue;
      }
      if (fact.claim_hash !== next.claim_hash) fail(id, 'required-claim-changed', fact.id);
      if (fact.delivered && !next.delivered) fail(id, 'required-delivery-loss', fact.id);
      if (fact.rank !== null && (next.rank === null || next.rank > fact.rank))
        fail(id, 'required-rank-loss', fact.id);
    }
    if (facts.size !== (p.required_facts ?? []).length) fail(id, 'required-facts-changed');
    if (p.answerable && !p.answer_delivered && c.answer_delivered) gains.push(id);
  }
  for (const id of after.keys()) if (!before.has(id)) fail(id, 'unexpected-case');
  return { safe: errors.length === 0, cases: before.size, gains, errors };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [baselinePath, candidatePath, budget = '1000'] = process.argv.slice(2);
  if (!baselinePath || !candidatePath)
    throw new Error(
      'usage: node scripts/eval/memory-regressions.mjs baseline.json candidate.json [budget]'
    );
  const result = auditMemoryResults(
    JSON.parse(fs.readFileSync(baselinePath)),
    JSON.parse(fs.readFileSync(candidatePath)),
    { memoryBudget: Number(budget) }
  );
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (!result.safe) process.exitCode = 1;
}
