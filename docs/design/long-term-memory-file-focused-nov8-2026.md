# Focused recall while preserving supplied file context

Baseline: `6528b37`, branch `ltm-improvements-nov9-2026`.

The existing denser full-pool suite improves from **26/28 to 28/28 development**.
Its exposed reserved split stays **28/28**. The two gains are the existing
`RetrievalQuery` and `MemoryRecord` file-dense cases; no historical case was changed.

## Review and hypothesis

The [hybrid-anchor trials](long-term-memory-hybrid-anchors-nov8-2026.md) rejected
unconditional replacement despite retaining file hints: it displaced requested
crowding observations. The
[focused-symbol review](long-term-memory-focused-symbols-nov8-2026.md) and
[atomic multi-name planner](long-term-memory-multi-symbols-nov8-2026.md) later
provided narrower intent checks, per-name coverage, and protected existing named
facts and exact context titles. They disabled focused replacement whenever files
were supplied. The two remaining dense development misses hit that guard.

The [registered protocol](../../scripts/eval/memory-file-focus-protocol.json)
retries file-hinted recovery using those later safeguards. It protects **every
incumbent matching any supplied file**, not just the leading file result. Only
other trailing slots may cover missing complete names. The protocol and both new
fixtures were frozen before changing production code or scoring trials.

## Selected policy

The existing atomic planner now accepts one to four compound names for requests
with supplied files. Requests without files keep their previous rules. Names may
use spaces, commas or semicolons, with at most five terms and one context term.
The same collection, numeric, wildcard, failure, exact-title and single-slot
guards remain. Every name must have an eligible match and all additions must fit
before any replacement happens.

Every returned record whose stored paths overlap any supplied path is protected
at its original rank. This reuses lexical retrieval's normalized whole-segment
path matching, including Windows separators and relative paths. The leading
record, represented named facts and literal context titles retain their existing
protections. If too few other slots remain, the entire plan is abandoned.
Repository, verification, tags, caller visibility and copy filtering still apply
before each retained lookup bound. Lookup errors preserve incumbent results.

No model, vector, schema, inference call, retrieval cutoff, result limit or packet
budget changes. The native model and its pinned release/download behavior stay
unchanged.

## Additional cases and results

The new 64-case file-focused fixture reuses the same eight exposed source
incidents. Each split has 16 positive cases and 16 negative controls. Positive
cases require the named source claim, a file-context claim, and any explicitly
named peer claims. They cover single names, reversed snake names, two-name lists
and four-name lists. Negatives use unknown names, collection wording, a supplied
failure, and numeric scope. They retain their exact result IDs and order; this is
not an empty-result or precision improvement.

| Hybrid measure                    | Development before → after | Reserved before → after |
| --------------------------------- | -------------------------: | ----------------------: |
| Historical dense complete answers |              26/28 → 28/28 |           28/28 → 28/28 |
| New file-focused complete answers |                0/16 → 8/16 |             1/16 → 8/16 |
| New file-focused primary fact     |               5/16 → 16/16 |           12/16 → 16/16 |
| New file-context fact             |                8/16 → 8/16 |             8/16 → 8/16 |

The eight remaining incomplete answers per split lack the file-context fact
before this planner runs. They remain misses even though the primary fact is
now delivered. Protecting existing file matches cannot restore a file claim
already displaced during initial fusion. Neither the expected facts nor the
complete-answer metric was changed to hide that limitation.

Another 16 retention cases associate every crowding fact with the supplied file.
Each requests eleven facts in an eight-record pool, so these are excluded from
headline recall. They ensure that terse named requests cannot displace existing
file matches when all slots are protected. Every previously supplied required
fact and its rank stays unchanged in both splits.

The 80 added cases produce 160 lexical/hybrid views. Their incidents and query
variants are correlated and exposed. They provide regression and repair evidence,
not blind validation or an independent semantic/live-agent performance estimate.

## Regression checks

All **2,308 historical policy views** were replayed. **2,306 deterministic
outputs remain identical**, excluding latency and timed telemetry. The two
changed historical views are the complete-answer gains above. Complex and snake
identifiers stay 28/28 per split, code lists stay 28/28 per split, long notes stay
35/40 per split, and combined windows stay 34/40 development and 33/40 reserved.

Across all **2,468 views**, there is no previously delivered primary or required
fact lost or ranked lower, no negative identity/order change, prohibited result
or overrun of eight records or 1,000 tokens. Unrequested trailing records can
still change under the focused policy. Fixture retention does not establish
universal precision or guarantee every displaced contextual record is irrelevant.

One fixed production trial was selected on development plus historical development
and crate contracts before reserved scoring. No production rule or benchmark case
was retuned from reserved results.

Unit tests cover a file claim at the last rank, path normalization, multiple
missing names, and an atomic abort when seven file matches leave only one slot
for two additions. Existing trust/tag/repository/caller/deprecation checks now
also exercise a supplied-file request. A pre-existing context-title unit fixture
could randomly exclude its context before the policy ran because nine records
competed for eight slots. Its setup now uses seven crowding records plus the
context, retaining the full eight-record pool and the original rank-seven and
named-fact assertions. Ten separate runs of the nine focused tests passed with
fresh UUIDs.

## Validation and reproduction

The memory crate passes **154 all-target tests**, both explicitly enabled native
ONNX reference tests, its doctest and clippy with warnings denied. All **1,046
frontend tests**, type checking and formatting pass. Rust crates are checked
individually. Concurrent execution makes these timings unsuitable for a latency
comparison.

The [machine-readable report](long-term-memory-file-focused-nov8-2026.json)
records every suite's metrics, changed cases, source and fixture hashes, and
deterministic fingerprints. Historical baselines reuse the preceding selected
runtime with verified identical retrieval source; every candidate view is replayed
afresh. Original fixtures retain their baseline commit contents.

Build the existing Rust `evaluate_memory` example and select the new file-focused
and retention fixtures, then every historical input in the report. Use
deterministic development/heldout splits, the pinned native MiniLM model, 384
dimensions, revision `chroma-minilm-l6-v2-913d7300-title-claim-native-v1`, threshold
0.3, short-query words two and scale 11/12, result limit eight and packet budget
1,000. Compare against `6528b37` and preserve per-fact ranks and negative IDs/order
when evaluating further changes.
