# Snake identifier recall

Baseline: `13ad4a5`, branch `memory-improvements-nov8-2026`.

The previous compound lookup supported CamelCase. SQLite already tokenizes
underscores in ordinary FTS phrases, but phrase lookup depends on word order:
`shared_memory_store` can match “Shared memory store”, while
`store_memory_shared` cannot. The fallback now accepts alphabetic snake_case and
UPPER_SNAKE_CASE names as complete conjunctions, using the same strict component,
name-length and candidate bounds as CamelCase. Ordinary literal/phrase lookup
still runs first. No stored aliases, claim edits, schema changes, model weights,
preprocessing or thresholds are added.

An entire fallback name is rejected if it has empty, numeric, short or generic
components. Leading, trailing or doubled underscores therefore cannot create
partial fallback matches. This does not suppress an existing ordinary exact
phrase match. The former internal “snake format unsupported” assertion is now
an explicit all-components positive test; generic and malformed rejection and
all existing literal/file protection tests remain covered.

The [80-case fixture](../../scripts/eval/memory-snake-identifier-cases.json)
extends the exposed source symbols with ordered controls, reversed and qualified
names, uppercase variants, competing text/file context, composed answers and
adjacent/generic/unrelated negatives. Four source incidents per split generate
correlated views; these are mechanism checks, not 80 independent human judgments
or a blind semantic benchmark. The
[protocol](../../scripts/eval/memory-snake-identifier-protocol.json) is frozen
before trials, with the previous eight-record and 1,000-token limits.

| Policy / split            | Baseline complete answers | Candidate complete answers |
| ------------------------- | ------------------------- | -------------------------- |
| Lexical / development     | 4/28                      | 24/28                      |
| Lexical / exposed heldout | 4/28                      | 24/28                      |
| Hybrid / development      | 27/28                     | 28/28                      |
| Hybrid / exposed heldout  | 28/28                     | 28/28                      |

The hybrid development gain is `snake-RetrievalQuery-composed`. No supplied
primary or additional required fact is lost or ranked lower. All 24 negative
controls keep their record IDs and order. Four unrelated controls per split
still abstain; the other eight retain their background context. Lexical
file-context cases remain unresolved because their deliberate protection is
unchanged. No full-pool eviction is introduced.

All **968 earlier replays** cover original, fresh, diagnostic, CamelCase,
complex-context and per-fact retention fixtures under both lexical and hybrid
policies. Their output guards and the original literal/file tests remain in
force. The earlier full-pool misses remain open; no tail replacement is enabled.
The [machine-readable report](long-term-memory-snake-identifiers-nov8-2026.json)
records changed cases, fingerprints, guards and validation.

Reproduce with the native Rust `evaluate_memory` example and
`dataset_file: "scripts/eval/memory-snake-identifier-cases.json"`,
`deterministic: true`, `memory_budget: 1000`, `short_query_words: 2`,
`short_query_scale: 0.9166666666666666`, and the desired split. For hybrid use the
pinned native model, 384 dimensions, its existing revision and threshold 0.3.
Clear `ROVIK_MEMORY_EVAL_*` variables for lexical retrieval. Paths are relative
to the process working directory. The reference binary uses unmodified baseline
code. Fixture registration uses two-space `JSON.stringify` plus a final newline;
formatter reflow must preserve that reconstructed hash and parsed data.
