# Long-term memory: full plan review, November 7, 2026

This review compares all six implementation stages with the code on
`long-term-memory-improvement-nov7-2026`, starting from `779e19a`. It covers
runtime callers, storage, tools, UI, migration behavior, regression fixtures,
and evaluation/rollout criteria. It is a code and acceptance review; deployment
and empirical model quality require additional evidence described below.

## Findings corrected during review

1. **Write leases were incorrect after clear and pause.** Fresh recall now captures
   the current generation; paused runs retain their original generation. A clear
   during a wait cannot grant an old writer a new lease. Generation admission,
   duplicate detection and insertion run in one transaction. Insertion joins that
   transaction rather than opening a nested transaction. Regressions cover a new
   run after clear, obsolete writers and source/vector clear races.
2. **Cached packets could outlive user changes.** Before a model request, the run
   reloads only its selected IDs. Edits, deletion, disablement and source changes
   rebuild or remove the packet. This is an ID refresh, not another retrieval or
   model call. A regression pauses at an actual question, deletes a memory, and
   inspects the next model request to ensure the memory is absent.
3. **Source applicability was not checked by the runtime.** Captured claims and
   procedures now carry a version of their associated files, including dirty
   changes. Missing/changed files prevent a memory from steering the model;
   unrelated source changes do not invalidate it. Git revision tags retain audit
   context where available. Historical records without source versions remain
   explicitly historical; no source state is invented during migration.
4. **Eligibility was applied too late.** Visibility and applicability now filter
   lexical and semantic candidates before their result limits. An eight-record
   set of ineligible stronger matches cannot starve an eligible record.
5. **Run success was still a ranking reward.** Counters remain for association
   reporting, but their default ranking weight is zero. Passing a run is not
   evidence that an arbitrary recalled assertion helped or was true.
6. **Memory tool descriptions crowded small windows.** The schema descriptions
   are shorter, while keeping the historical-evidence, provisional-claim and
   current-code requirements. The existing small-window tests remain intact.
7. **Test cleanup raced the next run.** The harness now waits for registry release
   after `runEnded`, using the registry condition rather than a fixed delay. This
   prevents “already working in this chat” failures on immediate subsequent runs.
8. **Future schemas could be opened for writing.** SQLite has an explicit schema
   version; a newer version is rejected before changing its contents. Existing
   migrations remain additive and repeatable. A regression checks preservation
   of a newer database's version and data.
9. **Management was unbounded.** The manager reads 50 rows per stable
   `(updated_at, id)` cursor and resets navigation after search or mutations.
   Store and component tests check page boundaries, navigation and scope.
10. **Capture and indexing needed independent bounds.** Structured capture allows
    at most three claims per run, retains its count across pause, rejects oversized
    inputs and recognizable credential values, and remains provisional. Automatic
    durable text scrubs recognizable credentials. Index backfill writes at most
    three changed claims per job; only one job per project runs at once.
11. **Semantic result lifecycle was absent.** Vectors are now a rebuildable SQLite
    derivative keyed by record ID, model revision and claim hash. Source edits
    suppress stale vectors; deleting/clearing/pruning sources cascades deletion.
    Late embeddings cannot restore cleared sources, and no embedding model call
    holds the SQLite store lock.

## Acceptance matrix

