# Memory recall: capacity experiments

The frozen semantic-recall corpus remains a control. This campaign tests two
different failure mechanisms in production retrieval and delivery, using new
constructed fixtures. These measure capacity under crowding, not generalization
to unseen user tasks or real-agent answer quality. Each gain is validated and
pushed separately.

## Experiment 1: continue packing after an oversized record

The old renderer stopped at the first record that did not fit. A long source
episode could therefore hide every shorter claim after it, even with most of
the packet allowance unused. Seven new data fixtures isolate this from retrieval:
they supply the same admitted records in the same order to both renderers.

The initial renderer supplied **3/7 required claims**. The revised renderer
supplies **7/7**. Complete claims retain their relative rank; remaining space
goes to fetch pointers for omitted records. Original text, evidence and IDs are
preserved, and no new candidates are admitted. Very small allowances also exposed
an existing overrun: the header and cut notice could exceed the entire budget.
Every emitted part now counts against the allowance.

Regression coverage includes oversized first and middle records, fetch hints
competing with complete claims, an unchanged complete packet, zero/tiny budgets,
and Unicode text across 300 different allowances. Traced IDs must appear in the
packet; an omitted record remains fetchable when its pointer fits.

Reproduce the constructed mechanism evaluation from `src-tauri/`:

```sh
cargo run --locked -p verified-code-memory --example evaluate_memory_capacity
cargo test --locked -p verified-code-memory --all-targets
```

The original corpus and hard-negative controls are rerun separately with
`evaluate_memory`. No semantic model, synonym dictionary, manual contextual key
or relevance threshold is added by this experiment.

The [machine-readable comparison](long-term-memory-packing-gain-nov7-2026.json)
records both capacity runs and the unchanged 132 original outcomes. Development
Recall@8 remains 58/70 and held-out Recall@8 remains 24/30. With the frozen hard
negatives included, no-match injection remains 2/22 on development and 3/10 on
held-out; this change does not solve those existing semantic/admission problems.

Before the first commit, all 88 memory tests and the memory doctest passed;
Clippy passed with warnings denied. All 1,044 frontend tests, type checking and
format checking passed. The full core unit run passed 2,572 tests with 27 existing
ignored tests. Its one failure was an environment issue: `/usr/bin/go` was a
different program, not the Go compiler. That exact test passed after provisioning
and verifying Go 1.25.4. All 20 core integration tests passed, and the core
doctest command passed with its existing ignored example. Command-execution tests
ran outside the nested agent sandbox after an isolated comparison established
that the sandbox itself blocked their child commands. No tests were weakened.

## Experiment 2: fill candidate pools after visibility and exact deduplication

The first fix was committed and pushed as `b7bfd26` before this implementation.
The previous SQLite queries cut off at 80 lexical rows and 40 file rows before
the caller could reject inaccessible/stale records or collapse exact copies.
Thus a hundred copies of one incident could exclude an independent answer even
though the final result list had spare slots. Raising the final result limit
cannot recover a record absent from those pools.

Seventeen new fixtures measure this at 100, 1,000 and 10,000 crowding records,
for both lexical and explicit-file retrieval. They include exact duplicates,
records hidden by a caller predicate, another project, excluded tags, deprecated
records, abstention and whole-file-segment boundaries. The initial implementation
recalled and delivered **9/21 required claims**; the revised implementation
recalls and delivers **21/21**, with no extra returned claims in any fixture.
The full stored history remains byte-for-byte equivalent as serialized records,
and repeat queries preserve their order.

The indexed scan now fills pools of 80/40 **distinct admissible claims** rather
than raw rows. Scoring and the existing score/time/ID tie-break choose the best
copy, including with custom outcome weights. Final ranking and the result limit
remain in place. There is no new keyword, synonym, semantic or incident-family
admission rule. Source and evidence differences still keep records distinct.

This trades more scanning for correct recall in heavily restricted or repetitive
histories. The constructed 10,000-row lookups took approximately 0.2–0.3 seconds
on this machine, excluding fixture insertion. These are individual local replay
timings, not a latency distribution or production benchmark. Retained pools stay
bounded, but an all-hidden or all-duplicate history can require scanning every
matching row. Telemetry now counts rows inspected, including overlaps between
the two pools.

Reproduce with:

```sh
cd src-tauri
cargo run --locked -p verified-code-memory --example evaluate_memory_pools
cargo test --locked -p verified-code-memory --all-targets
```

The [machine-readable comparison](long-term-memory-pool-gain-nov7-2026.json)
records all capacity cases, timings, safety checks and the frozen control rerun.
All 132 original outcomes are unchanged again, including delivered recall,
ranking, returned counts, packing sizes and prohibited-project checks. Before
the second commit, all 90 memory tests, the memory doctest and all 83 core tests
selected by the memory filter passed. Clippy passed with warnings denied; all
1,044 frontend tests, type checking and format checking passed again.
