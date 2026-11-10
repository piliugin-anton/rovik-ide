# Captured cause-field visibility experiments

Baseline: `f844552504389031769b9344f90ac0eea313ca1e`.
Branch: `ltm-improvements-nov10-2026`.

Three native experiments found **no accepted recall improvement**. Development
complete delivery remains **3/12** under every trial. All prototypes were removed;
the runtime source is byte-for-byte identical to the accepted baseline. This is a
negative result from the [recommended grounded-context direction](long-term-memory-directions-nov10-2026.md),
not evidence that additional source context necessarily improves matching.

## Capture-path finding

`memory_remember` already accepts an optional cause assertion and appends it as
`\nCause: ...` after the claim. The full stored claim can contain 4,000 characters
before that field, while MiniLM observes only the first 500 title/claim characters.
The existing supplementary view recognizes complete final paragraphs and sentences;
a final labeled line can remain invisible. A captured cause remains the model's
assertion and does not verify its record.

This study tested visibility of an existing field, rather than adding an extractor
or inventing problem descriptions from evaluation questions. The first prototype
kept the existing paragraph/sentence view, then recognized a complete final Cause
field only if it began beyond the encoder window and fit entirely within that
window. Inline labels, multiline tails, empty/oversized fields and visible short
fields could not create another view. Original text, hash, evidence, trust and
primary vectors remained intact. The derived vector used existing claim-bound
index invalidation, cancellation and filtered lookup machinery.

## Frozen cases

The [fixture](../../scripts/eval/memory-cause-field-cases.json) adds **40 cases**
using eight assertions quoted from existing repository reports, with excerpt and
source-file hashes. Four incidents belong to development and four to the reserved
reference split. Each incident has a hidden Cause-line case, an explicit-paragraph
control and a short-record control. Each split therefore has 12 positives and
eight unrelated-domain negative controls.

Assertions undergo whitespace/backtick normalization and are supplied as source
content, not generated from a query. A synthetic repeated background preface
isolates encoder visibility. The questions and assertions were frozen before
baseline/candidate scoring; the [protocol](../../scripts/eval/memory-cause-field-protocol.json)
uses canonical two-space JSON plus a newline for input hashes.

These are newly composed cases from already exposed report incidents. They do
not establish automatic extraction accuracy, blind generalization, authentic
capture distributions or adjacent-topic precision. They also do not replace any
old suite or its denominator.

## Trials

| Native development arm                                  | Lexical complete answers | Hybrid complete answers | Decision         |
| ------------------------------------------------------- | -----------------------: | ----------------------: | ---------------- |
| Baseline                                                |                     3/12 |                    3/12 | Control          |
| Complete labeled Cause view, append after incumbents    |                     3/12 |                    3/12 | Discard: no gain |
| Assertion-only view, remove generic field label         |                     3/12 |                    3/12 | Discard: no gain |
| Independent cause evidence fused before final selection |                     3/12 |                    3/12 | Discard: no gain |

The second trial tested whether the generic label diluted semantic evidence. The
third tested whether append-only placement was responsible for failure under full
pools. Both follow-ups were registered after preceding development results, with
unchanged questions, source assertions, normal cutoff and regression rules. No
candidate was scored on reserved cases. The baseline reserved reference is 6/12;
it did not select a candidate rule.

All runs use native pinned 384-dimensional MiniLM, normal cutoff 0.30, short-query
scale 11/12, eight results and a 1,000-estimated-token packet. The existing
per-record comparator checks full primary/required delivery, identities/hashes,
protected ranks, exact negative returned/rendered/full IDs, eligibility, fallback
and budgets. No gate or expected answer was weakened.

## What the failures show

The development pools incident succeeds in all three variants. The packing
incident is selected but not fully delivered in all three variants: its required
record appears at rank seven/eight in a full pool, with a packet near 1,000 tokens.
The indexing and clone incidents are absent from selected results in all six of
their variants, despite spare result capacity. Thus the nine misses comprise three
related delivery failures and six related matching/selection failures.

Making a field mechanically available did not by itself supply useful additional
native semantic evidence. Short and paragraph controls also fail on those same
incidents. A broad suffix extractor, looser cutoff or another packet-only change
would not follow from this evidence.

The next study should record expected-record and distractor cosine scores for
each primary/auxiliary view alongside lexical admission and selected/full delivery.
That distinguishes weak learned matching from selection and packing, before
choosing another representation or a retrieval-specific encoder. Keep source
assertions and questions independent of score inspection. Evaluate automatic
capture separately and inspect actual model prompts before making agent-quality
claims.

## Validation and artifacts

There are **120 candidate development policy views** across the three trials.
All pass the existing per-record regression comparison, but none adds a complete
answer. The labeled-field prototype also passed all **11 conclusions target
tests**, including two added contracts for hidden-field delivery without trust
promotion and rejection of malformed/oversized fields. Those new tests were
removed with the rejected feature; unchanged existing tests remain intact.

After restoration, **350 original/fresh/new development policy views** reproduce
baseline case outcomes exactly, including non-timing telemetry. The runtime source
is restored byte-for-byte. No prototype proceeded to a full historical/oversized
matrix or candidate reserved scoring; broader tests remain skipped under the
user's instruction.

The [machine report](long-term-memory-cause-fields-nov10-2026.json) contains the
protocol, patches, trial metrics, categories, changed-case evidence, output hashes,
miss classification and restoration checks. Reproduce through the existing native
`evaluate_memory --experiment-config` runner using the new dataset, deterministic
split, 1,000-token allowance and the pinned model configuration above. Test a
prototype against the fixed baseline with `scripts/eval/memory-regressions.mjs`.
