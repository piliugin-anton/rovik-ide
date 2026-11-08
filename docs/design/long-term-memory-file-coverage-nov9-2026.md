# Harder file-coverage cases and recall gains

Baseline: `7854861`, branch `ltm-improvements-nov9-2026`.

The existing file-focused suite improves from **8/16 to 16/16 complete answers**
on both development and exposed reserved splits. The historical dense suite stays
28/28 on both splits. These scores remain separate from the new harder cases.

## Suite extension

The frozen extension adds **104 cases and 16 retention controls**, using eight
additional source-grounded symbols: `OnnxMiniLm`, `SemanticIndex`,
`RetrievalWeights`, `RetrievalTelemetry`, `MemorySettings`, `MemoryState`,
`MemoryEntry` and `RetrievedMemory`. Claims and source hashes refer to the baseline
repository. These are exposed repair cases, not blind or live-agent validation.

Each split has 32 positive cases and 20 negative controls. Positive cases combine
three or four named facts, qualified names, multiple file hints, normalized and
duplicate paths, unknown paths and visibility filters. Complete answers require
every named peer and file claim. Retention controls request more facts than eight
slots can hold and guard every previously delivered fact and rank instead of
entering the complete-answer headline.

| Hybrid suite             | Development before → after | Reserved before → after |
| ------------------------ | -------------------------: | ----------------------: |
| Existing file-focused    |               8/16 → 16/16 |            8/16 → 16/16 |
| New harder file coverage |                3/32 → 6/32 |           20/32 → 30/32 |

The remaining failures are retained. The additional incidents and their query
variants are correlated; the different split scores do not establish independent
generalization. Existing negative result IDs/order remain unchanged in completed
evaluations, without requiring those pools to be empty.

The evaluator now supports an optional case-level `files` array alongside legacy
`file`. Baseline and candidate use that same parser; historical cases never used
the new field. Intent-only evaluation omits both fields. The fixtures and initial
[protocol](../../scripts/eval/memory-file-coverage-protocol.json) were frozen
before retrieval trials.

## Experiments and selected change

Earlier broad replacement trials lost requested facts. The later focused planner
protected incumbent named and file facts, but ordinary lexical ranking could
discard a file claim before hybrid fusion. The new policy reuses the existing
indexed, filtered file collector to cover missing supplied paths within that
same conservative focused-request scope.

The first trial planned names and file coverage as one atomic set. A development
counterexample rejected it: seven protected file incumbents leave one slot for a
missing name, but not for another file claim. The baseline supplies the name;
the combined plan cancels that gain. The original runtime passes this contract,
and the rejected trial fails it. Its patch and evidence are recorded in the report.

The selected policy preserves the original name plan and its exact slot
assignments. Missing file coverage uses only the remaining unprotected positions.
One eligible claim may cover several supplied paths; unknown paths with no
eligible memory are skipped. If file additions cannot all fit or a file lookup
fails, the name-only plan survives. Existing leaders, named facts, literal titles
and file matches retain their positions. Collection/failure/numeric/wildcard,
trust, repository, tag and caller protections remain unchanged.

No model, inference, cutoff, schema, stored claim, result limit or packet budget
changes. The pinned model release and download behavior stay unchanged.

## Completed validation and stopped work

Before reserved scoring, all **1,406 development policy views** and the crate
contracts passed. Across **1,883 completed development/reserved views**, there
are **29 complete-answer gains**, no delivered-fact or rank losses, negative
identity/order changes, prohibited results or eight-record/1,000-token overruns.
Of 1,643 completed historical views, 1,619 deterministic outputs are identical;
the other views gain required file facts or change unrequested file context.

At the user's explicit request, the remaining evaluation was stopped and this
change was committed and pushed. **825 planned historical reserved views remain
unscored**; this report does not claim a complete historical reserved regression
pass. The final format check was not run after that instruction.

Already completed checks: **158 Rust all-target tests**, both explicitly enabled
native ONNX tests, the doctest, clippy with warnings denied, **1,046 frontend
tests** and type checking. Twelve focused tests also passed ten runs with fresh
UUIDs. Tests cover scarce capacity, multiple paths, duplicate hints, normalized
whole-segment matching and filtering before the file pool bound. Rust crates were
checked individually.

The [machine-readable report](long-term-memory-file-coverage-nov9-2026.json)
contains completed results, pending groups, source/fixture hashes, deterministic
fingerprints and rejected-trial evidence. Reproduction uses the existing native
`evaluate_memory` example, deterministic splits, pinned 384-dimension MiniLM at
threshold 0.3, short-query words two and scale 11/12, eight results and 1,000 tokens.
Concurrent checks do not establish a latency improvement. Unrequested trailing
records can change; measured retention is not a universal precision guarantee.
