# Preserving source requests in episode capture

Baseline: `2b2ff4a39e32eb8a0f71d8880862bac8e1724199` on
`ltm-improvements-nov10-2026`.

**Reject both enrichment formats.** Quoting the source request improves new
synthetic history lookup from **18/32 to 32/32**, but changes **11 negative
packets** and worsens **six protected ranks**. The application capture path,
MiniLM model, existing records and verification behavior remain unchanged.
This is a source-information experiment, not an accepted application recall gain.

## Capture-path evidence

The application's `reflect` function stores an 80-character title from the task's
first nonblank line, the run outcome, changed file names and check statuses.
It does not retain the rest of the task. Automatic repair records retain a
failure signature and changed files, but their fix text does not describe the
corrective action. Neither path can establish an omitted action or cause from a
green check alone. The optional `memory_remember` tool can capture richer claims,
which remain provisional; this study does not change that tool.

For sixteen secret-free synthetic run inputs, a separately compiled probe executes
the exact extracted application `reflect`, `brief` and `redact_secrets` functions,
using the real `NewMemory` type and minimal command/status `CheckResult` inputs.
The native evaluator's legacy projection matches the resulting title/content
bytes exactly. The protocol pins the source-function hash. This checks projection
fidelity, **not full run orchestration, write leases, extraction accuracy or agent
usefulness**. The complete probe source and hashes are in the machine report.

## Frozen cases and distinct capture comparison

The [initial protocol](../../scripts/eval/memory-episode-capture-protocol.json) and
[96-case fixture](../../scripts/eval/memory-episode-capture-cases.json) precede
fixture scoring. The corpus contains sixteen synthetic source requests, changed
files, outcomes and check results. Eight incidents belong to development and eight
to the reserved split. All source episodes remain in both retrieval corpora.
Reserved query rows remain unscored.

Each incident supplies two historical-request queries plus literal-title and file
controls. Each split has 32 positives and sixteen no-match requests, including
adjacent topics. The title is a plausible short maintenance-ticket identifier;
the actual problem and intended work appear on the following line. These cases
isolate information lost from multiline intake. They are assistant-authored inputs
and labels, not real session traces or independently adjudicated relevance data.
No future query is inserted into a stored episode.

Queries ask which earlier task concerned a problem or requested work. Retrieving
such provenance does not show that the requested work was implemented. Both
candidate formats retain the full legacy content, followed by an explicitly
labeled exact source quotation. Existing title, outcome, file and check fields
stay intact. Overlong source tasks are rejected rather than silently clipped.

The added native evaluator support uses episode kind, generated-deduction source
kind and the actual source file associations. Existing fixtures retain their
project-fact default. This avoids modeling a captured episode as a reusable project
fact or substituting its query file for the files actually captured.

These are **alternative new-capture representations of one source input**; adding
text changes the generated claim hash. The unchanged general comparator correctly
reports 32 positive case-definition changes for each candidate. We retain those
errors in the report and do not normalize them away or claim that existing hashes
were preserved by a production rewrite. The separate capture comparison tracks
stable source-episode identity, complete delivery, protected ranks, native safety
fields and exact negative returned/rendered/full-text IDs and order. Both trials
also fail these source-delivery gates. The unchanged general comparator continues
to govern all existing frozen fact suites.

## Trials and failure localization

Use native pinned MiniLM at cutoff 0.30, short-query scale 11/12, eight results,
a 1,000-estimated-token packet and fixed 36-hour claim age. No model, cutoff,
query, label, result bound or budget is tuned between the capture arms.

| Arm                                                                           | Complete historical-episode delivery | Negative cases returning any item | Changed negative packets | Previously complete primary rank losses |
| ----------------------------------------------------------------------------- | -----------------------------------: | --------------------------------: | -----------------------: | --------------------------------------: |
| Existing episode format                                                       |                                18/32 |                              2/16 |                        — |                                       — |
| Append `Requested task (not proof of implementation):` and source text        |                                32/32 |                             11/16 |                       11 |                                       6 |
| Append `Intent only (not an implementation claim):` and identical source text |                                32/32 |                             11/16 |                       11 |                                       6 |