| Stage and acceptance item                                                 | Code and test evidence                                                                                      | Review conclusion                                                                                                                            |
| ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| PR 1: unrelated query abstains; stronger BM25 match wins                  | `verified-code-memory/src/retrieval.rs`, `tests/retrieval_ranking.rs`, `tests/retrieval_cases.rs`           | Implemented; trust cannot admit unrelated records.                                                                                           |
| PR 1: exact-file retrieval independent of text and recency                | `store.rs::file_candidates`, normalized `memory_files`, older-than-200 fixture                              | Implemented.                                                                                                                                 |
| PR 1: scope/trust exclusion before slots                                  | SQL query filters plus `retrieve_filtered_with_telemetry`; semantic eligibility regression                  | Implemented in both pools.                                                                                                                   |
| PR 2: unrelated checks cannot promote or contradict a claim               | `RunMemory::check_plan`, `tests/evidence.rs`, memory e2e checks                                             | Implemented; nearby failures request revalidation.                                                                                           |
| PR 2: edits cannot inherit evidence; uses cannot manufacture freshness    | Claim hash/validated hash and split timestamps; evidence tests                                              | Implemented; association ranking reward disabled by default.                                                                                 |
| PR 2: relevant changes invalidate applicability; unrelated commits do not | `scoped_version`, `fingerprint_matches`, source-change regression                                           | Implemented for newly versioned records. Unknown legacy provenance remains unknown.                                                          |
| PR 2: compatibility and atomic transitions                                | Explicit schema version, future-schema preservation test, migration/consolidation tests                     | Additive upgrades and future-writer rejection implemented. Cross-platform upgrades remain CI responsibilities.                               |
| PR 3: context arrives before the decision and stays bounded               | `render_packet_traced`, scripted first-request/read/failure/child tests                                     | Implemented.                                                                                                                                 |
| PR 3: on-demand search/get respects scope and disablement                 | Tool registry/dispatch plus connector/core tests; filtered retrieval                                        | Implemented; semantic mode uses the same visibility predicate.                                                                               |
| PR 3: pause/compaction/current cache state                                | `MemoryCarry`, packet ID refresh, delete-while-waiting regression                                           | Implemented; no repeated model retrieval call when selected IDs are unchanged.                                                               |
| PR 3: real use is distinguished from availability                         | Retrieved/shown/read-on-request instrumentation and neutral run associations                                | Implemented for supplied/read states. Causal benefit and validated application are not inferred from citations or a green run.               |
| PR 4: structured facts, procedures and read-only findings                 | `memory_remember`, `RememberArgs`, provisional capture tests                                                | Implemented through the current agent's tool rather than an additional extraction-model request. This is a deliberate implementation choice. |
| PR 4: distinct failures remain distinct; repeated work is idempotent      | Failure signature plus scope, exact identity lookup, consolidation and repair e2e tests                     | Implemented; cause remains unknown unless explicitly supplied as an assertion.                                                               |
| PR 4: bounded retention and no resurrection                               | Generation guards, transactional writes, episode pruning, vector race tests                                 | Implemented. Only the leader records shared work.                                                                                            |
| PR 4: richer repair explanations improve future tasks                     | Automatic records describe observed repairs; the structured tool can add the concrete explanation           | The mechanism is implemented; future-task usefulness is a release evaluation, not established by metadata tests.                             |
| PR 5: concrete backend and vector lifecycle                               | Offline `LocalSentenceEmbedder`, `SemanticIndex`, model/hash/dimension checks, lifecycle tests              | Implemented as a local pilot, off by default.                                                                                                |
| PR 5: hybrid ranking, bounded relevance and fallback                      | Cosine admission threshold plus reciprocal-rank fusion, scope filters before limits, lexical fallback       | Implemented. A positive fused rank is never the relevance threshold.                                                                         |
| PR 5: shadow comparison and operational limits                            | Project settings in `memory_semantic.rs`, cached worker, bounded backfill                                   | Implemented. Cold model loading is bounded at 15 seconds; warm latency requires measurement on the chosen model/hardware.                    |
| PR 5: held-out semantic/implicit gains and task quality                   | Optional model evaluation inputs and paired-task release gate                                               | Not yet established. No production default is changed on the strength of mock embedding tests.                                               |
| PR 6: replayable case set and reproducibility                             | `memory-cases.json`, `evaluate_memory` example, dataset fingerprint/version, per-case metadata              | 120 synthetic retrieval cases implemented, split by incident (84 development / 36 held out). They are not 120 authentic coding incidents.    |
| PR 6: recall/ranking/no-match and scope metrics                           | Recall@8, MRR, nDCG, no-match injections, prohibited-result flags, timings                                  | Implemented in the runner.                                                                                                                   |
| PR 6: real response/task success gating                                   | `memory-gates.mjs` and tests; paired held-out outcome schema                                                | Implemented; absent/mismatched/duplicate task evidence fails the gate.                                                                       |
| PR 6: pilot/rollback/user controls                                        | Semantic `off`/`shadow`/`hybrid`, `memoryCapture`, existing memory off/edit/delete/clear and new pagination | Implemented. Broader rollout is held until quality, latency and task outcomes are measured.                                                  |

