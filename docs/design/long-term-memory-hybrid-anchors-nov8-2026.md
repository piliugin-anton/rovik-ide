# Hybrid symbol anchors and retention experiments

Baseline: `a7195a2`. Branch: `memory-improvements-nov8-2026`.
The [protocol](../../scripts/eval/memory-hybrid-anchor-protocol.json) targets known
misses in an already exposed synthetic corpus. These are repair/regression
experiments, not a fresh blind validation or evidence of agent task success.

## Free hybrid slots

The protected lexical/file lookup is unchanged. After the incumbent hybrid
fusion and diagnostic view, a bounded FTS lookup can fill free slots with
complete conjunctive identifier matches. Known copies and caller-ineligible
records are excluded before the pool limit. Project, verification and tag
filters use the existing FTS collector. Exact task/failure title matches retain
protection; existing results and ranks precede supplements. A supplementary
lookup error preserves the successful incumbent result. Strict vector search
still enforces its cosine cutoff; hybrid already admits independent lexical
evidence, and this adds another complete-symbol lexical view.

| Complex corpus / policy | Before | After |
| ----------------------- | ------ | ----- |
| Development lexical     | 16/28  | 16/28 |
| Heldout lexical         | 16/28  | 16/28 |
| Development hybrid      | 24/28  | 24/28 |
| Exposed heldout hybrid  | 24/28  | 26/28 |

The added answers are `complex-CommandOutput-file-noise` and
`complex-CommandOutput-filtered`. No supplied primary or composed-answer member
is lost or ranked lower; negative returned IDs and ordering remain unchanged.
The first experiment leaves full pools untouched. The 792 earlier replays retain
all outcomes. The fixed eight-result and 1,000-token limits, native preprocessing,
model weights, stored claims, index schema and release assets are unchanged.

## Per-fact retention controls

The new [retention fixture](../../scripts/eval/memory-hybrid-retention-cases.json)
adds eight deliberately capacity-infeasible tasks. Each explicitly asks to retain
nine distinct crowding observations alongside the requested symbol: ten facts
cannot all fit in eight result slots. These cases do not enter recall headlines.
They test whether replacing a returned item loses a previously supplied fact,
even when the total number supplied stays constant. The free-slot change keeps
all prior fact identities and ranks in these controls.

The separately registered tail-replacement experiment was run after the free-slot
gain was committed and pushed as `bfd35d8`. With no complete symbol in a full
hybrid pool, it replaced the final result with a complete symbol candidate,
while preserving explicit files and exact titles. On the exposed complex corpus,
hybrid delivery rose to 28/28 in both splits.

That headline is rejected. Four development retention cases and two heldout
retention cases lost an already supplied crowding observation. Total supplied
fact counts stayed at eight: one requested fact was traded for another. The
per-fact checks caught what aggregate delivery counts would miss. No budget,
fixture, label, cutoff or existing test was changed to make the prototype pass.
The prototype was removed and the committed free-slot source restored exactly.
Its patch and measured losses are recorded in the report for reproducibility.
Full-pool misses remain open; no tail replacement is enabled.

## Verification and reproduction

Changed-crate tests run separately. Real native model tests, evaluator tests,
strict clippy, frontend tests, typecheck and format checks passed for the free-slot
change. The [machine-readable artifact](long-term-memory-hybrid-anchors-nov8-2026.json)
binds inputs, model files, code snapshots, outcome changes and verification.

Use the existing Rust `evaluate_memory` example and the dataset paths in the
protocol. Keep deterministic fixtures, an eight-record limit, a 1,000-token
budget, short-query words 2 and scale 11/12. Hybrid uses the pinned native model,
384 dimensions, its existing revision and threshold 0.3. Clear
`ROVIK_MEMORY_EVAL_*` variables to score lexical retrieval. Both legacy splits
are exposed and reported separately. New fixture registration hashes use
`JSON.stringify(parsed, null, 2)` plus a final newline; formatter-only reflow must
preserve the reconstructed registration hash and parsed data.