All sixteen literal/file controls are initially delivered, along with two of the
sixteen implicit history requests. Both enriched formats deliver all 32 episodes.
The fourteen gains are correlated across eight development incidents; they are
not fourteen successful agent repairs. No previously complete answer is lost,
but six primary ranks worsen: two literal controls and four file controls.
One literal control falls from rank one to rank five.

The second arm is an explicitly [registered adaptive follow-up](../../scripts/eval/memory-episode-capture-followup-protocol.json)
after the first rejection. It tests whether the generic indexed heading supplied
spurious matching terms such as `task` and `requested`. Keeping the source text
identical and changing only the heading does not remove the observed regressions.

After rejection, a **diagnostic lexical-only ablation** compares the old and second
formats without a model. Delivery rises 19/32 to 32/32, and it reproduces the same
eleven changed negative cases. It has no protected primary-rank losses. Thus the
negative-admission problem already exists in lexical retrieval after enrichment;
the six rank losses occur only in the measured hybrid arm. This does not identify
a single responsible token or prove that only the semantic component causes
ranking changes, because the underlying documents also change lexical scores.
The ablation does not select another policy or expose reserved outcomes.

The baseline's two negative returns remain failures. Nine additional negatives
start returning items, and the two already returning items change their packets.
Exact negative preservation rejects all eleven changes, including added records
when a negative already had results.

## Regression checks and scope

Three hybrid capture arms and two lexical arms cover **240 development views**.
A further **251 existing MiniLM development views** cover original/controls,
fresh-semantic, earlier encoder and BGE suites. Every existing case reproduces
all non-timing fields exactly, and all four unchanged per-record audits pass.
Original scores remain 65/70, 27/30, 29/32 and 31/32 respectively. No existing suite
or denominator is replaced by the new source-history score.

The native evaluator's 29 focused tests and Clippy with `-D warnings` pass.
Broader crate, core and frontend tests remain skipped as requested. No production
source, dependency, model asset, application setting, download or CI release
changes. The full historical/oversized matrix and both new reserved suites need
no candidate scoring after development rejection.

This study does not test real-session source selection, secret handling of a new
capture path, restricted-file orchestration, claim verification or end-to-end
agent behavior. Its source requests are synthetic and secret-free; the application
redactor and capture lifecycle have not been modified. A successful retrieval
projection alone would not authorize promoting requests into verified repairs.

## Reproduction

Build `evaluate_memory` from `src-tauri/`, then use a development config from the
repository root with the existing pinned MiniLM model/revision:

```json
{
  "dataset_file": "scripts/eval/memory-episode-capture-cases.json",
  "split": "development",
  "deterministic": true,
  "memory_budget": 1000,
  "short_query_words": 2,
  "short_query_scale": 0.9166666666666666,
  "episode_capture_projection": "legacy_episode",
  "diagnostics": true,
  "diagnostic_pool_limit": 32
}
```

```sh
ROVIK_MEMORY_EVAL_MODEL="$MINILM_MODEL" \
ROVIK_MEMORY_EVAL_DIMENSIONS=384 \
ROVIK_MEMORY_EVAL_REVISION=chroma-minilm-l6-v2-913d7300-title-claim-native-v1 \
ROVIK_MEMORY_EVAL_THRESHOLD=0.3 \
src-tauri/target/debug/examples/evaluate_memory --experiment-config config.json
```

Repeat with `requested_episode` and `intent_episode`. For lexical-only diagnostics,
remove both diagnostic fields and unset the `ROVIK_MEMORY_EVAL_*` variables.
The [machine report](long-term-memory-episode-capture-nov10-2026.json) records
source/fixture hashes, reference-probe source, native configurations/output hashes,
per-case evidence, every changed negative, protected-rank losses and the unmodified
comparator's errors. Projection modes validate frozen legacy content against their
raw inputs, quote only the bounded source task, and leave query text unchanged.

Preserving source information helps identify historical requests. Mixing that
information directly into the ordinary episode search text also broadens admission
and changes protected rankings. The next capture-quality study should evaluate
observed corrective actions and their evidence separately from intended work,
with provenance distinct from reusable verified claims. It needs independently
judged relevance and actual agent-use evidence before a new production admission
path. Another model cutoff sweep does not address the lexical failures measured
here.
