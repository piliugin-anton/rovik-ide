# Literal short suffixes in complete code names

Baseline: `6806901`. Branch: `ltm-improvements-nov9-2026-2`.

On the unchanged harder file-coverage suite, development complete answers improve
from **6/32 to 32/32**. The exposed reserved split stays **30/32**. The 26 gains
are correlated cases whose explicit name lists include `OnnxMiniLm`, rather than
26 independent semantic validations. No cases or expected facts changed.

## Review and selected policy

The [previous experiment](long-term-memory-file-coverage-nov9-2026.md) restored
missing file coverage while preserving named-fact ranks. Its new source-grounded
cases exposed another limit: complete-name lookup rejected every short component.
`OnnxMiniLm` therefore failed to match the prose title `ONNX Mini LM`; a composed
request could not complete its atomic name plan either.

The [registered protocol](../../scripts/eval/memory-short-suffix-protocol.json)
permits a non-generic two-letter **terminal** component only after at least two
distinct longer distinctive components. Every component remains in the indexed
conjunctive query: `OnnxMiniLm` requires `onnx AND mini AND lm`. It cannot match
`ONNX Mini DB` or a claim containing only the two longer anchors.

CamelCase, qualified and snake spellings use the same rule. Middle short
components such as `HttpIOState`, single-letter or numeric fragments, generic
components, and names with only one distinct long anchor retain their previous
rejection. The ordinary relevance gate and free-text tokenizer are unchanged.
Common two-letter function words now appear explicitly in the generic-word list;
the former length rule excluded them implicitly. New unit probes caught this
requirement before reserved scoring, including `OnnxMiniTo`, `OnnxMiniIn` and
`OnnxMiniOr`.

This reuses the existing complete-name collector and its repository, trust, tag,
caller and copy filtering. Focused scope, atomic name coverage, file coverage,
incumbent-rank protection and packet limits remain unchanged. No aliases, model,
vector, schema, inference call, cutoff or release asset changes are needed.

## Results and regression checks

| Hybrid suite             | Development before → after | Reserved before → after |
| ------------------------ | -------------------------: | ----------------------: |
| Harder file coverage     |               6/32 → 32/32 |           30/32 → 30/32 |
| Prior file-focused cases |              16/16 → 16/16 |           16/16 → 16/16 |
| Dense full-pool cases    |              28/28 → 28/28 |           28/28 → 28/28 |
| Code lists               |              28/28 → 28/28 |           28/28 → 28/28 |
| Long notes               |              35/40 → 35/40 |           35/40 → 35/40 |
| Combined windows         |              34/40 → 34/40 |           33/40 → 33/40 |

All **2,708 existing policy views** were replayed with the final implementation.
There are no lost delivered primary or required facts, worse fact ranks,
negative identity/order changes, prohibited results or eight-record/1,000-token
overruns. Reserved output remains unchanged; the two harder reserved misses
remain visible. Negative pools are compared by IDs and order, without assuming
they should be empty.

The accepted development replay and unchanged crate contracts passed before
reserved evaluation. Earlier uncontrolled-clock outputs were discarded. No fixture
claim, expected answer, cutoff or name rule was retuned from reserved outcomes.

## Controlling fixture age

An initial development check changed the IDs in one overflow negative. Running
the unchanged baseline reproduced the same difference: its fixed November 7 claim
timestamps had aged across a UTC-day boundary. The common recency bonus changed
floating-point ties in ordinary lexical ranking. This was independent of suffix
matching, so accepting a changed negative pool would have hidden a harness defect.

The deterministic evaluator now places all claim timestamps 36 hours before a
case starts. Whole-day recency stays one, with twelve hours of margin, and all
claims in a case share the same timestamp. Production time/scoring behavior is
unchanged. A unit test checks calendar boundaries and timestamp formatting.

Baseline retrieval and the candidate are rebuilt with that same evaluator and
all 2,708 comparisons are executed afresh. Acceptance uses only these controlled
outputs; archived outputs and the unchanged-code reproduction remain in the report.
Preliminary reserved work started before the gate completed was stopped and
discarded; it did not select the name rule or clock policy.

## Validation and limits

The memory crate passes **161 all-target tests**, both explicitly enabled native
ONNX tests, its doctest and clippy with warnings denied. Frontend tests, type
checking and formatting pass. Rust crates were checked separately. New tests
verify literal suffix matching against wrong-suffix, missing-suffix and foreign
records, plus generic, numeric, middle-short and repeated-anchor rejection. All
existing tests are retained.

The Windows CI fixture was fixed separately in `0d1656d`: seven crowding records
and the wanted record fill the semantic pool, eliminating a single-pool ranking
tie without removing the full-pool or 100-alternative protections. Its targeted
test passed 100 local runs with fresh UUIDs; actual Windows CI remains the
platform-specific confirmation.

The [machine-readable report](long-term-memory-short-suffix-nov9-2026.json)
records all suite metrics, changed cases, source and input hashes, deterministic
fingerprints and validation. Baseline retrieval and candidate outputs are both
executed afresh with the shared clock fix. Reproduce with the existing native
`evaluate_memory` example, frozen fixtures, deterministic splits, pinned
384-dimension MiniLM, threshold 0.3, short-query words two and scale 11/12,
eight results and 1,000 tokens.

These are exposed, correlated repair cases, with no independent semantic,
multilingual, precision or live-agent gain claimed. Unrequested trailing context
can change under focused coverage. Concurrent checks do not establish a latency
improvement.
