# verified-code-memory

Evidence-gated long-term memory for coding agents.

## v0.2 improvements

- SQLite FTS5/BM25 lexical retrieval with safely quoted queries
- Retrieval telemetry: candidates, FTS matches, file matches, rejected candidates,
  returned records, latency
- Repository-version invalidation for stale memories
- Conflict resolution: keep one verified memory and deprecate contradictions
- Pluggable `MemoryEmbedder` and `VectorIndex` traits
- Thread-safe `SharedMemoryStore`
- Integration tests, CI, docs, and runnable example

## Retrieval

Two pools feed one ranked answer, and neither stands in for the other: the
memories that share a *specific* word with the task or the failure (the full-text
index), and the memories that name one of the task's files (`src/queue.rs` and
`queue.rs` are the same file; `foo.rs` and `src/notfoo.rs` are not). Files are
looked up through a `memory_files` table of normalized paths, written with each
record, so "which memories are about this path" is an indexed lookup rather than
a scan that parses every record's JSON. A store written before either index
existed is brought up to date when it opens: the file rows are filled from the
records, and an empty full-text index is refilled from the table.

Relevance admits a memory; trust does not. A memory that shares no specific word,
no file and no failure term is not retrieved at all, so a query about something
the project has no memory of answers with nothing instead of with its most
trusted record. Verification state, kind, recency and past successes only order
the memories that already belong to the query.

`RetrievalQuery` carries the caller's own limits — the project, the trust states
it accepts, the tags it excludes (a chat doesn't recall its own work) — and they
are applied before the result limit, so a record the caller may not see can't
take a slot from one it may. Deprecated memories are never retrieved; `list` is
where they show.

The lexical and file pools retain at most 80 and 40 distinct admissible claims.
The indexed scan continues past rejected rows and exact copies to fill those
pools. Copies keep their highest-ranked representative and remain stored for
audit. Scan telemetry counts rows inspected in both pools, including rejected
rows and overlaps; a heavily restricted or repetitive history can require more
reads without increasing the retained pool or the final result limit.

SQLite's `bm25()` is negative and the better a row matches the more negative it
is. `bm25_relevance` keeps that direction (`rank = -4.0` scores above `-0.5`)
and bounds it to `0..1` before the contextual reranker adds its components.

`render_packet(memories, tokens)` renders what a prompt or a tool answer shows: at
most `tokens`' worth (`estimate_tokens`, four characters to a token), each record
with its id — what `memory_get` takes — its applicability, its evidence and its
files. A record that would not fit leaves room for later complete claims. Any
remaining allowance goes to fetch pointers for omitted records, in rank order,
and a truncation notice if it fits. The header, pointers and notice all count
toward the allowance; very small allowances can produce an empty packet.
`render_context` is the same rendering with no bound.

## Evidence, applicability and freshness

Three questions a record answers, kept apart because they have different answers:

- **verification** — what evidence once supported the claim;
- **applicability** — whether that is expected to hold for the code as it is now
  (`current`, `needs_revalidation`, `unknown`);
- **provenance** — what kind of source asserted it (user assertion, code
  observation, test result, generated deduction).

`verify(id, evidence)` is claim-scoped: it records the claim hash of the text the
evidence was about, so an edit cannot inherit trust — and a summary of that:

- editing a memory keeps its verification and evidence for audit, marks the claim
  `needs_revalidation` and the source as the user's assertion;
- `mark_needs_revalidation` is what a nearby failure or a changed revision earns,
  never `deprecate`: only a contradiction deprecates;
- `mark_stale_for_version` asks for revalidation rather than invalidating every
  versioned record on a new commit;
- `record_outcome` counts a use and nothing else, and `record_retrieval` stamps
  `last_retrieved_at`; recency reads `content_updated_at`, the claim's own stamp,
  so no amount of recall or association makes a claim look newer.

A store file written by an earlier build of this crate is brought up to date when
it opens: missing columns are added, claim hashes backfilled from the records,
the file index filled, and then — only after that — the full-text index created
and refilled. The order matters: FTS5 keeps an external-content index in step
through the triggers, and a trigger firing at a row an empty index never held is
what SQLite reports as `SQLITE_CORRUPT_VTAB`.

## Native ONNX embeddings

`embedding::OnnxMiniLm` loads the local Chroma `all-MiniLM-L6-v2` ONNX export
and tokenizer directly in Rust using tract and tokenizers. It implements
`MemoryEmbedder` with 384-dimensional normalized vectors and reuses its compiled
CPU model. No Python, child process or separately installed runtime is used.
The application provisions the pinned model from its dedicated GitHub release;
this crate's encoder only reads local files and never downloads on inference.

For a stored note longer than the encoder's character window, background indexing
also embeds its final nonempty paragraph (bounded to one window). Hybrid retrieval
can append a matching conclusion into unused result slots, after all existing
results. Conclusion lookup does not displace incumbents, and strict primary-vector
search keeps its existing behavior.
The complete stored claim must still fit the prompt budget to be delivered.
Conclusion vectors are derived data bound to the model and current claim hash;
edits, deletion, scope and visibility checks apply to both views. Existing primary
vectors gain conclusions during the next backfill without a model download or
reindex of unchanged primary text. This adds at most one inference and one vector
per long claim; it does not index every intermediate paragraph.

For a single-paragraph note, a complete final sentence can supply that extra view
when it starts outside the encoder window and fits entirely inside one window.
Incomplete fragments and abbreviation-like boundaries are excluded. Existing
paragraph views retain priority, and the complete stored note must still fit the
packet allowance. This uses the same model and at most one conclusion vector.

When a long task hides the failure text, hybrid retrieval reuses its independent
diagnostic embedding to search these conclusions too. Existing results and ordinary
conclusion matches keep priority. If slots remain, this adds one vector-table search
without another inference call.

For a full hybrid pool, a one- or two-word request naming exactly one complete
compound identifier may replace the final result with an eligible complete match
when none is already represented. Leading results retain their ranks. Single-slot
lookups, explicit failure context, exact incumbent titles, longer requests,
registered collection words, digits and wildcards keep their existing pools.
This focused rule does not apply to lexical or strict vector searches.

Requests naming two to four compound identifiers can cover each requested name
within the same pool. Existing named matches and exact titles mentioned in the
request keep their positions; only other trailing slots may be used. The request
may contain at most one other term. If a name has no eligible match or the slots
cannot cover all missing names, the pool stays intact. The same collection,
failure, numeric and wildcard protections apply, with no extra inference.
Spaces, commas and semicolons separate names and literal context terms; qualified
names are supported. The request may contain at most five nonempty terms after
splitting, including repeated mentions. Duplicate names need no additional slot.
With supplied files, this atomic planner accepts one to four names and protects
every incumbent whose stored paths overlap any supplied path. It preserves their
ranks even at the tail; too few remaining slots aborts the entire plan. File
matching uses the same normalized whole-segment rules as lexical retrieval.
After planning names, unused unprotected positions can cover missing supplied
paths with eligible file claims from the existing indexed collector. A claim
can cover several hints. Unknown paths with no eligible memory are skipped;
insufficient slots or a file lookup error preserves the successful name plan.
File additions are applied together, without moving earlier named facts.
Complete name matching permits a non-generic two-letter suffix after two distinct
longer components, including CamelCase, qualified and snake spellings. Every
component remains a literal conjunctive requirement. Middle short components,
generic words, digits, repeated single anchors and one-letter suffixes stay out.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo doc --no-deps
cargo run --example basic
```