## Evaluation and release decision

The lexical/file baseline on the synthetic set measured development Recall@8
of 82.86% and held-out Recall@8 of 80%; held-out MRR and nDCG@8 were both 0.80.
No-match injection was zero. These results expose paraphrase misses; they do
not satisfy the proposed 90% recall gate and do not measure answer quality.
The dataset contains independent-incident distractors but remains small and
synthetic. Its composition queries have one expected memory, so they must not
be represented as a multi-hop agent reasoning benchmark.

Semantic mode therefore remains off by default. A pilot needs a locally
provisioned, pinned model and a calibrated threshold. No model weights were
available in the supplied project configuration; neither multilingual quality
nor real-model latency has been claimed from deterministic embedding fixtures.
The mock backend tests verify index/ranking lifecycle and scope, not learned
semantic competence.

Before broad rollout, collect and review paired held-out agent results under the
same pinned model, tools and token budget. `memoryGate` requires at least 30
unique pairs, no scope violations, at least 90% retrieval recall, at most 5%
no-match injections, at least ten percentage points of task-success improvement,
and no observed regression on memory-irrelevant tasks. Human review, uncertainty
estimates, scale measurements at 100/1,000/10,000 records and current supported
model placement tests remain required operational evidence. A synthetic fixture
score cannot authorize rollout.

Deferred by measurement rather than silently substituted: ANN storage, graph
traversal, an additional extraction model, and a learned cross-encoder reranker.
The implemented pilot uses exact cosine search and deterministic rank fusion;
introduce the more expensive alternatives only if measured failures justify them.

Other plan details remain explicit limitations: there are no separate rollback
flags for each correctness fix, no historical-branch comparison of all five
proposed agent variants, and no new causal “applied/validated” event inferred
from model output. Existing supplied/read instrumentation remains observational.
Migrations are additive and repeatable, but interrupted-upgrade fault injection
and an old-writer rollback matrix have not been established. These are not
claimed as completed acceptance criteria. The current controls independently
disable capture and semantic ranking while preserving the authoritative store.

## Validation

Validation recorded before the requested immediate commit/push:

- `npm test`: 1,022 tests passed; `npm run typecheck` passed.
- `npm run test:eval`: 63 passed; `npm run test:scripts`: 8 passed.
- `npm run format:check` passed; it is repeated after this review update.
- Normal `cargo nextest` CI-profile core/memory run: 2,658 passed, 27 skipped.
  This includes the generation, cached-packet, credential and stable-cursor
  regressions. The final outside-project fingerprint regression was added
  afterward and is included in the in-progress Landlock rebuild.
- Protocol, eoka, desktop, client and headless CI-profile suites passed.
- The daemon build exhausted linker resources when all integration binaries
  were built together. A target-by-target normal/Landlock rerun is underway.
- Final Landlock core, daemon, workspace doctests, Clippy and evaluation dry-run
  checks are not yet complete at this push. Earlier Clippy inspection identified
  existing warnings outside the memory changes; no final clean claim is made.

The user requested an immediate push while the remaining checks were running.
This commit therefore does **not** claim that all required Rust CI gates passed
on its final source. Linux test tooling and isolated temporary storage were
provisioned without weakening test assertions. No macOS or Windows execution,
real embedding-model quality, or full paired-agent benchmark is claimed.

## Subsequent recall experiments

The [first experiment](long-term-memory-experiments-nov7-2026.md) evaluates real
word-vector representations and stops after three distinct non-improvements.
The [proposal follow-up](long-term-memory-proposal-followup-nov7-2026.md) tests
contextual keys, query fields, candidate depth, source granularity, capture and
incident collapse. Hard coding-task negatives expose false-positive admissions
absent from the original spectroscopy controls. These exploratory results do
not change production defaults or establish real-agent quality gains.

The [conservative implementation review](long-term-memory-safe-improvements-nov7-2026.md)
and [post-implementation rerun](long-term-memory-postimplementation-rerun-nov7-2026.md)
document the exact-copy and observed-file changes, their regression tests, and
the complete replay after the implementation was pushed.
