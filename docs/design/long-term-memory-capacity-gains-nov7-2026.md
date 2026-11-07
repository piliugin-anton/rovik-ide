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
