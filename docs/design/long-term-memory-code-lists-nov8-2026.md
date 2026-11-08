# Harder multi-fact cases and explicit code lists

Baseline: `7ab2d56`, branch `ltm-improvements-nov9-2026`.

The new list suite improves from **8/28 to 28/28 complete answers** on both
development and exposed reserved splits. These are new cases, reported separately
from the historical suites. All **2,100 historical policy views** retain identical
deterministic outputs, excluding latency and timed telemetry.

## Why these cases

The [focused-symbol review](long-term-memory-focused-symbols-nov8-2026.md)
rejected broad tail replacement, wider pools, symbol RRF and MMR because they
could displace requested facts. The
[multiple-symbol experiment](long-term-memory-multi-symbols-nov8-2026.md)
subsequently reached 4/4 complete answers per split on its three-fact subset by
planning coverage atomically and protecting existing named facts and literal
context titles. This experiment extends that successful subset.

The [registered protocol](../../scripts/eval/memory-code-list-protocol.json) and
both new fixtures were frozen before changing production code or scoring trials.
Each split contains four existing source incidents, with these harder views:

| Positive category        | Cases per split | Required facts per case | Baseline → candidate |
| ------------------------ | --------------: | ----------------------: | -------------------: |
| Three names, spaces      |               4 |                       3 |            4/4 → 4/4 |
| Four names, spaces       |               4 |                       4 |            4/4 → 4/4 |
| Three names, commas      |               4 |                       3 |            0/4 → 4/4 |
| Three names, semicolons  |               4 |                       3 |            0/4 → 4/4 |
| Four names, commas       |               4 |                       4 |            0/4 → 4/4 |
| Qualified names, commas  |               4 |                       3 |            0/4 → 4/4 |
| Repeated mention, commas |               4 |                       3 |            0/4 → 4/4 |

The table applies independently to development and reserved results. Each split
also has 12 negative controls: unknown names, explicit collection wording and
numeric requests. Negative controls preserve returned record IDs **and order**;
they are not required to return an empty pool.

The corpus retains eight source facts, nine distinct crowding observations,
twelve duplicate observations and an unrelated file-context fact. These exercise
full pools, distinct-claim capacity and atomic multi-fact delivery. Another 12
retention controls per split request all nine crowding observations alongside the
source fact. Ten facts cannot fit eight records: those controls are excluded from
headline recall and instead guard every previously delivered fact and its rank.

Together, the additions contain **80 list cases and 24 retention controls**, or
208 new views across lexical and hybrid policies. The fixtures reuse the same
eight exposed incidents. Their correlated query variants do not establish
independent semantic, multilingual or live-agent performance.

## Selected change

Previously, the focused multi-name planner split only on whitespace. A compact
list such as `SharedMemoryStore,RetrievalQuery,MemoryRecord crowding` was treated
as one ambiguous term and rejected. The planner now splits on commas and
semicolons as well as whitespace. Its exact-context-title protection uses that
same parser, including when the literal context title follows a comma.

The existing limits apply after splitting: at most five nonempty terms, two to
four distinct compound names and at most one other term. Repeated mentions need
no extra slot. Qualified names use the existing identifier lookup. Collection
wording, numbers, wildcards, file/failure context, exact incumbent titles and
single-slot requests keep their existing protections. An unknown name or
insufficient slots aborts the whole plan; existing named facts and context titles
retain their ranks. No model, inference, index, schema, cutoff or packet budget
changes are needed.

This was one development trial. The new development cases, every historical
development view and the unchanged crate contracts passed before reserved
evaluation. No fixture or rule was retuned from reserved outcomes. Unit tests
cover compact lists, qualified names, duplicate mentions, protected literal
context and unknown/collection/numeric/wildcard/narrative fallback behavior.

## Regression checks and limits

The 2,100 historical views are unchanged, including the denser full-pool suite
(26/28 development, 28/28 reserved), complex and snake identifiers (28/28 each),
long notes (35/40 each), and combined windows (34/40 development, 33/40 reserved).
The remaining historical misses remain visible; the new suite's 100% result does
not replace those scores. Lexical output, every previously supplied primary and
required fact and rank, negative identities/order, scope, trust, tags and caller
visibility remain protected. All 2,308 views stay within eight records and 1,000
tokens, with no prohibited result.

There are 40 new complete-answer gains, with no measured regression. Focused
coverage can still replace unrequested trailing records. The fixtures establish
retention of their requested facts, not universal precision for real requests.
Concurrent checks make timings unsuitable for a latency comparison. No general
recall or false-injection improvement beyond these explicit list cases is claimed.

## Validation and reproduction

The memory crate passes 152 all-target tests, two explicitly enabled native ONNX
tests, its doctest and clippy with warnings denied. All 1,046 frontend tests, type
checking and formatting pass. Rust crates were checked individually.

The [machine-readable report](long-term-memory-code-lists-nov8-2026.json)
contains all suite metrics, changed cases, deterministic fingerprints, source
hashes and frozen/committed fixture hashes. Historical baselines reuse the previous
selected runtime, verified against its recorded retrieval-source hash; all
candidate views were executed afresh.

Build the existing Rust `evaluate_memory` example. Use the two new fixtures,
deterministic development/heldout splits, the pinned native MiniLM model with 384
dimensions, revision `chroma-minilm-l6-v2-913d7300-title-claim-native-v1`, threshold
0.3, short-query words two and scale 11/12, result limit eight and packet budget
1,000. Compare against `7ab2d56` and replay every historical regression input in
the machine-readable report before accepting another change. The model release
and download behavior are unchanged.
